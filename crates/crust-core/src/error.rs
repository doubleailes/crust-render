use std::fmt;
use std::path::PathBuf;

/// Errors produced by the crust-core library. The library never exits the
/// process or panics on bad input — fallible entry points return this.
#[derive(Debug)]
pub enum Error {
    /// The scene path is not valid UTF-8 (required by the openusd API).
    NonUtf8Path(PathBuf),
    /// Opening or parsing the USD stage failed.
    UsdOpen { path: PathBuf, message: String },
    /// A resume checkpoint does not match the scene/settings being
    /// rendered (dimensions, settings fingerprint, or malformed buffers).
    CheckpointMismatch { reason: String },
    /// Checkpoint/resume is not supported for this render mode.
    CheckpointUnsupported(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NonUtf8Path(path) => {
                write!(f, "USD path is not valid UTF-8: {}", path.display())
            }
            Error::UsdOpen { path, message } => {
                write!(f, "failed to open USD stage {}: {}", path.display(), message)
            }
            Error::CheckpointMismatch { reason } => {
                write!(f, "resume checkpoint does not match this render: {reason}")
            }
            Error::CheckpointUnsupported(what) => {
                write!(f, "checkpoint/resume is not supported here: {what}")
            }
        }
    }
}

impl std::error::Error for Error {}
