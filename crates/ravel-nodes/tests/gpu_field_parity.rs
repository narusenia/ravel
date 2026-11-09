// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! CPU / GPU parity of every field, and of `field.apply`'s combine, plus the
//! fallback and one-dispatch contracts
//! (`docs/implementation/gpu-resident-geometry-plan.md`, unit 2).
//!
//! The CPU `apply_field` is the oracle. Each field is applied to the same
//! geometry on both paths and every column is compared: the target within an
//! explicit tolerance (the GPU is a different f32 pipeline: fused multiply-add,
//! `length` for `hypot`, its own `atan2`), every other column bit for bit.
//!
//! Needs a GPU adapter; without one the parity tests print `skipping` and pass,
//! so a machine with no adapter is **not** evidence. The WGSL of every field is
//! also validated by naga in a test that needs no adapter at all.

use std::sync::{Arc, Mutex};

use ravel_core::animation::interpolation::Interpolation;
use ravel_core::eval::EvalContext;
use ravel_core::geometry::{
    AddField, AngleField, ApplyKernelSpec, AttributeArray, AttributeField, AttributeSet,
    AttributeType, BlendField, CombineMode, ComponentField, ComponentMask, ComposeField,
    ConstantField, CurlNoiseField, CurveRemapField, DirectionToField, Domain, ExpressionField,
    FalloffField, FalloffShape, Field, FieldApply, FieldProgram, FieldValue, Geometry,
    GradientField, LengthField, MaxField, MultiplyField, NoiseField, RadialField, RampField,
    TimeField, TimeMode, apply_field, apply_kernel,
};
use ravel_core::param_curve::{CurveParam, CurvePoint};
use ravel_core::param_ramp::{RampInterpolation, RampParam};
use ravel_core::types::{Color, FrameRate, Vec2, Vec3, Vec4};
use ravel_gpu::{BufferPool, GpuContext, GpuGeometry, validate_wgsl};
use ravel_nodes::field::GpuFieldBackend;

const COUNT: usize = 1500;

fn ctx() -> EvalContext {
    // A non-zero, fractional position on the timeline so `field.time` is not 0.
    let mut ctx = EvalContext::new(42, FrameRate::new(24, 1), (1920, 1080));
    ctx.time = 1.75;
    ctx
}

/// Deterministic pseudo-random floats in `0..1` (an LCG, so a failure names a
/// reproducible element).
fn unit_values(seed: u32, count: usize) -> Vec<f32> {
    let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..count)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 8) as f32 / (1 << 24) as f32
        })
        .collect()
}

/// A point cloud with a column of every modulatable type, a Bool group and a
/// few special positions: the origin (where `radial` has no direction) and
/// exact lattice corners.
fn geometry() -> Geometry {
    let xs = unit_values(1, COUNT);
    let ys = unit_values(2, COUNT);
    let mut positions: Vec<Vec2> = xs
        .iter()
        .zip(&ys)
        .map(|(x, y)| Vec2(x * 8.0 - 4.0, y * 8.0 - 4.0))
        .collect();
    positions[0] = Vec2(0.0, 0.0);
    positions[1] = Vec2(1.0, 1.0);
    positions[2] = Vec2(-2.0, 3.0);
    let mut geometry = Geometry::from_points(positions);
    let a = unit_values(3, COUNT);
    let b = unit_values(4, COUNT);
    let c = unit_values(5, COUNT);
    let d = unit_values(6, COUNT);
    let points = geometry.points_mut();
    points
        .insert(
            "weight",
            AttributeArray::F32(a.iter().map(|v| v * 4.0 - 1.0).collect()),
        )
        .unwrap();
    points
        .insert(
            "vel",
            AttributeArray::Vec2(a.iter().zip(&b).map(|(x, y)| Vec2(*x, *y * 3.0)).collect()),
        )
        .unwrap();
    points
        .insert(
            "v3",
            AttributeArray::Vec3(
                a.iter()
                    .zip(&b)
                    .zip(&c)
                    .map(|((x, y), z)| Vec3(*x, *y, *z * 2.0))
                    .collect(),
            ),
        )
        .unwrap();
    points
        .insert(
            "v4",
            AttributeArray::Vec4(
                (0..COUNT)
                    .map(|i| Vec4(a[i], b[i], c[i], d[i] - 0.5))
                    .collect(),
            ),
        )
        .unwrap();
    points
        .insert(
            "Cd",
            AttributeArray::Color(
                (0..COUNT)
                    .map(|i| Color {
                        r: a[i],
                        g: b[i],
                        b: c[i],
                        a: 0.25 + d[i] * 0.5,
                    })
                    .collect(),
            ),
        )
        .unwrap();
    points
        .insert(
            "flag",
            AttributeArray::Bool(d.iter().map(|v| *v > 0.5).collect()),
        )
        .unwrap();
    // Values that land exactly on a curve's control point or a ramp's stop,
    // where a step segment hands over to the next point's own value.
    let knots = [-0.5f32, 0.2, 0.9, 0.4, 0.0, 1.0, 1.7, -3.0];
    points
        .insert(
            "knot",
            AttributeArray::F32((0..COUNT).map(|i| knots[i % knots.len()]).collect()),
        )
        .unwrap();
    // Non-finite sources: a curve or ramp reads its end value for NaN and
    // clamps an infinity, and the GPU must not interpolate garbage instead.
    let wild = [
        f32::NAN,
        0.3,
        f32::INFINITY,
        f32::NEG_INFINITY,
        -f32::NAN,
        0.7,
    ];
    points
        .insert(
            "wild",
            AttributeArray::F32((0..COUNT).map(|i| wild[i % wild.len()]).collect()),
        )
        .unwrap();
    points
        .insert(
            "speed",
            AttributeArray::F32(b.iter().map(|v| v * 10.0).collect()),
        )
        .unwrap();
    geometry
}

