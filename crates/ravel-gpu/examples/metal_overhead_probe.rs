// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Narrow probe for the `GPUBK-14` decision gate: how much CPU time and how
//! much blocking-wait latency does the wgpu abstraction add over talking to
//! Metal directly, for the *cost items* already under suspicion.
//!
//! This is deliberately **not** a re-implementation of the 10-layer shell
//! compositing chain. It reproduces the shape of that chain's GPU work — a
//! serial chain of `N` compute passes over a 512x512 `Rgba32Float` texture
//! pair, one bind group / argument set per pass — and measures three things:
//!
//! 1. **Blocking fence-wait granularity.** `wgpu`'s `PollType::Wait` on the
//!    Metal backend polls the command buffer status and `thread::sleep(1ms)`s
//!    when it is not done, so a wait rounds up to a 1 ms multiple
//!    (`perf-baseline.md`). Compared against `waitUntilCompleted` and against
//!    a status spin.
//! 2. **Per-pass encode cost.** `wgpu` records every dispatch as its own
//!    compute pass (`ComputePipeline::dispatch` in `crate::compute`), which on
//!    Metal means one `MTLComputeCommandEncoder` per dispatch. Compared
//!    against the same structure by hand, and against the cheaper structure
//!    `wgpu` cannot express: one encoder with `N` serial dispatches.
//! 3. **The cost of the barrier granularity that follows from (2)**, measured
//!    as GPU wall time for the same dependency chain under both structures.
//!
//! Because the two sides run the same kernel over the same textures with the
//! same threadgroup geometry, the difference is attributable to the
//! description layer. What it does *not* give is a frame-level number: see the
//! `GPUBK-14` section of `docs/implementation/gpu-backend-plan.md` for what
//! this probe is and is not evidence for.
//!
//! Run with `--release`; the numbers are meaningless in a debug build.
//!
//! ```text
//! cargo run --release -p ravel-gpu --example metal_overhead_probe [chain_len...]
//! ```

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("metal_overhead_probe measures the Metal backend; macOS only.");
}

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    probe::run()
}

#[cfg(target_os = "macos")]
mod probe {
    use std::time::{Duration, Instant};

    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_foundation::NSString;
    use objc2_metal::{
        MTLBuffer, MTLCommandBuffer, MTLCommandBufferStatus, MTLCommandEncoder, MTLCommandQueue,
        MTLComputeCommandEncoder, MTLComputePipelineState, MTLCreateSystemDefaultDevice, MTLDevice,
        MTLDispatchType, MTLLibrary, MTLPixelFormat, MTLResourceOptions, MTLSize, MTLStorageMode,
        MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
    };

    // `MTLCreateSystemDefaultDevice` lives in Metal but its symbol resolution
    // pulls in CoreGraphics; without this the example fails to link.
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {}

    /// Matches `perf_baseline`'s `RESOLUTION`.
    const RESOLUTION: u32 = 512;
    /// Matches every compositing shader's `@workgroup_size(8, 8, 1)`.
    const WORKGROUP: u32 = 8;
    /// Chain lengths to sweep, taken from the measured pass counts of the
    /// 10-layer shell chain rather than guessed:
    ///
    /// * `30` — recorded passes per completed evaluation in the 30 fps playback
    ///   form (`perf_baseline` scenario `(g)`: 2330 passes / 77 evaluations =
    ///   30.26, at 0.48 submits per evaluation).
    /// * `50` — recorded passes for the single cold evaluation of the scrub
    ///   form (scenario `(f)`).
    /// * `1` and `10` bracket them from below so the per-pass slope is
    ///   measured rather than assumed.
    const DEFAULT_CHAIN_LENGTHS: [u32; 4] = [1, 10, 30, 50];
    /// A/B rounds, interleaved base/new within each round.
    const ROUNDS: usize = 3;
    /// Iterations per (variant, round) sample.
    const ENCODE_ITERS: usize = 200;
    const WAIT_ITERS: usize = 200;

