// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! WGSL → backend shading language translation.
//!
//! Ravel authors every shader once, in WGSL. A backend that is not wgpu wants
//! that source in its own language: Metal wants MSL, D3D12 wants HLSL, Vulkan
//! wants SPIR-V. [`translate_wgsl`] is the single place that conversion
//! happens, and the same call serves both halves of REQ-GPU-002 — a built-in
//! embedded with [`include_str!`] and a user / plugin shader that only exists
//! at runtime are both just a `&str` here.
//!
//! The target is named with this crate's own [`ShaderTarget`], and the result
//! is a [`TranslatedShader`] — text or SPIR-V words plus the entry point names
//! the backend must ask for. No `naga` or `wgpu` type appears in the signature,
//! for the same reason [`BindingDesc`](crate::BindingDesc) and
//! [`TextureFormat`](crate::TextureFormat) do not: a backend replaces the
//! conversion, not its callers.
//!
//! # The acceptance contract is unchanged
//!
//! Translation begins by parsing and validating with
//! `shader::parse_and_validate`, the exact function
//! [`validate_wgsl`](crate::validate_wgsl) is built from. A source that
//! compiles today therefore reaches the writers unchanged, and a source that
//! naga rejects fails with the same [`GpuError::ShaderCompile`] diagnostic it
//! failed with before this module existed (REQ-GPU-003).
//!
//! # Binding slots are derived from the module
//!
//! MSL addresses resources by flat per-stage slot and HLSL by register, so
//! neither can be written without a mapping from WGSL's `@group` / `@binding`
//! pairs. wgpu's own backends build that mapping from a pipeline layout; here
//! there is no pipeline yet, so it is derived from the module itself
//! (`msl_entry_point_resources`, `hlsl_binding_map`) with the same slot
//! discipline wgpu-hal's Metal backend uses: buffers, textures and samplers
//! counted separately, per entry point, in declaration order.
//!
//! Deriving it *per entry point* is what lets a module such as
//! `rasterize.wgsl` translate at all. Its draw pass and its `unpremultiply`
//! compute pass are separate pipelines with separate bind group layouts, so
//! both start numbering at `@binding(0)`; a module-wide slot table would see
//! that as a collision.

use core::fmt;

use crate::error::{GpuError, GpuResult};
use crate::shader::parse_and_validate;

/// Target version of the Metal Shading Language.
///
/// 2.0 is the floor for the features Ravel's shaders already use — writable
/// storage textures in a compute stage need at least MSL 1.2 — and matches the
/// Metal 2 baseline of every macOS release Ravel targets.
const MSL_LANG_VERSION: (u8, u8) = (2, 0);

/// Target version of SPIR-V. 1.0 covers compute and the single raster pass.
const SPIRV_LANG_VERSION: (u8, u8) = (1, 0);

/// A shading language Ravel's WGSL can be translated into.
///
/// Closed over what the planned backends actually consume (REQ-INFRA-009);
/// a backend that needs another language adds its variant then.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShaderTarget {
    /// Metal Shading Language, for the Metal backend.
    Msl,
    /// High-Level Shader Language, for the D3D12 backend.
    Hlsl,
    /// SPIR-V binary, for the Vulkan backend.
    SpirV,
}

impl ShaderTarget {
    /// Every target this crate can translate to.
    ///
    /// Tests iterate it, so a new variant is covered by them the moment it is
    /// added here.
    pub const ALL: [Self; 3] = [Self::Msl, Self::Hlsl, Self::SpirV];

    /// Whether the artifact is source text rather than a binary.
    pub const fn is_text(self) -> bool {
        match self {
            Self::Msl | Self::Hlsl => true,
            Self::SpirV => false,
        }
    }
}

impl fmt::Display for ShaderTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Msl => "MSL",
            Self::Hlsl => "HLSL",
            Self::SpirV => "SPIR-V",
        })
    }
}

/// The artifact a translation produced.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Artifact {
    /// MSL or HLSL source text.
    Text(String),
    /// SPIR-V, as the 32-bit words the format is defined in.
    SpirV(Vec<u32>),
}

/// One WGSL module translated for one backend.
#[derive(Clone, Debug)]
pub struct TranslatedShader {
    name: String,
    target: ShaderTarget,
    artifact: Artifact,
    entry_points: Vec<String>,
}

