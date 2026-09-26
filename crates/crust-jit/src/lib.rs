//! A Cranelift JIT for [`crust_mtlx::Program`]s.
//!
//! Step 5 of `docs/shading_performance.md`. A compiled program becomes one
//! machine-code function that fills the same slot array [`Program::eval`]
//! fills, and must fill it with the same bits. That is arranged by splitting
//! the instruction set in two rather than by hoping two compilers agree:
//!
//! - **Emitted inline** are only the ops whose every step is exact IEEE-754
//!   single-precision arithmetic, where any correct compiler produces the same
//!   result: `+ − × ÷`, ordered comparisons and `select` (the divide guard,
//!   `f32::clamp`), `fabs`, and lane shuffles (`convert`, `extract`,
//!   `combine`). Each is emitted in the operand order the interpreter's source
//!   uses, and Cranelift never contracts `a·b + c` into a fused multiply-add,
//!   so nothing is re-associated or fused. Operand broadcasting mirrors
//!   [`Val::zip`] lane for lane, using widths known at compile time.
//! - **Handed back to the interpreter** is everything else — textures, `ln`,
//!   `exp`, `pow`, the trigonometric ops, `min`/`max` (Rust's `minnum`
//!   semantics are not Cranelift's `fmin`), `normalize`, `normalmap`,
//!   `artistic_ior`, the dot products — and any op whose operand width is only
//!   known at run time. The generated code calls [`host_apply`], which runs
//!   crust-mtlx's own interpreter step ([`Program::apply_op`]) on that one op
//!   over the slots computed so far. Those ops are therefore exact by
//!   construction; they only stop paying for dispatch between them.
//!
//! `tests/jit.rs` compares every slot, bitwise, against the interpreter.
//!
//! # `unsafe`
//!
//! This is the one crate in the workspace that is not `forbid(unsafe_code)`,
//! as the plan requires: calling generated code cannot be done without it.
//! `deny(unsafe_code)` holds everywhere except four audited blocks, each with
//! its safety argument beside it: the transmute of the finalized code pointer
//! to a function type ([`JitProgram::new`]), the raw-pointer accesses in the
//! two callbacks ([`host_apply`], [`host_texture`]), and the release of the
//! code memory in `JitProgram`'s `Drop`. The generated code itself reads and
//! writes only slots of the array [`JitProgram::eval`] sizes, which holds
//! because `new` refuses any program whose operands are not all earlier
//! slots.
#![deny(unsafe_code)]

use cranelift_codegen::ir::condcodes::FloatCC;
use cranelift_codegen::ir::immediates::Ieee32;
use cranelift_codegen::ir::{AbiParam, InstBuilder, MemFlagsData, Value, types};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module, default_libcall_names};
use crust_mtlx::{BinOp, Op, Program, ShadeCtx, TextureRef, UnOp, Val};
use std::mem::{offset_of, size_of};

/// The generated function: fills `slots[consts.len()..]` given the constant
/// prefix already in place.
type Shade = extern "C" fn(*mut Val, *const ShadeCtx);

/// A program compiled to machine code, evaluated exactly as
/// [`Program::eval`] evaluates it.
pub struct JitProgram {
    consts: Vec<Val>,
    len: usize,
    func: Shade,
    /// The ops [`host_apply`] is handed pointers to. Boxed so their addresses,
    /// baked into the generated code as constants, never move.
    _ops: Box<[Op]>,
    /// Owns the code `func` points into, freed when the program is dropped.
    /// Behind a `Mutex` only because `JITModule` is `Send` but not `Sync` and
    /// a material is shared across render threads; it is never touched again
    /// until `drop`.
    module: std::sync::Mutex<Option<JITModule>>,
    inline_ops: usize,
    host_ops: usize,
}

