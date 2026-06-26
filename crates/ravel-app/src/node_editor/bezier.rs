// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

pub struct BezierPath {
    pub source: (f32, f32),
    pub source_control: (f32, f32),
    pub target_control: (f32, f32),
    pub target: (f32, f32),
}

fn control_offset(distance: f32, curvature: f32) -> f32 {
    if distance >= 0.0 {
        0.5 * distance
    } else {
        curvature * 25.0 * (-distance).sqrt()
    }
}

pub fn horizontal_bezier(sx: f32, sy: f32, tx: f32, ty: f32, curvature: f32) -> BezierPath {
    let scx = sx + control_offset(tx - sx, curvature);
    let tcx = tx - control_offset(tx - sx, curvature);
    BezierPath {
        source: (sx, sy),
        source_control: (scx, sy),
        target_control: (tcx, ty),
        target: (tx, ty),
    }
}

pub fn point_to_bezier_distance(px: f32, py: f32, path: &BezierPath, samples: usize) -> f32 {
    let (x0, y0) = path.source;
    let (cx0, cy0) = path.source_control;
    let (cx1, cy1) = path.target_control;
    let (x1, y1) = path.target;

    let mut min_dist = f32::MAX;
    for i in 0..=samples {
        let t = i as f32 / samples as f32;
        let u = 1.0 - t;
        let bx = u * u * u * x0 + 3.0 * u * u * t * cx0 + 3.0 * u * t * t * cx1 + t * t * t * x1;
        let by = u * u * u * y0 + 3.0 * u * u * t * cy0 + 3.0 * u * t * t * cy1 + t * t * t * y1;
        let dist = ((px - bx).powi(2) + (py - by).powi(2)).sqrt();
        min_dist = min_dist.min(dist);
    }
    min_dist
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_bezier_endpoints() {
        let path = horizontal_bezier(0.0, 0.0, 200.0, 100.0, 0.25);
        assert_eq!(path.source, (0.0, 0.0));
        assert_eq!(path.target, (200.0, 100.0));
        assert!(path.source_control.0 > 0.0);
        assert!(path.target_control.0 < 200.0);
    }

    #[test]
    fn distance_at_endpoint_is_zero() {
        let path = horizontal_bezier(0.0, 0.0, 200.0, 0.0, 0.25);
        let d = point_to_bezier_distance(0.0, 0.0, &path, 20);
        assert!(d < 0.1);
    }
}
