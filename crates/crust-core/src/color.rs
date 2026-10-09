//! Colour management through OpenColorIO.
//!
//! Every transfer curve, every gamut conversion and every colour-space name
//! comes from one OCIO config, read by [`ocio`], a pure-Rust port of
//! OpenColorIO. The config is the builtin ACES CG config ([`DEFAULT_CONFIG`])
//! unless the host installs another with [`use_config`] before the first
//! colour is converted.
//!
//! **The working space** is the scene-linear space the renderer does its
//! arithmetic in: `lin_rec709` ([`Space::LIN_REC709`]) unless the stage's
//! `RenderSettings.renderingColorSpace` or the host names another — ACEScg,
//! linear Rec.2020, … It is not global: every request that converts a colour
//! carries the working space it converts *to* ([`ColorSpace`]), so two stages
//! rendered in different spaces in one process cannot see each other's.
//!
//! **One rule decides what is converted.** A value that names its colour
//! space — a MaterialX `colorspace`, a USD `colorSpace` metadatum, an sRGB
//! texture, a Ptex texel (which crust decodes as `g22_rec709`) — is converted
//! from that space to the working space. A value that names none is taken as
//! already in the working space, which is what MaterialX specifies for a
//! document without a `colorspace` and what UsdLux says of `inputs:color`.
//! Data (normals, roughness, heights: the config's `Raw`) is never converted.
//!
//! **A conversion is a curve and a matrix** ([`Conversion`]). Every texture
//! space an OCIO config defines is a per-channel transfer curve followed by a
//! 3x3 change of primaries, and the optimised OCIO processor says so: it is a
//! run of per-channel ops and at most one matrix. crust keeps the two apart,
//! taking the matrix exactly (in `f64`, from the processor) and running the
//! curve through OCIO. A texture stored as bytes then keeps its 256-entry
//! decode table — the curve — and applies the matrix once per lookup, after
//! filtering, which a linear map commutes with. A conversion with any other
//! shape (a 3D LUT, a crosstalk op before the matrix) is refused with a
//! warning and the value is used as stored.
//!
//! Two clamps are crust's rather than OCIO's. **Encoded values below zero
//! decode to zero** before a curve (the config's power laws pass them
//! through; a display-encoded value below black means nothing). And **a gamut
//! conversion clamps its result at zero**: a colour outside the working
//! gamut comes out with a negative component, which as a reflectance or an
//! emission is not a colour at all. A conversion with no matrix — every
//! conversion between spaces on the working primaries — is unchanged by the
//! second clamp, so a `lin_rec709` render is what it was.
//!
//! See `docs/color_management.md` for which input is decoded from which space.

use crate::warning;
use glam::{Mat3A, Vec3A};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

/// The config used when the host installs none: the builtin ACES CG config,
/// named by its full version rather than `ocio://cg-config-latest`, so an
/// `ocio` bump that ships a newer config cannot move a render on its own.
pub const DEFAULT_CONFIG: &str = "ocio://cg-config-v4.0.0_aces-v2.0_ocio-v2.5";

/// The display the preview PNG is encoded for by default.
pub const PREVIEW_DISPLAY: &str = "sRGB - Display";

/// The view the preview PNG is encoded with by default: the display's curve
/// and nothing else, so the PNG stays a clamp-and-encode of the EXR rather
/// than a grade of it. An ACES output transform is a `--view` away.
pub const PREVIEW_VIEW: &str = "Un-tone-mapped";

/// The names crust itself refers to, which a config must define (as a name or
/// an alias). They are the first five [`Space`]s, in this order.
const WELL_KNOWN: [&str; 5] = [
    "raw",
    "lin_rec709",
    "srgb_texture",
    "g22_rec709",
    "g18_rec709",
];

struct Installed {
    source: String,
    config: ocio::Config,
}

static CONFIG: OnceLock<Installed> = OnceLock::new();

fn load(source: &str) -> Result<ocio::Config, String> {
    let config =
        ocio::Config::create_from_file(source).map_err(|e| format!("OCIO config {source}: {e}"))?;
    for name in WELL_KNOWN {
        if config.get_color_space(name).is_none() {
            return Err(format!(
                "OCIO config {source} does not define `{name}`, which crust needs (any ACES \
                 CG or studio config does, as an alias)"
            ));
        }
    }
    Ok(config)
}

/// Installs the OCIO config every colour is converted with: a file path, an
/// `.ocioz` archive or an `ocio://` builtin URI.
///
/// Must run before the first colour is converted, since spaces and
/// conversions already handed out refer to the config they came from. Asking
/// again for the config already in use is not an error; asking for another
/// is.
pub fn use_config(source: &str) -> Result<(), crate::Error> {
    let in_use = |installed: &Installed| {
        if installed.source == source {
            Ok(())
        } else {
            Err(crate::Error::InvalidOcioConfig(format!(
                "OCIO config {} is already in use; {source} must be installed before the \
                 first colour is converted",
                installed.source
            )))
        }
    };
    if let Some(installed) = CONFIG.get() {
        return in_use(installed);
    }
    let config = load(source).map_err(crate::Error::InvalidOcioConfig)?;
    in_use(CONFIG.get_or_init(|| Installed {
        source: source.to_string(),
        config,
    }))
}

/// The config in use: the one [`use_config`] installed, else the builtin
/// [`DEFAULT_CONFIG`] (compiled into the `ocio` crate, so failing to load it
/// is a build defect, not an input error).
pub fn config() -> &'static ocio::Config {
    &installed().config
}

fn installed() -> &'static Installed {
    CONFIG.get_or_init(|| Installed {
        source: DEFAULT_CONFIG.to_string(),
        config: load(DEFAULT_CONFIG).unwrap_or_else(|e| panic!("{e}")),
    })
}

/// Where the config in use came from, as given to [`use_config`].
pub fn config_source() -> &'static str {
    &installed().source
}

// ---------------------------------------------------------------------------
// Spaces
// ---------------------------------------------------------------------------

/// A colour space of the config in use, interned: `Copy`, hashable, and one
/// id per space however it was spelled (any alias, any case).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Space(u16);

struct SpaceInfo {
    /// The config's own name for the space.
    name: &'static str,
    /// The spelling crust writes (`.tx` markers, logs): the well-known alias
    /// for the first five, the config's name otherwise.
    label: &'static str,
    data: bool,
}

