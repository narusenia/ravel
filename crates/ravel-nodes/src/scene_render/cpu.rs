// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The CPU reference renderer of `scene.render`.
//!
//! It draws what the GPU pass draws, by a different route: a scanline-free
//! edge-function triangle fill into an `f32` colour buffer and a `f32` depth
//! buffer. It exists for two reasons — it is the fallback when no GPU context
//! is attached, and it is what the CPU / GPU parity tests hold the shader to.
//! Where the two can differ, and by how much, is in the parity tests.
//!
//! The conventions match the GPU's pixel for pixel on purpose:
//!
//! - a fragment is the pixel **centre**, `(x + 0.5, y + 0.5)`;
//! - a pixel on a triangle's edge belongs to it only if the edge is a *top* or
//!   *left* one (the D3D / Metal / Vulkan rule), so abutting triangles never
//!   draw a shared edge twice or leave a crack;
//! - depth is the clip-space `z / w`, interpolated linearly in screen space,
//!   and the test is `LessEqual` against a buffer cleared to `1`;
//! - `uv`, vertex colour and every other varying are interpolated
//!   perspective-correctly;
//! - nothing is back-face culled.
//!
//! Triangles that cross the near or far plane are clipped here
//! (Sutherland–Hodgman against `0 <= z <= w`); the GPU does that in hardware.

use std::collections::HashMap;

use anyhow::Context as _;
use ravel_core::scene::Mat4;
use ravel_core::types::{Color, FrameBuffer};

use super::prepare::{Blended, MeshObject, Prepared, mesh_data};
use crate::rasterize;

/// One clip-space vertex with the varyings the fragment stage reads.
#[derive(Clone, Copy)]
struct ClipVertex {
    clip: [f32; 4],
    uv: [f32; 2],
    color: [f32; 4],
}

impl ClipVertex {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        let mix = |x: f32, y: f32| x + (y - x) * t;
        Self {
            clip: [
                mix(a.clip[0], b.clip[0]),
                mix(a.clip[1], b.clip[1]),
                mix(a.clip[2], b.clip[2]),
                mix(a.clip[3], b.clip[3]),
            ],
            uv: [mix(a.uv[0], b.uv[0]), mix(a.uv[1], b.uv[1])],
            color: [
                mix(a.color[0], b.color[0]),
                mix(a.color[1], b.color[1]),
                mix(a.color[2], b.color[2]),
                mix(a.color[3], b.color[3]),
            ],
        }
    }
}

/// Clip a convex polygon to one plane given as a signed distance per vertex
/// (`>= 0` inside).
fn clip_to_plane(polygon: &[ClipVertex], distance: impl Fn(&ClipVertex) -> f32) -> Vec<ClipVertex> {
    let mut out = Vec::with_capacity(polygon.len() + 1);
    for (index, current) in polygon.iter().enumerate() {
        let next = &polygon[(index + 1) % polygon.len()];
        let (d0, d1) = (distance(current), distance(next));
        if d0 >= 0.0 {
            out.push(*current);
        }
        if (d0 >= 0.0) != (d1 >= 0.0) {
            out.push(ClipVertex::lerp(current, next, d0 / (d0 - d1)));
        }
    }
    out
}

/// A vertex after the perspective divide and viewport transform.
#[derive(Clone, Copy)]
struct ScreenVertex {
    x: f32,
    y: f32,
    z: f32,
    inv_w: f32,
    /// `uv`, then colour, each already multiplied by `inv_w`.
    attributes: [f32; 6],
}

fn to_screen(vertex: &ClipVertex, width: f32, height: f32) -> ScreenVertex {
    let inv_w = 1.0 / vertex.clip[3];
    ScreenVertex {
        x: (vertex.clip[0] * inv_w * 0.5 + 0.5) * width,
        y: (0.5 - vertex.clip[1] * inv_w * 0.5) * height,
        z: vertex.clip[2] * inv_w,
        inv_w,
        attributes: [
            vertex.uv[0] * inv_w,
            vertex.uv[1] * inv_w,
            vertex.color[0] * inv_w,
            vertex.color[1] * inv_w,
            vertex.color[2] * inv_w,
            vertex.color[3] * inv_w,
        ],
    }
}

/// The edge function: twice the signed area of `(a, b, p)`, positive when `p`
/// is on the interior side of `a -> b` for a triangle wound with positive area.
fn edge(a: (f32, f32), b: (f32, f32), p: (f32, f32)) -> f32 {
    (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0)
}

/// Whether an edge `a -> b` of a positive-area triangle owns the pixels that
/// lie exactly on it (a top edge — horizontal with the interior below — or a
/// left one).
fn owns_edge(a: (f32, f32), b: (f32, f32)) -> bool {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    (dy == 0.0 && dx > 0.0) || dy < 0.0
}

/// The straight-alpha texel source an object samples.
type Textures<'a> = HashMap<*const (), rasterize::CpuImage<'a>>;