fn adapter() -> Option<(GpuContext, GpuFieldBackend, Arc<Mutex<BufferPool>>)> {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return None;
    };
    let pool = Arc::new(Mutex::new(BufferPool::new(gpu.clone(), 256 * 1024 * 1024)));
    let backend = GpuFieldBackend::new(gpu.clone(), pool.clone()).with_min_elements(0);
    Some((gpu, backend, pool))
}

fn fv(field: impl Field + 'static) -> FieldValue {
    FieldValue::new(field)
}

fn noise(seed: u32, frequency: f32, octaves: u32) -> NoiseField {
    NoiseField {
        seed,
        frequency,
        octaves,
    }
}

fn falloff() -> FalloffField {
    FalloffField {
        center: Vec2(0.5, -0.5),
        inner_radius: 0.5,
        outer_radius: 3.0,
        shape: FalloffShape::Sphere,
    }
}

/// The attribute type a field answers, found by sampling it on the CPU.
fn field_type(field: &FieldValue, geometry: &Geometry) -> AttributeType {
    let positions: Vec<Vec2> = (0..COUNT)
        .map(|i| match &**geometry.points().get("P").unwrap() {
            AttributeArray::Vec2(v) => v[i],
            _ => unreachable!(),
        })
        .collect();
    let ctx = ctx();
    field
        .sample(&ravel_core::geometry::FieldSample::new(
            &positions,
            geometry.points(),
            &ctx,
        ))
        .attr_type()
}

/// The column a field of `ty` is applied to.
fn target_for(ty: AttributeType) -> &'static str {
    match ty {
        AttributeType::F32 => "weight",
        AttributeType::Vec2 => "vel",
        AttributeType::Vec3 => "v3",
        AttributeType::Vec4 => "v4",
        AttributeType::Color => "Cd",
        other => panic!("not a field type: {other}"),
    }
}

/// Every element of a float column, as comparable lanes.
fn lanes(column: &AttributeArray) -> Vec<f32> {
    match column {
        AttributeArray::F32(v) => v.clone(),
        AttributeArray::Vec2(v) => v.iter().flat_map(|p| [p.0, p.1]).collect(),
        AttributeArray::Vec3(v) => v.iter().flat_map(|p| [p.0, p.1, p.2]).collect(),
        AttributeArray::Vec4(v) => v.iter().flat_map(|p| [p.0, p.1, p.2, p.3]).collect(),
        AttributeArray::Color(v) => v.iter().flat_map(|c| [c.r, c.g, c.b, c.a]).collect(),
        AttributeArray::I32(v) => v.iter().map(|x| *x as f32).collect(),
        AttributeArray::Bool(v) => v.iter().map(|x| f32::from(u8::from(*x))).collect(),
        AttributeArray::Str(_) => panic!("string column"),
    }
}

/// Compare the CPU and GPU results: the target within `tolerance` (relative to
/// `1 + |cpu|`), every other column exactly. Returns the worst error so a test
/// can show the tolerance is not vacuous.
fn assert_matches(name: &str, cpu: &Geometry, gpu: &Geometry, target: &str, tolerance: f32) -> f32 {
    let mut worst = 0.0f32;
    for (column_name, expected) in cpu.points().iter() {
        let actual = gpu
            .points()
            .get(column_name)
            .unwrap_or_else(|| panic!("{name}: GPU result lost column {column_name}"));
        assert_eq!(
            expected.attr_type(),
            actual.attr_type(),
            "{name}: {column_name}"
        );
        let (e, a) = (lanes(expected), lanes(actual));
        assert_eq!(e.len(), a.len(), "{name}: {column_name} length");
        if column_name.as_str() == target {
            for (index, (e, a)) in e.iter().zip(&a).enumerate() {
                // Identical non-finite answers agree (an empty curve passes a
                // NaN or an infinity straight through).
                if e == a || (e.is_nan() && a.is_nan()) {
                    continue;
                }
                let error = (e - a).abs() / (1.0 + e.abs());
                assert!(
                    error <= tolerance,
                    "{name}: {column_name}[{index}] cpu {e} gpu {a} (relative error {error} > {tolerance})"
                );
                worst = worst.max(error);
            }
        } else {
            assert_eq!(
                e.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                a.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "{name}: column {column_name} must be untouched"
            );
        }
    }
    worst
}

/// Apply `field` to `geometry` on the CPU and on the GPU, from both a CPU and
/// a resident input, and compare. Panics if the GPU path declines.
fn check_parity(
    gpu: &(GpuContext, GpuFieldBackend, Arc<Mutex<BufferPool>>),
    name: &str,
    field: &FieldValue,
    geometry: &Geometry,
    spec: &FieldApply<'_>,
    tolerance: f32,
) -> f32 {
    check_parity_reading_back(gpu, name, field, geometry, spec, tolerance, 0)
}

