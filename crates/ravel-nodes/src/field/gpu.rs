// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `field.apply` on the GPU (`docs/implementation/gpu-resident-geometry-plan.md`,
//! unit 2).
//!
//! [`GpuFieldBackend::apply`] samples a field tree and combines it into a
//! target column in **one dispatch**: the tree's WGSL functions
//! ([`FieldProgram`]) and the combine/mask/group arithmetic
//! ([`apply_kernel`]) are one module, and the kernel is specialised on the
//! target's type, the combine mode, the component mask and whether a group
//! restricts it. The CPU `apply_field` stays the reference: this path answers
//! `Ok(None)` for everything it does not do and the caller runs the CPU one.
//!
//! # When it runs
//!
//! * A **resident** input ([`GpuGeometry`]) always takes it when the field and
//!   the columns allow: the result is another `GpuGeometry` sharing every
//!   column but the target, and nothing is read back.
//! * A **CPU** geometry takes it only from [`GPU_MIN_ELEMENTS`] elements up.
//!   Below that the upload, the dispatch and the readback cost more than the
//!   CPU loop. Measured (Apple GPU, release build, a machine under load, so
//!   indicative): for `noise(3 octaves) * falloff + constant` the CPU loop took
//!   0.5 ms at 10 000 elements, 1.8 ms at 50 000 (GPU 1.6), 3.4 ms at 100 000
//!   (GPU 1.8) and 36.7 ms at 1 000 000 (GPU 5.7 with the upload and readback
//!   of a CPU geometry, 1.7 ms resident). A cheaper tree crosses later and a
//!   dearer one earlier; the constant sits just above the 50 000 crossing.
//!   Only the columns the kernel reads are uploaded, and only the target is
//!   read back.
//!
//! # What it declines (`Ok(None)`)
//!
//! A field tree with no WGSL version (`field.expression` is one), a `Detail`
//! domain, a non-modulatable target, a sampled type that does not match the
//! target, a missing target with `create_if_missing` off (or created and left
//! unmodulated by `amount == 0`), a group that is not a Bool column of the right
//! length — every case where the CPU path raises an error or warns and falls
//! back. The CPU path is the reference for all of them, so it produces the exact
//! error or warning.
//!
//! # Creating a missing target
//!
//! A target the geometry lacks is created with the CPU path's own rule, shared
//! rather than restated: [`created_column_type`] picks the type and
//! [`created_column`] fills it (`alpha` starts at 1, `Cd` white or its path's
//! colour, anything unreserved the sampled type zeroed). A CPU geometry builds
//! the column and uploads it; a resident one builds it on the host, reading
//! back only the colour columns a *derived* colour follows (none for a constant
//! default). The kernel then treats it as the existing column.
//!
//! # Calling the field from another kernel
//!
//! Particle forces will sample these fields at particle positions. That entry
//! point is not here: it is [`FieldProgram`] itself — `compile` the field
//! against the particle domain's attributes, put [`FieldProgram::wgsl`] in the
//! force kernel, bind [`FieldProgram::params`] and the attribute columns at the
//! bindings it declares, and call [`FieldProgram::root_call`] per particle.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use ravel_core::eval::EvalContext;
use ravel_core::geometry::absent::derived_color_sources;
use ravel_core::geometry::{
    APPLY_ROW_WIDTH, APPLY_WORKGROUP, ApplyKernelSpec, AttributeArray, AttributeLookup,
    AttributeType, Domain, Field, FieldApply, FieldProgram, Geometry, apply_kernel, created_column,
    created_column_type,
};
use ravel_core::types::NodeData;
use ravel_gpu::{
    BindingDesc, BindingKind, BufferBinding, BufferDispatch, BufferPool, BufferUsage, GpuColumn,
    GpuContext, GpuGeometry, ShaderManager, ShaderVisibility,
};

/// CPU-resident geometries with fewer elements than this are modulated on the
/// CPU. A resident input is exempt: it is already in VRAM.
pub const GPU_MIN_ELEMENTS: usize = 1 << 16;

/// Evaluates fields on the GPU for the `field.apply` processors of one
/// evaluation worker. Shares the worker's [`BufferPool`] and owns the shader
/// cache the generated sources compile through.
pub struct GpuFieldBackend {
    ctx: GpuContext,
    shaders: Mutex<ShaderManager>,
    pool: Arc<Mutex<BufferPool>>,
    min_elements: usize,
}