    const WGSL: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba32float, write>;

struct Params {
    scale: f32,
    pad0: f32,
    pad1: f32,
    pad2: f32,
}
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let size = textureDimensions(dst);
    if (gid.x >= size.x || gid.y >= size.y) {
        return;
    }
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(src, coord, 0);
    textureStore(dst, coord, c * params.scale);
}
"#;

    const MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct Params {
    float scale;
    float pad0;
    float pad1;
    float pad2;
};

kernel void probe_main(
    texture2d<float, access::read> src [[texture(0)]],
    texture2d<float, access::write> dst [[texture(1)]],
    constant Params& params [[buffer(0)]],
    uint2 gid [[thread_position_in_grid]]
) {
    uint2 size = uint2(dst.get_width(), dst.get_height());
    if (gid.x >= size.x || gid.y >= size.y) {
        return;
    }
    float4 c = src.read(gid);
    dst.write(c * params.scale, gid);
}
"#;

    /// Uniform block: `scale` plus padding to 16 bytes, matching the shape the
    /// compositing shaders use.
    const UNIFORM: [f32; 4] = [1.000_1, 0.0, 0.0, 0.0];

    fn median(mut samples: Vec<Duration>) -> Duration {
        samples.sort_unstable();
        samples[samples.len() / 2]
    }

    fn ms(d: Duration) -> f64 {
        d.as_secs_f64() * 1e3
    }

    fn us(d: Duration) -> f64 {
        d.as_secs_f64() * 1e6
    }

    // ---------------------------------------------------------------- wgpu

    struct WgpuRig {
        device: wgpu::Device,
        queue: wgpu::Queue,
        pipeline: wgpu::ComputePipeline,
        /// Bind groups for the two chain directions (A->B, B->A), created up
        /// front the way `crate::dispatch`'s cache serves them in steady state.
        bind_groups: [wgpu::BindGroup; 2],
        adapter_name: String,
    }

    impl WgpuRig {
        fn new() -> anyhow::Result<Self> {
            let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
            desc.backends = wgpu::Backends::METAL;
            let instance = wgpu::Instance::new(desc);
            let adapter =
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                }))?;
            let adapter_name = adapter.get_info().name;
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    label: Some("probe"),
                    ..Default::default()
                }))?;

            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("probe"),
                source: wgpu::ShaderSource::Wgsl(WGSL.into()),
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("probe"),
                layout: None,
                module: &module,
                entry_point: Some("main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });

            let textures: Vec<wgpu::Texture> = (0..2)
                .map(|_| {
                    device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("probe"),
                        size: wgpu::Extent3d {
                            width: RESOLUTION,
                            height: RESOLUTION,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba32Float,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::STORAGE_BINDING
                            | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    })
                })
                .collect();
            let views: Vec<wgpu::TextureView> = textures
                .iter()
                .map(|t| t.create_view(&wgpu::TextureViewDescriptor::default()))
                .collect();

            let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("probe"),
                size: std::mem::size_of_val(&UNIFORM) as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&uniform, 0, bytemuck::cast_slice(&UNIFORM));

            let layout = pipeline.get_bind_group_layout(0);
            let bind_group = |src: usize, dst: usize| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("probe"),
                    layout: &layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&views[src]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&views[dst]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: uniform.as_entire_binding(),
                        },
                    ],
                })
            };
            let bind_groups = [bind_group(0, 1), bind_group(1, 0)];

            Ok(Self {
                device,
                queue,
                pipeline,
                bind_groups,
                adapter_name,
            })
        }

        /// Record `chain` dispatches into a *single* compute pass and submit.
        ///
        /// The control for the pass-per-dispatch structure above: it is still
        /// wgpu, so whatever this recovers is available without a native
        /// backend. `crate::compute::ComputePipeline::dispatch` opens its own
        /// pass per dispatch today, which is a choice in our code rather than
        /// something wgpu forces.
        fn encode_single_pass(&self, chain: u32) -> Duration {
            let groups = RESOLUTION.div_ceil(WORKGROUP);
            let start = Instant::now();
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("probe"),
                });
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("probe"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.pipeline);
                for i in 0..chain {
                    pass.set_bind_group(0, &self.bind_groups[(i % 2) as usize], &[]);
                    pass.dispatch_workgroups(groups, groups, 1);
                }
            }
            let index = self.queue.submit(Some(encoder.finish()));
            let elapsed = start.elapsed();
            let _ = self.device.poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: None,
            });
            elapsed
        }

        /// Record `chain` passes and submit, returning the CPU time spent
        /// describing the work. Mirrors `DispatchState::record` +
        /// `DispatchState::flush` with every cache hit.
        fn encode(&self, chain: u32) -> Duration {
            let groups = RESOLUTION.div_ceil(WORKGROUP);
            let start = Instant::now();
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("probe"),
                });
            for i in 0..chain {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("probe"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_groups[(i % 2) as usize], &[]);
                pass.dispatch_workgroups(groups, groups, 1);
            }
            let index = self.queue.submit(Some(encoder.finish()));
            let elapsed = start.elapsed();
            // Symmetrical with the Metal side: drain after the clock stops so
            // the queue depth cannot drift across samples.
            let _ = self.device.poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: None,
            });
            elapsed
        }

        /// Record, submit, and block until completion.
        fn encode_and_wait(&self, chain: u32) -> Duration {
            let groups = RESOLUTION.div_ceil(WORKGROUP);
            let start = Instant::now();
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("probe"),
                });
            for i in 0..chain {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("probe"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_groups[(i % 2) as usize], &[]);
                pass.dispatch_workgroups(groups, groups, 1);
            }
            let index = self.queue.submit(Some(encoder.finish()));
            let _ = self.device.poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: None,
            });
            start.elapsed()
        }
    }

    // --------------------------------------------------------------- Metal

    /// How the `N` dispatches of a chain are laid out in command encoders.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Layout {
        /// One `MTLComputeCommandEncoder` per dispatch — the structure wgpu
        /// produces, because it records one compute pass per dispatch.
        EncoderPerDispatch,
        /// One encoder for the whole chain, relying on `MTLDispatchType`
        /// `Serial` for the inter-dispatch hazard tracking. wgpu has no way to
        /// express this from a `ComputeDispatch` list.
        SingleSerialEncoder,
    }

    struct MetalRig {
        queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
        pipeline: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
        textures: [Retained<ProtocolObject<dyn MTLTexture>>; 2],
        uniform: Retained<ProtocolObject<dyn MTLBuffer>>,
        device_name: String,
    }

    impl MetalRig {
        fn new() -> anyhow::Result<Self> {
            let device =
                MTLCreateSystemDefaultDevice().ok_or_else(|| anyhow::anyhow!("no Metal device"))?;
            let device_name = device.name().to_string();
            let queue = device
                .newCommandQueue()
                .ok_or_else(|| anyhow::anyhow!("no Metal command queue"))?;

            let library = device
                .newLibraryWithSource_options_error(&NSString::from_str(MSL), None)
                .map_err(|e| anyhow::anyhow!("MSL compile failed: {e:?}"))?;
            let function = library
                .newFunctionWithName(&NSString::from_str("probe_main"))
                .ok_or_else(|| anyhow::anyhow!("kernel probe_main not found"))?;
            let pipeline = device
                .newComputePipelineStateWithFunction_error(&function)
                .map_err(|e| anyhow::anyhow!("pipeline creation failed: {e:?}"))?;

            let mut textures = Vec::with_capacity(2);
            for _ in 0..2 {
                // SAFETY: `texture2DDescriptorWithPixelFormat_width_height_mipmapped`
                // is only unsafe because objc2 cannot prove the format /
                // dimension combination is one Metal accepts. `Rgba32Float` at
                // 512x512 without mipmaps is a valid 2D texture on every Metal
                // device, and the descriptor is consumed immediately below.
                let desc = unsafe {
                    MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                        MTLPixelFormat::RGBA32Float,
                        RESOLUTION as usize,
                        RESOLUTION as usize,
                        false,
                    )
                };
                desc.setUsage(MTLTextureUsage::ShaderRead | MTLTextureUsage::ShaderWrite);
                desc.setStorageMode(MTLStorageMode::Private);
                textures.push(
                    device
                        .newTextureWithDescriptor(&desc)
                        .ok_or_else(|| anyhow::anyhow!("texture allocation failed"))?,
                );
            }
            let textures: [_; 2] = textures
                .try_into()
                .map_err(|_| anyhow::anyhow!("expected two textures"))?;

            let uniform = device
                .newBufferWithLength_options(
                    std::mem::size_of_val(&UNIFORM),
                    MTLResourceOptions::StorageModeShared,
                )
                .ok_or_else(|| anyhow::anyhow!("uniform allocation failed"))?;
            // SAFETY: the buffer was just created with `StorageModeShared`, so
            // its contents pointer is CPU-visible and valid for `length`
            // bytes; `UNIFORM` is exactly that length and no GPU work has been
            // enqueued against the buffer yet, so there is no concurrent read.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    UNIFORM.as_ptr(),
                    uniform.contents().as_ptr().cast::<f32>(),
                    UNIFORM.len(),
                );
            }

            Ok(Self {
                queue,
                pipeline,
                textures,
                uniform,
                device_name,
            })
        }

        /// Encode `chain` dispatches under `layout` and commit, returning the
        /// CPU time spent describing the work. The completion wait happens
        /// after the clock stops, so the queue cannot run arbitrarily far ahead
        /// between samples without the wait being charged to the encode.
        fn encode(&self, chain: u32, layout: Layout) -> Duration {
            self.encode_inner(chain, layout, false)
        }

        /// As [`Self::encode`], but the completion wait is inside the measured
        /// window.
        fn encode_and_wait(&self, chain: u32, layout: Layout) -> Duration {
            self.encode_inner(chain, layout, true)
        }

        fn encode_inner(&self, chain: u32, layout: Layout, wait_inside: bool) -> Duration {
            let groups = RESOLUTION.div_ceil(WORKGROUP) as usize;
            let grid = MTLSize {
                width: groups,
                height: groups,
                depth: 1,
            };
            let threads = MTLSize {
                width: WORKGROUP as usize,
                height: WORKGROUP as usize,
                depth: 1,
            };
            let start = Instant::now();
            let buffer = self.queue.commandBuffer().expect("command buffer");
            match layout {
                Layout::EncoderPerDispatch => {
                    for i in 0..chain {
                        let encoder = buffer.computeCommandEncoder().expect("encoder");
                        self.record(&encoder, i, grid, threads);
                        encoder.endEncoding();
                    }
                }
                Layout::SingleSerialEncoder => {
                    let encoder = buffer
                        .computeCommandEncoderWithDispatchType(MTLDispatchType::Serial)
                        .expect("encoder");
                    for i in 0..chain {
                        self.record(&encoder, i, grid, threads);
                    }
                    encoder.endEncoding();
                }
            }
            buffer.commit();
            if wait_inside {
                buffer.waitUntilCompleted();
                return start.elapsed();
            }
            let elapsed = start.elapsed();
            buffer.waitUntilCompleted();
            elapsed
        }

        fn record(
            &self,
            encoder: &ProtocolObject<dyn MTLComputeCommandEncoder>,
            step: u32,
            grid: MTLSize,
            threads: MTLSize,
        ) {
            let (src, dst) = if step.is_multiple_of(2) {
                (0, 1)
            } else {
                (1, 0)
            };
            encoder.setComputePipelineState(&self.pipeline);
            // SAFETY: both calls are unsafe only because objc2 cannot check
            // that the index matches the kernel's declared argument table.
            // `probe_main` declares `texture(0)`, `texture(1)` and `buffer(0)`
            // exactly as bound here, the resources outlive the command buffer
            // (they are owned by `self`), and the texture usage flags were set
            // to `ShaderRead | ShaderWrite` at creation.
            unsafe {
                encoder.setTexture_atIndex(Some(&self.textures[src]), 0);
                encoder.setTexture_atIndex(Some(&self.textures[dst]), 1);
                encoder.setBuffer_offset_atIndex(Some(&self.uniform), 0, 0);
            }
            encoder.dispatchThreadgroups_threadsPerThreadgroup(grid, threads);
        }

        /// Encode one dispatch, commit, and block until completion using
        /// `waitUntilCompleted`.
        fn wait_blocking(&self) -> Duration {
            self.wait_with(|buffer| buffer.waitUntilCompleted())
        }

        /// Encode one dispatch, commit, and spin on the command buffer status.
        fn wait_spinning(&self) -> Duration {
            self.wait_with(|buffer| {
                while buffer.status() != MTLCommandBufferStatus::Completed {
                    std::hint::spin_loop();
                }
            })
        }

        fn wait_with(&self, finish: impl Fn(&ProtocolObject<dyn MTLCommandBuffer>)) -> Duration {
            let groups = RESOLUTION.div_ceil(WORKGROUP) as usize;
            let grid = MTLSize {
                width: groups,
                height: groups,
                depth: 1,
            };
            let threads = MTLSize {
                width: WORKGROUP as usize,
                height: WORKGROUP as usize,
                depth: 1,
            };
            let start = Instant::now();
            let buffer = self.queue.commandBuffer().expect("command buffer");
            let encoder = buffer.computeCommandEncoder().expect("encoder");
            self.record(&encoder, 0, grid, threads);
            encoder.endEncoding();
            buffer.commit();
            finish(&buffer);
            start.elapsed()
        }
    }

    // ---------------------------------------------------------------- runs

    fn sample<F: FnMut() -> Duration>(iters: usize, mut run: F) -> Duration {
        for _ in 0..20 {
            run();
        }
        median((0..iters).map(|_| run()).collect())
    }

    pub fn run() -> anyhow::Result<()> {
        let chains: Vec<u32> = {
            let args: Vec<String> = std::env::args().skip(1).collect();
            if args.is_empty() {
                DEFAULT_CHAIN_LENGTHS.to_vec()
            } else {
                args.iter()
                    .map(|a| a.parse::<u32>())
                    .collect::<Result<_, _>>()?
            }
        };

        let wgpu_rig = WgpuRig::new()?;
        let metal_rig = MetalRig::new()?;

        println!("# metal_overhead_probe ({RESOLUTION}x{RESOLUTION} Rgba32Float)");
        println!(
            "wgpu adapter: {} / Metal device: {}",
            wgpu_rig.adapter_name, metal_rig.device_name
        );
        println!(
            "release build; {ROUNDS} interleaved rounds, medians of {ENCODE_ITERS} \
             iterations (encode) / {WAIT_ITERS} iterations (wait)"
        );

        println!("\n## (1) blocking wait for one completed dispatch (us)");
        println!("round  wgpu PollType::Wait   Metal waitUntilCompleted   Metal status spin");
        let mut wait_rounds: Vec<(Duration, Duration, Duration)> = Vec::new();
        for round in 1..=ROUNDS {
            // Interleaved within the round, in the same order every time.
            let a = sample(WAIT_ITERS, || wgpu_rig.encode_and_wait(1));
            let b = sample(WAIT_ITERS, || metal_rig.wait_blocking());
            let c = sample(WAIT_ITERS, || metal_rig.wait_spinning());
            println!("{round:<6} {:<21.1} {:<25.1} {:.1}", us(a), us(b), us(c));
            wait_rounds.push((a, b, c));
        }
        let wait_median = (
            median(wait_rounds.iter().map(|r| r.0).collect()),
            median(wait_rounds.iter().map(|r| r.1).collect()),
            median(wait_rounds.iter().map(|r| r.2).collect()),
        );
        println!(
            "median {:<21.1} {:<25.1} {:.1}",
            us(wait_median.0),
            us(wait_median.1),
            us(wait_median.2)
        );

        println!("\n## (2) CPU time to describe and submit an N-pass chain (us)");
        println!(
            "chain  round  wgpu pass/disp  wgpu 1 pass   \
             Metal encoder/dispatch   Metal single encoder"
        );
        for &chain in &chains {
            let mut rounds: Vec<(Duration, Duration, Duration, Duration)> = Vec::new();
            for round in 1..=ROUNDS {
                let a = sample(ENCODE_ITERS, || wgpu_rig.encode(chain));
                let d = sample(ENCODE_ITERS, || wgpu_rig.encode_single_pass(chain));
                let b = sample(ENCODE_ITERS, || {
                    metal_rig.encode(chain, Layout::EncoderPerDispatch)
                });
                let c = sample(ENCODE_ITERS, || {
                    metal_rig.encode(chain, Layout::SingleSerialEncoder)
                });
                println!(
                    "{chain:<6} {round:<6} {:<15.1} {:<13.1} {:<24.1} {:.1}",
                    us(a),
                    us(d),
                    us(b),
                    us(c)
                );
                rounds.push((a, b, c, d));
            }
            let a = median(rounds.iter().map(|r| r.0).collect());
            let b = median(rounds.iter().map(|r| r.1).collect());
            let c = median(rounds.iter().map(|r| r.2).collect());
            let d = median(rounds.iter().map(|r| r.3).collect());
            println!(
                "{chain:<6} median {:<15.1} {:<13.1} {:<24.1} {:.1}",
                us(a),
                us(d),
                us(b),
                us(c)
            );
            println!(
                "{chain:<6} deltas wgpu_pass_per_dispatch - metal_per_dispatch = \
                 {:+.1} us ({:+.3} us/pass); \
                 recoverable inside wgpu (pass/disp - 1 pass) = {:+.1} us; \
                 irreducible (wgpu 1 pass - metal single encoder) = {:+.1} us",
                us(a) - us(b),
                (us(a) - us(b)) / chain as f64,
                us(a) - us(d),
                us(d) - us(c)
            );
        }

        println!("\n## (3) wall time for the chain including GPU completion (ms)");
        println!("chain  round  wgpu          Metal encoder/dispatch   Metal single encoder");
        for &chain in &chains {
            let mut rounds: Vec<(Duration, Duration, Duration)> = Vec::new();
            for round in 1..=ROUNDS {
                let a = sample(ENCODE_ITERS / 4, || wgpu_rig.encode_and_wait(chain));
                let b = sample(ENCODE_ITERS / 4, || {
                    metal_rig.encode_and_wait(chain, Layout::EncoderPerDispatch)
                });
                let c = sample(ENCODE_ITERS / 4, || {
                    metal_rig.encode_and_wait(chain, Layout::SingleSerialEncoder)
                });
                println!(
                    "{chain:<6} {round:<6} {:<13.3} {:<24.3} {:.3}",
                    ms(a),
                    ms(b),
                    ms(c)
                );
                rounds.push((a, b, c));
            }
            let a = median(rounds.iter().map(|r| r.0).collect());
            let b = median(rounds.iter().map(|r| r.1).collect());
            let c = median(rounds.iter().map(|r| r.2).collect());
            println!(
                "{chain:<6} median {:<13.3} {:<24.3} {:.3}   \
                 (wgpu - metal_per_dispatch = {:+.3} ms)",
                ms(a),
                ms(b),
                ms(c),
                ms(a) - ms(b)
            );
        }

        Ok(())
    }
}
