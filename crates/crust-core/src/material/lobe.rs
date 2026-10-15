//! The lobe vocabulary: what a surface lobe is, as an event a light path
//! expression can match — its scatter kind ([`Scatter`]), its OpenPBR
//! component ([`LobeLabel`]) — and a BSDF value split by lobe
//! ([`LobeSplit`]). Materials produce it; `crate::lpe` reads it (and
//! re-exports it, so `crate::material::lobe::LobeSplit` still names it).

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
    pub(crate) const ALL: [Scatter; 5] = [
        Scatter::Diffuse,
        Scatter::Glossy,
        Scatter::Singular,
        Scatter::Straight,
        Scatter::None,
    ];

    pub(crate) fn letter(c: char) -> Option<Scatter> {
        Some(match c {
            'D' => Scatter::Diffuse,
            'G' => Scatter::Glossy,
            'S' => Scatter::Singular,
            's' => Scatter::Straight,
            _ => return None,
        })
    }

    /// The scatter's letter in an event's `<…>` form: `.` for none, which
    /// only the wildcard matches.
    pub fn to_letter(self) -> char {
        match self {
            Scatter::Diffuse => 'D',
            Scatter::Glossy => 'G',
            Scatter::Singular => 'S',
            Scatter::Straight => 's',
            Scatter::None => '.',
        }
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