/// What `field.apply` was given, as one of the two representations.
enum Input<'a> {
    Cpu(&'a Geometry),
    Resident(&'a GpuGeometry),
}

/// Answers the field builder's attribute questions for one domain of either
/// representation.
struct DomainView<'a> {
    input: &'a Input<'a>,
    domain: Domain,
}

impl AttributeLookup for DomainView<'_> {
    fn attribute_info(&self, name: &str) -> Option<(AttributeType, usize)> {
        match self.input {
            Input::Cpu(geometry) => geometry.attribute_set(self.domain).attribute_info(name),
            Input::Resident(geometry) => geometry.attribute_info(self.domain, name),
        }
    }
}

/// The types a field can modulate (`component_arity` is `Some`).
fn is_modulatable(ty: AttributeType) -> bool {
    matches!(
        ty,
        AttributeType::F32
            | AttributeType::Vec2
            | AttributeType::Vec3
            | AttributeType::Vec4
            | AttributeType::Color
    )
}

fn component_count(ty: AttributeType) -> usize {
    match ty {
        AttributeType::F32 => 1,
        AttributeType::Vec2 => 2,
        AttributeType::Vec3 => 3,
        _ => 4,
    }
}

fn read_only(binding: u32) -> BindingDesc {
    BindingDesc::new(
        binding,
        BindingKind::ReadOnlyStorageBuffer,
        ShaderVisibility::COMPUTE,
    )
}

impl GpuFieldBackend {
    /// A backend over `ctx`, leasing buffers from `pool`.
    pub fn new(ctx: GpuContext, pool: Arc<Mutex<BufferPool>>) -> Self {
        Self {
            shaders: Mutex::new(ShaderManager::new(ctx.clone())),
            ctx,
            pool,
            min_elements: GPU_MIN_ELEMENTS,
        }
    }

    /// Lower (or raise) the element count at which a CPU geometry is sent to
    /// the GPU. Tests use `0` to exercise the path on small inputs.
    pub fn with_min_elements(mut self, min_elements: usize) -> Self {
        self.min_elements = min_elements;
        self
    }

    /// Compute pipelines this backend has built. Parameters are data, so a
    /// field whose numbers animate must leave it at one.
    pub fn pipelines_created(&self) -> usize {
        self.shaders
            .lock()
            .expect("shader manager poisoned")
            .created_pipeline_count()
    }

    /// The buffer pool this backend leases from.
    pub fn buffer_pool(&self) -> &Arc<Mutex<BufferPool>> {
        &self.pool
    }

    /// Modulate `input` by `field` on the GPU, or `Ok(None)` when the CPU path
    /// has to do it (see the module documentation). The result has the input's
    /// representation.
    pub fn apply(
        &self,
        input: &dyn NodeData,
        spec: &FieldApply<'_>,
        field: &dyn Field,
        ectx: &EvalContext,
    ) -> anyhow::Result<Option<Arc<dyn NodeData>>> {
        let input = if let Some(resident) = input.downcast_ref::<GpuGeometry>() {
            Input::Resident(resident)
        } else if let Some(geometry) = input.downcast_ref::<Geometry>() {
            Input::Cpu(geometry)
        } else {
            return Ok(None);
        };
        match self.apply_inner(&input, spec, field, ectx) {
            // A CPU geometry still has its own path to fall back on; a resident
            // one has nothing but the GPU, so its failure is the node's.
            Err(error) if matches!(input, Input::Cpu(_)) => {
                tracing::warn!(%error, "field.apply: GPU evaluation failed; using the CPU path");
                Ok(None)
            }
            result => result,
        }
    }