fn registry() -> &'static RwLock<Vec<SpaceInfo>> {
    static SPACES: OnceLock<RwLock<Vec<SpaceInfo>>> = OnceLock::new();
    SPACES.get_or_init(|| {
        let spaces = WELL_KNOWN
            .iter()
            .map(|&alias| {
                let cs = config().get_color_space(alias).expect("checked at load");
                SpaceInfo {
                    name: String::leak(cs.name().to_string()),
                    label: alias,
                    data: cs.is_data(),
                }
            })
            .collect();
        RwLock::new(spaces)
    })
}

impl Space {
    /// The config's data space (`Raw`): values that are not colours.
    pub const RAW: Space = Space(0);
    /// Linear light on Rec.709 primaries: the default working space.
    pub const LIN_REC709: Space = Space(1);
    /// The piecewise sRGB curve on Rec.709 primaries.
    pub const SRGB_TEXTURE: Space = Space(2);
    /// A pure 2.2 power law on Rec.709 primaries.
    pub const G22_REC709: Space = Space(3);
    /// A pure 1.8 power law on Rec.709 primaries.
    pub const G18_REC709: Space = Space(4);

    /// The space a name or alias denotes in the config in use, matched
    /// case-insensitively; `None` when the config does not define it.
    pub fn named(name: &str) -> Option<Space> {
        let cs = config().get_color_space(name)?;
        let canonical = cs.name();
        let reg = registry();
        let find = |spaces: &[SpaceInfo]| spaces.iter().position(|s| s.name == canonical);
        if let Some(i) = find(&reg.read().expect("space registry")) {
            return Some(Space(i as u16));
        }
        let mut spaces = reg.write().expect("space registry");
        // Another thread may have interned it between the two locks.
        if let Some(i) = find(&spaces) {
            return Some(Space(i as u16));
        }
        let name = String::leak(canonical.to_string());
        spaces.push(SpaceInfo {
            name,
            label: name,
            data: cs.is_data(),
        });
        Some(Space(
            u16::try_from(spaces.len() - 1).expect("fewer than 65536 colour spaces"),
        ))
    }

    fn info<R>(self, f: impl FnOnce(&SpaceInfo) -> R) -> R {
        f(&registry().read().expect("space registry")[self.0 as usize])
    }

    /// The config's name for the space.
    pub fn name(self) -> &'static str {
        self.info(|s| s.name)
    }

    /// The spelling crust records: `raw`, `lin_rec709`, `srgb_texture`,
    /// `g22_rec709`, `g18_rec709` for the well-known five, the config's name
    /// for any other.
    pub fn label(self) -> &'static str {
        self.info(|s| s.label)
    }

    /// Whether the space holds data rather than colour (the config's `Raw`).
    pub fn is_data(self) -> bool {
        self.info(|s| s.data)
    }

    /// Whether the space is linear light, which a working space must be.
    pub fn is_scene_linear(self) -> bool {
        config()
            .is_color_space_linear(self.name(), ocio::ReferenceSpaceType::Scene)
            .unwrap_or(false)
    }

    /// The space's `interop_id` (`lin_ap1_scene`, `lin_rec709_scene`, …), the
    /// name an EXR's `colorInteropID` records; `None` when the config gives
    /// it none.
    pub fn interop_id(self) -> Option<String> {
        let cs = config().get_color_space(self.name())?;
        let id = cs.interop_id();
        (!id.is_empty()).then(|| id.to_string())
    }
}

// ---------------------------------------------------------------------------
// Primaries: XYZ, luminance weights, identification
// ---------------------------------------------------------------------------

const D65: [f64; 2] = [0.3127, 0.3290];
const ACES_WHITE: [f64; 2] = [0.32168, 0.33767];

/// The ASWF Color Interop spaces whose primaries their standards fix:
/// `(interop ID, red / green / blue xy, white xy)`.
/// An interop ID with its primaries' and white's xy.
type Standard = (&'static str, [[f64; 2]; 3], [f64; 2]);

const STANDARDS: [Standard; 6] = [
    (
        "lin_rec709_scene",
        [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]],
        D65,
    ),
    (
        "lin_ap1_scene",
        [[0.713, 0.293], [0.165, 0.830], [0.128, 0.044]],
        ACES_WHITE,
    ),
    (
        "lin_ap0_scene",
        [[0.7347, 0.2653], [0.0, 1.0], [0.0001, -0.077]],
        ACES_WHITE,
    ),
    (
        "lin_rec2020_scene",
        [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]],
        D65,
    ),
    (
        "lin_p3d65_scene",
        [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060]],
        D65,
    ),
    (
        "lin_adobergb_scene",
        [[0.64, 0.33], [0.21, 0.71], [0.15, 0.06]],
        D65,
    ),
];

type M3 = [[f64; 3]; 3];

fn mul3(a: &M3, b: &M3) -> M3 {
    let mut m = [[0.0; 3]; 3];
    for (r, row) in m.iter_mut().enumerate() {
        for (c, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[r][k] * b[k][c]).sum();
        }
    }
    m
}

fn apply3(m: &M3, v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2])
}

fn invert3(m: &M3) -> M3 {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    [
        [
            (e * i - f * h) / det,
            (c * h - b * i) / det,
            (b * f - c * e) / det,
        ],
        [
            (f * g - d * i) / det,
            (a * i - c * g) / det,
            (c * d - a * f) / det,
        ],
        [
            (d * h - e * g) / det,
            (b * g - a * h) / det,
            (a * e - b * d) / det,
        ],
    ]
}

/// The XYZ of a chromaticity at `Y = 1`.
fn xy_to_xyz([x, y]: [f64; 2]) -> [f64; 3] {
    [x / y, 1.0, (1.0 - x - y) / y]
}

/// The normalised primary matrix: RGB → XYZ for `primaries` and `white`.
fn npm(primaries: &[[f64; 2]; 3], white: [f64; 2]) -> M3 {
    let p = primaries.map(xy_to_xyz);
    let pm = [0, 1, 2].map(|r| [p[0][r], p[1][r], p[2][r]]);
    let s = apply3(&invert3(&pm), xy_to_xyz(white));
    [0, 1, 2].map(|r| [pm[r][0] * s[0], pm[r][1] * s[1], pm[r][2] * s[2]])
}

/// The Bradford chromatic adaptation from white `from` to white `to`, the one
/// the ACES configs use between ACES's white and D65.
fn bradford(from: [f64; 2], to: [f64; 2]) -> M3 {
    const B: M3 = [
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ];
    let (s, d) = (apply3(&B, xy_to_xyz(from)), apply3(&B, xy_to_xyz(to)));
    let scale = [
        [d[0] / s[0], 0.0, 0.0],
        [0.0, d[1] / s[1], 0.0],
        [0.0, 0.0, d[2] / s[2]],
    ];
    mul3(&invert3(&B), &mul3(&scale, &B))
}