/// [`check_parity`], allowing a resident input `readbacks` column readbacks:
/// a created colour that follows another column reads that column.
fn check_parity_reading_back(
    gpu: &(GpuContext, GpuFieldBackend, Arc<Mutex<BufferPool>>),
    name: &str,
    field: &FieldValue,
    geometry: &Geometry,
    spec: &FieldApply<'_>,
    tolerance: f32,
    readbacks: u64,
) -> f32 {
    let (context, backend, pool) = gpu;
    let ctx = ctx();
    let cpu = apply_field(geometry, spec, field.0.as_ref(), &ctx).expect("CPU reference");

    // A CPU geometry in, a CPU geometry out.
    let from_cpu = backend
        .apply(geometry, spec, field.0.as_ref(), &ctx)
        .expect("GPU apply")
        .unwrap_or_else(|| panic!("{name}: the GPU path declined a field with a WGSL version"));
    let from_cpu = from_cpu
        .downcast_ref::<Geometry>()
        .unwrap_or_else(|| panic!("{name}: a CPU input must answer a CPU geometry"));
    let worst = assert_matches(name, &cpu, from_cpu, spec.target, tolerance);

    // A resident geometry in, a resident geometry out, nothing read back until
    // the comparison.
    let resident = GpuGeometry::from_geometry(context.clone(), pool, geometry).unwrap();
    let before_readbacks = context.transfer_stats().readbacks;
    let out = backend
        .apply(&resident, spec, field.0.as_ref(), &ctx)
        .expect("GPU apply")
        .unwrap_or_else(|| panic!("{name}: the GPU path declined a resident input"));
    assert_eq!(
        context.transfer_stats().readbacks,
        before_readbacks + readbacks,
        "{name}: a resident input must not be read back beyond the columns a created colour follows"
    );
    let out = out
        .downcast_ref::<GpuGeometry>()
        .unwrap_or_else(|| panic!("{name}: a resident input must answer a resident geometry"));
    let back = out.to_geometry().unwrap();
    assert_matches(name, &cpu, &back, spec.target, tolerance);
    worst
}

