use std::fmt;
use std::path::PathBuf;

/// Errors produced by the crust-core library. The library never exits the
/// process or panics on bad input — fallible entry points return this.
#[derive(Debug)]
pub enum Error {
    /// The scene path is not valid UTF-8 (required by the openusd API).
    NonUtf8Path(PathBuf),
    /// Opening or parsing the USD stage failed. The openusd error is kept
    /// whole, as the [`source`](std::error::Error::source), rather than
    /// flattened to its message.
    UsdOpen {
        path: PathBuf,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The requested frame is not a finite time code (`NaN` or `±inf`).
    /// Refused rather than passed on: `NaN` compares false against the
    /// stage's time range, so it would slip past the range check and reach
    /// time-sample interpolation and the sampler seed as garbage.
    InvalidFrame(f64),
    /// The requested camera is not an absolute prim path (`/root/cam`).
    /// Refused before the stage is opened, like a bad frame.
    InvalidCameraPath(String),
    /// The requested camera path names no `UsdGeomCamera` on the stage.
    /// An error rather than a fallback: rendering a sequence through the
    /// wrong camera is worse than not rendering it. Carries every camera the
    /// stage does have, since a production camera's path is usually buried
    /// in a referenced cache and cannot be guessed.
    CameraNotFound {
        path: String,
        available: Vec<String>,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NonUtf8Path(path) => {
                write!(f, "USD path is not valid UTF-8: {}", path.display())
            }
            Error::UsdOpen { path, source } => {
                write!(f, "failed to open USD stage {}: {}", path.display(), source)
            }
            Error::InvalidFrame(frame) => {
                write!(
                    f,
                    "invalid frame {frame}: a time code must be a finite number"
                )
            }
            Error::InvalidCameraPath(path) => {
                write!(
                    f,
                    "invalid camera path '{path}': expected an absolute prim path such as /root/cam"
                )
            }
            Error::CameraNotFound { path, available } => {
                write!(f, "no UsdGeomCamera at {path} on this stage")?;
                if available.is_empty() {
                    write!(f, " (it has no cameras)")
                } else {
                    write!(f, "; its cameras are:")?;
                    for c in available {
                        write!(f, "\n  {c}")?;
                    }
                    Ok(())
                }
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::UsdOpen { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}