/// A standard's RGB → XYZ, adapted to D65 as the configs' scene-referred XYZ
/// space is.
fn standard_to_xyz_d65(primaries: &[[f64; 2]; 3], white: [f64; 2]) -> M3 {
    let m = npm(primaries, white);
    if white == D65 {
        m
    } else {
        mul3(&bradford(white, D65), &m)
    }
}

fn mat3a(m: &M3) -> Mat3A {
    let col = |c: usize| Vec3A::new(m[0][c] as f32, m[1][c] as f32, m[2][c] as f32);
    Mat3A::from_cols(col(0), col(1), col(2))
}

/// A scene-linear space's RGB → CIE XYZ matrix, adapted to D65 — what the
/// config's scene-referred XYZ space (`cie_xyz_d65_scene`) holds, and what
/// Cycles and Typhoon derive their luminance and XYZ conversions from.
///
/// Read from the config: the space's conversion into its XYZ space, or,
/// for a config with no such space, into its `aces_interchange` role
/// followed by the standard AP0 → XYZ-D65 matrix (Cycles' fallback). `None`
/// for a space that is not linear, or a config with neither.
pub fn to_xyz(space: Space) -> Option<Mat3A> {
    let linear = |c: &Conversion| {
        c.curve
            .is_none()
            .then(|| c.gamut.unwrap_or(Mat3A::IDENTITY))
    };
    if let Some(xyz) = ["cie_xyz_d65_scene", "lin_ciexyzd65_scene"]
        .into_iter()
        .find_map(Space::named)
    {
        return linear(&conversion(space, xyz));
    }
    let aces = Space::named(config().role_color_space("aces_interchange"))?;
    let to_aces = linear(&conversion(space, aces))?;
    let (_, ap0, white) = STANDARDS[2];
    Some(mat3a(&standard_to_xyz_d65(&ap0, white)) * to_aces)
}

/// CIE XYZ (D65-adapted) as linear light in `working`: the inverse of
/// [`to_xyz`]; `None` when that is.
pub fn from_xyz(xyz: Vec3A, working: Space) -> Option<Vec3A> {
    Some(to_xyz(working)?.inverse() * xyz)
}

/// The luminance weights of `working`: the `Y` row of its RGB → XYZ matrix,
/// so a colour's luminance is the same light whatever space holds it.
///
/// Every heuristic that weighs a colour by one number uses them — light and
/// lobe selection, environment importance, guiding, adaptive sampling, the
/// `variance` AOV — so in ACEScg they weigh AP1 colours by AP1's luminance
/// rather than Rec.709's. `lin_rec709` keeps `utils::Luma::REC709`, the
/// config's own luma coefficients, which the matrix's row equals to four
/// digits: so the default render is bit-identical to what it was. A space
/// with no XYZ matrix gets Rec.709's too, with a warning.
pub fn luma(working: Space) -> utils::Luma {
    // A data request carries no working space; its colours, if any, are the
    // default's.
    if working == Space::LIN_REC709 || working.is_data() {
        return utils::Luma::REC709;
    }
    match to_xyz(working) {
        Some(m) => utils::Luma(m.row(1)),
        None => {
            warning!(
                ColorNoLuminance,
                "no RGB -> XYZ matrix for `{}` in the OCIO config; weighing colours by Rec.709 \
                 luminance",
                working.name()
            );
            utils::Luma::REC709
        }
    }
}

/// The ASWF Color Interop ID of a scene-linear space: the config's
/// `interop_id` when it gives one, else the standard whose primaries and
/// white the space's RGB → XYZ matrix matches to 1e-4 — the fingerprint
/// Cycles takes, so a studio config that names `ACEScg` without an interop
/// ID still writes `lin_ap1_scene`.
pub fn interop_id(space: Space) -> Option<String> {
    if let Some(id) = space.interop_id() {
        return Some(id);
    }
    fingerprint(to_xyz(space)?).map(str::to_string)
}

/// The standard whose D65-adapted RGB → XYZ matrix `m` is, to 1e-4.
fn fingerprint(m: Mat3A) -> Option<&'static str> {
    STANDARDS.iter().find_map(|(id, primaries, white)| {
        let reference = mat3a(&standard_to_xyz_d65(primaries, *white));
        let close = (0..3).all(|c| (m.col(c) - reference.col(c)).abs().max_element() < 1e-4);
        close.then_some(*id)
    })
}

/// The CIE xy chromaticities of a scene-linear space's red, green and blue
/// primaries and its white point — `[r, g, b, w]` — for an output that
/// records them (an EXR's `chromaticities`): those of the standard its
/// [`interop_id`] names, as the standard states them (ACES's own white, not
/// the D65 the config adapts it to). `None` for any other space, whose file
/// then carries no `chromaticities`.
pub fn chromaticities(space: Space) -> Option<[[f32; 2]; 4]> {
    let id = interop_id(space)?;
    let (_, p, w) = STANDARDS.iter().find(|(s, _, _)| *s == id)?;
    let f = |[x, y]: [f64; 2]| [x as f32, y as f32];
    Some([f(p[0]), f(p[1]), f(p[2]), f(*w)])
}