impl TranslatedShader {
    /// Logical name of the shader this was translated from.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The language the artifact is in.
    pub fn target(&self) -> ShaderTarget {
        self.target
    }

    /// The artifact as source text, or `None` for a binary target.
    pub fn as_text(&self) -> Option<&str> {
        match &self.artifact {
            Artifact::Text(text) => Some(text),
            Artifact::SpirV(_) => None,
        }
    }

    /// The artifact as SPIR-V words, or `None` for a text target.
    ///
    /// Words rather than bytes because that is how the format — and every API
    /// that consumes it — is defined.
    pub fn spirv_words(&self) -> Option<&[u32]> {
        match &self.artifact {
            Artifact::Text(_) => None,
            Artifact::SpirV(words) => Some(words),
        }
    }

    /// The artifact as bytes, for writing to a cache file or handing to a
    /// driver that takes a byte range. SPIR-V words are little-endian, the
    /// endianness the format's magic number declares.
    pub fn to_bytes(&self) -> Vec<u8> {
        match &self.artifact {
            Artifact::Text(text) => text.as_bytes().to_vec(),
            Artifact::SpirV(words) => words.iter().flat_map(|w| w.to_le_bytes()).collect(),
        }
    }

    /// Entry point names *in the artifact*, in the order the WGSL declares
    /// them.
    ///
    /// A writer renames an entry point whose WGSL name collides with a keyword
    /// of the target language, so a backend must look the name up here rather
    /// than reuse the WGSL one.
    pub fn entry_points(&self) -> &[String] {
        &self.entry_points
    }
}

/// Translate WGSL `source` into `target`.
///
/// The source is parsed and validated first, so a malformed shader fails as
/// [`GpuError::ShaderCompile`] with naga's span-annotated diagnostic exactly as
/// [`validate_wgsl`](crate::validate_wgsl) would report it. A source that is
/// valid WGSL but cannot be expressed in the target language fails as
/// [`GpuError::ShaderTranslate`], naming the target and the reason.
pub fn translate_wgsl(
    name: &str,
    source: &str,
    target: ShaderTarget,
) -> GpuResult<TranslatedShader> {
    let (module, info) = parse_and_validate(name, source)?;
    let (artifact, entry_points) = match target {
        ShaderTarget::Msl => write_msl(name, &module, &info)?,
        ShaderTarget::Hlsl => write_hlsl(name, &module, &info)?,
        ShaderTarget::SpirV => write_spirv(name, &module, &info)?,
    };
    Ok(TranslatedShader {
        name: name.to_string(),
        target,
        artifact,
        entry_points,
    })
}

/// Build a translation failure with a reason attached.
fn translate_error(name: &str, target: ShaderTarget, message: impl fmt::Display) -> GpuError {
    GpuError::ShaderTranslate {
        name: name.to_string(),
        target,
        message: message.to_string(),
    }
}

/// Turn a writer's per-entry-point results into one list, or an error.
///
/// Both the MSL and the HLSL writer report an entry point they could not write
/// *inside* an otherwise successful return: the failure is an `Err` element of
/// `entry_point_names`, and the entry point is simply absent from the emitted
/// source. Left unchecked, a shader whose only compute kernel was dropped
/// would count as translated. So a single `Err` fails the whole translation,
/// and the count is checked against the module in case a writer ever skips an
/// entry point without recording anything.
fn collect_entry_points(
    name: &str,
    target: ShaderTarget,
    expected: usize,
    reported: Vec<Result<String, impl fmt::Display>>,
) -> GpuResult<Vec<String>> {
    if reported.len() != expected {
        return Err(translate_error(
            name,
            target,
            format!(
                "the writer emitted {} of {expected} entry points",
                reported.len()
            ),
        ));
    }
    reported
        .into_iter()
        .enumerate()
        .map(|(index, result)| {
            result.map_err(|e| {
                translate_error(
                    name,
                    target,
                    format!("entry point #{index} was skipped: {e}"),
                )
            })
        })
        .collect()
}

