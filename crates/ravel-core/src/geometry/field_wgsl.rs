// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! WGSL versions of fields (`docs/implementation/gpu-resident-geometry-plan.md`,
//! unit 2).
//!
//! A [`Field`](super::Field) may describe itself as WGSL through
//! [`Field::wgsl`](super::Field::wgsl). **One field is one WGSL function**,
//! `fn field_fn_N(p: vec2<f32>, i: u32) -> T`: the position it is asked about
//! and the element index it reads attributes at. A combinator emits its
//! operands first and calls them from its own function, so a whole field tree
//! is one module with one root function — a combine chain is **one dispatch**,
//! not one per node.
//!
//! This module is pure text and numbers. It names no GPU type, so it lives
//! beside the fields it describes; `ravel-nodes` binds the result to buffers
//! and dispatches it.
//!
//! # Structure is source, numbers are data
//!
//! Every parameter a field has — a frequency, the clock, a curve's control
//! points — is a 32-bit word of [`FieldProgram::params`], read in the shader
//! through `pf(k)` / `field_params[k]`. The generated *source* therefore
//! depends on the tree's shape only, and the shader cache (keyed by source)
//! does not recompile when a parameter animates. Only a change that alters the
//! text — a different node, a different attribute type — builds a new pipeline.
//!
//! # CPU fallback
//!
//! `wgsl` returns `None` for a field with no WGSL version, **and** for every
//! configuration whose CPU answer is a warning-and-fallback (an unknown
//! attribute, a source of the wrong type): the CPU path is the reference and
//! already produces the right value and the right log line, so the GPU path
//! declines rather than re-implementing each of those. A `None` anywhere in a
//! tree makes the whole tree [`FieldProgram::compile`] to `None`, and the
//! caller samples it on the CPU. Expression fields are in that set on purpose
//! (see [`ExpressionField`](super::ExpressionField)).
//!
//! # Calling a program from another kernel
//!
//! [`FieldProgram::wgsl`] emits the helper prelude, the declarations and the
//! functions; a kernel that wants field values — `field.apply` here, a particle
//! force later — puts that text in its module and calls
//! [`FieldProgram::root_call`] per element.

use std::fmt::Write as _;

use super::{AttributeSet, AttributeType, CombineMode, Field};
use crate::eval::EvalContext;

/// The WGSL helper functions every field function may call.
const PRELUDE: &str = include_str!("field_prelude.wgsl");

/// How many distinct attributes one program may read.
///
/// Each is a storage-buffer binding, and a compute stage is guaranteed only
/// eight. The `field.apply` kernel spends five on its own columns (positions,
/// the target's old and new values, the group, the program's parameters), so
/// three are left. A tree that needs more is sampled on the CPU.
pub const MAX_FIELD_ATTRIBUTES: usize = 3;

/// The WGSL type an attribute column of `ty` is declared as, or `None` for
/// `Str`. A column is a tightly packed `array<T>` of it.
///
/// `Vec3` is `vec3<f32>`, whose array stride is 16 bytes; `Color` and `Vec4`
/// are both `vec4<f32>`, so the type alone does not tell them apart.
pub const fn wgsl_element_type(ty: AttributeType) -> Option<&'static str> {
    match ty {
        AttributeType::F32 => Some("f32"),
        AttributeType::Vec2 => Some("vec2<f32>"),
        AttributeType::Vec3 => Some("vec3<f32>"),
        AttributeType::Vec4 | AttributeType::Color => Some("vec4<f32>"),
        AttributeType::I32 => Some("i32"),
        AttributeType::Bool => Some("u32"),
        AttributeType::Str => None,
    }
}

/// What a field function answers: a type that modulation can carry.
pub(super) fn is_field_type(ty: AttributeType) -> bool {
    matches!(
        ty,
        AttributeType::F32
            | AttributeType::Vec2
            | AttributeType::Vec3
            | AttributeType::Vec4
            | AttributeType::Color
    )
}

/// Resolves what a field may read by name: which attributes the sampled domain
/// has, as a type and a length.
///
/// The CPU path asks the [`AttributeSet`] it was handed; the GPU path asks a
/// geometry whose columns may be resident, and has no `AttributeSet` to give.
pub trait AttributeLookup {
    fn attribute_info(&self, name: &str) -> Option<(AttributeType, usize)>;
}