/// Every field with a WGSL version, in the configurations that reach distinct
/// code: `(name, field, tolerance)`.
fn every_field() -> Vec<(&'static str, FieldValue, f32)> {
    let curve = |interpolation| {
        CurveParam::from_points([
            CurvePoint::new(-0.5, 0.0, interpolation),
            CurvePoint::new(0.2, 1.5, interpolation),
            CurvePoint::new(0.9, -0.5, interpolation),
        ])
    };
    let bezier = CurveParam::from_points([
        CurvePoint::new(-0.8, 0.0, Interpolation::Bezier)
            .with_tangents(Vec2(0.0, 0.0), Vec2(0.4, 0.8)),
        CurvePoint::new(0.8, 1.0, Interpolation::Bezier)
            .with_tangents(Vec2(-0.4, -0.2), Vec2(0.0, 0.0)),
    ]);
    let ramp = |interpolation| {
        RampParam::new(
            [
                ravel_core::param_ramp::RampStop::new(0.0, Color::new(1.0, 0.0, 0.0, 1.0)),
                ravel_core::param_ramp::RampStop::new(0.4, Color::new(0.0, 1.0, 0.0, 0.5)),
                ravel_core::param_ramp::RampStop::new(1.0, Color::new(0.0, 0.0, 1.0, 1.0)),
            ],
            interpolation,
        )
    };
    let attribute = |name: &str| fv(AttributeField::new(name));
    let radial_scalar = || {
        fv(LengthField::new(fv(RadialField {
            center: Vec2(0.0, 0.0),
            tangent: false,
        })))
    };
    vec![
        ("constant", fv(ConstantField(2.5)), 1e-6),
        ("time frame", fv(TimeField::new(TimeMode::Frame)), 1e-6),
        (
            "time seconds",
            fv(TimeField::new(TimeMode::Seconds)
                .with_scale(2.0)
                .with_offset(0.5)),
            1e-6,
        ),
        (
            "time normalized",
            fv(TimeField::new(TimeMode::Normalized).with_duration(100.0)),
            1e-6,
        ),
        ("noise", fv(noise(0, 1.0, 1)), 1e-4),
        ("noise seeded", fv(noise(7, 0.37, 1)), 1e-4),
        ("noise fbm", fv(noise(3, 0.8, 5)), 1e-4),
        (
            "direction_to",
            fv(DirectionToField {
                target: Vec2(1.0, -2.0),
            }),
            1e-5,
        ),
        (
            "radial outward",
            fv(RadialField {
                center: Vec2(0.0, 0.0),
                tangent: false,
            }),
            1e-5,
        ),
        (
            "radial tangent",
            fv(RadialField {
                center: Vec2(0.0, 0.0),
                tangent: true,
            }),
            1e-5,
        ),
        (
            "curl_noise",
            fv(CurlNoiseField::new(noise(1, 0.6, 2), 0.01)),
            5e-3,
        ),
        (
            "curl_noise default step",
            fv(CurlNoiseField::new(noise(1, 0.6, 1), 0.0)),
            5e-3,
        ),
        (
            "gradient",
            fv(GradientField::new(fv(noise(2, 0.5, 1)), 0.05)),
            5e-3,
        ),
        ("falloff sphere", fv(falloff()), 1e-5),
        (
            "falloff linear",
            fv(FalloffField {
                center: Vec2(0.0, 0.0),
                inner_radius: 0.2,
                outer_radius: 2.0,
                shape: FalloffShape::Linear {
                    direction: Vec2(1.0, 1.0),
                },
            }),
            1e-5,
        ),
        (
            "falloff linear without a direction",
            fv(FalloffField {
                center: Vec2(0.0, 0.0),
                inner_radius: 0.2,
                outer_radius: 2.0,
                shape: FalloffShape::Linear {
                    direction: Vec2(0.0, 0.0),
                },
            }),
            1e-5,
        ),
        (
            "falloff with no width",
            fv(FalloffField {
                center: Vec2(0.0, 0.0),
                inner_radius: 1.0,
                outer_radius: 1.0,
                shape: FalloffShape::Sphere,
            }),
            1e-5,
        ),
        (
            "curve linear",
            fv(CurveRemapField::new(
                fv(noise(1, 1.0, 1)),
                curve(Interpolation::Linear),
            )),
            1e-4,
        ),
        (
            "curve step",
            fv(CurveRemapField::new(
                fv(noise(1, 1.0, 1)),
                curve(Interpolation::Step),
            )),
            1e-4,
        ),
        (
            "curve bezier",
            fv(CurveRemapField::new(fv(noise(1, 1.0, 1)), bezier.clone())),
            1e-3,
        ),
        (
            "curve empty",
            fv(CurveRemapField::new(
                fv(noise(1, 1.0, 1)),
                CurveParam::from_points([]),
            )),
            1e-4,
        ),
        (
            "curve one point",
            fv(CurveRemapField::new(
                fv(noise(1, 1.0, 1)),
                CurveParam::linear([(0.0, 3.0)]),
            )),
            1e-4,
        ),
        (
            "curve step, on its own control points",
            fv(CurveRemapField::new(
                attribute("knot"),
                curve(Interpolation::Step),
            )),
            0.0,
        ),
        (
            "curve linear, on its own control points",
            fv(CurveRemapField::new(
                attribute("knot"),
                curve(Interpolation::Linear),
            )),
            1e-6,
        ),
        (
            "ramp constant, on its own stops",
            fv(RampField::new(
                attribute("knot"),
                ramp(RampInterpolation::Constant),
            )),
            0.0,
        ),
        (
            "curve linear, non-finite source",
            fv(CurveRemapField::new(
                attribute("wild"),
                curve(Interpolation::Linear),
            )),
            1e-6,
        ),
        (
            "curve bezier, non-finite source",
            fv(CurveRemapField::new(attribute("wild"), bezier.clone())),
            1e-3,
        ),
        (
            "curve step, non-finite source",
            fv(CurveRemapField::new(
                attribute("wild"),
                curve(Interpolation::Step),
            )),
            0.0,
        ),
        (
            "curve one point, non-finite source",
            fv(CurveRemapField::new(
                attribute("wild"),
                CurveParam::linear([(0.0, 3.0)]),
            )),
            0.0,
        ),
        (
            "curve empty, non-finite source",
            fv(CurveRemapField::new(
                attribute("wild"),
                CurveParam::from_points([]),
            )),
            0.0,
        ),
        (
            "ramp linear, non-finite source",
            fv(RampField::new(
                attribute("wild"),
                ramp(RampInterpolation::Linear),
            )),
            1e-5,
        ),
        (
            "ramp smooth, non-finite source",
            fv(RampField::new(
                attribute("wild"),
                ramp(RampInterpolation::Smooth),
            )),
            1e-5,
        ),
        (
            "ramp constant, non-finite source",
            fv(RampField::new(
                attribute("wild"),
                ramp(RampInterpolation::Constant),
            )),
            0.0,
        ),
        (
            "ramp one stop",
            fv(RampField::new(
                attribute("wild"),
                RampParam::linear([(0.5, Color::new(0.2, 0.4, 0.6, 0.8))]),
            )),
            0.0,
        ),
        (
            "ramp linear",
            fv(RampField::new(
                fv(falloff()),
                ramp(RampInterpolation::Linear),
            )),
            1e-4,
        ),
        (
            "ramp smooth",
            fv(RampField::new(
                fv(falloff()),
                ramp(RampInterpolation::Smooth),
            )),
            1e-4,
        ),
        (
            "ramp constant",
            fv(RampField::new(
                fv(falloff()),
                ramp(RampInterpolation::Constant),
            )),
            1e-4,
        ),
        (
            "ramp reversed range",
            fv(
                RampField::new(fv(noise(1, 1.0, 1)), ramp(RampInterpolation::Linear))
                    .with_range(1.0, -1.0),
            ),
            1e-4,
        ),
        (
            "ramp zero range",
            fv(
                RampField::new(fv(noise(1, 1.0, 1)), ramp(RampInterpolation::Linear))
                    .with_range(0.1, 0.1),
            ),
            1e-4,
        ),
        (
            "ramp non-finite range",
            fv(
                RampField::new(fv(noise(1, 1.0, 1)), ramp(RampInterpolation::Linear))
                    .with_range(0.0, f32::INFINITY),
            ),
            1e-4,
        ),
        ("attribute f32", attribute("speed"), 0.0),
        ("attribute i32", attribute("index"), 0.0),
        ("attribute bool", attribute("flag"), 0.0),
        (
            "attribute vec2 y",
            fv(AttributeField::new("vel").with_component("y")),
            0.0,
        ),
        (
            "attribute vec3 z",
            fv(AttributeField::new("v3").with_component("z")),
            0.0,
        ),
        (
            "attribute color a",
            fv(AttributeField::new("Cd").with_component("a")),
            0.0,
        ),
        (
            "attribute position",
            fv(AttributeField::new("P").with_component("y")),
            0.0,
        ),
        ("length", radial_scalar(), 1e-5),
        (
            "length of a scalar",
            fv(LengthField::new(fv(noise(1, 1.0, 1)))),
            1e-5,
        ),
        (
            "angle",
            fv(AngleField::new(fv(DirectionToField {
                target: Vec2(0.5, 0.5),
            }))),
            1e-4,
        ),
        (
            "angle of a tangent (the zero vector is (-0, 0))",
            fv(AngleField::new(fv(RadialField {
                center: Vec2(0.0, 0.0),
                tangent: true,
            }))),
            1e-4,
        ),
        (
            "component",
            fv(ComponentField::new(
                fv(DirectionToField {
                    target: Vec2(2.0, 1.0),
                }),
                1,
            )),
            1e-5,
        ),
        (
            "compose vec2",
            fv(ComposeField::new(vec![fv(noise(1, 1.0, 1)), fv(falloff())])),
            1e-4,
        ),
        (
            "compose vec3",
            fv(ComposeField::new(vec![
                fv(noise(1, 1.0, 1)),
                fv(falloff()),
                fv(ConstantField(0.25)),
            ])),
            1e-4,
        ),
        (
            "compose vec4",
            fv(ComposeField::new(vec![
                fv(noise(1, 1.0, 1)),
                fv(falloff()),
                attribute("speed"),
                fv(ConstantField(1.0)),
            ])),
            1e-4,
        ),
        (
            "add",
            fv(AddField {
                left: fv(noise(1, 1.0, 1)),
                right: fv(falloff()),
            }),
            1e-4,
        ),
        (
            "multiply",
            fv(MultiplyField {
                left: fv(noise(1, 1.0, 1)),
                right: fv(falloff()),
            }),
            1e-4,
        ),
        (
            "max",
            fv(MaxField {
                left: fv(noise(1, 1.0, 1)),
                right: fv(falloff()),
            }),
            1e-4,
        ),
        (
            "blend",
            fv(BlendField {
                left: fv(noise(1, 1.0, 1)),
                right: fv(falloff()),
                amount: 0.3,
            }),
            1e-4,
        ),
        (
            "blend with an amount past the range",
            fv(BlendField {
                left: fv(noise(1, 1.0, 1)),
                right: fv(falloff()),
                amount: 4.0,
            }),
            1e-4,
        ),
        (
            "vector add",
            fv(AddField {
                left: fv(DirectionToField {
                    target: Vec2(0.0, 1.0),
                }),
                right: fv(RadialField {
                    center: Vec2(1.0, 0.0),
                    tangent: true,
                }),
            }),
            1e-5,
        ),
        (
            "scalar times vector",
            fv(MultiplyField {
                left: fv(falloff()),
                right: fv(DirectionToField {
                    target: Vec2(0.0, 1.0),
                }),
            }),
            1e-5,
        ),
        (
            "color times scalar",
            fv(MultiplyField {
                left: fv(RampField::new(
                    fv(falloff()),
                    ramp(RampInterpolation::Linear),
                )),
                right: fv(noise(1, 1.0, 1)),
            }),
            1e-4,
        ),
        (
            "a chain: ramp(noise * falloff + attribute)",
            fv(RampField::new(
                fv(AddField {
                    left: fv(MultiplyField {
                        left: fv(noise(5, 0.5, 3)),
                        right: fv(falloff()),
                    }),
                    right: fv(MultiplyField {
                        left: attribute("speed"),
                        right: fv(ConstantField(0.1)),
                    }),
                }),
                ramp(RampInterpolation::Smooth),
            )),
            1e-4,
        ),
    ]
}