/// Per-entry-point MSL resource slots, derived from the module.
///
/// Metal binds resources by flat slot within a stage, with buffers, textures
/// and samplers counted separately. Each entry point gets its own table over
/// only the globals it reaches, which is both what Metal wants and what makes
/// two entry points sharing a `@binding` number translatable.
fn msl_entry_point_resources(
    module: &naga::Module,
    info: &naga::valid::ModuleInfo,
) -> naga::back::msl::EntryPointResourceMap {
    use naga::back::msl;

    let mut per_entry_point = msl::EntryPointResourceMap::default();
    for (index, entry_point) in module.entry_points.iter().enumerate() {
        let function_info = info.get_entry_point(index);
        let mut resources = msl::BindingMap::default();
        let mut buffers: msl::Slot = 0;
        let mut textures: msl::Slot = 0;
        let mut samplers: msl::Slot = 0;

        for (handle, variable) in module.global_variables.iter() {
            let Some(binding) = variable.binding.as_ref() else {
                continue;
            };
            if function_info[handle].is_empty() {
                continue;
            }
            let target = match module.types[variable.ty].inner {
                naga::TypeInner::Image { .. } => {
                    let slot = textures;
                    textures += 1;
                    msl::BindTarget {
                        texture: Some(slot),
                        mutable: is_written(variable),
                        ..Default::default()
                    }
                }
                naga::TypeInner::Sampler { .. } => {
                    let slot = samplers;
                    samplers += 1;
                    msl::BindTarget {
                        sampler: Some(msl::BindSamplerTarget::Resource(slot)),
                        ..Default::default()
                    }
                }
                // Everything else a `@binding` can name is a buffer: a uniform
                // block or a storage buffer.
                _ => {
                    let slot = buffers;
                    buffers += 1;
                    msl::BindTarget {
                        buffer: Some(slot),
                        mutable: is_written(variable),
                        ..Default::default()
                    }
                }
            };
            resources.insert(
                naga::ResourceBinding {
                    group: binding.group,
                    binding: binding.binding,
                },
                target,
            );
        }

        per_entry_point.insert(
            entry_point.name.clone(),
            msl::EntryPointResources {
                resources,
                // Ravel uses no immediates / push constants; a module that did
                // would fail translation rather than be written wrongly.
                immediates_buffer: None,
                // A buffer holding the lengths of the runtime-sized arrays,
                // which `rasterize.wgsl` needs and the writer only reads when
                // the module has one. Reserving the slot unconditionally costs
                // nothing and cannot be forgotten.
                sizes_buffer: Some(buffers),
            },
        );
    }
    per_entry_point
}

/// Whether a global is in an address space the shader may write through.
fn is_written(variable: &naga::GlobalVariable) -> bool {
    match variable.space {
        naga::AddressSpace::Storage { access } => access.contains(naga::StorageAccess::STORE),
        _ => false,
    }
}

/// HLSL registers, derived from the module.
///
/// HLSL has a register class per resource kind (`b` for constant buffers, `t`
/// for read-only views, `u` for writable ones, `s` for samplers) and the writer
/// picks the class from the resource itself, so `@group(g) @binding(b)` maps
/// straight onto `register(_b, space_g)`. Two globals in *different* classes may
/// therefore share a `@binding` number — which is what `rasterize.wgsl` does
/// across its two passes — while the mapping stays a plain identity.
fn hlsl_binding_map(name: &str, module: &naga::Module) -> GpuResult<naga::back::hlsl::BindingMap> {
    use naga::back::hlsl;

    let mut map = hlsl::BindingMap::default();
    for (_, variable) in module.global_variables.iter() {
        let Some(binding) = variable.binding.as_ref() else {
            continue;
        };
        let space = u8::try_from(binding.group).map_err(|_| {
            translate_error(
                name,
                ShaderTarget::Hlsl,
                format!(
                    "global '{}' declares @group({}), beyond the {} register spaces HLSL addresses",
                    variable.name.as_deref().unwrap_or("<unnamed>"),
                    binding.group,
                    u32::from(u8::MAX) + 1,
                ),
            )
        })?;
        map.insert(
            naga::ResourceBinding {
                group: binding.group,
                binding: binding.binding,
            },
            hlsl::BindTarget {
                space,
                register: binding.binding,
                binding_array_size: None,
                dynamic_storage_buffer_offsets_index: None,
                restrict_indexing: false,
            },
        );
    }
    Ok(map)
}

