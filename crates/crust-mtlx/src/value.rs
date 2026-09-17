//! The one runtime value a MaterialX pattern graph carries.
//!
//! MaterialX is statically typed — `float`, `vector2`, `vector3`, `color3`,
//! `color4`, `vector4`, `boolean`, `integer`, plus the opaque `BSDF` and
//! `surfaceshader` types — but the *arithmetic* nodes are all defined
//! componentwise over whichever width they were instantiated at, and the
//! `convert` nodes exist precisely to move between widths. So rather than
//! model the type lattice, every numeric value is carried as four lanes plus
//! the width it was authored at, and each operator works on the lanes.
//!
//! The width is kept (rather than always working in four lanes) for one
//! reason: `float` broadcasts. `multiply` of a `color3` by a `float` scales
//! all three channels, while `multiply` of two `color3`s is per-channel, and
//! the difference is visible — collapsing a float to `(x, 0, 0)` would
//! multiply two channels by zero.

/// A MaterialX numeric value: up to four lanes, plus how many are meaningful.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Val {
    pub v: [f32; 4],
    /// Meaningful lanes, 1..=4. A `Val` of arity 1 is a MaterialX `float` and
    /// broadcasts into any binary operation.
    pub arity: u8,
}

impl Val {
    pub const ZERO: Val = Val::float(0.0);
    pub const ONE: Val = Val::float(1.0);

    pub const fn float(x: f32) -> Val {
        Val {
            v: [x, x, x, x],
            arity: 1,
        }
    }

    pub const fn vec2(x: f32, y: f32) -> Val {
        Val {
            v: [x, y, 0.0, 0.0],
            arity: 2,
        }
    }

    pub const fn vec3(x: f32, y: f32, z: f32) -> Val {
        Val {
            v: [x, y, z, 0.0],
            arity: 3,
        }
    }

    pub const fn vec4(x: f32, y: f32, z: f32, w: f32) -> Val {
        Val {
            v: [x, y, z, w],
            arity: 4,
        }
    }

    /// The first lane — what a node expecting a `float` reads.
    #[inline]
    pub fn x(self) -> f32 {
        self.v[0]
    }

    /// The first three lanes as a vector, broadcasting a `float`.
    ///
    /// Broadcasting is what makes `color3` inputs fed by a `float` behave the
    /// way MaterialX's implicit float-to-colour promotion does: a roughness of
    /// `0.4` read as a colour is grey, not red.
    #[inline]
    pub fn rgb(self) -> glam::Vec3A {
        if self.arity == 1 {
            glam::Vec3A::splat(self.v[0])
        } else {
            glam::Vec3A::new(self.v[0], self.v[1], self.v[2])
        }
    }

    /// Rewrites the arity without touching the lanes, for an explicit
    /// `convert` between widths of the same data.
    #[inline]
    pub fn with_arity(mut self, arity: u8) -> Val {
        self.arity = arity.clamp(1, 4);
        self
    }

    /// Widens a `float` to `n` lanes by replicating it; leaves anything else
    /// alone. This is the promotion the componentwise operators apply to
    /// their narrower operand.
    #[inline]
    fn broadcast_to(self, n: u8) -> Val {
        if self.arity == 1 && n > 1 {
            Val {
                v: [self.v[0]; 4],
                arity: n,
            }
        } else {
            self
        }
    }

    /// Applies `f` lane-by-lane over two values, promoting a `float` operand
    /// to the other's width — MaterialX's rule for every binary math node.
    #[inline]
    pub fn zip(self, other: Val, f: impl Fn(f32, f32) -> f32) -> Val {
        let arity = self.arity.max(other.arity);
        let (a, b) = (self.broadcast_to(arity), other.broadcast_to(arity));
        Val {
            v: [
                f(a.v[0], b.v[0]),
                f(a.v[1], b.v[1]),
                f(a.v[2], b.v[2]),
                f(a.v[3], b.v[3]),
            ],
            arity,
        }
    }

    /// Applies `f` to every lane.
    #[inline]
    pub fn map(self, f: impl Fn(f32) -> f32) -> Val {
        Val {
            v: [f(self.v[0]), f(self.v[1]), f(self.v[2]), f(self.v[3])],
            arity: self.arity,
        }
    }
}

impl From<glam::Vec3A> for Val {
    fn from(v: glam::Vec3A) -> Val {
        Val::vec3(v.x, v.y, v.z)
    }
}

/// Parses a MaterialX literal, whose syntax is comma-separated numbers
/// regardless of the declared type (`"0.5"`, `"1, 1, 0.9"`).
///
/// The declared type is passed as `type_name` only to set the arity when the
/// literal itself is ambiguous — `"0.5"` on a `vector2` input is a one-lane
/// literal that must behave as `(0.5, 0.5)`, which broadcasting already gives,
/// so the count of parsed numbers wins whenever there is more than one.
pub fn parse_literal(text: &str, type_name: &str) -> Option<Val> {
    if let Some(b) = parse_bool(text) {
        return Some(Val::float(if b { 1.0 } else { 0.0 }));
    }
    let mut v = [0.0f32; 4];
    let mut n = 0usize;
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if n == 4 {
            break;
        }
        v[n] = part.parse::<f32>().ok()?;
        n += 1;
    }
    if n == 0 {
        return None;
    }
    let arity = if n > 1 { n as u8 } else { arity_of(type_name) };
    // A single number authored on a wide input is a broadcast, so it keeps
    // arity 1 and every lane already holds it.
    Some(if n == 1 {
        Val::float(v[0])
    } else {
        Val { v, arity }
    })
}

fn parse_bool(text: &str) -> Option<bool> {
    match text.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Lane count of a MaterialX type name; 1 for anything unrecognised, which
/// makes an unknown type behave as a broadcasting scalar rather than
/// silently zeroing lanes.
pub fn arity_of(type_name: &str) -> u8 {
    match type_name {
        "vector2" => 2,
        "vector3" | "color3" => 3,
        "vector4" | "color4" => 4,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_broadcasts_into_wider_operands() {
        // `multiply(color3, float)` must scale all three channels. Carrying a
        // float as (x, 0, 0) would zero two of them — the bug this arity
        // field exists to prevent.
        let c = Val::vec3(0.2, 0.4, 0.8);
        let s = Val::float(0.5);
        let got = c.zip(s, |a, b| a * b);
        assert_eq!(got.arity, 3);
        assert_eq!(got.v[0..3], [0.1, 0.2, 0.4]);
    }

    #[test]
    fn scalar_literal_on_a_wide_input_stays_a_broadcast() {
        let v = parse_literal("0.5", "vector2").unwrap();
        assert_eq!(v.arity, 1);
        assert_eq!(v.rgb(), glam::Vec3A::splat(0.5));
    }

    #[test]
    fn vector_literal_takes_its_arity_from_the_text() {
        let v = parse_literal("1, 1, 0.9", "color3").unwrap();
        assert_eq!(v.arity, 3);
        assert_eq!(v.v[0..3], [1.0, 1.0, 0.9]);
    }

    #[test]
    fn booleans_parse_as_zero_or_one() {
        assert_eq!(parse_literal("true", "boolean").unwrap().x(), 1.0);
        assert_eq!(parse_literal("false", "boolean").unwrap().x(), 0.0);
    }
}
