//! The curve mass field's MaterialX nodes: four lookups that read the field
//! of the geometry hit (`crust_mass_color`, `crust_mass_normal`,
//! `crust_mass_depth`, `crust_mass_density`) and the `crust_mass_diffuse_bsdf`
//! leaf that shades around the normal it is given. Their nodedefs ship in
//! [`CRUST_LIBRARY`], a MaterialX document beside the standard library's
//! definitions, so a tool that validates documents can read them.
//!
//! The reader knows nothing about the field itself: a host that bakes one
//! fills [`MassCtx`] per hit with what the program reads
//! ([`Program::mass_reads`](crate::Program::mass_reads) says which), and each
//! lookup returns its `default` input wherever the context has nothing — on
//! a mesh, off a field, or where the field defines no value.

use crate::eval::{Compiler, Op};
use crate::parse::Node;
use crate::surface::InputDef;
use crate::value::Val;
use glam::Vec3A;

/// The crust node library: the nodedefs of every `crust_*` node, as a
/// MaterialX document. Shipped with the reader rather than read by it (the
/// compiler knows its categories), so documents need not include it.
pub const CRUST_LIBRARY: &str = include_str!("../libraries/crust/crust_defs.mtlx");

/// The file [`CRUST_LIBRARY`] is, relative to the crate.
pub const CRUST_LIBRARY_PATH: &str = "libraries/crust/crust_defs.mtlx";

// The nodedef tables, in the library's order; `tests/nodedefs.rs` keeps
// them in step with the file.
#[rustfmt::skip]
mod tables {
    use super::InputDef;
    use crate::value::Val;

    pub const CRUST_MASS_COLOR: &[InputDef] = &[
        InputDef { name: "default", ty: "color3", default: Some(Val::vec3(0.0, 0.0, 0.0)) },
    ];

    pub const CRUST_MASS_NORMAL: &[InputDef] = &[
        InputDef { name: "default", ty: "vector3", default: Some(Val::vec3(0.0, 0.0, 0.0)) },
    ];

    pub const CRUST_MASS_DEPTH: &[InputDef] = &[
        InputDef { name: "default", ty: "float", default: Some(Val::float(0.0)) },
    ];

    pub const CRUST_MASS_DENSITY: &[InputDef] = &[
        InputDef { name: "default", ty: "float", default: Some(Val::float(0.0)) },
    ];

    pub const CRUST_MASS_DIFFUSE_BSDF: &[InputDef] = &[
        InputDef { name: "weight", ty: "float", default: Some(Val::float(1.0)) },
        InputDef { name: "color", ty: "color3", default: Some(Val::vec3(0.18, 0.18, 0.18)) },
        InputDef { name: "normal", ty: "vector3", default: None },
    ];
}

pub use tables::*;

/// What a mass lookup reads off the field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MassProp {
    /// `crust_mass_color`: the coverage-weighted mean colour, `color3`.
    Color,
    /// `crust_mass_normal`: the unit normal of the mass's surface, in world
    /// space, `vector3`.
    Normal,
    /// `crust_mass_depth`: how far inside the mass the point is, in world
    /// units, `float`; 0 outside.
    Depth,
    /// `crust_mass_density`: the strands' coverage, `float`.
    Density,
}

impl MassProp {
    /// The bit [`MassReads`] holds for this property.
    pub const fn bit(self) -> u8 {
        match self {
            MassProp::Color => 1,
            MassProp::Normal => 2,
            MassProp::Depth => 4,
            MassProp::Density => 8,
        }
    }

    /// The node category that reads it.
    pub const fn category(self) -> &'static str {
        match self {
            MassProp::Color => "crust_mass_color",
            MassProp::Normal => "crust_mass_normal",
            MassProp::Depth => "crust_mass_depth",
            MassProp::Density => "crust_mass_density",
        }
    }

    /// The property of the node category `name`, if it is a mass lookup.
    pub fn of_category(name: &str) -> Option<MassProp> {
        [
            MassProp::Color,
            MassProp::Normal,
            MassProp::Depth,
            MassProp::Density,
        ]
        .into_iter()
        .find(|p| p.category() == name)
    }

    /// The output width of the lookup.
    pub const fn arity(self) -> u8 {
        match self {
            MassProp::Color | MassProp::Normal => 3,
            MassProp::Depth | MassProp::Density => 1,
        }
    }
}

/// Which mass properties a program reads: the bits of [`MassProp::bit`].
/// A host fills [`MassCtx`] for these alone — a normal is three trilinear
/// reads and a gradient, which a program reading only the density should
/// not pay for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MassReads(pub u8);

impl MassReads {
    pub const NONE: MassReads = MassReads(0);

    pub fn any(self) -> bool {
        self.0 != 0
    }

    pub fn has(self, prop: MassProp) -> bool {
        self.0 & prop.bit() != 0
    }
}

/// What the field holds at a hit, for the lookups (see [`ShadeCtx::mass`]).
/// `None` in a field, or a lookup it does not answer, reads the node's
/// `default`.
///
/// [`ShadeCtx::mass`]: crate::ShadeCtx::mass
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MassCtx {
    /// The mass colour; `None` for a field baked without one, or where the
    /// host did not read it.
    pub color: Option<Vec3A>,
    /// The mass normal, unit length; `None` where the field defines none
    /// (no surface within its band, a vanishing gradient) or the host did
    /// not read it.
    pub normal: Option<Vec3A>,
    /// Depth into the mass, 0 outside.
    pub depth: f32,
    /// The coverage density.
    pub density: f32,
}

impl Compiler<'_> {
    /// A mass lookup node: [`Op::Mass`] with the node's `default` folded in
    /// as the fallback, like `geompropvalue`'s.
    pub(crate) fn compile_mass(&mut self, node: &Node, prop: MassProp) -> u32 {
        let arity = prop.arity();
        let fallback = match self.literal_of(node, "default") {
            Some(v) => v.broadcast_to(arity),
            None => {
                if node.input("default").is_some() {
                    self.unsupported
                        .insert(format!("{} (connected default)", prop.category()));
                }
                Val::ZERO.broadcast_to(arity)
            }
        };
        self.emit(Op::Mass {
            prop,
            arity,
            fallback,
        })
    }
}
