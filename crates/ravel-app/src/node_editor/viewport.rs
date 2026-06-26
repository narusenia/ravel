// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            zoom: 1.0,
        }
    }
}

impl Viewport {
    pub const MIN_ZOOM: f32 = 0.1;
    pub const MAX_ZOOM: f32 = 3.0;

    pub fn flow_to_screen(&self, fx: f32, fy: f32) -> (f32, f32) {
        (fx * self.zoom + self.x, fy * self.zoom + self.y)
    }

    pub fn screen_to_flow(&self, sx: f32, sy: f32) -> (f32, f32) {
        ((sx - self.x) / self.zoom, (sy - self.y) / self.zoom)
    }

    pub fn zoom_toward(&mut self, new_zoom: f32, focus_x: f32, focus_y: f32) {
        let new_zoom = new_zoom.clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        let old_zoom = self.zoom;
        self.x = focus_x - (focus_x - self.x) * (new_zoom / old_zoom);
        self.y = focus_y - (focus_y - self.y) * (new_zoom / old_zoom);
        self.zoom = new_zoom;
    }

    pub fn fit_to_content(
        &mut self,
        rects: &[(f32, f32, f32, f32)],
        container_w: f32,
        container_h: f32,
        padding: f32,
    ) {
        if rects.is_empty() {
            return;
        }

        let mut min_x = f32::MAX;
        let mut min_y = f32::MAX;
        let mut max_x = f32::MIN;
        let mut max_y = f32::MIN;

        for &(x, y, w, h) in rects {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x + w);
            max_y = max_y.max(y + h);
        }

        let content_w = max_x - min_x + padding * 2.0;
        let content_h = max_y - min_y + padding * 2.0;
        if content_w <= 0.0 || content_h <= 0.0 {
            return;
        }

        let zoom = (container_w / content_w)
            .min(container_h / content_h)
            .clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);

        self.zoom = zoom;
        self.x = (container_w - content_w * zoom) / 2.0 - (min_x - padding) * zoom;
        self.y = (container_h - content_h * zoom) / 2.0 - (min_y - padding) * zoom;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_coordinate_transform() {
        let vp = Viewport {
            x: 50.0,
            y: 30.0,
            zoom: 2.0,
        };
        let (sx, sy) = vp.flow_to_screen(100.0, 200.0);
        let (fx, fy) = vp.screen_to_flow(sx, sy);
        assert!((fx - 100.0).abs() < 0.001);
        assert!((fy - 200.0).abs() < 0.001);
    }

    #[test]
    fn zoom_toward_clamps() {
        let mut vp = Viewport::default();
        vp.zoom_toward(10.0, 0.0, 0.0);
        assert_eq!(vp.zoom, Viewport::MAX_ZOOM);
        vp.zoom_toward(0.01, 0.0, 0.0);
        assert_eq!(vp.zoom, Viewport::MIN_ZOOM);
    }

    #[test]
    fn fit_to_content_centers() {
        let mut vp = Viewport::default();
        let rects = vec![(0.0, 0.0, 100.0, 100.0)];
        vp.fit_to_content(&rects, 400.0, 400.0, 20.0);
        assert!(vp.zoom > 0.0);
        let (cx, cy) = vp.flow_to_screen(50.0, 50.0);
        assert!((cx - 200.0).abs() < 1.0);
        assert!((cy - 200.0).abs() < 1.0);
    }
}
