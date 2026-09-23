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
    /// The requested frame is not a finite time code (`NaN` or `±inf`).
    /// Refused rather than passed on: `NaN` compares false against the
    /// stage's time range, so it would slip past the range check and reach
    /// time-sample interpolation and the sampler seed as garbage.
    InvalidFrame(f64),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NonUtf8Path(path) => {
                write!(f, "USD path is not valid UTF-8: {}", path.display())
            }
            Error::UsdOpen { path, message } => {
                write!(
                    f,
                    "failed to open USD stage {}: {}",
                    path.display(),
                    message
                )
            }
            Error::InvalidFrame(frame) => {
                write!(
                    f,
                    "invalid frame {frame}: a time code must be a finite number"
                )
            }
        }
    }
}

impl std::error::Error for Error {}