struct Target<'a> {
    width: u32,
    height: u32,
    /// Premultiplied RGBA.
    color: &'a mut [f32],
    depth: &'a mut [f32],
}

impl Target<'_> {
    fn blend(&mut self, index: usize, premultiplied: [f32; 4]) {
        let dst = &mut self.color[index * 4..index * 4 + 4];
        let keep = 1.0 - premultiplied[3];
        for channel in 0..4 {
            dst[channel] = premultiplied[channel] + dst[channel] * keep;
        }
    }
}

/// The premultiplied colour of one fragment.
///
/// The tint multiplies the straight-alpha texel; premultiplied that is the
/// texel's premultiplied value scaled by the tint's colour and its alpha,
/// which is the same expression `mesh_fragment` evaluates.
fn fragment(
    object: &MeshObject,
    texture: Option<&rasterize::CpuImage<'_>>,
    uv: [f32; 2],
    vertex_color: [f32; 4],
) -> [f32; 4] {
    let tint = [
        vertex_color[0] * object.color.r,
        vertex_color[1] * object.color.g,
        vertex_color[2] * object.color.b,
        vertex_color[3] * object.color.a,
    ];
    let Some(texture) = texture else {
        return [
            tint[0] * tint[3],
            tint[1] * tint[3],
            tint[2] * tint[3],
            tint[3],
        ];
    };
    let (width, height) = texture.size();
    let Some(Color { r, g, b, a }) = texture.sample(uv[0] * width as f32, uv[1] * height as f32)
    else {
        return [0.0; 4];
    };
    [
        r * a * tint[0] * tint[3],
        g * a * tint[1] * tint[3],
        b * a * tint[2] * tint[3],
        a * tint[3],
    ]
}

fn draw_triangle(
    target: &mut Target<'_>,
    object: &MeshObject,
    texture: Option<&rasterize::CpuImage<'_>>,
    corners: [ClipVertex; 3],
    write_depth: bool,
) {
    // `0 <= z <= w` is the clip volume in depth; x and y are handled by the
    // bounding box, which is what the rasterizer's guard band amounts to.
    let mut polygon = corners.to_vec();
    polygon = clip_to_plane(&polygon, |v| v.clip[2]);
    if polygon.len() < 3 {
        return;
    }
    polygon = clip_to_plane(&polygon, |v| v.clip[3] - v.clip[2]);
    if polygon.len() < 3 {
        return;
    }
    let (width, height) = (target.width as f32, target.height as f32);
    let screen: Vec<ScreenVertex> = polygon
        .iter()
        .map(|v| to_screen(v, width, height))
        .collect();
    for fan in 1..screen.len() - 1 {
        fill_triangle(
            target,
            object,
            texture,
            [screen[0], screen[fan], screen[fan + 1]],
            write_depth,
        );
    }
}

fn fill_triangle(
    target: &mut Target<'_>,
    object: &MeshObject,
    texture: Option<&rasterize::CpuImage<'_>>,
    mut v: [ScreenVertex; 3],
    write_depth: bool,
) {
    let mut area = edge((v[0].x, v[0].y), (v[1].x, v[1].y), (v[2].x, v[2].y));
    if !area.is_finite() || area == 0.0 {
        return;
    }
    if area < 0.0 {
        v.swap(1, 2);
        area = -area;
    }
    let p = |index: usize| (v[index].x, v[index].y);
    let min_x = v
        .iter()
        .map(|s| s.x)
        .fold(f32::INFINITY, f32::min)
        .floor()
        .max(0.0) as u32;
    let max_x = (v
        .iter()
        .map(|s| s.x)
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil())
    .min(target.width as f32)
    .max(0.0) as u32;
    let min_y = v
        .iter()
        .map(|s| s.y)
        .fold(f32::INFINITY, f32::min)
        .floor()
        .max(0.0) as u32;
    let max_y = (v
        .iter()
        .map(|s| s.y)
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil())
    .min(target.height as f32)
    .max(0.0) as u32;
    let owns = [
        owns_edge(p(1), p(2)),
        owns_edge(p(2), p(0)),
        owns_edge(p(0), p(1)),
    ];
    for y in min_y..max_y {
        for x in min_x..max_x {
            let centre = (x as f32 + 0.5, y as f32 + 0.5);
            let weights = [
                edge(p(1), p(2), centre),
                edge(p(2), p(0), centre),
                edge(p(0), p(1), centre),
            ];
            let inside = weights
                .iter()
                .zip(owns)
                .all(|(&w, owned)| w > 0.0 || (w == 0.0 && owned));
            if !inside {
                continue;
            }
            let lambda = weights.map(|w| w / area);
            let z = lambda[0] * v[0].z + lambda[1] * v[1].z + lambda[2] * v[2].z;
            let index = (y * target.width + x) as usize;
            if z > target.depth[index] {
                continue;
            }
            let inv_w = lambda[0] * v[0].inv_w + lambda[1] * v[1].inv_w + lambda[2] * v[2].inv_w;
            let mut varying = [0.0f32; 6];
            for (slot, value) in varying.iter_mut().enumerate() {
                *value = (lambda[0] * v[0].attributes[slot]
                    + lambda[1] * v[1].attributes[slot]
                    + lambda[2] * v[2].attributes[slot])
                    / inv_w;
            }
            let color = fragment(
                object,
                texture,
                [varying[0], varying[1]],
                [varying[2], varying[3], varying[4], varying[5]],
            );
            target.blend(index, color);
            if write_depth {
                target.depth[index] = z;
            }
        }
    }
}