impl Drop for JitProgram {
    fn drop(&mut self) {
        let module = match self.module.get_mut() {
            Ok(m) => m.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(module) = module {
            #[allow(unsafe_code)]
            // SAFETY: `func` points into this module's code and is only ever
            // called by `eval`, which borrows `self`. `drop` has `&mut self`,
            // so no `eval` is running and none can start; `func` is dropped
            // with `self` and never called again.
            unsafe {
                module.free_memory();
            }
        }
    }
}

/// Why a program was not compiled.
#[derive(Debug)]
pub struct JitError(String);

impl std::fmt::Display for JitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "shader JIT: {}", self.0)
    }
}

impl std::error::Error for JitError {}

fn err(e: impl std::fmt::Display) -> JitError {
    JitError(e.to_string())
}

impl JitProgram {
    /// Compiles `program` for the host CPU.
    ///
    /// The generated code lives in a module the program owns, and is freed
    /// when the program is dropped, so a host that reloads scenes does not
    /// accumulate executable pages.
    ///
    /// A malformed program — an operand that does not precede its user — is
    /// refused rather than compiled: the generated code reads operands without
    /// the interpreter's bounds checks, so it must never be given one out of
    /// range. The caller then runs the program on the interpreter, which
    /// reads such an operand as zero.
    pub fn new(program: &Program) -> Result<JitProgram, JitError> {
        if !program.is_well_formed() {
            return Err(JitError(
                "an operand does not precede its user; not compiling a malformed program".into(),
            ));
        }
        let mut flags = settings::builder();
        flags.set("opt_level", "speed").map_err(err)?;
        flags.set("is_pic", "false").map_err(err)?;
        flags.set("use_colocated_libcalls", "false").map_err(err)?;
        let isa = cranelift_native::builder()
            .map_err(err)?
            .finish(settings::Flags::new(flags))
            .map_err(err)?;
        let mut jb = JITBuilder::with_isa(isa, default_libcall_names());
        jb.symbol("crust_jit_host_apply", host_apply as *const u8);
        jb.symbol("crust_jit_host_texture", host_texture as *const u8);
        let mut module = JITModule::new(jb);
        let frontend = module.target_config();
        let ptr = frontend.pointer_type();

        let mut host_sig = module.make_signature();
        for t in [ptr, ptr, ptr, ptr] {
            host_sig.params.push(AbiParam::new(t));
        }
        let host_id = module
            .declare_function("crust_jit_host_apply", Linkage::Import, &host_sig)
            .map_err(err)?;
        let mut tex_sig = module.make_signature();
        for t in [ptr, types::F32, types::F32, types::F32, ptr, ptr] {
            tex_sig.params.push(AbiParam::new(t));
        }
        let tex_id = module
            .declare_function("crust_jit_host_texture", Linkage::Import, &tex_sig)
            .map_err(err)?;

        let mut ctx = module.make_context();
        ctx.func.signature.params.push(AbiParam::new(ptr));
        ctx.func.signature.params.push(AbiParam::new(ptr));
        let func_id = module
            .declare_function("crust_jit_shade", Linkage::Local, &ctx.func.signature)
            .map_err(err)?;

        let ops: Box<[Op]> = program.ops.clone().into_boxed_slice();
        let (inline_ops, host_ops);
        {
            let mut fbc = FunctionBuilderContext::new();
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            b.seal_block(entry);
            let host = module.declare_func_in_func(host_id, b.func);
            let texture = module.declare_func_in_func(tex_id, b.func);
            let params = b.block_params(entry).to_vec();
            let mut g = Gen {
                b,
                ptr,
                slots: params[0],
                ctx: params[1],
                host,
                texture,
                lanes: vec![None; program.len()],
                arity: vec![None; program.len()],
            };
            for (i, v) in program.consts.iter().enumerate() {
                // The constant prefix is copied in by `eval`; the generated
                // code only needs the values, as immediates.
                let lanes = v.v.map(|x| g.f32(x));
                g.lanes[i] = Some(lanes);
                g.arity[i] = Some(v.arity);
            }
            let base = program.consts.len();
            let (mut n_inline, mut n_host) = (0, 0);
            for (i, op) in ops.iter().enumerate() {
                if g.emit(op, base + i) {
                    n_inline += 1;
                } else {
                    n_host += 1;
                }
            }
            (inline_ops, host_ops) = (n_inline, n_host);
            g.b.ins().return_(&[]);
            g.b.finalize(frontend);
        }
        module.define_function(func_id, &mut ctx).map_err(err)?;
        module.clear_context(&mut ctx);
        module.finalize_definitions().map_err(err)?;
        let code = module.get_finalized_function(func_id);

        #[allow(unsafe_code)]
        // SAFETY: `code` is the finalized entry of the function defined above
        // with signature `(ptr, ptr) -> ()` in the host's default calling
        // convention, which is `extern "C"` for the native ISA. The module
        // that owns the code moves into the returned program and is freed only
        // in its `Drop`, after which `func` is gone too. Its two arguments are
        // only ever supplied by `JitProgram::eval`, which upholds what the code
        // assumes of them.
        let func: Shade = unsafe { std::mem::transmute::<*const u8, Shade>(code) };
        Ok(JitProgram {
            consts: program.consts.clone(),
            len: program.len(),
            func,
            _ops: ops,
            module: std::sync::Mutex::new(Some(module)),
            inline_ops,
            host_ops,
        })
    }

