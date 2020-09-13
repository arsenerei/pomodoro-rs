use std::fmt;
use std::fmt::{Display, Formatter};
use std::io;
use std::io::{Cursor, Write};
use std::ops::SubAssign;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use rodio::Source;
use structopt::StructOpt;
use termion::event::Key;
use termion::input::TermRead;
use termion::raw::IntoRawMode;

// `mspc`'s tx and rx need to send and receive something of the same type. We use `Event`
// here to wrap our mixed types in a container to appease the compiler. I haven't fully
// groked how enums of mixed types work.
enum Event {
    Key(Key),
}

#[derive(PartialEq)]
enum Mode {
    Pomodoro,
    Break,
    AwaitingPomodoroEndedAck,
    PomodoroEndedAcked,
    AwaitingBreakEndedAck,
    BreakEndedAcked,
    SystemError,
    End,
}

struct Interval {
    elapsed: Duration,
    duration: Duration,
}

impl Interval {
    fn from_secs(secs: u64) -> Interval {
        Interval {
            elapsed: Duration::from_secs(0),
            duration: Duration::from_secs(secs),
        }
    }

    fn has_ended(&self) -> bool {
        self.elapsed >= self.duration
    }
}

impl SubAssign<Duration> for Interval {
    fn sub_assign(&mut self, rhs: Duration) {
        // count up on `elapsed` because Duration can't be negative
        self.elapsed += rhs;
    }
}

impl Display for Interval {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let secs = if self.elapsed >= self.duration {
            0
        } else {
            (self.duration - self.elapsed).as_secs()
        };

        write!(f, "{:02}:{:02}", secs / 60, secs % 60)
    }
}

// TODO: add more descriptions
#[derive(StructOpt)]
#[structopt(name = "pomodoro")]
struct Opt {
    #[structopt(short, long, default_value = "25")]
    pomodoro_duration: u8,

    #[structopt(short, long, default_value = "4")]
    break_duration: u8,

    #[structopt(short, long, default_value = "4")]
    max_pomodoros: u8,
}

// include_bytes! adds the song to the binary
static GONG: &'static [u8] = include_bytes!("indian-gong.mp3");

// TODO: add option to play synchronously when ending
fn play_sound() -> () {
    let device = rodio::default_output_device().unwrap();
    let cursor = Cursor::new(GONG);
    let source = rodio::Decoder::new(cursor).unwrap();
    let source = source.take_duration(Duration::from_secs(20)); // there's something off about the duration
    rodio::play_raw(&device, source.convert_samples());
}

fn main() {
    let opt = Opt::from_args();

    let break_duration: u64 = opt.break_duration as u64 * 60;
    let pomodoro_duration: u64 = opt.pomodoro_duration as u64 * 60;
    let max_pomodoros = opt.max_pomodoros;

    // We create a channel for communication. We can have as many `tx`s as we want, but
    // only a single `rx`.
    let (tx, rx) = channel();

    thread::spawn(move || {
        let stdin = io::stdin();
        for c in stdin.keys() {
            // this means it has closed from the other side
            if tx.send(Event::Key(c.unwrap())).is_err() {
                break;
            }
        }
    });

    // NB: stdout must be in raw mode for individual keypresses to work
    let mut stdout = io::stdout().into_raw_mode().unwrap();

    write!(stdout, "{}", termion::cursor::Hide).unwrap();

    // TODO: write tests
    let mut interval = Interval::from_secs(pomodoro_duration);
    let mut pomodoro_count = 1;
    let mut break_count = 1;
    let mut paused = false;
    let mut mode = Mode::Pomodoro;
    loop {
        let start = Instant::now();
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Event::Key(_)) if mode == Mode::AwaitingPomodoroEndedAck => {
                mode = Mode::PomodoroEndedAcked
            }
            Ok(Event::Key(_)) if mode == Mode::AwaitingBreakEndedAck => {
                mode = Mode::BreakEndedAcked
            }
            Ok(Event::Key(Key::Char('q'))) | Ok(Event::Key(Key::Ctrl('c'))) => break,
            Ok(Event::Key(Key::Char('p'))) => paused = !paused,
            Err(RecvTimeoutError::Disconnected) => mode = Mode::SystemError,
            _ => (),
        }

        if !paused && (mode == Mode::Pomodoro || mode == Mode::Break) {
            // per https://rust-lang-nursery.github.io/rust-cookbook/datetime/duration.html#measure-the-elapsed-time-between-two-code-sections
            interval -= start.elapsed();
        }

        // The nice thing about using match with Enums in Rust is you get
        // exhaustive match checking. This ensures you're covering all cases.
        match mode {
            Mode::Pomodoro if interval.has_ended() => {
                if pomodoro_count == max_pomodoros {
                    play_sound();
                    mode = Mode::End;
                } else {
                    play_sound();
                    mode = Mode::AwaitingPomodoroEndedAck;
                }
            }
            Mode::PomodoroEndedAcked => {
                pomodoro_count += 1;
                interval = Interval::from_secs(break_duration);
                mode = Mode::Break;
            }
            Mode::Break if interval.has_ended() => {
                play_sound();
                mode = Mode::AwaitingBreakEndedAck;
            }
            Mode::BreakEndedAcked => {
                break_count += 1;
                interval = Interval::from_secs(pomodoro_duration);
                mode = Mode::Pomodoro;
            }
            _ => (),
        }

        // TODO: control the rate of writing independently from tick?
        // \r\n: https://stackoverflow.com/a/48497050
        // In raw_mode \n keep the cursor at the same column; \r is needed to put the cursor at the
        // beginning of the line.
        match mode {
            Mode::Pomodoro => {
                if paused {
                    write!(
                        stdout,
                        "{}Pomodoro {}: {} (paused)\r",
                        termion::clear::CurrentLine,
                        pomodoro_count,
                        interval,
                    )
                    .unwrap();
                } else {
                    write!(
                        stdout,
                        "{}Pomodoro {}: {}\r",
                        termion::clear::CurrentLine,
                        pomodoro_count,
                        interval,
                    )
                    .unwrap();
                }
            }
            Mode::Break => {
                if paused {
                    write!(
                        stdout,
                        "{}Break {}: {} (paused)\r",
                        termion::clear::CurrentLine,
                        break_count,
                        interval,
                    )
                    .unwrap();
                } else {
                    write!(
                        stdout,
                        "{}Break {}: {}\r",
                        termion::clear::CurrentLine,
                        break_count,
                        interval,
                    )
                    .unwrap();
                }
            }
            Mode::AwaitingPomodoroEndedAck => write!(
                stdout,
                "{}Pomodoro ended. Press key to begin break.\r",
                termion::clear::CurrentLine,
            )
            .unwrap(),
            Mode::AwaitingBreakEndedAck => write!(
                stdout,
                "{}Break ended. Press key to begin a new pomodoro.\r",
                termion::clear::CurrentLine,
            )
            .unwrap(),
            Mode::SystemError => write!(
                stdout,
                "{}System error. Shutting down.\r\n",
                termion::clear::CurrentLine,
            )
            .unwrap(),
            Mode::End => {
                write!(stdout, "{}Done.\r\n", termion::clear::CurrentLine).unwrap();
                break;
            }
            _ => unreachable!(),
        }
        stdout.flush().unwrap();
    }

    write!(stdout, "{}", termion::cursor::Show).unwrap();
    stdout.flush().unwrap();
}
