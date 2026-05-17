use std::time::SystemTime;

/// Events produced by the polling loop and consumed by the TUI state.
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// A new port forward has been established.
    ForwardStarted {
        remote_port: u16,
        local_port: u16,
        label: Option<String>,
    },
    /// A port forward was torn down (remote stopped listening or config removed).
    ForwardStopped { remote_port: u16, reason: String },
    /// A poll cycle completed successfully.
    PollOk { discovered: usize },
    /// A poll cycle failed.
    PollError { message: String },
    /// An ssh -L process died unexpectedly.
    ForwardDied { remote_port: u16 },
    /// Generic log message.
    Log { level: LogLevel, message: String },
    /// Clean shutdown was requested (reserved for future use).
    #[allow(dead_code)]
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// A timestamped log entry stored in the TUI log panel.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub time: SystemTime,
    pub level: LogLevel,
    pub message: String,
}

impl LogEntry {
    pub fn new(level: LogLevel, message: impl Into<String>) -> Self {
        Self {
            time: SystemTime::now(),
            level,
            message: message.into(),
        }
    }
}
