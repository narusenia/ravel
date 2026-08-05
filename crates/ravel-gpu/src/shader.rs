// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! WGSL shader module management: compilation, validation, caching, and
//! (debug-only) hot reload.
//!
//! Built-in shaders are embedded at build time with [`include_str!`] for fast
//! startup. User / runtime shaders are compiled on demand. Every WGSL source
//! is first validated with `naga` so compilation failures surface as
//! human-readable, span-annotated diagnostics ([`GpuError::ShaderCompile`])
//! instead of opaque driver panics.
//!
//! Compiled [`wgpu::ShaderModule`]s are cached by a SHA-256 hash of their
//! source so identical sources are only compiled once.
//!
//! The same sources reach a non-wgpu backend through
//! [`ShaderManager::translate`] and [`translate`](crate::translate).

use std::collections::HashMap;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::binding::BindingDesc;
use crate::device::GpuContext;
use crate::error::{GpuError, GpuResult};
use crate::translate::{ShaderTarget, TranslatedShader, translate_wgsl};

/// Built-in shaders embedded into the binary at compile time.
///
/// `(name, wgsl_source)`. Names are stable identifiers used by pipelines.
pub const BUILTIN_SHADERS: &[(&str, &str)] = &[("invert", include_str!("shaders/invert.wgsl"))];

