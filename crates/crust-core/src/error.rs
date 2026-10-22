use std::fmt;
use std::path::PathBuf;

use crate::region::PixelRect;

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
    /// The requested render pass
    /// ([`UsdImportOptions::pass`](crate::UsdImportOptions::pass)) is not an
    /// absolute prim path, or names no `RenderPass` prim on the stage. An error
    /// rather than a fallback, like a missing camera: rendering a pass without
    /// its holdouts writes a plate that looks right and composites wrong.
    /// Carries the path and why.
    PassNotFound { path: String, why: String },
    /// A group import of passes
    /// ([`Scene::from_usd_passes`](crate::Scene::from_usd_passes)) was given
    /// no pass to import.
    NoPasses,
    /// The host's working colour space
    /// ([`UsdImportOptions::working_space`](crate::UsdImportOptions::working_space),
    /// [`color::working_space`](crate::color::working_space)) is not a
    /// scene-linear space of the OCIO config; carries the reason. An error
    /// rather than a fallback, like a bad camera: rendering in a space other
    /// than the one asked for would mislabel every pixel.
    InvalidWorkingSpace(String),
    /// The OCIO config a host asked for
    /// ([`color::use_config`](crate::color::use_config)) cannot be used: it
    /// does not load, lacks a space crust needs, or another config is
    /// already in use. Carries the reason.
    InvalidOcioConfig(String),
    /// A render region
    /// ([`RenderSettings::with_region`](crate::RenderSettings::with_region))
    /// that holds no pixel of the frame once clipped to it. Refused rather
    /// than rendered as an empty image.
    EmptyRegion {
        region: PixelRect,
        width: usize,
        height: usize,
    },
    /// A light path inspection ([`inspect::run`](crate::inspect::run),
    /// [`inspect::check_request`](crate::inspect::check_request)) asks for
    /// what no inspection runs with; refused before anything renders.
    Inspection(Refusal),
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
            Error::InvalidWorkingSpace(why) | Error::InvalidOcioConfig(why) => write!(f, "{why}"),
            Error::EmptyRegion {
                region,
                width,
                height,
            } => write!(
                f,
                "region {region} holds no pixel of the {width}x{height} frame"
            ),
            Error::Inspection(refusal) => write!(f, "{refusal}"),
            Error::PassNotFound { path, why } => {
                write!(f, "render pass {path}: {why}")
            }
            Error::NoPasses => write!(f, "a group import of render passes names none"),
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

/// Why a change cannot be applied in place ([`crate::live`]): the host
/// re-imports instead. Raised by the world and the light list as well as by
/// the rebuild, so it sits here, below all three.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Structural(pub String);

impl std::fmt::Display for Structural {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Structural {}

// The light path inspector's limits, beside the refusal that names them;
// `crust_core::inspect` re-exports them.

/// The most pixels one inspection renders: a firefly is local, and the cap
/// is what bounds the record store.
pub const MAX_PIXELS: usize = 100;

/// The most records one inspection keeps: with the pixel cap, what bounds
/// the record store and the report (each work unit keeps up to this many).
pub const MAX_TOP: usize = 1000;

/// Why the records carry no prim paths yet, and the grouping that needs
/// them is refused.
pub const PRIMS_NOTE: &str = "crust keeps no table from a hit to its prim yet \
     (add-identity-aovs-openexrid): prims are null, and --group-by expr+prims+light is refused";

/// Why an inspection is refused before anything renders
/// ([`crate::Error::Inspection`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The region holds no pixel.
    EmptyRegion(PixelRect),
    /// The region holds more than [`MAX_PIXELS`] pixels: `pixels` of them,
    /// `None` when there are more than a `usize` counts.
    RegionTooLarge {
        region: PixelRect,
        pixels: Option<usize>,
    },
    /// More than [`MAX_TOP`] records asked for.
    TooManyRecords(usize),
    /// `expr+prims+light`, before a hit can be resolved to its prim.
    PrimsUnavailable,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::EmptyRegion(region) => write!(f, "the region {region} is empty"),
            Refusal::RegionTooLarge { region, pixels } => {
                write!(f, "the region {region} covers ")?;
                match pixels {
                    Some(n) => write!(f, "{n} pixels")?,
                    None => write!(f, "more pixels than can be counted")?,
                }
                write!(
                    f,
                    "; an inspection renders at most {MAX_PIXELS} (10x10, say)"
                )
            }
            Refusal::TooManyRecords(top) => write!(
                f,
                "{top} records asked for; an inspection lists at most {MAX_TOP} (its groups \
                 count every record)"
            ),
            Refusal::PrimsUnavailable => write!(
                f,
                "grouping by expr+prims+light needs the prim of every hit: {PRIMS_NOTE}"
            ),
        }
    }
}