#[test]
fn every_field_matches_the_cpu() {
    let Some(gpu) = adapter() else { return };
    let geometry = geometry();
    let mut worst = 0.0f32;
    for (name, field, tolerance) in every_field() {
        let ty = field_type(&field, &geometry);
        let spec = FieldApply::new(Domain::Point, target_for(ty));
        let error = check_parity(&gpu, name, &field, &geometry, &spec, tolerance);
        worst = worst.max(error);
    }
    // The GPU is a different pipeline, so an error of exactly zero everywhere
    // would mean the comparison is not looking at the GPU's answer.
    eprintln!("worst relative error across all fields: {worst}");
}

#[test]
fn the_wgsl_of_every_field_validates_without_an_adapter() {
    let geometry = geometry();
    let ctx = ctx();
    let mut compiled = 0;
    for (name, field, _) in every_field() {
        let program = FieldProgram::compile(field.0.as_ref(), &ctx, geometry.points(), COUNT)
            .unwrap_or_else(|| panic!("{name}: has no WGSL version"));
        for (target, ty) in [
            ("weight", AttributeType::F32),
            ("vel", AttributeType::Vec2),
            ("v3", AttributeType::Vec3),
            ("v4", AttributeType::Vec4),
            ("Cd", AttributeType::Color),
        ] {
            if program.root_type() != AttributeType::F32 && program.root_type() != ty {
                continue;
            }
            for group in [false, true] {
                let source = apply_kernel(
                    &program,
                    &ApplyKernelSpec {
                        position: AttributeType::Vec2,
                        target: ty,
                        combine: CombineMode::Add,
                        mask: ComponentMask::ALL.bits() & ((1 << 4) - 1),
                        group,
                    },
                );
                validate_wgsl(&format!("{name} -> {target}"), &source)
                    .unwrap_or_else(|e| panic!("{name} -> {target}: {e}\n{source}"));
                compiled += 1;
            }
        }
    }
    assert!(compiled > 100, "only {compiled} kernels were validated");
}

#[test]
fn every_combine_mode_mask_group_and_amount_matches_the_cpu() {
    let Some(gpu) = adapter() else { return };
    let geometry = geometry();
    let field = fv(AddField {
        left: fv(noise(9, 0.7, 2)),
        right: fv(falloff()),
    });
    for combine in [
        CombineMode::Set,
        CombineMode::Add,
        CombineMode::Multiply,
        CombineMode::Min,
        CombineMode::Max,
    ] {
        for (target, mask) in [
            ("weight", ""),
            ("vel", ""),
            ("vel", "y"),
            ("v3", "xz"),
            ("v4", ""),
            ("Cd", ""),
            ("Cd", "a"),
            ("Cd", "rb"),
            ("vel", "z"),
        ] {
            for (amount, group) in [
                (1.0, ""),
                (0.35, ""),
                (1.0, "flag"),
                (0.6, "flag"),
                (7.0, ""),
            ] {
                let spec = FieldApply::new(Domain::Point, target)
                    .with_combine(combine)
                    .with_components(ComponentMask::parse(mask))
                    .with_amount(amount)
                    .with_group(group);
                let name =
                    format!("{combine:?} {target} mask={mask:?} amount={amount} group={group:?}");
                check_parity(&gpu, &name, &field, &geometry, &spec, 1e-4);
            }
        }
    }
}