impl AttributeLookup for AttributeSet {
    fn attribute_info(&self, name: &str) -> Option<(AttributeType, usize)> {
        self.get(name)
            .map(|column| (column.attr_type(), column.len()))
    }
}

/// A WGSL function a field emitted: its name and the type it returns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WgslFn {
    name: String,
    ty: AttributeType,
}

impl WgslFn {
    /// The type the function returns.
    pub fn ty(&self) -> AttributeType {
        self.ty
    }

    /// A call expression: `p` is a `vec2<f32>` position, `i` a `u32` element
    /// index.
    pub fn call(&self, p: &str, i: &str) -> String {
        format!("{}({p}, {i})", self.name)
    }
}

/// The four components of a field value as a `vec4<f32>`, zero past the value's
/// own width — what `transform_components` reads on the CPU.
///
/// With `broadcast`, a scalar fills every lane instead: the promotion rule a
/// binary combination uses (`sampled_component` answers an `F32` column's value
/// for every slot).
pub(super) fn lanes(ty: AttributeType, value: &str, broadcast: bool) -> String {
    match ty {
        AttributeType::F32 if broadcast => format!("vec4<f32>({value})"),
        AttributeType::F32 => format!("vec4<f32>({value}, 0.0, 0.0, 0.0)"),
        AttributeType::Vec2 => format!("vec4<f32>({value}, 0.0, 0.0)"),
        AttributeType::Vec3 => format!("vec4<f32>({value}, 0.0)"),
        _ => value.to_string(),
    }
}

/// How many components a field value of `ty` carries (`component_arity`).
pub(super) fn arity(ty: AttributeType) -> usize {
    match ty {
        AttributeType::F32 => 1,
        AttributeType::Vec2 => 2,
        AttributeType::Vec3 => 3,
        _ => 4,
    }
}

/// The swizzle that narrows a `vec4<f32>` to a value of `ty`.
pub(super) fn narrow(ty: AttributeType) -> &'static str {
    match ty {
        AttributeType::F32 => ".x",
        AttributeType::Vec2 => ".xy",
        AttributeType::Vec3 => ".xyz",
        _ => "",
    }
}

/// Builds a field tree's WGSL: the functions, the parameter words, and the
/// attributes the tree reads.
///
/// Handed to [`Field::wgsl`](super::Field::wgsl). A combinator calls
/// `wgsl` on its operands with the same builder before emitting its own
/// function, so operand functions precede their callers in the module.
pub struct FieldWgsl<'a> {
    ctx: &'a EvalContext,
    attributes_of: &'a dyn AttributeLookup,
    length: usize,
    functions: String,
    params: Vec<u32>,
    attributes: Vec<(String, AttributeType)>,
    emitted: usize,
}

impl<'a> FieldWgsl<'a> {
    fn new(ctx: &'a EvalContext, attributes_of: &'a dyn AttributeLookup, length: usize) -> Self {
        Self {
            ctx,
            attributes_of,
            length,
            functions: String::new(),
            params: Vec::new(),
            attributes: Vec::new(),
            emitted: 0,
        }
    }

    /// The evaluation context the program is built for. A field that reads the
    /// clock reads it here, **once**, and ships the value as a parameter.
    pub fn ctx(&self) -> &EvalContext {
        self.ctx
    }

    /// Element count of the batch being sampled: an attribute of another
    /// length is not readable (the CPU path warns and reads the default).
    pub fn length(&self) -> usize {
        self.length
    }

    /// Type and length of attribute `name` on the sampled domain.
    pub fn lookup(&self, name: &str) -> Option<(AttributeType, usize)> {
        self.attributes_of.attribute_info(name)
    }

    /// A float parameter. Returns the WGSL expression that reads it.
    pub fn f32(&mut self, value: f32) -> String {
        let index = self.words(&[value.to_bits()]);
        format!("pf({index}u)")
    }

    /// An integer parameter. Returns the WGSL expression that reads it.
    pub fn u32(&mut self, value: u32) -> String {
        let index = self.words(&[value]);
        format!("field_params[{index}u]")
    }

