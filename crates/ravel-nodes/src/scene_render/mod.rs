// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `scene.render`: a [`Scene`] seen through its camera, as a frame buffer
//! (REQ-3D-002, REQ-3D-007).
//!
//! # Shape
//!
//! ```text
//! Scene ──flatten──▶ prepare ──▶ Prepared ──┬─▶ cpu::render   (reference, fallback)
//!                    (this file's           └─▶ gpu::render   (ScenePass + unpremultiply)
//!                     sibling)
//! ```
//!
//! [`prepare`] is shared and decides everything that is a *judgement*: which
//! pieces exist, which pass they belong to, in what order the second pass draws
//! them. The two renderers only execute that list, so they cannot disagree about
//! the picture — only about how exactly a pixel is computed, which the parity
//! tests bound.
//!
//! # Two passes
//!
//! 1. **Opaque meshes**, depth test and depth write on. Order is irrelevant to
//!    the picture; self-occlusion is the depth buffer's job.
//! 2. **Everything else**, back to front by per-object view-space centroid `z`,
//!    depth test on and depth write **off**: translucent meshes, image
//!    instances (textured quads), and Path objects. A translucent surface is
//!    hidden by an opaque mesh in front of it per fragment, and never hides
//!    anything itself.
//!
//! Path objects — paths and point sprites — are projected through the camera
//! into output pixels and drawn by the same analytic code `rasterize` uses
//! ([`crate::rasterize`]), at one depth per object. Keeping depth writes off is
//! what keeps their antialiased edges: a half-covered pixel blends over what is
//! beneath it instead of punching a hole through the depth buffer.
//!
//! # Where later work plugs in
//!
//! - **Primitives** (`mesh.box` / `sphere` / …) emit ordinary `Primitive::Mesh`
//!   geometry with `N` and `uv`; nothing here changes.
//! - **Lighting** adds light records to the frame and shades in `mesh_fragment`;
//!   `N` already reaches the fragment stage and `ObjectRecord` has the model
//!   matrix that carries it to scene space.
//! - **Extrusion** emits meshes too; the back cap's winding is its author's,
//!   which is why nothing is culled.
//! - **Model import** emits meshes plus a basis change at the import boundary.
//! - A **texture on a mesh** is `MeshObject::texture`, which the sampler already
//!   reads through the mesh's own `uv`; it needs a `scene.add` port, not a
//!   renderer change.
//!
//! # Limits stated rather than hidden
//!
//! - Translucent objects are sorted whole, so two that interleave in depth are
//!   wrong (REQ-3D-007's scope line).
//! - Nothing is back-face culled.
//! - A Path object strokes at one width measured at its centroid.
//! - Cap, join and dash attributes are not honoured on Path objects.
//! - Path objects are clipped against the near plane before projection: only
//!   the part in front of the camera is drawn (a closed path stays a closed
//!   polygon). Curves are flattened in scene space first, through the same
//!   flattener `rasterize` uses, so they are drawn as curves.
//! - Instances placed in 3D are not drawn yet; instance placement is 2D.

use std::sync::Arc;

use anyhow::Context as _;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::scene::Scene;
use ravel_core::types::NodeData;
use ravel_gpu::{GpuContext, ShaderManager, TexturePool};
use std::sync::Mutex;

mod cpu;
mod gpu;
mod prepare;
#[cfg(test)]
mod tests;

use crate::rasterize::node_base_color;

pub struct SceneRenderProcessor {
    gpu: Option<Arc<gpu::GpuSceneRenderer>>,
}

impl SceneRenderProcessor {
    /// The CPU reference constructor: draws on the CPU, returns a CPU frame.
    pub fn from_node(_node: &Node) -> Self {
        Self { gpu: None }
    }

    /// The GPU constructor used by graph evaluation.
    pub fn new(
        ctx: GpuContext,
        shaders: &mut ShaderManager,
        pool: Arc<Mutex<TexturePool>>,
        _node: &Node,
    ) -> Self {
        Self {
            gpu: Some(Arc::new(gpu::GpuSceneRenderer::new(ctx, shaders, pool))),
        }
    }

    /// How many static mesh buffer sets the GPU renderer has uploaded in its
    /// life; `0` for the CPU constructor. A scene whose meshes did not change
    /// must not move it between frames.
    pub fn static_buffer_allocations(&self) -> u64 {
        self.gpu
            .as_ref()
            .map_or(0, |renderer| renderer.static_allocations())
    }
}

impl NodeProcessor for SceneRenderProcessor {
    /// Everything read comes from `params` at dispatch, so a parameter edit
    /// must not rebuild the pipelines.
    fn rebuild_on_node_change(&self) -> bool {
        false
    }

    fn process(
        &self,
        _node: &Node,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let scene = inputs
            .first()
            .and_then(|input| input.as_ref())
            .and_then(|input| input.downcast_ref::<Scene>())
            .context("scene.render expects a Scene input")?;
        // A scene with no camera is drawn through the default one rather than
        // refused: an empty `scene.camera` is what a user has not added yet,
        // and a blank error frame explains less than the default view does.
        let camera = scene.primary_camera().copied().unwrap_or_default();
        let prepared = prepare::prepare(
            scene,
            &camera,
            ctx,
            &prepare::SceneParams {
                fill: params.bool_or("fill", true),
                stroke_width: params.f32_or("stroke_width", 0.0),
                color: node_base_color(params),
            },
        )?;
        match &self.gpu {
            Some(renderer) => renderer.render(&prepared),
            None => Ok(Arc::new(cpu::render(&prepared)?)),
        }
    }
}
