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
    /// A camera could not be built from the given matrices (non-invertible
    /// view/projection, or a projection kind the camera model cannot
    /// express, e.g. orthographic).
    InvalidCamera(String),
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
            Error::InvalidCamera(message) => {
                write!(f, "cannot build camera from matrices: {message}")
            }
        }
    }
}

impl std::error::Error for Error {}
