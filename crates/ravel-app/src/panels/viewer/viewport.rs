// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Pure viewport math for the Viewer panel.

pub const MIN_ZOOM: f32 = 0.05;
pub const MAX_ZOOM: f32 = 32.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Fit,
    Explicit { zoom: f32, offset: (f32, f32) },
}

/// Maps composition pixels into panel-local screen pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewerViewport {
    mode: Mode,
}

impl Default for ViewerViewport {
    fn default() -> Self {
        Self { mode: Mode::Fit }
    }
}

impl ViewerViewport {
    pub fn rect(self, panel: (f32, f32), composition: (u32, u32)) -> Rect {
        let (comp_width, comp_height) = (composition.0 as f32, composition.1 as f32);
        if panel.0 <= 0.0 || panel.1 <= 0.0 || comp_width <= 0.0 || comp_height <= 0.0 {
            return Rect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            };
        }

        match self.mode {
            Mode::Fit => {
                let zoom = (panel.0 / comp_width).min(panel.1 / comp_height);
                let width = comp_width * zoom;
                let height = comp_height * zoom;
                Rect {
                    x: (panel.0 - width) * 0.5,
                    y: (panel.1 - height) * 0.5,
                    width,
                    height,
                }
            }
            Mode::Explicit { zoom, offset } => Rect {
                x: offset.0,
                y: offset.1,
                width: comp_width * zoom,
                height: comp_height * zoom,
            },
        }
    }

    pub fn zoom(self, panel: (f32, f32), composition: (u32, u32)) -> f32 {
        let rect = self.rect(panel, composition);
        if composition.0 == 0 {
            1.0
        } else {
            rect.width / composition.0 as f32
        }
    }

    pub fn zoom_to_fit(&mut self) {
        self.mode = Mode::Fit;
    }

    pub fn zoom_toward(
        &mut self,
        requested_zoom: f32,
        anchor: (f32, f32),
        panel: (f32, f32),
        composition: (u32, u32),
    ) {
        let old = self.rect(panel, composition);
        if old.width <= 0.0 || old.height <= 0.0 {
            return;
        }
        let old_zoom = self.zoom(panel, composition);
        let zoom = requested_zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        let comp_x = (anchor.0 - old.x) / old_zoom;
        let comp_y = (anchor.1 - old.y) / old_zoom;
        self.mode = Mode::Explicit {
            zoom,
            offset: (anchor.0 - comp_x * zoom, anchor.1 - comp_y * zoom),
        };
    }

    pub fn begin_pan(&mut self, panel: (f32, f32), composition: (u32, u32)) -> (f32, f32) {
        let rect = self.rect(panel, composition);
        let zoom = self.zoom(panel, composition).clamp(MIN_ZOOM, MAX_ZOOM);
        let offset = (rect.x, rect.y);
        self.mode = Mode::Explicit { zoom, offset };
        offset
    }

    pub fn set_offset(&mut self, offset: (f32, f32)) {
        if let Mode::Explicit { zoom, .. } = self.mode {
            self.mode = Mode::Explicit { zoom, offset };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_contains_and_centers_the_composition() {
        let viewport = ViewerViewport::default();
        let rect = viewport.rect((1000.0, 1000.0), (1920, 1080));
        assert!(rect.x.abs() < 0.001);
        assert!((rect.y - 218.75).abs() < 0.001);
        assert!((rect.width - 1000.0).abs() < 0.001);
        assert!((rect.height - 562.5).abs() < 0.001);
    }

    #[test]
    fn fit_tracks_panel_resize() {
        let viewport = ViewerViewport::default();
        assert_eq!(viewport.zoom((960.0, 540.0), (1920, 1080)), 0.5);
        assert_eq!(viewport.zoom((1920.0, 1080.0), (1920, 1080)), 1.0);
    }

    #[test]
    fn zoom_keeps_the_anchor_over_the_same_composition_pixel() {
        let mut viewport = ViewerViewport::default();
        let panel = (1000.0, 800.0);
        let composition = (1000, 500);
        let anchor = (250.0, 300.0);
        let before = viewport.rect(panel, composition);
        let before_comp = (
            (anchor.0 - before.x) / viewport.zoom(panel, composition),
            (anchor.1 - before.y) / viewport.zoom(panel, composition),
        );

        viewport.zoom_toward(2.0, anchor, panel, composition);
        let after = viewport.rect(panel, composition);
        let after_comp = ((anchor.0 - after.x) / 2.0, (anchor.1 - after.y) / 2.0);
        assert_eq!(before_comp, after_comp);
    }

    #[test]
    fn zoom_is_clamped() {
        let mut viewport = ViewerViewport::default();
        viewport.zoom_toward(0.001, (50.0, 50.0), (100.0, 100.0), (100, 100));
        assert_eq!(viewport.zoom((100.0, 100.0), (100, 100)), MIN_ZOOM);
        viewport.zoom_toward(100.0, (50.0, 50.0), (100.0, 100.0), (100, 100));
        assert_eq!(viewport.zoom((100.0, 100.0), (100, 100)), MAX_ZOOM);
    }

    #[test]
    fn pan_converts_fit_to_explicit_and_composes_delta() {
        let mut viewport = ViewerViewport::default();
        let start = viewport.begin_pan((1000.0, 800.0), (1000, 500));
        viewport.set_offset((start.0 + 40.0, start.1 - 20.0));
        let rect = viewport.rect((1000.0, 800.0), (1000, 500));
        assert_eq!((rect.x, rect.y), (40.0, 130.0));
    }
}