    /// A run of parameter words laid out by the caller (a curve's points, a
    /// ramp's stops). Returns the index of the first, for the helper that walks
    /// them.
    pub fn block(&mut self, words: &[u32]) -> usize {
        self.words(words)
    }

    fn words(&mut self, words: &[u32]) -> usize {
        let index = self.params.len();
        self.params.extend_from_slice(words);
        index
    }

    /// The binding name of attribute `name` read as a column of `ty`, claiming
    /// a binding the first time it is asked for. `None` once the program would
    /// need more than [`MAX_FIELD_ATTRIBUTES`].
    pub fn attribute(&mut self, name: &str, ty: AttributeType) -> Option<String> {
        wgsl_element_type(ty)?;
        let slot = match self
            .attributes
            .iter()
            .position(|(known, known_ty)| known == name && *known_ty == ty)
        {
            Some(slot) => slot,
            None => {
                if self.attributes.len() == MAX_FIELD_ATTRIBUTES {
                    return None;
                }
                self.attributes.push((name.to_string(), ty));
                self.attributes.len() - 1
            }
        };
        Some(format!("field_attr_{slot}"))
    }

    /// Emit `fn field_fn_N(p: vec2<f32>, i: u32) -> ty { body }`.
    pub fn function(&mut self, ty: AttributeType, body: &str) -> WgslFn {
        let name = format!("field_fn_{}", self.emitted);
        self.emitted += 1;
        let ret = wgsl_element_type(ty).expect("a field function returns a column type");
        writeln!(
            self.functions,
            "fn {name}(p: vec2<f32>, i: u32) -> {ret} {{\n{body}\n}}\n"
        )
        .expect("writing to a String cannot fail");
        WgslFn { name, ty }
    }

    /// A function that answers `value` (a WGSL expression of type `ty`) at every
    /// position.
    pub fn constant(&mut self, ty: AttributeType, value: &str) -> WgslFn {
        self.function(ty, &format!("    return {value};"))
    }

    /// Combine two operands component-wise with `operation`, a WGSL expression
    /// over `l` and `r` (both `vec4<f32>`), following the CPU's typing rule:
    /// two operands of one field type combine slot for slot, and a scalar
    /// broadcasts into the other's type. `None` for operands that do not combine.
    pub fn binary(&mut self, left: &WgslFn, right: &WgslFn, operation: &str) -> Option<WgslFn> {
        let ty = match (left.ty, right.ty) {
            (l, r) if l == r && is_field_type(l) => l,
            (AttributeType::F32, other) | (other, AttributeType::F32) if is_field_type(other) => {
                other
            }
            _ => return None,
        };
        let body = format!(
            "    let l = {};\n    let r = {};\n    return ({operation}){};",
            lanes(left.ty, &left.call("p", "i"), true),
            lanes(right.ty, &right.call("p", "i"), true),
            narrow(ty),
        );
        Some(self.function(ty, &body))
    }
}

/// Everything a kernel needs to evaluate one field tree on the GPU.
#[derive(Debug)]
pub struct FieldProgram {
    functions: String,
    params: Vec<u32>,
    attributes: Vec<(String, AttributeType)>,
    root: WgslFn,
}

impl FieldProgram {
    /// Build the program for `field`, or `None` when some node of the tree has
    /// no WGSL version (see the module documentation).
    ///
    /// `attributes_of` answers for the domain the field is sampled over, which
    /// is what the CPU path's [`FieldSample::attributes`](super::FieldSample)
    /// is, and `length` the batch's element count.
    pub fn compile(
        field: &dyn Field,
        ctx: &EvalContext,
        attributes_of: &dyn AttributeLookup,
        length: usize,
    ) -> Option<Self> {
        let mut builder = FieldWgsl::new(ctx, attributes_of, length);
        let root = field.wgsl(&mut builder)?;
        Some(Self {
            functions: builder.functions,
            params: builder.params,
            attributes: builder.attributes,
            root,
        })
    }

    /// The type the root function returns.
    pub fn root_type(&self) -> AttributeType {
        self.root.ty
    }

    /// The parameter words, to be bound as `field_params`. Never empty: a
    /// storage binding cannot be zero-sized.
    pub fn params(&self) -> Vec<u32> {
        if self.params.is_empty() {
            vec![0]
        } else {
            self.params.clone()
        }
    }