/// The working space a name selects: a space the config knows that is scene
/// linear. Anything else is refused with the reason.
pub fn working_space(name: &str) -> Result<Space, crate::Error> {
    let space = Space::named(name).ok_or_else(|| {
        crate::Error::InvalidWorkingSpace(format!(
            "working colour space `{name}` is not defined by the OCIO config {}",
            config_source()
        ))
    })?;
    if space.is_data() || !space.is_scene_linear() {
        return Err(crate::Error::InvalidWorkingSpace(format!(
            "working colour space `{name}` ({}) is not scene-linear",
            space.name()
        )));
    }
    Ok(space)
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

/// A conversion from one space to another, split into its per-channel curve
/// and its change of primaries (see the module documentation).
pub struct Conversion {
    /// The per-channel part, `None` when there is none.
    curve: Option<ocio::CpuProcessor>,
    /// The change of primaries, `None` when the two share them.
    gamut: Option<Mat3A>,
}

impl Conversion {
    const IDENTITY: Conversion = Conversion {
        curve: None,
        gamut: None,
    };

    /// The change of primaries, applied after the curve; `None` when the two
    /// spaces share them.
    pub fn gamut(&self) -> Option<Mat3A> {
        self.gamut
    }

    /// Whether the conversion changes nothing.
    pub fn is_identity(&self) -> bool {
        self.curve.is_none() && self.gamut.is_none()
    }

    /// The curve alone, over samples of any channel layout: what a decode
    /// table holds. Negative inputs decode to zero.
    pub fn decode_curve_slice(&self, values: &mut [f32]) {
        let Some(cpu) = &self.curve else { return };
        for v in values.iter_mut() {
            *v = v.max(0.0);
        }
        let whole = values.len() / 3 * 3;
        let (head, tail) = values.split_at_mut(whole);
        if !head.is_empty() {
            cpu.apply_rgb_slice(head);
        }
        if !tail.is_empty() {
            let mut px = [0.0f32; 3];
            px[..tail.len()].copy_from_slice(tail);
            cpu.apply_rgb(&mut px);
            tail.copy_from_slice(&px[..tail.len()]);
        }
    }

    /// The change of primaries alone, on a colour the curve already decoded,
    /// clamped at zero. The identity when the spaces share primaries.
    #[inline]
    pub fn apply_gamut(&self, rgb: Vec3A) -> Vec3A {
        apply_gamut(self.gamut.as_ref(), rgb)
    }

    /// One colour, curve then primaries.
    pub fn convert(&self, rgb: Vec3A) -> Vec3A {
        let mut px = rgb.to_array();
        self.decode_curve_slice(&mut px);
        self.apply_gamut(Vec3A::from_array(px))
    }

    /// Interleaved RGB in place, curve then primaries.
    pub fn convert_rgb_slice(&self, rgb: &mut [f32]) {
        assert!(rgb.len().is_multiple_of(3), "converted samples are RGB");
        self.decode_curve_slice(rgb);
        if self.gamut.is_some() {
            for px in rgb.as_chunks_mut::<3>().0 {
                *px = self.apply_gamut(Vec3A::from_array(*px)).to_array();
            }
        }
    }
}

/// A change of primaries as a texture lookup applies it: `m · rgb` clamped at
/// zero, or `rgb` untouched when there is no matrix. One function so every
/// decoder — preloaded, streamed, Ptex — clamps the same way.
#[inline]
pub fn apply_gamut(gamut: Option<&Mat3A>, rgb: Vec3A) -> Vec3A {
    match gamut {
        Some(m) => (*m * rgb).max(Vec3A::ZERO),
        None => rgb,
    }
}

/// Splits the optimised processor `src -> dst` into a per-channel curve and a
/// trailing matrix; `Err` with the reason when it has another shape.
fn build(src: Space, dst: Space) -> Result<Conversion, String> {
    if src == dst || src.is_data() || dst.is_data() {
        return Ok(Conversion::IDENTITY);
    }
    let cfg = config();
    let p = cfg
        .get_processor(src.name(), dst.name())
        .map_err(|e| e.to_string())?
        .optimized(ocio::OptimizationFlags::DEFAULT);
    if p.is_no_op() {
        return Ok(Conversion::IDENTITY);
    }
    let mut ops = p.create_group_transform().transforms;
    let gamut = match ops.last() {
        Some(ocio::Transform::Matrix(m)) => {
            if m.direction != ocio::TransformDirection::Forward
                || m.offset[..3].iter().any(|&o| o != 0.0)
            {
                return Err("its matrix is not a plain change of primaries".into());
            }
            let at = |r: usize, c: usize| m.matrix[r * 4 + c];
            let identity = (0..3)
                .all(|r| (0..3).all(|c| (at(r, c) - if r == c { 1.0 } else { 0.0 }).abs() < 1e-12));
            let col = |c: usize| Vec3A::new(at(0, c) as f32, at(1, c) as f32, at(2, c) as f32);
            let gamut = (!identity).then(|| Mat3A::from_cols(col(0), col(1), col(2)));
            ops.pop();
            gamut
        }
        _ => None,
    };
    let curve = if ops.is_empty() {
        None
    } else {
        if ops.iter().any(|t| matches!(t, ocio::Transform::Matrix(_))) {
            return Err("it changes primaries before its transfer curve".into());
        }
        let group = ocio::GroupTransform {
            transforms: ops,
            ..Default::default()
        };
        let cpu = cfg
            .get_processor_for_transform(
                &ocio::Transform::Group(group),
                ocio::TransformDirection::Forward,
            )
            .map_err(|e| e.to_string())?
            .default_cpu_processor();
        if cpu.has_channel_crosstalk() {
            return Err("its transfer curve mixes channels".into());
        }
        Some(cpu)
    };
    Ok(Conversion { curve, gamut })
}

/// The conversion `src -> dst`, built once per pair and shared. A pair whose
/// processor has no curve-and-matrix shape is refused once, with a warning,
/// and converts as the identity.
pub fn conversion(src: Space, dst: Space) -> Arc<Conversion> {
    type Cache = RwLock<HashMap<(Space, Space), Arc<Conversion>>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(c) = cache.read().expect("conversion cache").get(&(src, dst)) {
        return c.clone();
    }
    let built = build(src, dst).unwrap_or_else(|why| {
        warning!(
            ColorNoConversion,
            "colour space `{}` -> `{}` is not converted ({why}); values are used as stored",
            src.name(),
            dst.name()
        );
        Conversion::IDENTITY
    });
    cache
        .write()
        .expect("conversion cache")
        .entry((src, dst))
        .or_insert_with(|| Arc::new(built))
        .clone()
}

/// A colour authored in `src`, in the working space `working`.
pub fn convert(rgb: Vec3A, src: Space, working: Space) -> Vec3A {
    conversion(src, working).convert(rgb)
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

/// How a texture file's stored values relate to linear light in the working
/// space: the space they were authored in, and the working space to bring
/// them to.
///
/// Carried across the [`crate::AssetLoader`] seam rather than decided inside
/// it, because the file itself does not say: an 8-bit PNG holding albedo is
/// display-encoded while the *same encoding* holding a normal map, a roughness
/// or a mask is raw data, and un-gamma'ing the latter would bend every value
/// toward zero. MaterialX states it per input (`colorspace="srgb_texture"`),
/// UsdUVTexture by `sourceColorSpace` — see `docs/color_management.md`.
///
/// The constants are into the default working space, `lin_rec709`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ColorSpace {
    /// `None` is "decide from the file" (UsdUVTexture's `auto`).
    source: Option<Space>,
    working: Space,
}

impl ColorSpace {
    /// sRGB display-encoded, into `lin_rec709`.
    pub const SRGB: ColorSpace = ColorSpace::new_const(Space::SRGB_TEXTURE);
    /// A pure 2.2 power law (`g22_rec709`, *not* the sRGB curve), into
    /// `lin_rec709`.
    pub const GAMMA22: ColorSpace = ColorSpace::new_const(Space::G22_REC709);
    /// A pure 1.8 power law (`g18_rec709`), into `lin_rec709`.
    pub const GAMMA18: ColorSpace = ColorSpace::new_const(Space::G18_REC709);
    /// Nothing converted: data (normals, roughness, masks), or values already
    /// in the working space.
    pub const RAW: ColorSpace = ColorSpace {
        source: Some(Space::RAW),
        working: Space::RAW,
    };
    /// "Decide from the file" — UsdUVTexture's `sourceColorSpace = "auto"`,
    /// which is also its fallback — into `lin_rec709`. Resolved at open
    /// through [`ColorSpace::resolve_auto`] and **never reaches a lookup**.
    pub const AUTO: ColorSpace = ColorSpace {
        source: None,
        working: Space::LIN_REC709,
    };

    const fn new_const(source: Space) -> ColorSpace {
        ColorSpace {
            source: Some(source),
            working: Space::LIN_REC709,
        }
    }

    /// Values authored in `source`, wanted in `working`. A data source is
    /// [`RAW`], which carries no working space; one equal to the working
    /// space converts nothing but keeps it, since an environment map needs
    /// the working space's luminance weights whatever its pixels are in.
    ///
    /// [`RAW`]: ColorSpace::RAW
    pub fn new(source: Space, working: Space) -> ColorSpace {
        if source.is_data() {
            ColorSpace::RAW
        } else {
            ColorSpace {
                source: Some(source),
                working,
            }
        }
    }

    /// The same request into another working space.
    pub fn into_working(self, working: Space) -> ColorSpace {
        match self.source {
            Some(s) if self != ColorSpace::RAW => ColorSpace::new(s, working),
            Some(_) => ColorSpace::RAW,
            None => ColorSpace {
                source: None,
                working,
            },
        }
    }

    /// The working space the values are converted to.
    pub fn working(self) -> Space {
        self.working
    }

    /// Maps a MaterialX `colorspace` onto a request into `working`.
    ///
    /// The name resolves through the OCIO config, so every alias it lists is
    /// accepted — `Utility - sRGB - Texture`, `srgb_rec709_scene`, `acescg`,
    /// `g22_ap1`, … — case-insensitively; `srgb`, which older documents use
    /// and the config does not list, is kept as `srgb_texture`. **An absent
    /// attribute means the values are already in the working space**, as
    /// MaterialX specifies for a document with no `colorspace`; that is also
    /// right for normal, roughness and mask maps, which carry no colour. A
    /// name the config does not know is refused with a warning and read the
    /// same way.
    ///
    /// `g22_rec709` and `g18_rec709` are *not* spellings of sRGB: both are
    /// pure power laws with no linear toe, where sRGB's EOTF is piecewise.
    /// Decoding either through the sRGB curve is invisible in midtones and up
    /// to an order of magnitude too bright in near-black.
    pub fn from_mtlx(name: Option<&str>, working: Space) -> ColorSpace {
        let Some(name) = name else {
            return ColorSpace::RAW;
        };
        if name.eq_ignore_ascii_case("srgb") {
            return ColorSpace::new(Space::SRGB_TEXTURE, working);
        }
        match Space::named(name) {
            Some(space) => ColorSpace::new(space, working),
            None => {
                warning!(
                    ColorUnknownSpace,
                    "MaterialX colorspace `{name}` is not defined by the OCIO config; read as \
                     stored"
                );
                ColorSpace::RAW
            }
        }
    }

    /// Maps a UsdUVTexture `sourceColorSpace` token onto a request into
    /// `working`.
    ///
    /// The schema allows three values: `raw`, `sRGB` and `auto`, the last of
    /// which is also the fallback, so an unauthored attribute means `auto`
    /// rather than raw — the opposite default to MaterialX's, and the reason
    /// this is a separate function from [`ColorSpace::from_mtlx`]. An
    /// unrecognised token falls back to `auto` too, as the schema's fallback
    /// would.
    pub fn from_usd(token: Option<&str>, working: Space) -> ColorSpace {
        match token.map(str::to_ascii_lowercase).as_deref() {
            Some("srgb") => ColorSpace::new(Space::SRGB_TEXTURE, working),
            Some("raw") => ColorSpace::RAW,
            _ => ColorSpace::AUTO.into_working(working),
        }
    }

    /// Resolves `auto` against the file's pixel format; every other request
    /// is returned resolved as it is.
    ///
    /// The UsdUVTexture rule, which Hydra implements: absent colour metadata
    /// in the file, an **8-bit image with three or four channels** is sRGB and
    /// everything else — single-channel, 16-bit, float — is used as read. So a
    /// greyscale roughness PNG stays raw while an RGB albedo PNG decodes, and
    /// an EXR is taken as already in the working space.
    pub fn resolve_auto(self, eight_bit: bool, channels: u8) -> ResolvedColorSpace {
        match self.resolved() {
            Some(space) => space,
            None if eight_bit && (channels == 3 || channels == 4) => {
                ResolvedColorSpace::new(Space::SRGB_TEXTURE, self.working)
            }
            None => ResolvedColorSpace::RAW,
        }
    }

    /// The request already decided, when it is — `None` for `auto`, which
    /// only a file can decide.
    pub fn resolved(self) -> Option<ResolvedColorSpace> {
        match self.source {
            _ if self == ColorSpace::RAW => Some(ResolvedColorSpace::RAW),
            Some(s) => Some(ResolvedColorSpace::new(s, self.working)),
            None => None,
        }
    }
}

/// A [`ColorSpace`] with `auto` decided: the conversion a decoder actually
/// applies.
///
/// A separate type rather than a promise in a comment. Everything past the
/// open — decode tables, mip re-encoding, `.tx` markers, the load report —
/// holds this, so an unresolved request cannot get there.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ResolvedColorSpace {
    source: Space,
    working: Space,
}