fn write_msl(
    name: &str,
    module: &naga::Module,
    info: &naga::valid::ModuleInfo,
) -> GpuResult<(Artifact, Vec<String>)> {
    let options = naga::back::msl::Options {
        lang_version: MSL_LANG_VERSION,
        per_entry_point_map: msl_entry_point_resources(module, info),
        inline_samplers: Vec::new(),
        spirv_cross_compatibility: false,
        // A resource the derivation above missed must be an error. The default
        // is to emit MSL with placeholder attributes instead, which compiles
        // nowhere and would make this path look like it works.
        fake_missing_bindings: false,
        bounds_check_policies: Default::default(),
        zero_initialize_workgroup_memory: true,
        force_loop_bounding: true,
    };
    // `entry_point: None` writes every entry point, so one module maps to one
    // artifact regardless of how many passes it holds.
    let pipeline_options = naga::back::msl::PipelineOptions::default();

    let (text, translation) =
        naga::back::msl::write_string(module, info, &options, &pipeline_options)
            .map_err(|e| translate_error(name, ShaderTarget::Msl, e))?;
    let entry_points = collect_entry_points(
        name,
        ShaderTarget::Msl,
        module.entry_points.len(),
        translation.entry_point_names,
    )?;
    Ok((Artifact::Text(text), entry_points))
}

fn write_hlsl(
    name: &str,
    module: &naga::Module,
    info: &naga::valid::ModuleInfo,
) -> GpuResult<(Artifact, Vec<String>)> {
    let options = naga::back::hlsl::Options {
        binding_map: hlsl_binding_map(name, module)?,
        // As with MSL: an unmapped resource is a bug in the derivation, not
        // something to paper over with an invented register.
        fake_missing_bindings: false,
        ..Default::default()
    };
    let pipeline_options = naga::back::hlsl::PipelineOptions::default();

    let mut text = String::new();
    let reflection = naga::back::hlsl::Writer::new(&mut text, &options, &pipeline_options)
        .write(module, info, None)
        .map_err(|e| translate_error(name, ShaderTarget::Hlsl, e))?;
    let entry_points = collect_entry_points(
        name,
        ShaderTarget::Hlsl,
        module.entry_points.len(),
        reflection.entry_point_names,
    )?;
    Ok((Artifact::Text(text), entry_points))
}

fn write_spirv(
    name: &str,
    module: &naga::Module,
    info: &naga::valid::ModuleInfo,
) -> GpuResult<(Artifact, Vec<String>)> {
    use naga::back::spv;

    let mut binding_map = spv::BindingMap::default();
    for (_, variable) in module.global_variables.iter() {
        let Some(binding) = variable.binding.as_ref() else {
            continue;
        };
        // SPIR-V decorates a resource with the descriptor set and binding
        // directly, so the mapping is an identity — stated explicitly so that
        // a resource the loop misses becomes an error rather than a guess.
        binding_map.insert(
            naga::ResourceBinding {
                group: binding.group,
                binding: binding.binding,
            },
            spv::BindingInfo {
                descriptor_set: binding.group,
                binding: binding.binding,
                binding_array_size: None,
            },
        );
    }

    let options = spv::Options {
        lang_version: SPIRV_LANG_VERSION,
        // Pinned rather than taken from `Options::default()`, which adds
        // `DEBUG` under `cfg(debug_assertions)`: the artifact of a given source
        // must not depend on the profile Ravel itself was built with, or the
        // shader cache REQ-GPU-002 asks for would key one source to two
        // different binaries.
        flags: spv::WriterFlags::ADJUST_COORDINATE_SPACE
            | spv::WriterFlags::LABEL_VARYINGS
            | spv::WriterFlags::CLAMP_FRAG_DEPTH,
        fake_missing_bindings: false,
        binding_map,
        ..Default::default()
    };

    // `None` writes every entry point into one SPIR-V module, matching the
    // text targets.
    let words = spv::write_vec(module, info, &options, None)
        .map_err(|e| translate_error(name, ShaderTarget::SpirV, e))?;
    // SPIR-V carries the WGSL names verbatim in its `OpEntryPoint`s.
    let entry_points = module
        .entry_points
        .iter()
        .map(|ep| ep.name.clone())
        .collect();
    Ok((Artifact::SpirV(words), entry_points))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One compute entry point, one uniform, one sampled texture, one storage
    /// texture — the shape every Ravel filter node has.
    const COMPUTE: &str = r#"
struct Params { amount: f32, _pad: vec3<f32> }

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var input_tex: texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba32float, write>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(input_tex, coord, 0);
    textureStore(output_tex, coord, c * params.amount);
}
"#;

    /// `rasterize.wgsl` in miniature: a draw pass and a compute pass in one
    /// module, each numbering its bindings from zero, plus a runtime-sized
    /// storage array.
    const TWO_PASSES: &str = r#"
