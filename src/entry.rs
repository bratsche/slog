//! The source-independent log entry every source produces and every output consumes.

use chrono::{DateTime, Local};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Default,
    Error,
    Fault,
}

impl Level {
    pub const ALL: [Level; 5] = [
        Level::Debug,
        Level::Info,
        Level::Default,
        Level::Error,
        Level::Fault,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Default => "default",
            Level::Error => "error",
            Level::Fault => "fault",
        }
    }

    pub fn parse(s: &str) -> Option<Level> {
        Level::ALL
            .into_iter()
            .find(|l| l.as_str().eq_ignore_ascii_case(s))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub time: DateTime<Local>,
    pub level: Level,
    /// Process name (basename of the executable).
    pub process: String,
    pub pid: u32,
    /// Empty when the message was not logged with an os_log subsystem.
    pub subsystem: String,
    pub category: String,
    /// Image (binary or library) that emitted the message.
    pub library: String,
    pub thread: u64,
    pub message: String,
}

/// Basename of a path, used for process and library names.
pub fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}