    /// The attributes the program reads, in binding order: slot `k` is bound at
    /// `first_binding + 1 + k` and read as a column of the type given.
    pub fn attributes(&self) -> &[(String, AttributeType)] {
        &self.attributes
    }

    /// `p`: a `vec2<f32>` position; `i`: the element index (`u32`).
    pub fn root_call(&self, p: &str, i: &str) -> String {
        self.root.call(p, i)
    }

    /// The module text a kernel includes: the helper prelude, the bindings
    /// (`field_params` at `first_binding`, then one `field_attr_K` per
    /// attribute, all read-only storage in group 0) and the field functions.
    pub fn wgsl(&self, first_binding: u32) -> String {
        let mut out = String::from(PRELUDE);
        let _ = writeln!(
            out,
            "\n@group(0) @binding({first_binding}) var<storage, read> field_params: array<u32>;"
        );
        for (slot, (_, ty)) in self.attributes.iter().enumerate() {
            let _ = writeln!(
                out,
                "@group(0) @binding({}) var<storage, read> field_attr_{slot}: array<{}>;",
                first_binding + 1 + slot as u32,
                wgsl_element_type(*ty).expect("attribute types are column types"),
            );
        }
        out.push('\n');
        out.push_str(&self.functions);
        out
    }
}

// ---- field.apply as one kernel ---------------------------------------------------

/// Elements one row of the apply kernel's grid covers. The kernel is
/// dispatched `ceil(count / APPLY_ROW_WIDTH)` rows tall so a column past
/// 65 535 workgroups still fits a dispatch dimension.
pub const APPLY_ROW_WIDTH: u32 = 4096;

/// Threads per workgroup of the apply kernel along x.
pub const APPLY_WORKGROUP: [u32; 2] = [64, 1];

/// What the apply kernel is specialised for. Everything here is part of the
/// generated *source*; `amount` and the element count are the only runtime
/// inputs (the uniform below).
#[derive(Clone, Copy, Debug)]
pub struct ApplyKernelSpec {
    /// `P` of the domain: `Vec2` or `Vec3` (`xy` is what a field is sampled at).
    pub position: AttributeType,
    /// The target column's type.
    pub target: AttributeType,
    pub combine: CombineMode,
    /// Components written, already narrowed to the target's arity
    /// ([`ComponentMask::resolved_for`](super::ComponentMask)).
    pub mask: u8,
    /// Whether a Bool `group` column restricts the affected elements.
    pub group: bool,
}