#[test]
fn a_zero_amount_returns_the_target_untouched_without_a_dispatch() {
    let Some((context, backend, _)) = adapter() else {
        return;
    };
    let geometry = geometry();
    let before = context.dispatch_stats();
    let spec = FieldApply::new(Domain::Point, "weight").with_amount(0.0);
    let out = backend
        .apply(&geometry, &spec, fv(noise(1, 1.0, 1)).0.as_ref(), &ctx())
        .unwrap()
        .expect("handled on the GPU path");
    assert_eq!(before.delta(&context.dispatch_stats()).dispatches, 0);
    let out = out.downcast_ref::<Geometry>().unwrap();
    assert_eq!(
        lanes(out.points().get("weight").unwrap()),
        lanes(geometry.points().get("weight").unwrap())
    );
}

#[test]
fn a_three_dimensional_position_column_is_sampled_at_its_xy() {
    let Some(gpu) = adapter() else { return };
    let base = geometry();
    let AttributeArray::Vec2(xy) = &**base.points().get("P").unwrap() else {
        unreachable!()
    };
    let mut geometry =
        Geometry::from_points3(xy.iter().map(|p| Vec3(p.0, p.1, p.0 - p.1)).collect());
    geometry
        .points_mut()
        .insert(
            "weight",
            base.points().get("weight").unwrap().as_ref().clone(),
        )
        .unwrap();
    let field = fv(AddField {
        left: fv(noise(4, 0.9, 2)),
        right: fv(falloff()),
    });
    let (context, backend, pool) = &gpu;
    let ctx = ctx();
    let spec = FieldApply::new(Domain::Point, "weight");
    let cpu = apply_field(&geometry, &spec, field.0.as_ref(), &ctx).unwrap();
    let out = backend
        .apply(&geometry, &spec, field.0.as_ref(), &ctx)
        .unwrap()
        .expect("a Vec3 P is handled");
    assert_matches(
        "vec3 P",
        &cpu,
        out.downcast_ref::<Geometry>().unwrap(),
        "weight",
        1e-4,
    );
    let resident = GpuGeometry::from_geometry(context.clone(), pool, &geometry).unwrap();
    let out = backend
        .apply(&resident, &spec, field.0.as_ref(), &ctx)
        .unwrap()
        .expect("a resident Vec3 P is handled");
    let back = out
        .downcast_ref::<GpuGeometry>()
        .unwrap()
        .to_geometry()
        .unwrap();
    assert_matches("resident vec3 P", &cpu, &back, "weight", 1e-4);
}

#[test]
fn a_field_without_a_wgsl_version_is_left_to_the_cpu() {
    let Some((_, backend, _)) = adapter() else {
        return;
    };
    let geometry = geometry();
    let spec = FieldApply::new(Domain::Point, "weight");
    let ctx = ctx();

    // An expression field never has one.
    let expression = fv(ExpressionField::new("@P.x * 2.0 + sin(time)", 0.0));
    assert!(FieldProgram::compile(expression.0.as_ref(), &ctx, geometry.points(), COUNT).is_none());
    assert!(
        backend
            .apply(&geometry, &spec, expression.0.as_ref(), &ctx)
            .unwrap()
            .is_none()
    );

    // One buried in a tree takes the whole tree to the CPU: a half-accelerated
    // tree has no single function to call.
    let buried = fv(AddField {
        left: fv(noise(1, 1.0, 1)),
        right: expression,
    });
    assert!(
        backend
            .apply(&geometry, &spec, buried.0.as_ref(), &ctx)
            .unwrap()
            .is_none()
    );

    // So does every configuration whose CPU answer is a warning and a default.
    for (name, declined) in [
        ("an unknown attribute", fv(AttributeField::new("nope"))),
        (
            "a normalized column",
            fv(AttributeField::new("speed").with_normalize(true)),
        ),
        (
            "a component the column lacks",
            fv(AttributeField::new("speed").with_component("y")),
        ),
        (
            "a vector read where a scalar is needed",
            fv(CurveRemapField::new(
                fv(DirectionToField {
                    target: Vec2(0.0, 0.0),
                }),
                CurveParam::identity(),
            )),
        ),
        (
            "a component the source lacks",
            fv(ComponentField::new(fv(ConstantField(1.0)), 2)),
        ),
        (
            "operands that do not combine",
            fv(AddField {
                left: fv(DirectionToField {
                    target: Vec2(0.0, 0.0),
                }),
                right: fv(ComposeField::new(vec![
                    fv(ConstantField(1.0)),
                    fv(ConstantField(1.0)),
                    fv(ConstantField(1.0)),
                ])),
            }),
        ),
    ] {
        assert!(
            backend
                .apply(&geometry, &spec, declined.0.as_ref(), &ctx)
                .unwrap()
                .is_none(),
            "{name} must be left to the CPU"
        );
    }

    // The same goes for what the call asks for: a target the field's type does
    // not fit (the CPU path reports the error), a missing target the CPU path must create or reject, a group that is not a Bool column, the detail domain.
    let vector = fv(DirectionToField {
        target: Vec2(0.0, 0.0),
    });
    let scalar = fv(ConstantField(1.0));
    for (name, field, spec) in [
        (
            "a type mismatch",
            &vector,
            FieldApply::new(Domain::Point, "v3"),
        ),
        (
            "a missing target with creation switched off",
            &scalar,
            FieldApply::new(Domain::Point, "created").with_create_if_missing(false),
        ),
        (
            "a missing target a vector field cannot fill",
            &vector,
            FieldApply::new(Domain::Point, "Cd"),
        ),
        (
            "a missing target that is not modulatable",
            &scalar,
            FieldApply::new(Domain::Point, "fill"),
        ),
        (
            "a missing target left unmodulated",
            &scalar,
            FieldApply::new(Domain::Point, "created").with_amount(0.0),
        ),
        (
            "a non-Bool group",
            &scalar,
            FieldApply::new(Domain::Point, "weight").with_group("speed"),
        ),
        (
            "a missing group",
            &scalar,
            FieldApply::new(Domain::Point, "weight").with_group("nope"),
        ),
        (
            "the detail domain",
            &scalar,
            FieldApply::new(Domain::Detail, "weight"),
        ),
        (
            "an index target",
            &scalar,
            FieldApply::new(Domain::Point, "index"),
        ),
    ] {
        assert!(
            backend
                .apply(&geometry, &spec, field.0.as_ref(), &ctx)
                .unwrap()
                .is_none(),
            "{name} must be left to the CPU"
        );
    }
}