impl ResolvedColorSpace {
    /// [`ColorSpace::SRGB`], resolved.
    pub const SRGB: ResolvedColorSpace = ResolvedColorSpace::new_const(Space::SRGB_TEXTURE);
    /// [`ColorSpace::GAMMA22`], resolved.
    pub const GAMMA22: ResolvedColorSpace = ResolvedColorSpace::new_const(Space::G22_REC709);
    /// [`ColorSpace::GAMMA18`], resolved.
    pub const GAMMA18: ResolvedColorSpace = ResolvedColorSpace::new_const(Space::G18_REC709);
    /// [`ColorSpace::RAW`], resolved.
    pub const RAW: ResolvedColorSpace = ResolvedColorSpace {
        source: Space::RAW,
        working: Space::RAW,
    };

    const fn new_const(source: Space) -> ResolvedColorSpace {
        ResolvedColorSpace {
            source,
            working: Space::LIN_REC709,
        }
    }

    /// Values authored in `source`, wanted in `working`; normalised as
    /// [`ColorSpace::new`] is.
    pub fn new(source: Space, working: Space) -> ResolvedColorSpace {
        if source.is_data() {
            ResolvedColorSpace::RAW
        } else {
            ResolvedColorSpace { source, working }
        }
    }

    /// The space the stored values are in.
    pub fn source(self) -> Space {
        self.source
    }