/// The `field.apply` kernel: sample `program` at every element's position and
/// combine the answer with the target column's existing value.
///
/// **Bindings, group 0**:
///
/// | binding | resource |
/// |---|---|
/// | 0 | uniform `{ count: u32, amount: f32, _: u32, _: u32 }` |
/// | 1 | `P` of the domain, read |
/// | 2 | the target column's existing values, read |
/// | 3 | the new column, read-write |
/// | 4 | the Bool group column, read — **only when `spec.group`** |
/// | next | [`FieldProgram::wgsl`]'s `field_params`, then its attributes |
///
/// Entry point `main`, workgroup [`APPLY_WORKGROUP`], row width
/// [`APPLY_ROW_WIDTH`].
///
/// The per-component arithmetic is `combine_arrays`'s term for term:
/// `existing + (combine(existing, sampled) - existing) * amount`, an element
/// outside the group or a component outside the mask keeps its existing value.
/// `amount == 0` ("no modulation" must be exact) is decided by the caller and
/// never reaches the kernel.
pub fn apply_kernel(program: &FieldProgram, spec: &ApplyKernelSpec) -> String {
    let field_binding = if spec.group { 5 } else { 4 };
    let target = wgsl_element_type(spec.target).expect("a modulatable target");
    let position = wgsl_element_type(spec.position).expect("positions are vectors");
    let position_xy = if spec.position == AttributeType::Vec3 {
        ".xy"
    } else {
        ""
    };
    let combined = match spec.combine {
        CombineMode::Set => "sampled",
        CombineMode::Add => "existing + sampled",
        CombineMode::Multiply => "existing * sampled",
        CombineMode::Min => "min(existing, sampled)",
        CombineMode::Max => "max(existing, sampled)",
    };

    let root = program.root_type();
    let target_arity = arity(spec.target);
    let slot_names = ["x", "y", "z", "w"];
    let mut writes = String::new();
    for (slot, lane) in slot_names.iter().enumerate().take(target_arity) {
        if spec.mask & (1 << slot) == 0 {
            continue;
        }
        let (existing, sampled, result) = if target_arity == 1 {
            ("e".to_string(), "s".to_string(), "result".to_string())
        } else {
            (
                format!("e.{lane}"),
                sample_lane(root, slot),
                format!("result.{lane}"),
            )
        };
        let _ = writeln!(writes, "        {result} = resolve({existing}, {sampled});");
    }

    let group_open = if spec.group {
        "    if (groups[i] != 0u) {\n"
    } else {
        "    {\n"
    };
    let group_binding = if spec.group {
        "@group(0) @binding(4) var<storage, read> groups: array<u32>;\n"
    } else {
        ""
    };

    format!(
        "{field_module}
struct ApplyParams {{
    count: u32,
    amount: f32,
    pad0: u32,
    pad1: u32,
}}

@group(0) @binding(0) var<uniform> params: ApplyParams;
@group(0) @binding(1) var<storage, read> positions: array<{position}>;
@group(0) @binding(2) var<storage, read> existing_column: array<{target}>;
@group(0) @binding(3) var<storage, read_write> output_column: array<{target}>;
{group_binding}
fn resolve(existing: f32, sampled: f32) -> f32 {{
    let combined = {combined};
    return existing + (combined - existing) * params.amount;
}}

@compute @workgroup_size({wx}, {wy}, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let i = gid.y * {row}u + gid.x;
    if (i >= params.count) {{
        return;
    }}
    let e = existing_column[i];
    var result = e;
{group_open}        let p = positions[i]{position_xy};
        let s = {root_call};
{writes}    }}
    output_column[i] = result;
}}
",
        field_module = program.wgsl(field_binding),
        wx = APPLY_WORKGROUP[0],
        wy = APPLY_WORKGROUP[1],
        row = APPLY_ROW_WIDTH,
        root_call = program.root_call("p", "i"),
    )
}