    /// Evaluates into `slots` exactly as [`Program::eval`] would.
    pub fn eval(&self, ctx: &ShadeCtx, slots: &mut Vec<Val>) {
        // Every op slot is overwritten below, so a buffer already the right
        // length (the usual case: one buffer per thread, reused per hit) is
        // not zeroed first. The constants are copied every time, since the
        // buffer is shared with other materials' programs.
        if slots.len() != self.len {
            slots.clear();
            slots.resize(self.len, Val::ZERO);
        }
        slots[..self.consts.len()].copy_from_slice(&self.consts);
        // The generated code writes slots `consts.len()..len` — each exactly
        // once, in order, reading only lower slots — and reads `ctx` only
        // through `host_apply`. `slots` has exactly `len` initialised
        // elements, so every access is in bounds.
        (self.func)(slots.as_mut_ptr(), ctx);
    }

    /// Ops emitted as machine code, and ops handed back to the interpreter.
    pub fn split(&self) -> (usize, usize) {
        (self.inline_ops, self.host_ops)
    }
}

/// Runs one op through the interpreter, from generated code.
///
/// `op` is one of the program's ops, `slots` the slot array, `slot` the op's
/// own index, `ctx` the shading point. Reads `slots[..slot]` and writes
/// `slots[slot]`.
extern "C" fn host_apply(op: *const Op, slots: *mut Val, slot: usize, ctx: *const ShadeCtx) {
    #[allow(unsafe_code)]
    // SAFETY: the generated code passes `op` as the address of an element of
    // the `JitProgram`'s boxed `_ops`, which outlives every call; `slots` and
    // `ctx` are the pointers `JitProgram::eval` passed in, valid for `len`
    // initialised `Val`s and one `ShadeCtx`; `slot < len`. The shared slice
    // covers `[0, slot)` and the write targets `slot`, so they never overlap,
    // and nothing else touches `slots` during the call.
    unsafe {
        let v = Program::apply_op(&*op, std::slice::from_raw_parts(slots, slot), &*ctx);
        slots.add(slot).write(v);
    }
}

/// Samples a texture from generated code, which has already computed the
/// lookup coordinates and footprint exactly as the interpreter's texture op
/// does; writes the slot's lanes and `arity`.
extern "C" fn host_texture(
    tex: *const TextureRef,
    u: f32,
    v: f32,
    w: f32,
    arity: usize,
    out: *mut Val,
) {
    #[allow(unsafe_code)]
    // SAFETY: `tex` is the address of a `TextureRef` inside one of the
    // `JitProgram`'s boxed `_ops`, which outlives every call; `out` is a slot
    // of the array `JitProgram::eval` passed in, in bounds, and nothing else
    // references it during the call.
    unsafe {
        out.write(Val {
            v: (*tex).eval(u, v, w),
            arity: arity as u8,
        });
    }
}

