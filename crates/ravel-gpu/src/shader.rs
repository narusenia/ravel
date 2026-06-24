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

use std::collections::HashMap;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::device::GpuContext;
use crate::error::{GpuError, GpuResult};

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

/// Validate WGSL source with `naga`, returning a human-readable diagnostic on
/// failure. This runs without a GPU device, so it is fully unit-testable and
/// also lets us reject bad user shaders before touching the driver.
pub fn validate_wgsl(name: &str, source: &str) -> GpuResult<()> {
    let module = naga::front::wgsl::parse_str(source).map_err(|e| GpuError::ShaderCompile {
        name: name.to_string(),
        message: e.emit_to_string(source),
    })?;

    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    validator
        .validate(&module)
        .map_err(|e| GpuError::ShaderCompile {
            name: name.to_string(),
            message: e.emit_to_string(source),
        })?;

    Ok(())
}

/// A compiled shader together with the source it was built from.
#[derive(Clone)]
pub struct CompiledShader {
    /// Logical shader name.
    pub name: String,
    /// The compiled GPU module.
    pub module: Arc<wgpu::ShaderModule>,
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
}

impl ShaderManager {
    /// Create a manager and register all built-in shaders.
    pub fn new(ctx: GpuContext) -> Self {
        let mut mgr = Self {
            ctx,
            sources: HashMap::new(),
            cache: HashMap::new(),
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
        validate_wgsl(name, source)?;

        let hash = source_hash(source);
        self.sources.insert(name.to_string(), source.to_string());

        if let Some(module) = self.cache.get(&hash) {
            return Ok(CompiledShader {
                name: name.to_string(),
                module: module.clone(),
                hash,
            });
        }

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
