//! [`AssetError`]: why a file could not be decoded.
//!
//! The [`AssetLoader`](crust_core::AssetLoader) contract is that `None` means
//! "fall back" — a surface on its constants, a light untextured — and that
//! stays. What this adds is inside crust-assets: the decoders return the
//! failure rather than a bare `None` (or a `String`), so it is logged once, as
//! a `WARN`, at the one place it becomes the seam's `None` (`FileAssets`), and
//! a real decode failure is never confused with a texture declined on purpose
//! (`CRUST_TEX=0`, a preload policy), which is not an error at all.

use std::path::PathBuf;

/// A file that could not be turned into an asset.
#[derive(Debug)]
pub enum AssetError {
    /// The file could not be opened or read.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The image decoder refused it.
    Image {
        path: PathBuf,
        source: image::ImageError,
    },
    /// The EXR decoder refused it.
    Exr {
        path: PathBuf,
        source: exr::error::Error,
    },
    /// The Ptex reader refused it.
    Ptex { path: PathBuf, source: ptex::Error },
    /// It decoded, but into nothing usable: `reason` says what.
    Unusable { path: PathBuf, reason: String },
}

impl AssetError {
    pub(crate) fn unusable(path: &std::path::Path, reason: impl Into<String>) -> AssetError {
        AssetError::Unusable {
            path: path.to_path_buf(),
            reason: reason.into(),
        }
    }

    pub(crate) fn io(path: &std::path::Path) -> impl FnOnce(std::io::Error) -> AssetError {
        let path = path.to_path_buf();
        move |source| AssetError::Io { path, source }
    }

    pub(crate) fn image(path: &std::path::Path) -> impl FnOnce(image::ImageError) -> AssetError {
        let path = path.to_path_buf();
        move |source| AssetError::Image { path, source }
    }

    pub(crate) fn exr(path: &std::path::Path) -> impl FnOnce(exr::error::Error) -> AssetError {
        let path = path.to_path_buf();
        move |source| AssetError::Exr { path, source }
    }

    pub(crate) fn ptex(path: &std::path::Path) -> impl FnOnce(ptex::Error) -> AssetError {
        let path = path.to_path_buf();
        move |source| AssetError::Ptex { path, source }
    }

    /// The file the error is about.
    pub fn path(&self) -> &std::path::Path {
        match self {
            AssetError::Io { path, .. }
            | AssetError::Image { path, .. }
            | AssetError::Exr { path, .. }
            | AssetError::Ptex { path, .. }
            | AssetError::Unusable { path, .. } => path,
        }
    }
}

impl std::fmt::Display for AssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let path = self.path().display();
        match self {
            AssetError::Io { source, .. } => write!(f, "could not read {path}: {source}"),
            AssetError::Image { source, .. } => write!(f, "could not decode {path}: {source}"),
            AssetError::Exr { source, .. } => write!(f, "could not decode EXR {path}: {source}"),
            AssetError::Ptex { source, .. } => write!(f, "could not read Ptex {path}: {source}"),
            AssetError::Unusable { reason, .. } => write!(f, "{path}: {reason}"),
        }
    }
}

impl std::error::Error for AssetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AssetError::Io { source, .. } => Some(source),
            AssetError::Image { source, .. } => Some(source),
            AssetError::Exr { source, .. } => Some(source),
            AssetError::Ptex { source, .. } => Some(source),
            AssetError::Unusable { .. } => None,
        }
    }
}