@group(0) @binding(0) var<uniform> resolution: vec2<f32>;
@group(0) @binding(1) var<storage, read> points: array<vec2<f32>>;

@vertex
fn draw_vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let p = points[index] / resolution;
    return vec4<f32>(p, 0.0, 1.0);
}

@fragment
fn draw_fragment() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 0.0, 0.0, 1.0);
}

@group(0) @binding(0) var pass_input: texture_2d<f32>;
@group(0) @binding(1) var pass_output: texture_storage_2d<rgba32float, write>;

@compute @workgroup_size(8, 8, 1)
fn pass_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(pass_output, coord, textureLoad(pass_input, coord, 0));
}
"#;

    #[test]
    fn every_target_translates_a_compute_shader() {
        for target in ShaderTarget::ALL {
            let translated = translate_wgsl("compute", COMPUTE, target)
                .unwrap_or_else(|e| panic!("{target} translation failed: {e}"));
            assert_eq!(translated.target(), target);
            assert_eq!(translated.name(), "compute");
            assert_eq!(translated.entry_points().len(), 1);
            assert!(!translated.to_bytes().is_empty());
        }
    }

    /// Why [`TranslatedShader::entry_points`] exists rather than the caller
    /// reusing the WGSL name: `main` is reserved in MSL, so the writer renames
    /// the function and a backend that assumed otherwise would look up a symbol
    /// the artifact does not contain.
    #[test]
    fn a_reserved_entry_point_name_is_reported_as_renamed() {
        let msl = translate_wgsl("compute", COMPUTE, ShaderTarget::Msl).expect("msl");
        let renamed = &msl.entry_points()[0];
        assert_ne!(renamed, "main", "MSL must not keep a reserved name");
        assert!(
            msl.as_text().is_some_and(|t| t.contains(renamed.as_str())),
            "the reported name must be the one in the artifact",
        );
    }

    #[test]
    fn the_builtin_shader_translates_to_every_target() {
        for (name, source) in crate::shader::BUILTIN_SHADERS {
            for target in ShaderTarget::ALL {
                translate_wgsl(name, source, target)
                    .unwrap_or_else(|e| panic!("builtin '{name}' to {target} failed: {e}"));
            }
        }
    }

    /// The case the plan flagged as the first likely failure: two pipelines in
    /// one module, both starting at `@binding(0)`. MSL and HLSL both address
    /// resources by a flat slot, so this only works because the mapping is
    /// derived per entry point (MSL) or per register class (HLSL).
    #[test]
    fn entry_points_may_reuse_binding_numbers() {
        for target in ShaderTarget::ALL {
            let translated = translate_wgsl("two_passes", TWO_PASSES, target)
                .unwrap_or_else(|e| panic!("{target} translation failed: {e}"));
            assert_eq!(
                translated.entry_points(),
                ["draw_vertex", "draw_fragment", "pass_main"],
                "{target} dropped an entry point",
            );
        }
    }

    /// Text targets carry text and the binary target carries words; neither
    /// pretends to be the other.
    #[test]
    fn the_artifact_matches_the_target_kind() {
        let msl = translate_wgsl("compute", COMPUTE, ShaderTarget::Msl).expect("msl");
        assert!(ShaderTarget::Msl.is_text());
        assert!(msl.as_text().is_some_and(|t| t.contains("metal_stdlib")));
        assert!(msl.spirv_words().is_none());

        let hlsl = translate_wgsl("compute", COMPUTE, ShaderTarget::Hlsl).expect("hlsl");
        assert!(hlsl.as_text().is_some_and(|t| t.contains("register(")));
        assert!(hlsl.spirv_words().is_none());

        let spirv = translate_wgsl("compute", COMPUTE, ShaderTarget::SpirV).expect("spirv");
        assert!(!ShaderTarget::SpirV.is_text());
        assert!(spirv.as_text().is_none());
        let words = spirv.spirv_words().expect("words");
        // The SPIR-V magic number, as the specification fixes it.
        assert_eq!(words.first(), Some(&0x0723_0203));
        assert_eq!(
            spirv.to_bytes().len(),
            words.len() * 4,
            "bytes must be the words little-endian",
        );
        assert_eq!(&spirv.to_bytes()[..4], &words[0].to_le_bytes());
    }

    /// Deriving the MSL slots per entry point is load-bearing: the same module
    /// with one module-wide table (what `fake_missing_bindings` or a naive
    /// mapping would amount to) loses an entry point instead of failing.
    #[test]
    fn a_missing_msl_slot_fails_instead_of_dropping_an_entry_point() {
        let (module, info) = parse_and_validate("two_passes", TWO_PASSES).expect("valid");
        let options = naga::back::msl::Options {
            lang_version: MSL_LANG_VERSION,
            // No map at all: every resource is unmapped.
            per_entry_point_map: naga::back::msl::EntryPointResourceMap::default(),
            fake_missing_bindings: false,
            ..Default::default()
        };
        let (_, translation) = naga::back::msl::write_string(
            &module,
            &info,
            &options,
            &naga::back::msl::PipelineOptions::default(),
        )
        .expect("the writer itself succeeds — that is the trap");
        assert!(
            translation.entry_point_names.iter().any(|r| r.is_err()),
            "an unmapped resource must show up as a skipped entry point",
        );
        // And the checked path turns exactly that into an error.
        assert!(
            collect_entry_points(
                "two_passes",
                ShaderTarget::Msl,
                module.entry_points.len(),
                translation.entry_point_names,
            )
            .is_err()
        );
    }

    /// Instruction opcodes this test walks, from the SPIR-V specification.
    const OP_ENTRY_POINT: u16 = 15;
    /// Number of words at the head of a SPIR-V module before the instructions.
    const SPIRV_HEADER_WORDS: usize = 5;

    /// Walk a SPIR-V module's instruction stream and count `OpEntryPoint`s.
    ///
    /// Verifies the artifact is structurally SPIR-V rather than merely
    /// non-empty: a wrong word count in any instruction header walks off the
    /// end and the count comes out wrong.
    fn spirv_entry_point_count(words: &[u32]) -> usize {
        assert_eq!(words[0], 0x0723_0203, "not a SPIR-V module");
        assert!(words.len() > SPIRV_HEADER_WORDS, "no instructions");
        let mut index = SPIRV_HEADER_WORDS;
        let mut entry_points = 0;
        while index < words.len() {
            let opcode = (words[index] & 0xffff) as u16;
            let length = (words[index] >> 16) as usize;
            assert!(length > 0, "instruction at word {index} has no length");
            if opcode == OP_ENTRY_POINT {
                entry_points += 1;
            }
            index += length;
        }
        assert_eq!(index, words.len(), "instruction stream is not word-exact");
        entry_points
    }

    /// The SPIR-V artifact is a walkable module whose declared entry points are
    /// the ones the translation reported.
    #[test]
    fn the_spirv_artifact_declares_every_entry_point() {
        for (name, source, expected) in
            [("compute", COMPUTE, 1_usize), ("two_passes", TWO_PASSES, 3)]
        {
            let translated =
                translate_wgsl(name, source, ShaderTarget::SpirV).expect("spirv translation");
            let words = translated.spirv_words().expect("words");
            assert_eq!(
                spirv_entry_point_count(words),
                expected,
                "{name} declares the wrong number of entry points",
            );
            assert_eq!(translated.entry_points().len(), expected);
        }
    }

    #[test]
    fn invalid_wgsl_fails_as_a_compile_error_for_every_target() {
        for target in ShaderTarget::ALL {
            let err = translate_wgsl("bad", "@compute fn main( {", target).unwrap_err();
            assert!(
                matches!(err, GpuError::ShaderCompile { ref name, .. } if name == "bad"),
                "{target} reported {err:?} instead of a compile error",
            );
        }
    }

    #[test]
    fn a_translation_failure_names_the_target_and_a_reason() {
        let err = translate_error("thing", ShaderTarget::Hlsl, "no such register class");
        let GpuError::ShaderTranslate {
            name,
            target,
            message,
        } = &err
        else {
            panic!("expected ShaderTranslate, got {err:?}");
        };
        assert_eq!(name, "thing");
        assert_eq!(*target, ShaderTarget::Hlsl);
        assert_eq!(message, "no such register class");
        let rendered = err.to_string();
        assert!(rendered.contains("HLSL"), "{rendered}");
        assert!(rendered.contains("no such register class"), "{rendered}");
    }

    #[test]
    fn every_target_has_a_distinct_name() {
        let mut names: Vec<String> = ShaderTarget::ALL.iter().map(|t| t.to_string()).collect();
        names.sort();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count);
    }
}
