//! Light path expressions: OSL's LPE grammar over crust's event alphabet.
//!
//! A path is a word of **events**, camera first, light last: `C`, then one
//! event per scattering vertex, then the emission that ends it. An event has
//! a type, a scatter kind and at most one custom label:
//!
//! | type | meaning |
//! |------|---------|
//! | `C` | the camera, always the first event |
//! | `R` / `T` | reflection / transmission at a surface lobe |
//! | `V` | a volume scatter (a volume region, or a carried medium) |
//! | `L` | emission from a light-list entry (area, dome, distant; every NEE sample) |
//! | `O` | emission from anything else: an emissive material outside the light list, volume emission |
//!
//! | scatter | meaning |
//! |---------|---------|
//! | `D` | diffuse |
//! | `G` | glossy (a rough microfacet or sheen lobe) |
//! | `S` | singular (a lobe at zero roughness, a delta sample) |
//! | `s` | straight (passing through without turning: a cutout) |
//!
//! Labels are the OpenPBR component of a lobe ([`LobeLabel`]) or a light's
//! `crust:light:lpeTag`.
//!
//! Every LPE of a render compiles into **one** DFA ([`Lpe`]); a path carries
//! one `u16` state and each event is one table lookup. Each state records, as
//! a bitmask, which expressions accept there. See `openspec/specs/aovs` for
//! how the integrator routes contributions through it.

mod dfa;
mod parse;

pub use dfa::Lpe;
pub use parse::ParseError;

/// The most DFA states one render's expressions may compile to. Subset
/// construction is exponential in the worst case — `C.*<RD>.{16}[LO]` needs
/// about 2¹⁷ states — so it stops here and refuses the set, rather than
/// running out of memory or `u16` state ids. Real compositing sets compile
/// to tens of states.
pub const MAX_STATES: usize = 4096;

/// Why a set of expressions does not compile.
#[derive(Debug, Clone, PartialEq)]
pub enum CompileError {
    /// Expression `index` does not parse.
    Parse { index: usize, error: ParseError },
    /// More than [`MAX_EXPRESSIONS`].
    TooManyExpressions(usize),
    /// So many distinct labels that the event alphabet would not fit a
    /// `u16` symbol.
    TooManyLabels(usize),
    /// The DFA would exceed [`MAX_STATES`].
    TooComplex,
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::Parse { index, error } => write!(f, "expression {index}: {error}"),
            CompileError::TooManyExpressions(n) => {
                write!(f, "{n} expressions, at most {MAX_EXPRESSIONS} per render")
            }
            CompileError::TooManyLabels(n) => write!(f, "{n} distinct labels is too many"),
            CompileError::TooComplex => write!(
                f,
                "the expressions need more than {MAX_STATES} automaton states"
            ),
        }
    }
}

/// The type of an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EventType {
    Camera = 0,
    Reflect = 1,
    Transmit = 2,
    Volume = 3,
    Light = 4,
    Object = 5,
}

impl EventType {
    pub(crate) const COUNT: usize = 6;
    pub(crate) const ALL: [EventType; 6] = [
        EventType::Camera,
        EventType::Reflect,
        EventType::Transmit,
        EventType::Volume,
        EventType::Light,
        EventType::Object,
    ];

    fn letter(c: char) -> Option<EventType> {
        Some(match c {
            'C' => EventType::Camera,
            'R' => EventType::Reflect,
            'T' => EventType::Transmit,
            'V' => EventType::Volume,
            'L' => EventType::Light,
            'O' => EventType::Object,
            _ => return None,
        })
    }
}

/// The scatter kind of an event. Camera, volume and emission events have
/// none, which only the `.` wildcard matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Scatter {
    Diffuse = 0,
    Glossy = 1,
    Singular = 2,
    Straight = 3,
    None = 4,
}

impl Scatter {
    pub(crate) const COUNT: usize = 5;

    fn letter(c: char) -> Option<Scatter> {
        Some(match c {
            'D' => Scatter::Diffuse,
            'G' => Scatter::Glossy,
            'S' => Scatter::Singular,
            's' => Scatter::Straight,
            _ => return None,
        })
    }
}

/// The label of a surface lobe: the OpenPBR component it belongs to — the
/// names Arnold's built-in AOVs use, so `C<RG'coat'>L` reads as it does there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum LobeLabel {
    Diffuse = 1,
    Specular = 2,
    Coat = 3,
    Sheen = 4,
    Transmission = 5,
    Subsurface = 6,
    Translucent = 7,
}

impl LobeLabel {
    pub const ALL: [LobeLabel; 7] = [
        LobeLabel::Diffuse,
        LobeLabel::Specular,
        LobeLabel::Coat,
        LobeLabel::Sheen,
        LobeLabel::Transmission,
        LobeLabel::Subsurface,
        LobeLabel::Translucent,
    ];