fn draw_mesh(
    target: &mut Target<'_>,
    object: &MeshObject,
    textures: &Textures<'_>,
    write_depth: bool,
) {
    let Some(data) = mesh_data(&object.geometry, object.primitive) else {
        return;
    };
    let texture = object
        .texture
        .as_ref()
        .and_then(|frame| textures.get(&(std::sync::Arc::as_ptr(frame) as *const ())));
    let clip: Vec<ClipVertex> = data
        .vertices
        .iter()
        .enumerate()
        .map(|(index, vertex)| ClipVertex {
            clip: transform(&object.mvp, vertex.position),
            uv: vertex.uv,
            color: data.colors.get(index).copied().unwrap_or([1.0; 4]),
        })
        .collect();
    for triangle in data.indices.chunks_exact(3) {
        draw_triangle(
            target,
            object,
            texture,
            [
                clip[triangle[0] as usize],
                clip[triangle[1] as usize],
                clip[triangle[2] as usize],
            ],
            write_depth,
        );
    }
}

fn transform(matrix: &Mat4, p: [f32; 3]) -> [f32; 4] {
    matrix.transform_vec4([p[0], p[1], p[2], 1.0])
}

/// Render a prepared scene on the CPU into a straight-alpha frame.
pub(super) fn render(prepared: &Prepared) -> anyhow::Result<FrameBuffer> {
    let (width, height) = prepared.resolution;
    let pixel_count = width as usize * height as usize;
    let mut color = vec![0.0f32; pixel_count * 4];
    let mut depth = vec![1.0f32; pixel_count];

    // Texels are read once per distinct frame, the same economy the 2D image
    // path has; a GPU-resident frame is read back here, which only the
    // reference path pays.
    let mut textures: Textures<'_> = HashMap::new();
    for object in &prepared.meshes {
        if let Some(frame) = &object.texture {
            let key = std::sync::Arc::as_ptr(frame) as *const ();
            if let std::collections::hash_map::Entry::Vacant(slot) = textures.entry(key) {
                slot.insert(
                    rasterize::resolve_frame(frame.as_ref())
                        .context("scene.render: reading an image into CPU memory")?,
                );
            }
        }
    }

    let mut target = Target {
        width,
        height,
        color: &mut color,
        depth: &mut depth,
    };
    for &index in &prepared.opaque {
        draw_mesh(&mut target, &prepared.meshes[index], &textures, true);
    }

    // Path objects draw through the 2D rasterizer into a scratch frame, then
    // composite per pixel against the depth the meshes left. One scratch is
    // reused; it is cleared per object, which is O(resolution) each — a scene
    // with hundreds of Path objects is what the GPU path is for.
    let mut scratch = vec![0.0f32; pixel_count * 4];
    let mut coverage = vec![0u8; pixel_count];
    for entry in &prepared.blended {
        match *entry {
            Blended::Mesh(index) => {
                draw_mesh(&mut target, &prepared.meshes[index], &textures, false);
            }
            Blended::Path(index) => {
                let path = &prepared.paths[index];
                scratch.fill(0.0);
                rasterize::raster_projected(
                    &path.geometry,
                    prepared.paint,
                    prepared.resolution,
                    &mut scratch,
                    &mut coverage,
                );
                for pixel in 0..pixel_count {
                    let texel = &scratch[pixel * 4..pixel * 4 + 4];
                    if texel[3] <= 0.0 || path.ndc_depth > target.depth[pixel] {
                        continue;
                    }
                    target.blend(
                        pixel,
                        [
                            texel[0] * texel[3],
                            texel[1] * texel[3],
                            texel[2] * texel[3],
                            texel[3],
                        ],
                    );
                }
            }
        }
    }

    // Premultiplied → straight, the CPU twin of the `unpremultiply` pass.
    for texel in color.chunks_exact_mut(4) {
        if texel[3] > 1e-7 {
            let alpha = texel[3];
            texel[0] /= alpha;
            texel[1] /= alpha;
            texel[2] /= alpha;
        } else {
            texel.fill(0.0);
        }
    }
    Ok(FrameBuffer::from_f32(width, height, color))
}