    /// The space they are converted to.
    pub fn working(self) -> Space {
        self.working
    }

    /// Whether the values are data, never converted.
    pub fn is_raw(self) -> bool {
        self == ResolvedColorSpace::RAW
    }

    /// The conversion itself.
    pub fn conversion(self) -> Arc<Conversion> {
        conversion(self.source, self.working)
    }

    /// The change of primaries a lookup applies after filtering; `None` when
    /// the source shares the working primaries.
    pub fn gamut(self) -> Option<Mat3A> {
        self.conversion().gamut()
    }

    /// The curve alone over samples of any layout (see
    /// [`Conversion::decode_curve_slice`]).
    pub fn decode_curve_slice(self, values: &mut [f32]) {
        self.conversion().decode_curve_slice(values);
    }

    /// One sample through the curve alone.
    pub fn decode_curve(self, encoded: f32) -> f32 {
        let mut px = [encoded; 3];
        self.decode_curve_slice(&mut px);
        px[0]
    }

    /// One colour, curve then primaries.
    pub fn decode_rgb(self, encoded: Vec3A) -> Vec3A {
        self.conversion().convert(encoded)
    }

    /// Interleaved RGB in place, curve then primaries.
    pub fn decode_rgb_slice(self, rgb: &mut [f32]) {
        self.conversion().convert_rgb_slice(rgb);
    }
}

impl From<ResolvedColorSpace> for ColorSpace {
    fn from(space: ResolvedColorSpace) -> ColorSpace {
        ColorSpace::new(space.source, space.working)
    }
}

// ---------------------------------------------------------------------------
// The preview
// ---------------------------------------------------------------------------

