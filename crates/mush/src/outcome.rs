//! What one executed command produced.
//!
//! A Mush command can finish, or it can be interrupted by its deadline. Callers
//! need that distinction as data rather than as an error string, because an
//! agent harness chooses its next step from it and a hung tool call gives it
//! nothing to choose from.

/// Exit status used by convention when a command is interrupted by its deadline.
pub const TIMEOUT_STATUS: i32 = 124;
/// Exit status used by convention when a caller cancelled the command (SIGINT).
pub const CANCELLED_STATUS: i32 = 130;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// The command ran to completion.
    Done,
    /// The command exceeded its budget and the Spore was interrupted.
    Timeout,
    /// A caller stopped the command before it finished.
    Cancelled,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Done => "done",
            State::Timeout => "timeout",
            State::Cancelled => "cancelled",
        }
    }

    /// Whether the command did not return on its own.
    ///
    /// A harness that only reads one field should read this one: a `true` here
    /// means the work did not finish and must not be treated as a result.
    pub fn blocking(self) -> bool {
        matches!(self, State::Timeout | State::Cancelled)
    }
}

#[derive(Debug)]
pub struct Executed {
    pub output: String,
    pub stderr: String,
    pub exit: bool,
    pub status: i32,
    pub state: State,
}

impl Executed {
    /// A command a caller stopped before it finished.
    pub fn cancelled() -> Self {
        Self {
            output: String::new(),
            stderr: "mush: command cancelled\n".into(),
            exit: true,
            status: CANCELLED_STATUS,
            state: State::Cancelled,
        }
    }

    pub fn done(output: String, stderr: String, exit: bool, status: i32) -> Self {
        Self {
            output,
            stderr,
            exit,
            status,
            state: State::Done,
        }
    }
}