    pub fn name(self) -> &'static str {
        match self {
            LobeLabel::Diffuse => "diffuse",
            LobeLabel::Specular => "specular",
            LobeLabel::Coat => "coat",
            LobeLabel::Sheen => "sheen",
            LobeLabel::Transmission => "transmission",
            LobeLabel::Subsurface => "subsurface",
            LobeLabel::Translucent => "translucent",
        }
    }
}

/// What a surface lobe contributes as an event: `R`/`T`, its scatter kind
/// and its label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LobeEvent {
    pub transmit: bool,
    pub scatter: Scatter,
    pub label: LobeLabel,
}

impl LobeEvent {
    pub const fn reflect(scatter: Scatter, label: LobeLabel) -> Self {
        LobeEvent {
            transmit: false,
            scatter,
            label,
        }
    }

    pub const fn transmit(scatter: Scatter, label: LobeLabel) -> Self {
        LobeEvent {
            transmit: true,
            scatter,
            label,
        }
    }
}

/// A label id within an [`Lpe`]: 0 is "no label", `1..=7` the lobe labels
/// (in [`LobeLabel`] order), then the light tags the render was compiled
/// with, then labels that only the expressions mention.
pub type LabelId = u16;

/// The most LPE vars one render accepts: each DFA state holds the set of
/// expressions accepting there as a `u64`.
pub const MAX_EXPRESSIONS: usize = 64;

/// The GGX α at or below which a microfacet lobe is reported as singular
/// (`S`) rather than glossy (`G`). Crust floors α at 1e-4 (roughness 0.01),
/// so a "mirror" is a very narrow continuous lobe; it is still the lobe an
/// LPE user calls singular. 1e-3 is roughness ≈ 0.03.
pub const SINGULAR_ALPHA: f32 = 1e-3;

/// `G`, or `S` for a microfacet lobe of GGX α at most [`SINGULAR_ALPHA`].
pub fn microfacet_scatter(alpha: f32) -> Scatter {
    if alpha <= SINGULAR_ALPHA {
        Scatter::Singular
    } else {
        Scatter::Glossy
    }
}

/// The most lobes one shading point splits into: a MaterialX closure's
/// leaves ([`crate::material::closure::MAX_LEAVES`]), more than OpenPBR's five.
pub const MAX_SPLIT: usize = 8;

/// A BSDF value toward one direction, split by lobe: each lobe's event and
/// its share. The shares sum to what `Material::eval` returns for the same
/// direction — bitwise for a MaterialX closure (the same terms, summed in the
/// same order), to rounding for OpenPBR (whose layering nests the sum).
#[derive(Debug, Clone, Copy)]
pub struct LobeSplit {
    len: usize,
    events: [LobeEvent; MAX_SPLIT],
    values: [glam::Vec3A; MAX_SPLIT],
}

impl Default for LobeSplit {
    fn default() -> Self {
        LobeSplit {
            len: 0,
            events: [LobeEvent::reflect(Scatter::Diffuse, LobeLabel::Diffuse); MAX_SPLIT],
            values: [glam::Vec3A::ZERO; MAX_SPLIT],
        }
    }
}

impl LobeSplit {
    pub fn clear(&mut self) {
        self.len = 0;
    }

    pub fn push(&mut self, event: LobeEvent, value: glam::Vec3A) {
        if self.len < MAX_SPLIT {
            self.events[self.len] = event;
            self.values[self.len] = value;
            self.len += 1;
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = (LobeEvent, glam::Vec3A)> + '_ {
        self.events[..self.len]
            .iter()
            .copied()
            .zip(self.values[..self.len].iter().copied())
    }

    /// Every share multiplied by `k`.
    pub fn scale(&mut self, k: glam::Vec3A) {
        for v in &mut self.values[..self.len] {
            *v *= k;
        }
    }

    /// Share `i` multiplied by `k(i)`, for every share.
    pub fn scale_each(&mut self, k: impl Fn(usize) -> f32) {
        for (i, v) in self.values[..self.len].iter_mut().enumerate() {
            *v *= k(i);
        }
    }

    /// The shares' sum, in order.
    pub fn total(&self) -> glam::Vec3A {
        self.values[..self.len]
            .iter()
            .fold(glam::Vec3A::ZERO, |a, v| a + *v)
    }
}

/// Validates `expr` (with any `lpe:` prefix already stripped) without
/// compiling it: the import-time check that refuses a var with one warning.
pub fn validate(expr: &str) -> Result<(), ParseError> {
    parse::parse(expr).map(|_| ())
}

/// The expression a RenderVar's `sourceName` holds: Hydra puts an `lpe:`
/// prefix on the AOV name, so it is dropped when present.
pub fn strip_prefix(source_name: &str) -> &str {
    source_name.strip_prefix("lpe:").unwrap_or(source_name)
}

#[cfg(test)]
mod tests;