#[test]
fn a_small_cpu_geometry_stays_on_the_cpu_and_a_resident_one_does_not() {
    let Some((context, _, pool)) = adapter() else {
        return;
    };
    let backend = GpuFieldBackend::new(context.clone(), pool.clone());
    let geometry = geometry();
    let spec = FieldApply::new(Domain::Point, "weight");
    let field = fv(noise(1, 1.0, 1));
    let ctx = ctx();
    const { assert!(COUNT < ravel_nodes::field::GPU_MIN_ELEMENTS) };
    assert!(
        backend
            .apply(&geometry, &spec, field.0.as_ref(), &ctx)
            .unwrap()
            .is_none()
    );
    let resident = GpuGeometry::from_geometry(context.clone(), &pool, &geometry).unwrap();
    assert!(
        backend
            .apply(&resident, &spec, field.0.as_ref(), &ctx)
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_combine_chain_is_one_dispatch() {
    let Some((context, backend, pool)) = adapter() else {
        return;
    };
    let geometry = geometry();
    // Six field nodes and the combine, as `field.apply` receives them.
    let chain = fv(RampField::new(
        fv(AddField {
            left: fv(MultiplyField {
                left: fv(noise(5, 0.5, 3)),
                right: fv(falloff()),
            }),
            right: fv(MaxField {
                left: fv(AttributeField::new("speed")),
                right: fv(TimeField::new(TimeMode::Seconds)),
            }),
        }),
        RampParam::black_to_white(),
    ));
    let spec = FieldApply::new(Domain::Point, "Cd").with_combine(CombineMode::Multiply);
    let resident = GpuGeometry::from_geometry(context.clone(), &pool, &geometry).unwrap();

    context.flush();
    let before = context.dispatch_stats();
    let out = backend
        .apply(&resident, &spec, chain.0.as_ref(), &ctx())
        .unwrap()
        .expect("the whole chain has WGSL");
    let after = context.dispatch_stats();
    let delta = before.delta(&after);
    assert_eq!(
        delta.dispatches, 1,
        "the chain, its combine and the write are one pass"
    );
    assert_eq!(
        delta.submits, 0,
        "and it stays in the batch until something reads it"
    );
    context.flush();
    drop(out);

    // The CPU geometry path is the same single dispatch, plus the readback.
    let before = context.dispatch_stats();
    backend
        .apply(&geometry, &spec, chain.0.as_ref(), &ctx())
        .unwrap()
        .expect("the whole chain has WGSL");
    let delta = before.delta(&context.dispatch_stats());
    assert_eq!(delta.dispatches, 1);
    assert_eq!(delta.submits, 1, "the readback is the one flush");
}

#[test]
fn identical_fields_reuse_one_pipeline_while_their_parameters_change() {
    let Some((_, backend, _)) = adapter() else {
        return;
    };
    let geometry = geometry();
    let spec = FieldApply::new(Domain::Point, "weight");
    let ctx = ctx();
    let run = |seed: u32, frequency: f32| {
        backend
            .apply(
                &geometry,
                &spec,
                fv(noise(seed, frequency, 2)).0.as_ref(),
                &ctx,
            )
            .unwrap()
            .unwrap()
    };
    let first = run(1, 1.0);
    let second = run(9, 3.5);
    assert_ne!(
        lanes(
            first
                .downcast_ref::<Geometry>()
                .unwrap()
                .points()
                .get("weight")
                .unwrap()
        ),
        lanes(
            second
                .downcast_ref::<Geometry>()
                .unwrap()
                .points()
                .get("weight")
                .unwrap()
        ),
        "different parameters, different values"
    );
    assert_eq!(
        backend.pipelines_created(),
        1,
        "parameters are data: they must not build a pipeline"
    );
}

#[test]
fn attribute_columns_beyond_the_binding_budget_decline() {
    // A field reading four distinct attributes needs more storage bindings
    // than a compute stage is guaranteed.
    let mut geometry = geometry();
    for name in ["a1", "a2", "a3", "a4"] {
        geometry
            .points_mut()
            .insert(name, AttributeArray::F32(vec![1.0; COUNT]))
            .unwrap();
    }
    let mut tree = fv(AttributeField::new("a1"));
    for name in ["a2", "a3", "a4"] {
        tree = fv(AddField {
            left: tree,
            right: fv(AttributeField::new(name)),
        });
    }
    let program = FieldProgram::compile(tree.0.as_ref(), &ctx(), geometry.points(), COUNT);
    assert!(
        program.is_none(),
        "four attributes exceed the budget of three"
    );
    let program = FieldProgram::compile(
        fv(AddField {
            left: fv(AttributeField::new("a1")),
            right: fv(AttributeField::new("a1")),
        })
        .0
        .as_ref(),
        &ctx(),
        geometry.points(),
        COUNT,
    );
    assert_eq!(
        program
            .expect("one attribute read twice is one binding")
            .attributes()
            .len(),
        1
    );
}

#[test]
fn an_empty_attribute_set_gives_the_builder_nothing_to_read() {
    // The builder asks the lookup, never the geometry: with no attributes an
    // attribute field declines instead of binding a column that is not there.
    let set = AttributeSet::new();
    let program = FieldProgram::compile(
        fv(AttributeField::new("speed")).0.as_ref(),
        &ctx(),
        &set,
        COUNT,
    );
    assert!(program.is_none());
}

/// A target the geometry does not have is created on the GPU path with the CPU
/// path's initial value and type: parity of the whole result, new column
/// included, from a CPU and from a resident input.
#[test]
fn a_missing_target_is_created_the_way_the_cpu_creates_it() {
    let Some(gpu) = adapter() else { return };
    let geometry = geometry();
    let ramp_field = fv(RampField::new(
        fv(noise(2, 0.8, 2)),
        RampParam::black_to_white(),
    ));
    let vector = fv(DirectionToField {
        target: Vec2(1.0, 2.0),
    });
    let scalar = fv(noise(3, 0.5, 2));
    for (name, field, spec) in [
        // An unreserved name takes the sampled type, zeroed.
        (
            "a new scalar column",
            &scalar,
            FieldApply::new(Domain::Point, "created").with_combine(CombineMode::Add),
        ),
        (
            "a new vector column",
            &vector,
            FieldApply::new(Domain::Point, "created2").with_combine(CombineMode::Add),
        ),
        // Reserved names start from what a reader sees without them.
        (
            "alpha starts at one",
            &scalar,
            FieldApply::new(Domain::Point, "alpha").with_combine(CombineMode::Multiply),
        ),
        (
            "pscale starts at two",
            &scalar,
            FieldApply::new(Domain::Point, "pscale").with_combine(CombineMode::Add),
        ),
        (
            "stroke_width keeps its declared type",
            &scalar,
            FieldApply::new(Domain::Point, "stroke_width").with_combine(CombineMode::Add),
        ),
        // A colour a scalar field cannot name the type of: declared Color, and
        // `stroke_color` follows the point's own `Cd` until it is modulated.
        (
            "stroke_color starts from Cd",
            &scalar,
            FieldApply::new(Domain::Point, "stroke_color")
                .with_combine(CombineMode::Multiply)
                .with_components(ComponentMask::parse("rgb")),
        ),
        (
            "stroke_color from a ramp",
            &ramp_field,
            FieldApply::new(Domain::Point, "stroke_color").with_amount(0.5),
        ),
        (
            "a group leaves the rest at the initial value",
            &scalar,
            FieldApply::new(Domain::Point, "alpha")
                .with_combine(CombineMode::Add)
                .with_group("flag"),
        ),
    ] {
        // A colour that follows another reads that one column.
        let readbacks = u64::from(spec.target == "stroke_color");
        check_parity_reading_back(&gpu, name, field, &geometry, &spec, 1e-4, readbacks);
    }
}

/// `Cd` on a geometry with no colours of its own starts from the colour of the
/// path the point sits on, which the GPU path reads back from only the
/// colour columns of a resident geometry.
#[test]
fn a_created_point_colour_follows_its_path_on_both_representations() {
    let Some(gpu) = adapter() else { return };
    let mut path = Geometry::from_points((0..8).map(|i| Vec2(i as f32, 0.0)).collect());
    path.push_primitive(ravel_core::geometry::Primitive::Path {
        verts: 0..4,
        closed: false,
    });
    path.push_primitive(ravel_core::geometry::Primitive::Path {
        verts: 4..8,
        closed: false,
    });
    path.primitive_attrs_mut()
        .insert(
            "Cd",
            AttributeArray::Color(vec![
                Color::new(1.0, 0.0, 0.0, 1.0),
                Color::new(0.0, 1.0, 0.0, 1.0),
            ]),
        )
        .unwrap();
    let field = fv(ConstantField(0.5));
    let spec = FieldApply::new(Domain::Point, "Cd").with_combine(CombineMode::Multiply);
    let (context, backend, pool) = &gpu;
    let cpu = apply_field(&path, &spec, field.0.as_ref(), &ctx()).unwrap();
    let first = cpu.points().get("Cd").unwrap().as_color("Cd").unwrap()[0];
    assert_eq!(
        (first.r, first.g),
        (0.5, 0.0),
        "the CPU follows the first path"
    );
    let from_cpu = backend
        .apply(&path, &spec, field.0.as_ref(), &ctx())
        .unwrap()
        .expect("handled");
    assert_matches(
        "cpu input",
        &cpu,
        from_cpu.downcast_ref::<Geometry>().unwrap(),
        "Cd",
        1e-6,
    );
    let resident = GpuGeometry::from_geometry(context.clone(), pool, &path).unwrap();
    let from_resident = backend
        .apply(&resident, &spec, field.0.as_ref(), &ctx())
        .unwrap()
        .expect("handled");
    let back = from_resident
        .downcast_ref::<GpuGeometry>()
        .unwrap()
        .to_geometry()
        .unwrap();
    assert_matches("resident input", &cpu, &back, "Cd", 1e-6);
}