    fn apply_inner(
        &self,
        input: &Input<'_>,
        spec: &FieldApply<'_>,
        field: &dyn Field,
        ectx: &EvalContext,
    ) -> anyhow::Result<Option<Arc<dyn NodeData>>> {
        let domain = spec.domain;
        if !matches!(domain, Domain::Point | Domain::Instance) {
            return Ok(None);
        }
        let view = DomainView { input, domain };

        let Some((position_type, count)) = view.attribute_info("P") else {
            return Ok(None);
        };
        if count == 0
            || !matches!(position_type, AttributeType::Vec2 | AttributeType::Vec3)
            || (matches!(input, Input::Cpu(_)) && count < self.min_elements)
        {
            return Ok(None);
        }
        // A missing target is created the way the CPU path creates it, or the
        // CPU path answers with its `AttributeNotFound`.
        let existing_info = view.attribute_info(spec.target);
        if existing_info.is_none() && !spec.create_if_missing {
            return Ok(None);
        }
        let group = (!spec.group.is_empty()).then_some(spec.group);
        if let Some(group) = group
            && view.attribute_info(group) != Some((AttributeType::Bool, count))
        {
            return Ok(None);
        }

        let Some(program) = FieldProgram::compile(field, ectx, &view, count) else {
            return Ok(None);
        };
        let sampled_type = program.root_type();
        // The created column's type is the CPU rule (`created_column_type`),
        // which may differ from the sampled type (`Cd` is a Color however the
        // field samples).
        let creating = existing_info.is_none();
        let (target_type, target_length) = existing_info.unwrap_or((
            created_column_type(domain, spec.target, sampled_type),
            count,
        ));
        if !is_modulatable(target_type) || target_length != count {
            return Ok(None);
        }
        if sampled_type != AttributeType::F32 && sampled_type != target_type {
            return Ok(None);
        }

        // "No modulation" is exact: the column is returned untouched. A column
        // that had to be created still appears (the CPU inserts it), which the
        // CPU path does.
        let amount = spec.amount.clamp(0.0, 1.0);
        if amount == 0.0 && creating {
            return Ok(None);
        }
        if amount == 0.0 {
            return Ok(Some(match input {
                Input::Cpu(geometry) => Arc::new((*geometry).clone()),
                Input::Resident(geometry) => Arc::new((*geometry).clone()),
            }));
        }
        let mask = spec
            .components
            .resolved_for(component_count(target_type), spec.target)
            .bits();

        // Every column the kernel binds, resident or uploaded for this call.
        let mut columns = ColumnSource {
            backend: self,
            input,
            domain,
            uploaded: HashMap::new(),
        };
        let Some(positions) = columns.get("P") else {
            return Ok(None);
        };
        let existing = if creating {
            let array = match input {
                Input::Cpu(geometry) => {
                    created_column(geometry, domain, spec.target, sampled_type, count)
                }
                Input::Resident(geometry) => {
                    // Only a colour that follows another (a point's `Cd` follows
                    // its path's) reads columns; read back just those, and
                    // nothing at all for a constant default.
                    let wanted = if target_type == AttributeType::Color {
                        derived_color_sources(domain, spec.target)
                    } else {
                        &[]
                    };
                    let host = geometry.host_with_columns(wanted)?;
                    created_column(&host, domain, spec.target, sampled_type, count)
                }
            };
            match GpuColumn::upload(&self.ctx, &self.pool, &array) {
                Some(column) => column,
                None => return Ok(None),
            }
        } else {
            match columns.get(spec.target) {
                Some(column) => column,
                None => return Ok(None),
            }
        };
        let group_column = match group {
            Some(name) => match columns.get(name) {
                Some(column) => Some(column),
                None => return Ok(None),
            },
            None => None,
        };
        let mut attributes = Vec::new();
        for (name, ty) in program.attributes() {
            match columns.get(name) {
                Some(column) if column.attr_type() == *ty => attributes.push(column),
                _ => return Ok(None),
            }
        }

        let source = apply_kernel(
            &program,
            &ApplyKernelSpec {
                position: position_type,
                target: target_type,
                combine: spec.combine,
                mask,
                group: group.is_some(),
            },
        );
        // Binding 0 is the uniform, 1 `P`, 2 the old target, 3 the new one
        // (read-write), then the group column if any, the field parameters and
        // the attributes the field reads: `apply_kernel`'s table.
        let last_binding = 3 + u32::from(group.is_some()) + 1 + program.attributes().len() as u32;
        let mut layout = vec![
            BindingDesc::new(0, BindingKind::UniformBuffer, ShaderVisibility::COMPUTE),
            read_only(1),
            read_only(2),
            BindingDesc::new(3, BindingKind::StorageBuffer, ShaderVisibility::COMPUTE),
        ];
        layout.extend((4..=last_binding).map(read_only));
        let pipeline = match self
            .shaders
            .lock()
            .expect("shader manager poisoned")
            .compute_pipeline("field.apply", &source, "main", &layout, APPLY_WORKGROUP)
        {
            Ok(pipeline) => pipeline,
            Err(error) => {
                // A generated source that does not compile is a bug in the
                // generator, not in the document: say so and let the CPU answer.
                tracing::warn!(%error, "field.apply: the generated WGSL does not compile");
                return Ok(None);
            }
        };

        let stride = u64::from(
            ravel_gpu::column_stride(target_type).expect("a modulatable target has a layout"),
        );
        let output = self
            .pool
            .lock()
            .expect("buffer pool poisoned")
            .acquire(count as u64 * stride, BufferUsage::COLUMN);
        let params = program.params();
        let params_bytes: &[u8] = bytemuck::cast_slice(&params);
        let params_buffer = self.pool.lock().expect("buffer pool poisoned").acquire(
            params_bytes.len() as u64,
            BufferUsage::STORAGE | BufferUsage::COPY_DST,
        );
        ravel_gpu::upload_buffer(&self.ctx, &params_buffer, params_bytes);

        let uniform = [count as u32, amount.to_bits(), 0, 0];
        let mut buffers = vec![
            BufferBinding {
                buffer: positions.buffer(),
                written: false,
            },
            BufferBinding {
                buffer: existing.buffer(),
                written: false,
            },
            BufferBinding {
                buffer: &output,
                written: true,
            },
        ];
        if let Some(group_column) = &group_column {
            buffers.push(BufferBinding {
                buffer: group_column.buffer(),
                written: false,
            });
        }
        buffers.push(BufferBinding {
            buffer: &params_buffer,
            written: false,
        });
        buffers.extend(attributes.iter().map(|column| BufferBinding {
            buffer: column.buffer(),
            written: false,
        }));
        self.ctx.dispatch_buffers(&BufferDispatch {
            label: "field.apply",
            pipeline: &pipeline,
            uniform: bytemuck::cast_slice(&uniform),
            buffers: &buffers,
            width: APPLY_ROW_WIDTH,
            height: (count as u32).div_ceil(APPLY_ROW_WIDTH),
        });
        drop(buffers);
        // The batch marked it in flight, so the pool will not hand it out
        // before the submission.
        self.pool
            .lock()
            .expect("buffer pool poisoned")
            .release(params_buffer);

        let result = GpuColumn::adopt(&self.pool, output, target_type, count);
        Ok(Some(match input {
            Input::Resident(geometry) => {
                Arc::new(geometry.with_column(domain, spec.target, result))
            }
            Input::Cpu(geometry) => {
                let array = result
                    .read(&self.ctx)
                    .context("field.apply: reading the modulated column back")?;
                let mut modulated = (*geometry).clone();
                let set = match domain {
                    Domain::Instance => modulated.instances_mut(),
                    _ => modulated.points_mut(),
                };
                set.insert(spec.target, array)?;
                Arc::new(modulated)
            }
        }))
    }
}

/// The columns one `apply` binds: borrowed from a resident geometry, or
/// uploaded once each from a CPU one (an uploaded column returns its buffer to
/// the pool when this value drops).
struct ColumnSource<'a, 'b> {
    backend: &'a GpuFieldBackend,
    input: &'a Input<'b>,
    domain: Domain,
    uploaded: HashMap<String, GpuColumn>,
}

impl ColumnSource<'_, '_> {
    fn get(&mut self, name: &str) -> Option<GpuColumn> {
        if let Some(column) = self.uploaded.get(name) {
            return Some(column.clone());
        }
        let column = match self.input {
            Input::Resident(geometry) => return geometry.column(self.domain, name).cloned(),
            Input::Cpu(geometry) => {
                let array: &AttributeArray = geometry.attribute_set(self.domain).get(name)?;
                GpuColumn::upload(&self.backend.ctx, &self.backend.pool, array)?
            }
        };
        self.uploaded.insert(name.to_string(), column.clone());
        Some(column)
    }
}