/// The sampled value's component for target slot `slot`: an `F32` answer
/// broadcasts into every slot, a vector reads slot for slot (`sampled_component`).
fn sample_lane(root: AttributeType, slot: usize) -> String {
    if root == AttributeType::F32 {
        "s".to_string()
    } else {
        format!("s.{}", ["x", "y", "z", "w"][slot])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{
        AddField, AttributeArray, AttributeField, ConstantField, FieldValue, NoiseField,
    };
    use crate::types::FrameRate;

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (100, 100))
    }

    fn set_with(names: &[&str], length: usize) -> AttributeSet {
        let mut set = AttributeSet::new();
        for name in names {
            set.insert(*name, AttributeArray::F32(vec![0.0; length]))
                .unwrap();
        }
        set
    }

    fn builder<'a>(ctx: &'a EvalContext, set: &'a AttributeSet) -> FieldWgsl<'a> {
        FieldWgsl::new(ctx, set, 4)
    }

    /// The generated text depends on the tree's shape only: the numbers are
    /// parameter words, which is what keeps an animated parameter from
    /// building a pipeline per frame.
    #[test]
    fn parameters_are_data_and_never_part_of_the_source() {
        let set = AttributeSet::new();
        let program = |frequency: f32, seed: u32| {
            FieldProgram::compile(
                &NoiseField {
                    seed,
                    frequency,
                    octaves: 2,
                },
                &ctx(),
                &set,
                4,
            )
            .unwrap()
        };
        let (a, b) = (program(1.0, 0), program(3.5, 9));
        assert_eq!(a.wgsl(4), b.wgsl(4));
        assert_ne!(a.params(), b.params());
        assert_eq!(
            a.params(),
            vec![0, 1.0f32.to_bits(), 2],
            "seed, frequency and octaves, in the order the function reads them"
        );
    }

    #[test]
    fn a_program_with_no_parameters_still_binds_a_word() {
        let set = AttributeSet::new();
        let program = FieldProgram::compile(&AttributeField::new("x"), &ctx(), &set, 4);
        assert!(program.is_none(), "an unknown attribute is left to the CPU");
        let program = FieldProgram::compile(
            &AddField {
                left: FieldValue::new(AttributeField::new("x")),
                right: FieldValue::new(AttributeField::new("x")),
            },
            &ctx(),
            &set_with(&["x"], 4),
            4,
        )
        .unwrap();
        assert_eq!(
            program.params(),
            vec![0],
            "a storage binding is never empty"
        );
    }

    #[test]
    fn an_attribute_claims_one_binding_however_often_it_is_read_and_the_budget_caps_them() {
        let (ctx, set) = (ctx(), AttributeSet::new());
        let mut b = builder(&ctx, &set);
        let first = b.attribute("a", AttributeType::F32).unwrap();
        assert_eq!(
            b.attribute("a", AttributeType::F32).as_deref(),
            Some(first.as_str())
        );
        // The same name as another type is another column to bind.
        assert_ne!(b.attribute("a", AttributeType::I32).unwrap(), first);
        b.attribute("c", AttributeType::F32).unwrap();
        assert!(
            b.attribute("d", AttributeType::F32).is_none(),
            "a fourth binding exceeds MAX_FIELD_ATTRIBUTES"
        );
        assert!(
            b.attribute("a", AttributeType::F32).is_some(),
            "a known one still answers once the budget is spent"
        );
        assert!(
            b.attribute("s", AttributeType::Str).is_none(),
            "Str has no layout"
        );
    }

    #[test]
    fn binary_combination_follows_the_cpu_typing_rule() {
        let (ctx, set) = (ctx(), AttributeSet::new());
        let mut b = builder(&ctx, &set);
        let scalar = b.constant(AttributeType::F32, "1.0");
        let vec2 = b.constant(AttributeType::Vec2, "vec2<f32>(1.0, 2.0)");
        let vec3 = b.constant(AttributeType::Vec3, "vec3<f32>(1.0, 2.0, 3.0)");
        let vec4 = b.constant(AttributeType::Vec4, "vec4<f32>(1.0)");
        let color = b.constant(AttributeType::Color, "vec4<f32>(1.0)");
        let ty =
            |b: &mut FieldWgsl<'_>, l: &WgslFn, r: &WgslFn| b.binary(l, r, "l + r").map(|f| f.ty());
        assert_eq!(ty(&mut b, &scalar, &scalar), Some(AttributeType::F32));
        assert_eq!(ty(&mut b, &vec2, &vec2), Some(AttributeType::Vec2));
        assert_eq!(ty(&mut b, &scalar, &color), Some(AttributeType::Color));
        assert_eq!(ty(&mut b, &vec3, &scalar), Some(AttributeType::Vec3));
        assert_eq!(ty(&mut b, &vec2, &vec3), None, "different widths");
        assert_eq!(
            ty(&mut b, &vec4, &color),
            None,
            "four components each, but not the same type"
        );
    }

    /// `apply_kernel` writes only the components the mask names and gates the
    /// whole body on the group.
    #[test]
    fn the_apply_kernel_is_specialised_on_mask_group_and_mode() {
        let set = AttributeSet::new();
        let program = FieldProgram::compile(&ConstantField(1.0), &ctx(), &set, 4).unwrap();
        let spec = |mask, group, combine| ApplyKernelSpec {
            position: AttributeType::Vec2,
            target: AttributeType::Vec3,
            combine,
            mask,
            group,
        };
        let plain = apply_kernel(&program, &spec(0b101, false, CombineMode::Add));
        assert!(plain.contains("result.x = resolve(e.x, s);"));
        assert!(plain.contains("result.z = resolve(e.z, s);"));
        assert!(!plain.contains("result.y"), "y is not in the mask");
        assert!(plain.contains("existing + sampled"));
        assert!(!plain.contains("groups"));
        let grouped = apply_kernel(&program, &spec(0b101, true, CombineMode::Min));
        assert!(grouped.contains("if (groups[i] != 0u)"));
        assert!(grouped.contains("min(existing, sampled)"));
        assert!(
            grouped.contains("@binding(5) var<storage, read> field_params"),
            "the program's bindings follow the group's"
        );
        assert!(plain.contains("@binding(4) var<storage, read> field_params"));
    }
}