/// Hex-encoded SHA-256 of a shader source. Used as the cache key.
pub fn source_hash(source: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Parse and validate WGSL, keeping what naga produced.
///
/// The single definition of "WGSL Ravel accepts". [`validate_wgsl`] is this
/// function with the result thrown away; [`translate`](crate::translate) is this
/// function with the result used. Sharing it is what makes the acceptance
/// contract for user shaders (REQ-GPU-003) one thing rather than two that can
/// drift: a source cannot be translatable but uncompilable, or vice versa.
pub(crate) fn parse_and_validate(
    name: &str,
    source: &str,
) -> GpuResult<(naga::Module, naga::valid::ModuleInfo)> {
    let module = naga::front::wgsl::parse_str(source).map_err(|e| GpuError::ShaderCompile {
        name: name.to_string(),
        message: e.emit_to_string(source),
    })?;

    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    let info = validator
        .validate(&module)
        .map_err(|e| GpuError::ShaderCompile {
            name: name.to_string(),
            message: e.emit_to_string(source),
        })?;

    Ok((module, info))
}

/// Validate WGSL source with `naga`, returning a human-readable diagnostic on
/// failure. This runs without a GPU device, so it is fully unit-testable and
/// also lets us reject bad user shaders before touching the driver.
pub fn validate_wgsl(name: &str, source: &str) -> GpuResult<()> {
    parse_and_validate(name, source).map(|_| ())
}

/// A compiled shader together with the source it was built from.
#[derive(Clone)]
pub struct CompiledShader {
    /// Logical shader name.
    pub name: String,
    /// The compiled GPU module.
    ///
    /// Crate-internal: the two things a module is for — a
    /// [`ComputePipeline`](crate::ComputePipeline) and a
    /// [`RasterPipeline`](crate::RasterPipeline) — are built inside this
    /// crate, so the compiled artefact never has to be named in the backend's
    /// terms outside it (`GPUBK-4`).
    pub(crate) module: Arc<wgpu::ShaderModule>,
    /// Hash of the source used to build `module`.
    pub hash: String,
}

/// Manages compilation and caching of WGSL shader modules.
pub struct ShaderManager {
    ctx: GpuContext,
    /// name -> currently registered source.
    sources: HashMap<String, String>,
    /// source-hash -> compiled module (deduplicates identical sources).
    cache: HashMap<String, Arc<wgpu::ShaderModule>>,
    /// Compute pipelines built from those modules, shared across the nodes
    /// that need the same one (see [`ShaderManager::compute_pipeline`]).
    pipelines: crate::compute::PipelineCache,
    /// How many sources have been through `validate_wgsl`. Validation is the
    /// expensive half of a compile and a pure function of the source, so a
    /// module-cache hit must not re-run it; the counter is what lets a test
    /// state that.
    validated: usize,
}

impl ShaderManager {
    /// Create a manager and register all built-in shaders.
    pub fn new(ctx: GpuContext) -> Self {
        let mut mgr = Self {
            ctx,
            sources: HashMap::new(),
            cache: HashMap::new(),
            pipelines: crate::compute::PipelineCache::default(),
            validated: 0,
        };
        for (name, src) in BUILTIN_SHADERS {
            mgr.sources.insert((*name).to_string(), (*src).to_string());
        }
        mgr
    }

    /// Number of registered shader sources.
    pub fn len(&self) -> usize {
        self.sources.len()
    }

    /// Whether any shaders are registered.
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Number of distinct compiled modules currently cached.
    pub fn cached_module_count(&self) -> usize {
        self.cache.len()
    }

    /// Number of sources validated since this manager was created; a repeat of
    /// an already-compiled source does not advance it.
    pub fn validated_count(&self) -> usize {
        self.validated
    }

    /// Number of compute pipelines built since this manager was created.
    ///
    /// Repeated requests for the same (source, entry point, layout, workgroup
    /// size) do not advance it — see [`Self::compute_pipeline`].
    pub fn created_pipeline_count(&self) -> usize {
        self.pipelines.created_count()
    }

    /// The compute pipeline for `source`'s `entry_point`, compiling and
    /// building it only the first time.
    ///
    /// This is the one call a GPU node processor needs. Keeping compilation and
    /// pipeline creation together is what lets both be cached: processors are
    /// constructed per node and re-constructed on structural edits, so N nodes
    /// of one type now share a single pipeline instead of each paying for a
    /// driver compile.
    pub fn compute_pipeline(
        &mut self,
        name: &str,
        source: &str,
        entry_point: &str,
        bind_group_layout: &[BindingDesc],
        workgroup_size: [u32; 2],
    ) -> GpuResult<Arc<crate::compute::ComputePipeline>> {
        let compiled = self.compile_source(name, source)?;
        Ok(self.pipelines.get_or_create(
            &self.ctx,
            &compiled,
            entry_point,
            bind_group_layout,
            workgroup_size,
        ))
    }

    /// Translate the shader registered under `name` into `target`.
    ///
    /// The manager holds built-in sources embedded at build time and user /
    /// plugin sources registered at runtime in the same map, so both reach a
    /// backend shading language through this one call — the pair REQ-GPU-002
    /// asks for. Translation needs no device, only the source.
    pub fn translate(&self, name: &str, target: ShaderTarget) -> GpuResult<TranslatedShader> {
        let source = self
            .sources
            .get(name)
            .ok_or_else(|| GpuError::ShaderNotFound(name.to_string()))?;
        translate_wgsl(name, source, target)
    }

    /// Register (or replace) a shader source under `name` without compiling.
    pub fn register(&mut self, name: impl Into<String>, source: impl Into<String>) {
        self.sources.insert(name.into(), source.into());
    }

    /// Validate and compile the shader registered under `name`, returning a
    /// cached module when the identical source was compiled before.
    pub fn compile(&mut self, name: &str) -> GpuResult<CompiledShader> {
        let source = self
            .sources
            .get(name)
            .ok_or_else(|| GpuError::ShaderNotFound(name.to_string()))?
            .clone();
        self.compile_source(name, &source)
    }

    /// Validate and compile arbitrary `source`, registering it under `name`.
    ///
    /// Used for user / runtime shaders.
    pub fn compile_source(&mut self, name: &str, source: &str) -> GpuResult<CompiledShader> {
        let hash = source_hash(source);

        // Cache hit means this exact source already passed `validate_wgsl`.
        // Validation is a pure function of the source, so re-running naga's
        // parse and validate could only reach the same verdict — and doing it
        // ahead of the lookup meant the module cache saved the driver
        // compilation but none of the validation cost, which is the expensive
        // half on a hot path (a processor rebuilt per parameter edit).
        if let Some(module) = self.cache.get(&hash) {
            self.sources.insert(name.to_string(), source.to_string());
            return Ok(CompiledShader {
                name: name.to_string(),
                module: module.clone(),
                hash,
            });
        }

        // A source that fails validation is not registered, so `compile(name)`
        // cannot later serve it as if it had been accepted.
        self.validated += 1;
        validate_wgsl(name, source)?;
        self.sources.insert(name.to_string(), source.to_string());

        let module = self
            .ctx
            .device()
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(name),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
        let module = Arc::new(module);
        self.cache.insert(hash.clone(), module.clone());

        Ok(CompiledShader {
            name: name.to_string(),
            module,
            hash,
        })
    }
}

/// Debug-only shader hot-reload support.
///
/// Watches a directory of `.wgsl` files and reports change events; callers
/// recompile and rebuild affected pipelines. Compiled into the binary only
/// when the `hot-reload` feature is enabled, and only intended for use under
/// `cfg(debug_assertions)`.
#[cfg(feature = "hot-reload")]
pub mod hot_reload {
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::Receiver;

    use notify::{RecursiveMode, Watcher};

    use crate::error::{GpuError, GpuResult};

    /// Watches a shader directory and surfaces changed `.wgsl` paths.
    pub struct ShaderWatcher {
        _watcher: notify::RecommendedWatcher,
        rx: Receiver<PathBuf>,
    }

    impl ShaderWatcher {
        /// Begin watching `dir` recursively for `.wgsl` modifications.
        pub fn new(dir: impl AsRef<Path>) -> GpuResult<Self> {
            let (tx, rx) = std::sync::mpsc::channel();
            let mut watcher =
                notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                    if let Ok(event) = res
                        && matches!(
                            event.kind,
                            notify::EventKind::Modify(_) | notify::EventKind::Create(_)
                        )
                    {
                        for path in event.paths {
                            if path.extension().is_some_and(|ext| ext == "wgsl") {
                                let _ = tx.send(path);
                            }
                        }
                    }
                })
                .map_err(|e| GpuError::HotReload(e.to_string()))?;

            watcher
                .watch(dir.as_ref(), RecursiveMode::Recursive)
                .map_err(|e| GpuError::HotReload(e.to_string()))?;

            Ok(Self {
                _watcher: watcher,
                rx,
            })
        }

        /// Drain and return any shader paths changed since the last poll.
        pub fn poll_changes(&self) -> Vec<PathBuf> {
            let mut changed = Vec::new();
            while let Ok(path) = self.rx.try_recv() {
                changed.push(path);
            }
            changed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding::{BindingKind, ShaderVisibility};

    const GOOD: &str = r#"
@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba32float, write>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(input_tex, coord, 0);
    textureStore(output_tex, coord, vec4<f32>(1.0 - c.rgb, c.a));
}
"#;

    #[test]
    fn source_hash_is_deterministic_and_distinct() {
        assert_eq!(source_hash("abc"), source_hash("abc"));
        assert_ne!(source_hash("abc"), source_hash("abd"));
        // SHA-256 hex is 64 chars.
        assert_eq!(source_hash("abc").len(), 64);
    }

    #[test]
    fn valid_wgsl_passes_validation() {
        assert!(validate_wgsl("good", GOOD).is_ok());
    }

    #[test]
    fn builtin_invert_shader_validates() {
        for (name, src) in BUILTIN_SHADERS {
            validate_wgsl(name, src)
                .unwrap_or_else(|e| panic!("builtin shader '{name}' failed: {e}"));
        }
    }

    /// RESP-3: identical sources share one compiled module, and registering the
    /// same source under a second name still works after the validation moved
    /// behind the cache lookup.
    #[test]
    fn identical_sources_share_one_module_under_either_name() {
        let Some(ctx) = GpuContext::new_blocking().ok() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let mut mgr = ShaderManager::new(ctx);
        let first = mgr.compile_source("good", GOOD).expect("first compile");
        let second = mgr
            .compile_source("good_alias", GOOD)
            .expect("second compile, served from the cache");

        assert_eq!(first.hash, second.hash);
        assert!(Arc::ptr_eq(&first.module, &second.module));
        assert_eq!(mgr.cached_module_count(), 1);
        // The point of the reorder: the second compile skipped naga entirely.
        assert_eq!(
            mgr.validated_count(),
            1,
            "a module-cache hit must not re-validate the source"
        );
        // Both names resolve, so a later `compile(name)` finds either.
        assert!(mgr.compile("good").is_ok());
        assert!(mgr.compile("good_alias").is_ok());
        assert_eq!(
            mgr.validated_count(),
            1,
            "and neither must compiling by name"
        );
    }

    /// A source that fails validation must not be registered, whichever side of
    /// the cache lookup the validation runs on — otherwise `compile(name)` would
    /// later hand the driver something naga rejected.
    #[test]
    fn a_rejected_source_is_not_registered() {
        let Some(ctx) = GpuContext::new_blocking().ok() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let mut mgr = ShaderManager::new(ctx);
        assert!(mgr.compile_source("bad", "@compute fn main( {").is_err());
        assert!(matches!(
            mgr.compile("bad"),
            Err(GpuError::ShaderNotFound(_))
        ));
        assert_eq!(mgr.cached_module_count(), 0);
    }

    /// The pipeline is built once per (source, entry point, layout, workgroup
    /// size), so N nodes of a type share it instead of each paying a driver
    /// compile.
    #[test]
    fn compute_pipelines_are_shared_per_shader_and_entry_point() {
        let Some(ctx) = GpuContext::new_blocking().ok() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let mut mgr = ShaderManager::new(ctx);
        let layout = [
            BindingDesc::new(0, BindingKind::InputTexture, ShaderVisibility::COMPUTE),
            BindingDesc::new(
                1,
                BindingKind::OutputStorageTexture,
                ShaderVisibility::COMPUTE,
            ),
        ];

        let first = mgr
            .compute_pipeline("shared", GOOD, "main", &layout, [8, 8])
            .expect("first pipeline");
        assert_eq!(mgr.created_pipeline_count(), 1);

        // What a parameter edit used to do: rebuild the processor, and with it
        // the pipeline. Now it hands back the same one.
        for _ in 0..5 {
            let again = mgr
                .compute_pipeline("shared", GOOD, "main", &layout, [8, 8])
                .expect("cached pipeline");
            assert!(Arc::ptr_eq(&first, &again));
        }
        assert_eq!(
            mgr.created_pipeline_count(),
            1,
            "repeated requests must not create pipelines"
        );

        // A different workgroup size dispatches differently, so it is a
        // different pipeline even for the same shader and entry point.
        mgr.compute_pipeline("shared", GOOD, "main", &layout, [16, 16])
            .expect("distinct pipeline");
        assert_eq!(mgr.created_pipeline_count(), 2);

        // The cache identifies a layout by its rendered form, so a layout that
        // differs in any field it uses must render differently — otherwise a
        // pipeline would be handed out for a layout it was not built for. Only
        // the identity is asserted here: a layout that disagrees with the
        // shader's declared bindings cannot be turned into a pipeline at all
        // (wgpu rejects it), so the collision this guards against would surface
        // as a driver validation error rather than as wrong pixels.
        for mutate in [
            (|e: &mut BindingDesc| e.binding = 7) as fn(&mut BindingDesc),
            |e| e.visibility = ShaderVisibility::FRAGMENT,
            |e| e.kind = BindingKind::UniformBuffer,
        ] {
            let mut altered = layout;
            mutate(&mut altered[1]);
            assert_ne!(
                format!("{layout:?}"),
                format!("{altered:?}"),
                "every layout field the cache keys on must be part of its identity"
            );
        }
    }

    /// REQ-GPU-002's two halves reach a backend shading language through the
    /// same call: a built-in embedded at build time, and a user / plugin source
    /// that only existed at runtime (REQ-GPU-003, REQ-PLUGIN-002). The runtime
    /// one is compiled for wgpu first, so the test also states that a source
    /// wgpu accepted translates — the two paths agree because they share
    /// [`parse_and_validate`].
    #[test]
    fn the_manager_translates_builtin_and_runtime_sources() {
        let Some(ctx) = GpuContext::new_blocking().ok() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let mut mgr = ShaderManager::new(ctx);
        mgr.compile_source("user_effect", GOOD)
            .expect("a user shader compiles at runtime");

        for name in ["invert", "user_effect"] {
            for target in ShaderTarget::ALL {
                let translated = mgr
                    .translate(name, target)
                    .unwrap_or_else(|e| panic!("'{name}' to {target} failed: {e}"));
                assert_eq!(translated.name(), name);
                assert_eq!(translated.target(), target);
                assert_eq!(translated.entry_points().len(), 1);
            }
        }

        assert!(matches!(
            mgr.translate("nope", ShaderTarget::Msl),
            Err(GpuError::ShaderNotFound(_))
        ));
    }

    /// A source naga rejects is not registered, so it cannot be translated
    /// either — the acceptance contract is one contract, not one per output.
    #[test]
    fn a_rejected_source_translates_nowhere() {
        for target in ShaderTarget::ALL {
            let err = translate_wgsl("bad", "@compute fn main( {", target).unwrap_err();
            assert!(matches!(err, GpuError::ShaderCompile { .. }), "{err:?}");
        }
    }

    #[test]
    fn syntax_error_reports_human_readable_message() {
        let bad = "@compute fn main( { this is not wgsl }";
        let err = validate_wgsl("bad", bad).unwrap_err();
        match err {
            GpuError::ShaderCompile { name, message } => {
                assert_eq!(name, "bad");
                // naga's diagnostic includes the shader label and is non-trivial.
                assert!(!message.is_empty());
                assert!(message.contains("wgsl") || message.contains('^') || message.len() > 10);
            }
            other => panic!("expected ShaderCompile, got {other:?}"),
        }
    }
}