/// Four lanes of a slot, as SSA values.
type Lanes = [Value; 4];

struct Gen<'a> {
    b: FunctionBuilder<'a>,
    ptr: types::Type,
    slots: Value,
    ctx: Value,
    host: cranelift_codegen::ir::FuncRef,
    texture: cranelift_codegen::ir::FuncRef,
    /// A slot's lanes, when they are live as SSA values; loaded from memory on
    /// first use after an interpreter call wrote them.
    lanes: Vec<Option<Lanes>>,
    /// A slot's width, when it is known at compile time.
    arity: Vec<Option<u8>>,
}

const GUARD: f32 = 1e-20;

/// The slot array is ours alone and every access is in bounds and aligned
/// (see [`JitProgram::eval`]), so loads and stores are trusted.
const TRUSTED: MemFlagsData = MemFlagsData::trusted();

impl Gen<'_> {
    fn f32(&mut self, x: f32) -> Value {
        // By bits, so a constant's sign of zero and NaN payload survive.
        self.b.ins().f32const(Ieee32::with_bits(x.to_bits()))
    }

    fn lane_addr(&self, slot: usize, lane: usize) -> i32 {
        (slot * size_of::<Val>() + offset_of!(Val, v) + lane * 4) as i32
    }

    fn get(&mut self, slot: u32) -> Lanes {
        let slot = slot as usize;
        if let Some(l) = self.lanes[slot] {
            return l;
        }
        let l = [0, 1, 2, 3].map(|i| {
            let off = self.lane_addr(slot, i);
            self.b.ins().load(types::F32, TRUSTED, self.slots, off)
        });
        self.lanes[slot] = Some(l);
        l
    }

    fn put(&mut self, slot: usize, lanes: Lanes, arity: u8) {
        for (i, &v) in lanes.iter().enumerate() {
            let off = self.lane_addr(slot, i);
            self.b.ins().store(TRUSTED, v, self.slots, off);
        }
        let a = self.b.ins().iconst(types::I8, i64::from(arity));
        let off = (slot * size_of::<Val>() + offset_of!(Val, arity)) as i32;
        self.b.ins().store(TRUSTED, a, self.slots, off);
        self.lanes[slot] = Some(lanes);
        self.arity[slot] = Some(arity);
    }

    /// [`Val::zip`]: the result is as wide as the wider operand, and a
    /// one-lane operand is splatted only when the other is wider.
    fn zip(
        &mut self,
        (a, na): (Lanes, u8),
        (b, nb): (Lanes, u8),
        f: impl Fn(&mut Self, Value, Value) -> Value,
    ) -> (Lanes, u8) {
        let n = na.max(nb);
        let a = if na == 1 && n > 1 { [a[0]; 4] } else { a };
        let b = if nb == 1 && n > 1 { [b[0]; 4] } else { b };
        ([0, 1, 2, 3].map(|i| f(self, a[i], b[i])), n)
    }

    fn map(&mut self, (a, n): (Lanes, u8), f: impl Fn(&mut Self, Value) -> Value) -> (Lanes, u8) {
        (a.map(|x| f(self, x)), n)
    }

    /// `if y.abs() > 1e-20 { x / y } else { 0.0 }`.
    fn div_guarded(&mut self, x: Value, y: Value) -> Value {
        let ay = self.b.ins().fabs(y);
        let k = self.f32(GUARD);
        let ok = self.b.ins().fcmp(FloatCC::GreaterThan, ay, k);
        let q = self.b.ins().fdiv(x, y);
        let z = self.f32(0.0);
        self.b.ins().select(ok, q, z)
    }

    /// `f32::clamp`: `if x < lo { x = lo }; if x > hi { x = hi }`.
    fn clamp(&mut self, x: Value, lo: f32, hi: f32) -> Value {
        let (lo, hi) = (self.f32(lo), self.f32(hi));
        let below = self.b.ins().fcmp(FloatCC::LessThan, x, lo);
        let x = self.b.ins().select(below, lo, x);
        let above = self.b.ins().fcmp(FloatCC::GreaterThan, x, hi);
        self.b.ins().select(above, hi, x)
    }

    /// An operand's lanes and width, if its width is known.
    fn operand(&mut self, slot: u32) -> Option<(Lanes, u8)> {
        let n = self.arity[slot as usize]?;
        Some((self.get(slot), n))
    }

    /// Emits `op` into `slot`: inline when it is exact and its operand widths
    /// are known (returns `true`), otherwise as an interpreter call.
    fn emit(&mut self, op: &Op, slot: usize) -> bool {
        match self.inline(op) {
            Some((lanes, n)) => {
                self.put(slot, lanes, n);
                true
            }
            None if matches!(op, Op::Texture { tex: Some(_), .. }) => {
                self.texture(op, slot);
                false
            }
            None => {
                let op_ptr = self.b.ins().iconst(self.ptr, op as *const Op as i64);
                let slot_v = self.b.ins().iconst(self.ptr, slot as i64);
                let args = [op_ptr, self.slots, slot_v, self.ctx];
                self.b.ins().call(self.host, &args);
                self.lanes[slot] = None;
                self.arity[slot] = self.width(op);
                false
            }
        }
    }

    /// A texture lookup: the coordinates and footprint in generated code, in
    /// the interpreter's operand order, then one call to sample.
    fn texture(&mut self, op: &Op, slot: usize) {
        let Op::Texture {
            tex: Some(t),
            scale,
            offset,
            arity,
            ..
        } = op
        else {
            unreachable!("only called for a resolved texture")
        };
        let load =
            |g: &mut Self, off: usize| g.b.ins().load(types::F32, TRUSTED, g.ctx, off as i32);
        let u0 = load(self, offset_of!(ShadeCtx, uv.0));
        let v0 = load(self, offset_of!(ShadeCtx, uv.1));
        let width = load(self, offset_of!(ShadeCtx, uv_width));
        // u = uv.0 * scale[0] + offset[0]; v likewise.
        let (s0, o0, s1, o1) = (
            self.f32(scale[0]),
            self.f32(offset[0]),
            self.f32(scale[1]),
            self.f32(offset[1]),
        );
        let u = self.b.ins().fmul(u0, s0);
        let u = self.b.ins().fadd(u, o0);
        let v = self.b.ins().fmul(v0, s1);
        let v = self.b.ins().fadd(v, o1);
        // w = uv_width * 0.5 * (|scale[0]| + |scale[1]|): the parenthesised
        // sum is of two constants, taken here by the same IEEE addition.
        let half = self.f32(0.5);
        let span = self.f32(scale[0].abs() + scale[1].abs());
        let w = self.b.ins().fmul(width, half);
        let w = self.b.ins().fmul(w, span);
        let tex_ptr = self.b.ins().iconst(self.ptr, t as *const TextureRef as i64);
        let ar = self.b.ins().iconst(self.ptr, i64::from(*arity));
        let off = self
            .b
            .ins()
            .iconst(self.ptr, (slot * size_of::<Val>()) as i64);
        let out = self.b.ins().iadd(self.slots, off);
        self.b
            .ins()
            .call(self.texture, &[tex_ptr, u, v, w, ar, out]);
        self.lanes[slot] = None;
        self.arity[slot] = Some(*arity);
    }

    /// The width `op` produces, when it can be told without running it —
    /// the interpreter's rules, one arm each.
    fn width(&self, op: &Op) -> Option<u8> {
        let w = |s: &u32| self.arity[*s as usize];
        let max =
            |xs: &[&u32]| -> Option<u8> { xs.iter().try_fold(1u8, |m, s| w(s).map(|n| m.max(n))) };
        match op {
            Op::Const(v) => Some(v.arity),
            Op::Texture {
                tex: Some(_),
                arity,
                ..
            } => Some(*arity),
            Op::Texture {
                tex: None,
                fallback,
                ..
            } => Some(fallback.arity),
            Op::TexCoord => Some(2),
            Op::Normal | Op::ViewDirection | Op::Position => Some(3),
            // `normalize` returns a `vector3` — or, for a zero-length input,
            // the input itself, whose width is only equal when it is 3.
            Op::Unary {
                op: UnOp::Normalize,
                a,
            } => (w(a) == Some(3)).then_some(3),
            Op::Unary { a, .. } => w(a),
            Op::Binary { a, b, .. } => max(&[a, b]),
            Op::Mix { fg, bg, m } => max(&[fg, bg, m]),
            Op::Clamp { a, low, high } | Op::Smoothstep { a, low, high } => max(&[a, low, high]),
            Op::Contrast { a, amount, pivot } => max(&[a, amount, pivot]),
            Op::Remap {
                a,
                in_low,
                in_high,
                out_low,
                out_high,
            } => max(&[a, in_low, in_high, out_low, out_high]),
            Op::Invert { a, amount } => max(&[a, amount]),
            Op::Convert { arity, .. } => Some((*arity).clamp(1, 4)),
            Op::Extract { .. } | Op::DotProduct { .. } | Op::Luminance { .. } => Some(1),
            Op::Combine3 { .. } | Op::NormalMap { .. } | Op::ArtisticIor { .. } => Some(3),
            Op::Combine2 { .. } => Some(2),
        }
    }

    /// The lanes of `op`, when it can be emitted exactly; `None` sends it to
    /// the interpreter. Each arm transcribes the interpreter's expression in
    /// the interpreter's operand order.
    fn inline(&mut self, op: &Op) -> Option<(Lanes, u8)> {
        Some(match op {
            Op::Const(v) => (v.v.map(|x| self.f32(x)), v.arity),
            Op::Texture {
                tex: None,
                fallback,
                ..
            } => (fallback.v.map(|x| self.f32(x)), fallback.arity),
            Op::Unary { op: UnOp::Abs, a } => {
                let a = self.operand(*a)?;
                self.map(a, |g, x| g.b.ins().fabs(x))
            }
            Op::Binary { op, a, b } => {
                if !matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div) {
                    return None;
                }
                let (a, b) = (self.operand(*a)?, self.operand(*b)?);
                match op {
                    BinOp::Add => self.zip(a, b, |g, x, y| g.b.ins().fadd(x, y)),
                    BinOp::Sub => self.zip(a, b, |g, x, y| g.b.ins().fsub(x, y)),
                    BinOp::Mul => self.zip(a, b, |g, x, y| g.b.ins().fmul(x, y)),
                    _ => self.zip(a, b, |g, x, y| g.div_guarded(x, y)),
                }
            }
            // bg.zip(1 − m, ×).zip(fg.zip(m, ×), +)
            Op::Mix { fg, bg, m } => {
                let (fg, bg, m) = (self.operand(*fg)?, self.operand(*bg)?, self.operand(*m)?);
                let inv = self.map(m, |g, x| {
                    let one = g.f32(1.0);
                    g.b.ins().fsub(one, x)
                });
                let l = self.zip(bg, inv, |g, b, t| g.b.ins().fmul(b, t));
                let r = self.zip(fg, m, |g, f, t| g.b.ins().fmul(f, t));
                self.zip(l, r, |g, a, b| g.b.ins().fadd(a, b))
            }
            // a.zip(piv, −).zip(amt, ×).zip(piv, +)
            Op::Contrast { a, amount, pivot } => {
                let (a, amt, piv) = (
                    self.operand(*a)?,
                    self.operand(*amount)?,
                    self.operand(*pivot)?,
                );
                let t = self.zip(a, piv, |g, x, p| g.b.ins().fsub(x, p));
                let t = self.zip(t, amt, |g, x, m| g.b.ins().fmul(x, m));
                self.zip(t, piv, |g, x, p| g.b.ins().fadd(x, p))
            }
            // t = a.zip(il, −).zip(ih.zip(il, −), ÷?)
            // ol.zip(oh.zip(ol, −).zip(t, ×), +)
            Op::Remap {
                a,
                in_low,
                in_high,
                out_low,
                out_high,
            } => {
                let (a, il, ih, ol, oh) = (
                    self.operand(*a)?,
                    self.operand(*in_low)?,
                    self.operand(*in_high)?,
                    self.operand(*out_low)?,
                    self.operand(*out_high)?,
                );
                let x = self.zip(a, il, |g, x, l| g.b.ins().fsub(x, l));
                let d = self.zip(ih, il, |g, h, l| g.b.ins().fsub(h, l));
                let t = self.zip(x, d, |g, x, d| g.div_guarded(x, d));
                let span = self.zip(oh, ol, |g, h, l| g.b.ins().fsub(h, l));
                let s = self.zip(span, t, |g, d, t| g.b.ins().fmul(d, t));
                self.zip(ol, s, |g, l, x| g.b.ins().fadd(l, x))
            }
            // amount.zip(a, −)
            Op::Invert { a, amount } => {
                let (a, amt) = (self.operand(*a)?, self.operand(*amount)?);
                self.zip(amt, a, |g, m, x| g.b.ins().fsub(m, x))
            }
            Op::Convert { a, arity } => {
                let (lanes, _) = self.operand(*a)?;
                (lanes, (*arity).clamp(1, 4))
            }
            Op::Extract { a, index } => {
                let (lanes, _) = self.operand(*a)?;
                ([lanes[(*index).min(3)]; 4], 1)
            }
            Op::Combine3 { a, b, c } => {
                let (a, b, c) = (self.operand(*a)?, self.operand(*b)?, self.operand(*c)?);
                let z = self.f32(0.0);
                ([a.0[0], b.0[0], c.0[0], z], 3)
            }
            Op::Combine2 { a, b } => {
                let (a, b) = (self.operand(*a)?, self.operand(*b)?);
                let z = self.f32(0.0);
                ([a.0[0], b.0[0], z, z], 2)
            }
            // a.zip(lo, −).zip(hi.zip(lo, −), |x, d| guarded (x/d).clamp(0,1))
            //  .map(|t| t·t·(3 − 2t))
            Op::Smoothstep { a, low, high } => {
                let (a, lo, hi) = (self.operand(*a)?, self.operand(*low)?, self.operand(*high)?);
                let x = self.zip(a, lo, |g, x, l| g.b.ins().fsub(x, l));
                let d = self.zip(hi, lo, |g, h, l| g.b.ins().fsub(h, l));
                let t = self.zip(x, d, |g, x, d| {
                    let ad = g.b.ins().fabs(d);
                    let k = g.f32(GUARD);
                    let ok = g.b.ins().fcmp(FloatCC::GreaterThan, ad, k);
                    let q = g.b.ins().fdiv(x, d);
                    let q = g.clamp(q, 0.0, 1.0);
                    let z = g.f32(0.0);
                    g.b.ins().select(ok, q, z)
                });
                self.map(t, |g, t| {
                    let tt = g.b.ins().fmul(t, t);
                    let two = g.f32(2.0);
                    let three = g.f32(3.0);
                    let tw = g.b.ins().fmul(two, t);
                    let r = g.b.ins().fsub(three, tw);
                    g.b.ins().fmul(tt, r)
                })
            }
            _ => return None,
        })
    }
}