/// Encodes linear `working` RGB for an 8-bit preview, in place, through the
/// config's `display` / `view`. Values are clamped to `[0, 1]` first when the
/// view is [`PREVIEW_VIEW`] — a clamp and an encode — and handed to the view
/// as they are otherwise, since an output transform such as ACES's maps the
/// whole scene-linear range itself.
pub fn encode_preview(
    rgb: &mut [f32],
    working: Space,
    display: &str,
    view: &str,
) -> Result<(), String> {
    type Cache = Mutex<HashMap<(Space, String, String), Arc<ocio::CpuProcessor>>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let key = (working, display.to_string(), view.to_string());
    let cache = CACHE.get_or_init(Default::default);
    let cached = cache.lock().expect("preview cache").get(&key).cloned();
    let cpu = match cached {
        Some(cpu) => cpu,
        None => {
            let cpu = config()
                .get_display_view_processor(working.name(), display, view)
                .map_err(|e| format!("OCIO display `{display}` / view `{view}`: {e}"))?
                .default_cpu_processor();
            let cpu = Arc::new(cpu);
            cache
                .lock()
                .expect("preview cache")
                .insert(key, cpu.clone());
            cpu
        }
    };
    let clamp = view == PREVIEW_VIEW;
    for v in rgb.iter_mut() {
        // `clamp` keeps a NaN; the preview has no use for one.
        *v = if v.is_nan() {
            0.0
        } else if clamp {
            v.clamp(0.0, 1.0)
        } else {
            *v
        };
    }
    assert!(rgb.len().is_multiple_of(3), "preview pixels are RGB");
    cpu.apply_rgb_slice(rgb);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference curves the OCIO config must reproduce, written out so
    /// the tests do not compare OCIO with itself.
    fn srgb_eotf(c: f32) -> f32 {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    fn acescg() -> Space {
        working_space("acescg").expect("ACEScg is in the builtin config")
    }

    #[test]
    fn the_builtin_config_defines_the_well_known_spaces() {
        assert_eq!(Space::LIN_REC709.name(), "Linear Rec.709 (sRGB)");
        for (i, alias) in WELL_KNOWN.iter().enumerate() {
            let s = Space::named(alias).expect(alias);
            assert_eq!(s, Space(i as u16), "{alias}");
            assert_eq!(s.label(), *alias);
        }
        assert!(Space::RAW.is_data());
        assert!(!Space::SRGB_TEXTURE.is_data());
        assert_eq!(config_source(), DEFAULT_CONFIG);
        assert!(use_config(DEFAULT_CONFIG).is_ok());
        assert!(use_config("ocio://studio-config-latest").is_err());
    }

    #[test]
    fn names_intern_once_whatever_the_spelling() {
        let a = Space::named("acescg").unwrap();
        assert_eq!(Space::named("ACES - ACEScg"), Some(a));
        assert_eq!(Space::named("lin_ap1_scene"), Some(a));
        assert_eq!(Space::named("ACEScg"), Some(a));
        assert_eq!(a.name(), "ACEScg");
        assert_eq!(a.interop_id().as_deref(), Some("lin_ap1_scene"));
        assert_eq!(Space::named("not_a_space"), None);
        assert_eq!(Space::named("srgb_tx"), Some(Space::SRGB_TEXTURE));
    }

    #[test]
    fn standard_working_spaces_know_their_primaries() {
        let rec709 = chromaticities(Space::LIN_REC709).unwrap();
        assert_eq!(rec709[0], [0.64, 0.33]);
        let ap1 = chromaticities(acescg()).unwrap();
        assert_eq!(ap1[3], [0.32168, 0.33767]);
        assert!(chromaticities(working_space("lin_rec2020").unwrap()).is_some());
        assert_eq!(chromaticities(Space::RAW), None);
    }

    #[test]
    fn every_standard_working_space_is_recognised_by_its_matrix() {
        // The fingerprint must find each standard from the config's own
        // matrix, so a config without interop IDs still names them.
        for (name, id) in [
            ("lin_rec709", "lin_rec709_scene"),
            ("acescg", "lin_ap1_scene"),
            ("aces2065_1", "lin_ap0_scene"),
            ("lin_rec2020", "lin_rec2020_scene"),
            ("lin_p3d65", "lin_p3d65_scene"),
            ("lin_adobergb", "lin_adobergb_scene"),
        ] {
            let m = to_xyz(Space::named(name).unwrap()).expect(name);
            assert_eq!(fingerprint(m), Some(id), "{name}");
        }
        // A space on no standard's primaries matches none.
        assert_eq!(
            fingerprint(Mat3A::from_diagonal(glam::Vec3::new(0.9, 1.0, 1.1))),
            None
        );
        // Not linear: no matrix.
        assert_eq!(to_xyz(Space::SRGB_TEXTURE), None);
    }

    #[test]
    fn luminance_weights_are_the_working_spaces_y_row() {
        assert_eq!(luma(Space::LIN_REC709), utils::Luma::REC709);
        let ap1 = luma(acescg()).0;
        // ACEScg's luminance, D65-adapted as the ACES configs adapt it.
        assert!((ap1.element_sum() - 1.0).abs() < 1e-4, "{ap1}");
        let want = Vec3A::new(0.2722, 0.6741, 0.0537);
        assert!((ap1 - want).abs().max_element() < 5e-3, "{ap1}");
        // A colour's luminance does not depend on the space holding it.
        let c = Vec3A::new(0.8, 0.3, 0.1);
        let in_aces = convert(c, Space::LIN_REC709, acescg());
        let (a, b) = (utils::Luma::REC709.of(c), luma(acescg()).of(in_aces));
        assert!((a - b).abs() < 1e-3, "{a} vs {b}");
    }

    #[test]
    fn xyz_round_trips_through_any_working_space() {
        let xyz = Vec3A::new(0.4, 0.35, 0.2);
        for w in [Space::LIN_REC709, acescg()] {
            let rgb = from_xyz(xyz, w).unwrap();
            let back = to_xyz(w).unwrap() * rgb;
            assert!((back - xyz).abs().max_element() < 1e-5, "{back}");
        }
    }

    #[test]
    fn a_working_space_must_be_scene_linear() {
        assert_eq!(working_space("lin_rec709").unwrap(), Space::LIN_REC709);
        assert!(working_space("acescg").is_ok());
        assert!(working_space("lin_rec2020").is_ok());
        assert!(working_space("srgb_texture").is_err());
        assert!(working_space("raw").is_err());
        assert!(working_space("not_a_space").is_err());
    }

    #[test]
    fn utils_luminance_uses_the_config_luma_coefficients() {
        // `utils::luminance` hard-codes Rec.709 luma for the hot path; this
        // is the pair it must stay equal to.
        let [r, g, b] = config().default_luma_coefs();
        for (axis, want) in [(Vec3A::X, r), (Vec3A::Y, g), (Vec3A::Z, b)] {
            assert_eq!(utils::luminance(axis), want as f32, "{axis}");
        }
    }

    #[test]
    fn the_curves_match_their_definitions_at_every_byte() {
        for i in 0..=255 {
            let e = i as f32 / 255.0;
            let srgb = ResolvedColorSpace::SRGB.decode_curve(e);
            assert!((srgb - srgb_eotf(e)).abs() <= 1e-6, "srgb {i}: {srgb}");
            // The power laws are bit-identical to `powf`.
            let g22 = ResolvedColorSpace::GAMMA22.decode_curve(e);
            assert_eq!(g22, e.powf(2.2), "g22 {i}");
            let g18 = ResolvedColorSpace::GAMMA18.decode_curve(e);
            assert_eq!(g18, e.powf(1.8), "g18 {i}");
            let raw = ResolvedColorSpace::RAW.decode_curve(e);
            assert_eq!(raw.to_bits(), e.to_bits());
        }
    }

    #[test]
    fn rec709_sources_into_lin_rec709_have_no_matrix() {
        for s in [
            ResolvedColorSpace::SRGB,
            ResolvedColorSpace::GAMMA22,
            ResolvedColorSpace::GAMMA18,
            ResolvedColorSpace::RAW,
        ] {
            assert_eq!(s.gamut(), None, "{s:?}");
        }
        assert!(conversion(Space::LIN_REC709, Space::LIN_REC709).is_identity());
    }

    #[test]
    fn negatives_clamp_for_curves_and_pass_for_raw() {
        assert_eq!(ResolvedColorSpace::GAMMA22.decode_curve(-0.5), 0.0);
        assert_eq!(ResolvedColorSpace::SRGB.decode_curve(-0.5), 0.0);
        assert_eq!(ResolvedColorSpace::RAW.decode_curve(-0.5), -0.5);
    }

    #[test]
    fn slices_of_any_length_decode_like_single_samples() {
        for n in [1, 2, 3, 4, 5, 7] {
            let mut v: Vec<f32> = (0..n).map(|i| i as f32 / 7.0).collect();
            let want: Vec<f32> = v
                .iter()
                .map(|&x| ResolvedColorSpace::SRGB.decode_curve(x))
                .collect();
            ResolvedColorSpace::SRGB.decode_curve_slice(&mut v);
            assert_eq!(v, want, "len {n}");
        }
    }

    /// The split must reproduce the whole OCIO processor: curve then matrix
    /// is the conversion, for every texture space of the ACES config and in
    /// both directions of gamut.
    #[test]
    fn curve_then_matrix_is_the_ocio_processor() {
        let aces = acescg();
        for work in [Space::LIN_REC709, aces] {
            for name in [
                "srgb_texture",
                "g22_rec709",
                "lin_rec709",
                "acescg",
                "g22_ap1",
                "srgb_ap1",
                "adobergb",
                "lin_adobergb",
                "srgb_displayp3",
                "lin_rec2020",
                "aces2065_1",
                "acescct",
            ] {
                let src = Space::named(name).unwrap();
                let conv = conversion(src, work);
                let full = config()
                    .get_processor(src.name(), work.name())
                    .unwrap()
                    .default_cpu_processor();
                for k in 0..200 {
                    let x = [
                        (k * 37 % 101) as f32 / 100.0,
                        (k * 53 % 97) as f32 / 96.0,
                        (k * 71 % 89) as f32 / 88.0,
                    ];
                    let mut want = x;
                    full.apply_rgb(&mut want);
                    // Only a change of primaries clamps: a log curve alone
                    // (ACEScct) decodes its lowest codes below zero, as
                    // OCIO's does.
                    let want = Vec3A::from_array(want);
                    let want = match conv.gamut() {
                        Some(_) => want.max(Vec3A::ZERO),
                        None => want,
                    };
                    let got = conv.convert(Vec3A::from_array(x));
                    let err = (got - want).abs().max_element();
                    let tol = 2e-5 * want.max_element().max(1.0);
                    assert!(err < tol, "{name} -> {}: {x:?} {got} {want}", work.name());
                }
            }
        }
    }

    #[test]
    fn rec709_white_and_primaries_land_where_aces_says() {
        let aces = acescg();
        // sRGB white is white in ACEScg (the config adapts D65 to ACES's
        // white), and pure Rec.709 red is inside AP1.
        let white = convert(Vec3A::ONE, Space::SRGB_TEXTURE, aces);
        assert!((white - Vec3A::ONE).abs().max_element() < 1e-4, "{white}");
        let red = convert(Vec3A::X, Space::LIN_REC709, aces);
        let want = Vec3A::new(0.613_097, 0.070_194, 0.020_616);
        assert!((red - want).abs().max_element() < 1e-4, "{red}");
        // The other way, saturated AP1 red is outside Rec.709: the negative
        // components clamp at zero.
        let back = convert(Vec3A::X, aces, Space::LIN_REC709);
        assert!(back.x > 1.7 && back.y == 0.0 && back.z == 0.0, "{back}");
    }

    #[test]
    fn requests_name_their_working_space() {
        let rec709 = Space::LIN_REC709;
        let aces = acescg();
        let mtlx = ColorSpace::from_mtlx;
        assert_eq!(mtlx(Some("g22_rec709"), rec709), ColorSpace::GAMMA22);
        assert_eq!(mtlx(Some("G18_Rec709"), rec709), ColorSpace::GAMMA18);
        for s in [
            "srgb_texture",
            "sRGB",
            "srgb_tx",
            "Utility - sRGB - Texture",
        ] {
            assert_eq!(mtlx(Some(s), rec709), ColorSpace::SRGB, "{s}");
        }
        // Absent and data are raw; the working space itself converts
        // nothing but keeps its working space.
        for s in [None, Some("raw")] {
            assert_eq!(mtlx(s, rec709), ColorSpace::RAW, "{s:?}");
        }
        for (s, w) in [("lin_rec709", rec709), ("acescg", aces)] {
            let r = mtlx(Some(s), w).resolved().unwrap();
            assert!(r.conversion().is_identity(), "{s}");
            assert_eq!(r.working(), w, "{s}");
        }
        // Another gamut is converted now, rather than read raw.
        let ap1 = mtlx(Some("g22_ap1"), rec709).resolved().unwrap();
        assert!(!ap1.is_raw());
        assert!(ap1.gamut().is_some());
        // The same name into ACEScg needs no matrix, and sRGB then does.
        let ap1 = mtlx(Some("g22_ap1"), aces).resolved().unwrap();
        assert!(ap1.gamut().is_none());
        let srgb = mtlx(Some("srgb_texture"), aces).resolved().unwrap();
        assert!(srgb.gamut().is_some());
    }

    #[test]
    fn usd_source_color_space_defaults_to_auto_not_raw() {
        let w = Space::LIN_REC709;
        assert_eq!(ColorSpace::from_usd(Some("sRGB"), w), ColorSpace::SRGB);
        assert_eq!(ColorSpace::from_usd(Some("raw"), w), ColorSpace::RAW);
        for s in [None, Some("auto"), Some("bogus")] {
            assert_eq!(ColorSpace::from_usd(s, w), ColorSpace::AUTO, "{s:?}");
        }
    }

    #[test]
    fn auto_resolves_by_pixel_format() {
        let a = ColorSpace::AUTO;
        assert_eq!(a.resolve_auto(true, 3), ResolvedColorSpace::SRGB);
        assert_eq!(a.resolve_auto(true, 4), ResolvedColorSpace::SRGB);
        assert_eq!(a.resolve_auto(true, 1), ResolvedColorSpace::RAW);
        assert_eq!(a.resolve_auto(true, 2), ResolvedColorSpace::RAW);
        assert_eq!(a.resolve_auto(false, 3), ResolvedColorSpace::RAW);
        // An explicit space is never second-guessed by the file.
        let raw = ColorSpace::RAW.resolve_auto(true, 3);
        assert_eq!(raw, ResolvedColorSpace::RAW);
        let srgb = ColorSpace::SRGB.resolve_auto(false, 1);
        assert_eq!(srgb, ResolvedColorSpace::SRGB);
        // `auto` into ACEScg keeps its working space.
        let aces = acescg();
        let r = a.into_working(aces).resolve_auto(true, 3);
        assert_eq!((r.source(), r.working()), (Space::SRGB_TEXTURE, aces));
        for r in [
            ResolvedColorSpace::SRGB,
            ResolvedColorSpace::GAMMA22,
            ResolvedColorSpace::RAW,
        ] {
            assert_eq!(ColorSpace::from(r).resolved(), Some(r));
        }
        assert_eq!(a.resolved(), None);
    }

    #[test]
    fn the_preview_is_a_clamped_srgb_encode() {
        let mut px = [0.5, -1.0, 7.0, f32::NAN, 0.0, 1.0];
        let w = Space::LIN_REC709;
        encode_preview(&mut px, w, PREVIEW_DISPLAY, PREVIEW_VIEW).unwrap();
        // The display/view path goes through the config's reference spaces,
        // so white comes back to within a matrix round trip, not exactly.
        let want = [0.735_356_6, 0.0, 1.0, 0.0, 0.0, 1.0];
        for (got, want) in px.iter().zip(want) {
            assert!((got - want).abs() < 1e-6, "{px:?}");
        }
    }

    #[test]
    fn an_aces_view_tone_maps_from_any_working_space() {
        let mut px = [0.18, 0.18, 0.18, 16.0, 16.0, 16.0];
        let view = "ACES 2.0 - SDR 100 nits (Rec.709)";
        encode_preview(&mut px, acescg(), PREVIEW_DISPLAY, view).unwrap();
        // Mid grey lands mid-range and a highlight far above 1 rolls off
        // below white instead of clipping.
        assert!(px[0] > 0.3 && px[0] < 0.6, "{px:?}");
        assert!(px[3] > 0.9 && px[3] <= 1.0, "{px:?}");
        let bad = encode_preview(&mut px, Space::LIN_REC709, PREVIEW_DISPLAY, "no such view");
        assert!(bad.is_err());
    }
}
