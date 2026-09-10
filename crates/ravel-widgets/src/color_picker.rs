// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ravel's colour picker.
//!
//! The same split [`crate::input`] makes: `gpui_base`'s [`ColorPickerState`]
//! owns the committed colour, the preview, the open state, the hex field and
//! the four HSLA slider states, and this module owns every visual decision —
//! which `gpui-base`'s own documentation says outright, because a picker's
//! palette and layout are the application's to choose.
//!
//! # What the popup is
//!
//! A 2D face, a hue strip beside it, an alpha rail and a hex field, in that
//! Tab order. There is no palette grid: gpui-component's picker offered nine
//! nine-step ramps of its own theme colours, none of which mean anything to a
//! composition's colour parameter.
//!
//! # The face is two divs, not a canvas
//!
//! A saturation × brightness square is a horizontal white → hue gradient with
//! a vertical transparent → black gradient over it. Both have exactly two
//! stops, which is all [`gpui::linear_gradient`] carries, so no custom
//! painting is needed — and the black layer is on *top* because it is what
//! darkens the hue, which is multiplication, which is what an alpha composite
//! over the hue does.
//!
//! Both gradients interpolate in **sRGB** rather than the `linear_gradient`
//! default of Oklab, and so does the hue strip. That is not a taste call: the
//! hue circle is piecewise linear between its six vertices in gamma-encoded
//! sRGB, so [`HUE_BANDS`] two-stop bands in sRGB reproduce the hue ramp
//! *exactly*, while the same six bands in Oklab bow away from it in the middle
//! of every band. Six is the right number only because of the colour space.
//!
//! # HSV on screen, HSL in the state
//!
//! The state stores [`gpui::Hsla`], and an HSL saturation × lightness plane is
//! not what those two divs paint — at lightness 1 every saturation is white,
//! so the marker would sit on a bright hue while the value was white. The face
//! is therefore HSV, and [`face_point`] / [`face_color`] convert. The pair is
//! exact in both directions, including at black, where HSV loses the
//! saturation a whole row shares and HSL's own saturation field carries it.
//!
//! # One value road
//!
//! [`pointer_color`] and [`nudged`] are the pointer's and the keyboard's
//! answers to "which colour does this surface write", and both end in
//! [`with_face_point`] / [`with_hue`] / [`with_alpha`], which end in
//! [`ColorPickerState::update_color`]. So the arrow keys emit the same
//! `ColorPickerEvent::Change` a drag does and inherit the debounce the
//! consumer already has for it — holding an arrow down records one undo step
//! for the same reason dragging does, and not because this module counts
//! anything.
//!
//! # The arrows are owned, so the playhead does not take them
//!
//! `Left` / `Right` are bound to frame stepping application-wide. A popup is
//! not a `PopupMenu` and the face is not an `Input`, so nothing in the
//! existing predicate would stop the playhead from stepping while the user
//! nudges a colour. The popup surface therefore declares
//! [`COLOR_PICKER_SURFACE_CONTEXT`], and the host excludes that context from
//! its own bindings the way it already excludes menus.
//!
//! The context sits on the popup, **not** on the trigger: a closed picker owns
//! no arrows, and `gpui_base`'s root already uses its own key context to bind
//! Enter and Escape, so overriding that would cost the picker its keyboard
//! opening.

use gpui::{
    App, AppContext as _, Background, Bounds, ColorSpace, Div, Empty, Entity, EntityId,
    FocusHandle, Focusable as _, Hsla, InteractiveElement as _, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, ParentElement as _, Pixels, Point, Render, RenderOnce,
    SharedString, StatefulInteractiveElement, StyleRefinement, Styled, Window, black, div,
    linear_color_stop, linear_gradient, pattern_slash, prelude::FluentBuilder as _, px, relative,
    white,
};
use gpui_base::{
    ColorPicker as BaseColorPicker, ColorPickerState, Popover, Slider, StyledExt as _,
};

use ravel_i18n::t;

use crate::input::{Input, frame_is_focused};
use crate::theme::ActiveTokens as _;
use crate::tokens::{
    Colors, Density, RavelTheme, hsla_from_hsv, hsv_of, hue_color, hue_fraction, with_alpha,
    with_hue,
};

/// The key context the popup's surfaces declare.
///
/// It means "this element owns the arrow keys". A host that binds arrows of
/// its own excludes this context from those bindings, which is the only thing
/// that stops a nudge from also stepping the playhead.
pub const COLOR_PICKER_SURFACE_CONTEXT: &str = "ColorPickerSurface";

/// The side of the saturation × brightness face.
pub const FACE_SIZE: Pixels = px(120.0);
/// The width of the hue strip beside the face.
pub const HUE_STRIP_WIDTH: Pixels = px(12.0);
/// The side of the trigger swatch.
pub const SWATCH_SIZE: Pixels = px(16.0);
/// The corner radius of the trigger swatch.
///
/// Named rather than taken from `radius`: the swatch is 16px square and the
/// theme radius is a control's radius, which on a square this small reads as a
/// circle.
pub const SWATCH_RADIUS: Pixels = px(2.0);
/// The diameter of the face's marker.
pub const MARKER_SIZE: Pixels = px(10.0);
/// The height of the hue strip's marker.
pub const HUE_MARKER_HEIGHT: Pixels = px(2.0);

/// The stripe width of the slash underlay a translucent swatch shows through.
const SLASH_WIDTH: f32 = 2.0;
/// The gap between the underlay's stripes.
const SLASH_INTERVAL: f32 = 2.0;

/// How many two-stop bands the hue strip is built from.
///
/// Six, because the hue circle's vertices are 60° apart: a band between two
/// neighbouring vertices is a straight line in sRGB, so six bands are the hue
/// ramp rather than an approximation of it (see the module docs).
pub const HUE_BANDS: usize = 6;

/// What one arrow press moves an axis by.
pub const NUDGE_STEP: f32 = 0.01;
/// What one arrow press moves an axis by while Shift is held.
pub const NUDGE_STEP_COARSE: f32 = 0.10;

/// The colour a picker with no value shows and edits from.
const FALLBACK: Hsla = black();

// ---------------------------------------------------------------------------
// The value road: where a colour comes from, whatever moved
// ---------------------------------------------------------------------------

/// A point on the picker's face, both axes `0..=1`.
///
/// These are HSV axes, which is what the face paints — see the module docs for
/// why the state's HSL saturation and lightness are not the two axes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FacePoint {
    /// Horizontal: HSV saturation, 0 at the white edge.
    pub saturation: f32,
    /// Vertical: HSV brightness, 1 at the top edge.
    pub value: f32,
}

/// Where `color` sits on the face.
///
/// The conversion itself is [`crate::tokens::hsv_of`] — the token module owns
/// every colour-space operation in this crate — and this names its two
/// components as the face's axes.
pub fn face_point(color: Hsla) -> FacePoint {
    let (saturation, value) = hsv_of(color);
    FacePoint { saturation, value }
}

/// The colour at `point` on the face, for a hue and an alpha it does not own.
pub fn face_color(hue: f32, point: FacePoint, alpha: f32) -> Hsla {
    hsla_from_hsv(hue, point.saturation, point.value, alpha)
}

/// `color` moved to `point` on the face, keeping its hue and alpha.
pub fn with_face_point(color: Hsla, point: FacePoint) -> Hsla {
    face_color(hue_fraction(color), point, color.alpha)
}

/// One of the popup's three graphical surfaces.
///
/// Each owns its own axes, and the arrows it does not own it does not take
/// (see [`nudged`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerSurface {
    /// The saturation × brightness face.
    Face,
    /// The vertical hue strip.
    Hue,
    /// The horizontal alpha rail.
    Alpha,
}

/// One arrow press, named by the direction the marker moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nudge {
    Left,
    Right,
    Up,
    Down,
}

impl Nudge {
    /// The nudge a keystroke names, or `None` when it is not an arrow.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            _ => None,
        }
    }
}

/// How far one arrow press moves an axis.
pub fn nudge_step(shift: bool) -> f32 {
    if shift { NUDGE_STEP_COARSE } else { NUDGE_STEP }
}

/// `point` moved one step in `nudge`'s direction, clamped to the face.
///
/// Up is brighter because up is where the bright edge is painted: the arrows
/// move the marker, not the number.
fn nudge_point(point: FacePoint, nudge: Nudge, step: f32) -> FacePoint {
    let clamp = |value: f32| value.clamp(0.0, 1.0);
    match nudge {
        Nudge::Left => FacePoint {
            saturation: clamp(point.saturation - step),
            ..point
        },
        Nudge::Right => FacePoint {
            saturation: clamp(point.saturation + step),
            ..point
        },
        Nudge::Up => FacePoint {
            value: clamp(point.value + step),
            ..point
        },
        Nudge::Down => FacePoint {
            value: clamp(point.value - step),
            ..point
        },
    }
}

/// The colour an arrow press writes on `surface`.
///
/// `None` means nothing is written: either the surface does not own that arrow
/// (the hue strip has no horizontal axis) or the axis is already at the end it
/// was pushed towards. A gesture that changed nothing records nothing — UX
/// invariant 3 — and this is where a held arrow stops emitting at the edge.
pub fn nudged(color: Hsla, surface: PickerSurface, nudge: Nudge, shift: bool) -> Option<Hsla> {
    let step = nudge_step(shift);
    let clamp = |value: f32| value.clamp(0.0, 1.0);
    let next = match (surface, nudge) {
        (PickerSurface::Face, _) => {
            with_face_point(color, nudge_point(face_point(color), nudge, step))
        }
        // The strip paints hue 0 at the top, so up is a smaller hue.
        (PickerSurface::Hue, Nudge::Up) => with_hue(color, clamp(hue_fraction(color) - step)),
        (PickerSurface::Hue, Nudge::Down) => with_hue(color, clamp(hue_fraction(color) + step)),
        (PickerSurface::Alpha, Nudge::Left) => with_alpha(color, clamp(color.alpha - step)),
        (PickerSurface::Alpha, Nudge::Right) => with_alpha(color, clamp(color.alpha + step)),
        _ => return None,
    };
    (next != color).then_some(next)
}

/// Where `position` falls across `bounds`, `0..1` from its left edge.
pub fn horizontal_fraction(bounds: Bounds<Pixels>, position: Point<Pixels>) -> f32 {
    let width = f32::from(bounds.size.width);
    if width <= 0.0 {
        return 0.0;
    }
    ((f32::from(position.x) - f32::from(bounds.origin.x)) / width).clamp(0.0, 1.0)
}

/// Where `position` falls down `bounds`, `0..1` from its **top** edge.
pub fn vertical_fraction(bounds: Bounds<Pixels>, position: Point<Pixels>) -> f32 {
    let height = f32::from(bounds.size.height);
    if height <= 0.0 {
        return 0.0;
    }
    ((f32::from(position.y) - f32::from(bounds.origin.y)) / height).clamp(0.0, 1.0)
}

/// The face point `position` names inside `bounds`.
pub fn face_point_at(bounds: Bounds<Pixels>, position: Point<Pixels>) -> FacePoint {
    FacePoint {
        saturation: horizontal_fraction(bounds, position),
        value: 1.0 - vertical_fraction(bounds, position),
    }
}

/// The colour a pointer at `position` writes on `surface`.
///
/// The other half of [`nudged`], and deliberately the same three writers: a
/// second way to build the colour is a second thing to keep in step.
pub fn pointer_color(
    color: Hsla,
    surface: PickerSurface,
    bounds: Bounds<Pixels>,
    position: Point<Pixels>,
) -> Hsla {
    match surface {
        PickerSurface::Face => with_face_point(color, face_point_at(bounds, position)),
        PickerSurface::Hue => with_hue(color, vertical_fraction(bounds, position)),
        PickerSurface::Alpha => with_alpha(color, horizontal_fraction(bounds, position)),
    }
}

// ---------------------------------------------------------------------------
// The swatch
// ---------------------------------------------------------------------------

/// The layers a swatch paints, bottom first.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SwatchLayers {
    /// The opaque ground the underlay's stripes are drawn on, so a swatch
    /// reads the same over any panel.
    pub ground: Hsla,
    /// The slash pattern a translucent colour shows through.
    pub underlay: Background,
    /// The colour itself, at its own alpha. `None` when the picker has no
    /// value yet.
    pub color: Option<Hsla>,
}

/// What a swatch of `color` paints.
///
/// The colour keeps its alpha, which is the whole point of the underlay: in a
/// compositor "this colour is black" and "this colour is transparent" are two
/// different things and a flattened swatch cannot tell them apart.
pub fn swatch_layers(color: Option<Hsla>, colors: &Colors) -> SwatchLayers {
    SwatchLayers {
        ground: colors.background,
        // `border` rather than a grey of its own: the stripes are chrome, and
        // a neutral that already reads against `background` is what the token
        // set has for that.
        underlay: pattern_slash(colors.border, SLASH_WIDTH, SLASH_INTERVAL),
        color,
    }
}

/// A swatch: the colour over a slash underlay, in a 16px rounded square.
pub fn swatch(color: Option<Hsla>, colors: &Colors) -> Div {
    let layers = swatch_layers(color, colors);
    div()
        .relative()
        .flex_shrink_0()
        .size(SWATCH_SIZE)
        .rounded(SWATCH_RADIUS)
        .overflow_hidden()
        .border_1()
        .border_color(colors.border)
        .bg(layers.ground)
        .child(div().absolute().inset_0().bg(layers.underlay))
        .when_some(layers.color, |this, color| {
            this.child(div().absolute().inset_0().bg(color))
        })
}

// ---------------------------------------------------------------------------
// The element
// ---------------------------------------------------------------------------

/// The hue fractions band `index` of the strip runs between.
pub fn hue_band(index: usize) -> (f32, f32) {
    let step = 1.0 / HUE_BANDS as f32;
    (index as f32 * step, (index as f32 + 1.0) * step)
}

/// The popup surfaces' resolved bounds, recorded at prepaint.
///
/// A pointer event carries a window position, so mapping it onto an axis needs
/// the surface's geometry; this is the same slot `gpui_base`'s own slider
/// keeps for the same reason.
#[derive(Debug, Clone, Copy, Default)]
struct SurfaceBounds {
    face: Bounds<Pixels>,
    hue: Bounds<Pixels>,
    alpha: Bounds<Pixels>,
}

impl SurfaceBounds {
    fn get(&self, surface: PickerSurface) -> Bounds<Pixels> {
        match surface {
            PickerSurface::Face => self.face,
            PickerSurface::Hue => self.hue,
            PickerSurface::Alpha => self.alpha,
        }
    }

    fn set(&mut self, surface: PickerSurface, bounds: Bounds<Pixels>) {
        match surface {
            PickerSurface::Face => self.face = bounds,
            PickerSurface::Hue => self.hue = bounds,
            PickerSurface::Alpha => self.alpha = bounds,
        }
    }
}

/// The drag in flight on one of the popup's surfaces.
///
/// It names the **surface** as well as the picker, and both halves are load
/// bearing. `Interactivity::on_drag_move` fires in the capture phase for
/// every painted listener of the payload's type *without consulting its
/// hitbox*, so a drag anywhere in the window reaches all three surfaces of
/// every picker in it. The picker alone cannot tell them apart, because all
/// three read the same `ColorPickerState`: dragging the alpha rail wrote the
/// face as well, with a pointer below the face, which clamps brightness to
/// zero and pinned the marker to the bottom edge.
#[derive(Clone)]
struct SurfaceDrag(EntityId, PickerSurface);

impl Render for SurfaceDrag {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// Write a colour. Every pointer and every keystroke ends here.
fn commit(state: &Entity<ColorPickerState>, color: Hsla, window: &mut Window, cx: &mut App) {
    state.update(cx, |state, cx| state.update_color(color, window, cx));
}

/// The colour the popup is editing.
fn edited(state: &Entity<ColorPickerState>, cx: &App) -> Hsla {
    state.read(cx).displayed_color().unwrap_or(FALLBACK)
}

/// A Ravel colour picker.
///
/// The state is the caller's, for the same reason [`crate::Input`]'s is: the
/// value, the open state and the hex field live on [`ColorPickerState`], which
/// needs a `Window` to be built. This element decides appearance.
#[derive(IntoElement)]
pub struct ColorPicker {
    state: Entity<ColorPickerState>,
    density: Density,
    disabled: bool,
    accessibility_label: Option<SharedString>,
    tab_index: isize,
    style: StyleRefinement,
}

impl ColorPicker {
    /// Paint `state` on the default density step.
    pub fn new(state: &Entity<ColorPickerState>) -> Self {
        Self {
            state: state.clone(),
            density: Density::default(),
            disabled: false,
            accessibility_label: None,
            tab_index: 0,
            style: StyleRefinement::default(),
        }
    }

    /// Draw the trigger and the popup's fields on the compact step.
    pub fn compact(mut self) -> Self {
        self.density = Density::Compact;
        self
    }

    /// Draw the picker on `density`.
    pub fn density(mut self, density: Density) -> Self {
        self.density = density;
        self
    }

    /// Whether the picker refuses to open.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// The name announced for the trigger.
    pub fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }

    /// Where the trigger sits in the Tab order.
    pub fn tab_index(mut self, tab_index: isize) -> Self {
        self.tab_index = tab_index;
        self
    }
}

impl Styled for ColorPicker {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for ColorPicker {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.tokens().clone();
        // A value handed to `default_value` cannot reach the sliders or the
        // hex field without a `Window`; flushing it from render is the
        // contract `gpui_base` states for it.
        self.state
            .update(cx, |state, cx| state.sync_pending_value(window, cx));

        let entity_id = self.state.entity_id();
        let open = self.state.read(cx).is_open() && !self.disabled;
        let trigger_focus = self.state.read(cx).focus_handle(cx);
        let density = self.density;
        let disabled = self.disabled;

        let trigger_theme = theme.clone();
        let trigger_state = self.state.clone();
        let popup_state = self.state.clone();
        let open_state = self.state.clone();
        let popover_state = self.state.clone();

        BaseColorPicker::new(("ravel-color-picker", entity_id))
            .open(open)
            .disabled(disabled)
            .track_focus(&trigger_focus)
            .tab_index(self.tab_index)
            .when_some(self.accessibility_label, |this, label| {
                this.accessibility_label(label)
            })
            .on_open_change(move |open, _, cx| {
                open_state.update(cx, |state, cx| state.set_open(open, cx));
            })
            .child(
                Popover::new(("ravel-color-picker-popover", entity_id))
                    .open(open)
                    .on_open_change(move |open: &bool, _, cx| {
                        popover_state.update(cx, |state, cx| state.set_open(*open, cx));
                    })
                    .trigger_with(move |_, _, cx| {
                        let colors = &trigger_theme.colors;
                        let metrics = density.metrics(&trigger_theme);
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .h(metrics.height)
                            .child(swatch(trigger_state.read(cx).value(), colors))
                            .into_any_element()
                    })
                    .content(move |_, window, cx| popup(&popup_state, &theme, density, window, cx)),
            )
            .refine_style(&self.style)
    }
}

/// The popup: the face and the strip, the alpha rail, the hex field.
fn popup(
    state: &Entity<ColorPickerState>,
    theme: &RavelTheme,
    density: Density,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement + use<> {
    let colors = &theme.colors;
    let bounds = window.use_keyed_state(
        ("ravel-color-picker-bounds", state.entity_id()),
        cx,
        |_, _| SurfaceBounds::default(),
    );
    let color = edited(state, cx);

    div()
        // The one element the host's arrow bindings have to yield to. It sits
        // on the whole popup rather than on each surface so that the hex
        // field's neighbours cannot be reached without it in the chain.
        .key_context(COLOR_PICKER_SURFACE_CONTEXT)
        .flex()
        .flex_col()
        .gap(theme.spacing.xs)
        .p(theme.spacing.xs)
        .rounded(theme.radius.radius)
        // A border rather than a shadow, the same choice `crate::tooltip`
        // argues for: a shadow under a small popup in a dense tool reads as
        // blur instead of as height.
        .border_1()
        .border_color(colors.border)
        .bg(colors.raised_surface())
        .text_size(density.metrics(theme).font_size)
        .text_color(colors.foreground)
        .child(
            div()
                .flex()
                .gap(theme.spacing.xs)
                .child(face(state, &bounds, color, colors, window, cx))
                .child(hue_strip(state, &bounds, color, colors, window, cx)),
        )
        .child(alpha_rail(
            state, &bounds, color, theme, density, window, cx,
        ))
        .child(Input::new(state.read(cx).hex_input()).density(density))
}

/// The border a popup surface paints: the focus ring while it holds focus.
///
/// A surface's ring follows plain focus rather than `focus_visible`, the same
/// disagreement with `Button` that [`crate::input`] makes and for the same
/// reason: these are surfaces the user clicks into, and after a click the
/// pointer moves on while the arrows still belong to whatever was clicked.
fn surface_border(handle: &FocusHandle, colors: &Colors, window: &Window) -> Hsla {
    if frame_is_focused(handle, false, window) {
        colors.focus_ring()
    } else {
        colors.border
    }
}

/// The name one surface goes by: the key its focus handle is kept under, and
/// the debug selector a test finds its rectangle by.
fn surface_name(surface: PickerSurface) -> &'static str {
    match surface {
        PickerSurface::Face => "ravel-color-picker-face",
        PickerSurface::Hue => "ravel-color-picker-hue",
        PickerSurface::Alpha => "ravel-color-picker-alpha",
    }
}

/// One popup surface's focus handle, stable while the popup is open.
fn surface_focus(
    surface: PickerSurface,
    state: &Entity<ColorPickerState>,
    window: &mut Window,
    cx: &mut App,
) -> FocusHandle {
    let key = surface_name(surface);
    window
        .use_keyed_state((key, state.entity_id()), cx, |_, cx| {
            cx.focus_handle().tab_stop(true)
        })
        .read(cx)
        .clone()
}

/// Give `element` a surface's pointer, drag and arrow-key handling.
///
/// One function for all three surfaces, because "the pointer and the keyboard
/// write through the same road" is only true if there is one road: the pointer
/// goes through [`pointer_color`], the arrows through [`nudged`], and both
/// through [`commit`].
fn wire_surface<E>(
    element: E,
    surface: PickerSurface,
    state: &Entity<ColorPickerState>,
    bounds: &Entity<SurfaceBounds>,
    tab_index: isize,
    handle: &FocusHandle,
) -> E
where
    E: StatefulInteractiveElement,
{
    let entity_id = state.entity_id();
    let write = {
        let state = state.clone();
        let bounds = bounds.clone();
        move |position: Point<Pixels>, window: &mut Window, cx: &mut App| {
            let area = bounds.read(cx).get(surface);
            let next = pointer_color(edited(&state, cx), surface, area, position);
            commit(&state, next, window, cx);
        }
    };

    element
        .track_focus(handle)
        .tab_index(tab_index)
        // Where a surface ended up is otherwise invisible to a test: the
        // popup is placed by the popover, so a test that wants to drag on one
        // surface has no other way to find its rectangle.
        .debug_selector(move || surface_name(surface).to_owned())
        // The surface's geometry, recorded where the pointer handling that
        // needs it lives: a mouse event carries a window position and nothing
        // else can turn that into a fraction of an axis.
        .on_prepaint({
            let bounds = bounds.clone();
            move |prepaint: &gpui::InteractivityPrepaint, _, cx| {
                bounds.update(cx, |bounds, _| bounds.set(surface, prepaint.bounds));
            }
        })
        // **No `stop_propagation` here.** gpui's own click-to-focus for a
        // `track_focus`ed element is a mouse-down listener registered *before*
        // this one, and paint-time listeners run innermost-first — so stopping
        // propagation here silences it and the surface can only be reached by
        // Tab. Measured in the running application: the click picked a colour
        // and then the arrow keys did nothing, because nothing had focus.
        .on_mouse_down(MouseButton::Left, {
            let write = write.clone();
            move |event: &MouseDownEvent, window, cx| {
                write(event.position, window, cx);
            }
        })
        .on_drag(SurfaceDrag(entity_id, surface), |drag, _, _, cx| {
            cx.stop_propagation();
            cx.new(|_| drag.clone())
        })
        .on_drag_move(
            move |event: &gpui::DragMoveEvent<SurfaceDrag>, window, cx| {
                let SurfaceDrag(dragged, dragged_surface) = event.drag(cx);
                // Both halves, for the reason `SurfaceDrag` documents.
                if *dragged != entity_id || *dragged_surface != surface {
                    return;
                }
                write(event.event.position, window, cx);
            },
        )
        .on_key_down({
            let state = state.clone();
            move |event: &KeyDownEvent, window, cx| {
                let Some(nudge) = Nudge::from_key(event.keystroke.key.as_str()) else {
                    return;
                };
                let shift = event.keystroke.modifiers.shift;
                // Only a press that writes is consumed: an arrow this surface
                // has no axis for, or one already against the end of its
                // axis, is left to whatever else is listening.
                if let Some(next) = nudged(edited(&state, cx), surface, nudge, shift) {
                    cx.stop_propagation();
                    commit(&state, next, window, cx);
                }
            }
        })
}

/// The saturation × brightness face, with the marker on it.
fn face(
    state: &Entity<ColorPickerState>,
    bounds: &Entity<SurfaceBounds>,
    color: Hsla,
    colors: &Colors,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement + use<> {
    let point = face_point(color);
    let hue = hue_color(hue_fraction(color));
    let handle = surface_focus(PickerSurface::Face, state, window, cx);
    let border = surface_border(&handle, colors, window);

    let element = div()
        .id("face")
        .relative()
        .size(FACE_SIZE)
        .flex_shrink_0()
        .border_1()
        .border_color(border)
        // White on the left, the hue on the right.
        .bg(linear_gradient(
            90.0,
            linear_color_stop(white(), 0.0),
            linear_color_stop(hue, 1.0),
        )
        .color_space(ColorSpace::Srgb))
        .child(
            // Black over it, transparent at the top: the layer that darkens
            // is the layer on top, because darkening is what compositing
            // black over the hue does.
            div().absolute().inset_0().bg(linear_gradient(
                180.0,
                linear_color_stop(with_alpha(black(), 0.0), 0.0),
                linear_color_stop(black(), 1.0),
            )
            .color_space(ColorSpace::Srgb)),
        )
        .child(
            div()
                .absolute()
                .left(relative(point.saturation))
                .top(relative(1.0 - point.value))
                .size(MARKER_SIZE)
                .ml(-(MARKER_SIZE / 2.0))
                .mt(-(MARKER_SIZE / 2.0))
                .rounded_full()
                .border_2()
                // Whichever of the two ends of the palette is readable on the
                // colour under the marker, so the ring survives both the white
                // corner and the black row.
                .border_color(colors.readable_on(color)),
        );

    wire_surface(element, PickerSurface::Face, state, bounds, 0, &handle)
}

/// The hue strip: [`HUE_BANDS`] two-stop bands, and a line at the hue.
fn hue_strip(
    state: &Entity<ColorPickerState>,
    bounds: &Entity<SurfaceBounds>,
    color: Hsla,
    colors: &Colors,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement + use<> {
    let hue = hue_fraction(color);
    let handle = surface_focus(PickerSurface::Hue, state, window, cx);
    let border = surface_border(&handle, colors, window);

    let element = div()
        .id("hue")
        .relative()
        .w(HUE_STRIP_WIDTH)
        .h(FACE_SIZE)
        .flex_shrink_0()
        .overflow_hidden()
        .border_1()
        .border_color(border)
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .children((0..HUE_BANDS).map(|band| {
                    let (from, to) = hue_band(band);
                    div().flex_1().bg(linear_gradient(
                        180.0,
                        linear_color_stop(hue_color(from), 0.0),
                        linear_color_stop(hue_color(to), 1.0),
                    )
                    .color_space(ColorSpace::Srgb))
                })),
        )
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(relative(hue))
                .h(HUE_MARKER_HEIGHT)
                .mt(-(HUE_MARKER_HEIGHT / 2.0))
                .bg(colors.readable_on(hue_color(hue))),
        );

    wire_surface(element, PickerSurface::Hue, state, bounds, 1, &handle)
}

/// The alpha rail: a filled surface with no thumb, and the percentage beside it.
fn alpha_rail(
    state: &Entity<ColorPickerState>,
    bounds: &Entity<SurfaceBounds>,
    color: Hsla,
    theme: &RavelTheme,
    density: Density,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement + use<> {
    let colors = &theme.colors;
    let metrics = density.metrics(theme);
    let handle = surface_focus(PickerSurface::Alpha, state, window, cx);
    let border = surface_border(&handle, colors, window);
    let alpha = color.alpha;

    let rail = div()
        .id("alpha")
        .relative()
        .flex_1()
        .h(metrics.height)
        .overflow_hidden()
        .rounded(theme.radius.radius)
        .border_1()
        .border_color(border)
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(relative(alpha))
                .bg(colors.slider_fill()),
        );

    div()
        .flex()
        .items_center()
        .gap(theme.spacing.xs)
        .child(
            div()
                .flex_shrink_0()
                .text_color(colors.muted_foreground)
                .child(SharedString::from(t!("color_picker.alpha"))),
        )
        // `gpui_base`'s slider root carries the accessibility semantics — the
        // slider role, the value, the range and the step — read from the same
        // `SliderState` the picker keeps the alpha in. The presentation and
        // both input roads are this module's.
        .child(
            Slider::new(state.read(cx).sliders().alpha())
                .flex()
                .flex_1()
                .items_center()
                .child(wire_surface(
                    rail,
                    PickerSurface::Alpha,
                    state,
                    bounds,
                    2,
                    &handle,
                )),
        )
        .child(
            div()
                .flex_shrink_0()
                .w(metrics.height * 2.0)
                .text_color(colors.muted_foreground)
                .child(SharedString::from(format!("{:.0}%", alpha * 100.0))),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size};

    fn area() -> Bounds<Pixels> {
        Bounds {
            origin: point(px(10.0), px(20.0)),
            size: size(FACE_SIZE, FACE_SIZE),
        }
    }

    /// The face's two axes, and which arrow owns which. `Left`/`Right` are the
    /// pair the host also binds, so getting them the wrong way round here is
    /// the mistake with two symptoms rather than one.
    #[test]
    fn the_arrows_move_saturation_across_and_brightness_up() {
        let start = face_color(
            0.5,
            FacePoint {
                saturation: 0.5,
                value: 0.5,
            },
            1.0,
        );
        let point = |color: Hsla| face_point(color);

        let right = nudged(start, PickerSurface::Face, Nudge::Right, false).expect("a colour");
        assert!((point(right).saturation - 0.51).abs() < 1e-4);
        assert!(
            (point(right).value - 0.5).abs() < 1e-4,
            "the other axis moved"
        );

        let left = nudged(start, PickerSurface::Face, Nudge::Left, false).expect("a colour");
        assert!((point(left).saturation - 0.49).abs() < 1e-4);

        let up = nudged(start, PickerSurface::Face, Nudge::Up, false).expect("a colour");
        assert!((point(up).value - 0.51).abs() < 1e-4);
        assert!(
            (point(up).saturation - 0.5).abs() < 1e-4,
            "the other axis moved"
        );

        let down = nudged(start, PickerSurface::Face, Nudge::Down, false).expect("a colour");
        assert!((point(down).value - 0.49).abs() < 1e-4);
    }

    /// Shift is ten steps, not two and not a hundred.
    #[test]
    fn shift_makes_the_step_ten_times_the_plain_one() {
        assert_eq!(nudge_step(false), NUDGE_STEP);
        assert_eq!(nudge_step(true), NUDGE_STEP_COARSE);
        assert!((NUDGE_STEP_COARSE - NUDGE_STEP * 10.0).abs() < 1e-6);

        let start = face_color(
            0.5,
            FacePoint {
                saturation: 0.5,
                value: 0.5,
            },
            1.0,
        );
        let coarse = nudged(start, PickerSurface::Face, Nudge::Right, true).expect("a colour");
        assert!((face_point(coarse).saturation - 0.6).abs() < 1e-4);
    }

    /// A surface takes only the arrows its geometry has. The hue strip is
    /// vertical, so `Right` inside it must fall through rather than silently
    /// changing something — and `None` is also what keeps the host's binding
    /// from being shadowed by a surface that does nothing with it.
    #[test]
    fn a_surface_ignores_the_arrows_it_does_not_own() {
        let color = hsla_from_hsv(0.3, 0.6, 0.8, 0.5);
        assert_eq!(nudged(color, PickerSurface::Hue, Nudge::Left, false), None);
        assert_eq!(nudged(color, PickerSurface::Hue, Nudge::Right, false), None);
        assert_eq!(nudged(color, PickerSurface::Alpha, Nudge::Up, false), None);
        assert_eq!(
            nudged(color, PickerSurface::Alpha, Nudge::Down, false),
            None
        );

        assert!(nudged(color, PickerSurface::Hue, Nudge::Up, false).is_some());
        assert!(nudged(color, PickerSurface::Alpha, Nudge::Right, false).is_some());
    }

    /// UX invariant 3 at the axis's end: an arrow held against the edge stops
    /// writing, so the consumer's debounce is never fed a value it already has.
    #[test]
    fn an_axis_already_at_its_end_writes_nothing() {
        let opaque = hsla_from_hsv(0.3, 0.6, 0.8, 1.0);
        assert_eq!(
            nudged(opaque, PickerSurface::Alpha, Nudge::Right, false),
            None
        );

        let white = face_color(
            0.3,
            FacePoint {
                saturation: 0.0,
                value: 1.0,
            },
            1.0,
        );
        assert_eq!(nudged(white, PickerSurface::Face, Nudge::Up, false), None);
        assert_eq!(nudged(white, PickerSurface::Face, Nudge::Left, false), None);
    }

    /// The pointer and the arrows are the same road: land an arrow, then click
    /// exactly where that arrow left the marker, and the colour is identical.
    /// A keyboard path that wrote HSL saturation directly — the obvious
    /// shortcut — passes neither this nor the eye.
    #[test]
    fn the_pointer_writes_what_the_arrow_wrote_for_the_same_point() {
        let start = face_color(
            0.5,
            FacePoint {
                saturation: 0.4,
                value: 0.7,
            },
            0.8,
        );
        let nudged_color = nudged(start, PickerSurface::Face, Nudge::Right, false).expect("colour");
        let landed = face_point(nudged_color);

        let bounds = area();
        let position = point(
            bounds.origin.x + bounds.size.width * landed.saturation,
            bounds.origin.y + bounds.size.height * (1.0 - landed.value),
        );
        let clicked = pointer_color(start, PickerSurface::Face, bounds, position);

        assert!((face_point(clicked).saturation - landed.saturation).abs() < 1e-3);
        assert!((face_point(clicked).value - landed.value).abs() < 1e-3);
        assert_eq!(clicked.alpha, nudged_color.alpha, "alpha is not the face's");
    }

    /// The HSV ↔ HSL pair, including the two places it is easy to lose: white,
    /// where HSL saturation is undefined, and black, where a whole face row
    /// collapses onto one HSL colour.
    #[test]
    fn the_face_round_trips_every_corner() {
        let cases = [
            FacePoint {
                saturation: 0.0,
                value: 0.0,
            },
            FacePoint {
                saturation: 1.0,
                value: 0.0,
            },
            FacePoint {
                saturation: 0.0,
                value: 1.0,
            },
            FacePoint {
                saturation: 1.0,
                value: 1.0,
            },
            FacePoint {
                saturation: 0.5,
                value: 0.5,
            },
            FacePoint {
                saturation: 0.25,
                value: 0.9,
            },
        ];
        for expected in cases {
            let color = face_color(0.35, expected, 1.0);
            let actual = face_point(color);
            assert!(
                (actual.saturation - expected.saturation).abs() < 1e-4
                    && (actual.value - expected.value).abs() < 1e-4,
                "{expected:?} came back as {actual:?}"
            );
        }
    }

    /// The face is HSV, and this is the assertion that says so: the top-right
    /// corner is the fully saturated hue, which in HSL is lightness 0.5. A
    /// face that used HSL lightness for its vertical axis would put white
    /// there and the marker would sit on a colour the face is not painting.
    #[test]
    fn the_bright_saturated_corner_is_the_pure_hue() {
        let corner = face_color(
            0.0,
            FacePoint {
                saturation: 1.0,
                value: 1.0,
            },
            1.0,
        );
        assert!((corner.saturation - 1.0).abs() < 1e-5);
        assert!((corner.lightness - 0.5).abs() < 1e-5);

        let top_left = face_color(
            0.0,
            FacePoint {
                saturation: 0.0,
                value: 1.0,
            },
            1.0,
        );
        assert!((top_left.lightness - 1.0).abs() < 1e-5, "white corner");
    }

    /// The pointer maps onto the face the way the face is painted: white on
    /// the left, bright at the top.
    #[test]
    fn the_pointer_maps_onto_the_painted_axes() {
        let bounds = area();
        let corner = |x: f32, y: f32| face_point_at(bounds, point(px(x), px(y)));

        let top_left = corner(10.0, 20.0);
        assert!(top_left.saturation.abs() < 1e-5 && (top_left.value - 1.0).abs() < 1e-5);

        let bottom_right = corner(130.0, 140.0);
        assert!((bottom_right.saturation - 1.0).abs() < 1e-5 && bottom_right.value.abs() < 1e-5);

        // Outside the surface clamps rather than wrapping: a drag that leaves
        // the face keeps writing the edge it left by.
        let outside = corner(-500.0, 5000.0);
        assert_eq!(outside.saturation, 0.0);
        assert_eq!(outside.value, 0.0);
    }

    /// Six bands, meeting at the sixths, covering the whole circle. Fewer
    /// bands or a gap between two of them is a hue the strip cannot reach.
    #[test]
    fn the_hue_strip_is_six_bands_covering_the_circle() {
        assert_eq!(HUE_BANDS, 6);
        assert_eq!(hue_band(0).0, 0.0);
        assert!((hue_band(HUE_BANDS - 1).1 - 1.0).abs() < 1e-6);
        for band in 1..HUE_BANDS {
            assert!(
                (hue_band(band).0 - hue_band(band - 1).1).abs() < 1e-6,
                "band {band} does not start where {} ended",
                band - 1
            );
        }
    }

    /// A translucent colour has to be *seen* to be translucent: the slash
    /// underlay is under it and the colour keeps its own alpha. Flattening the
    /// swatch onto its ground is the bug — "black" and "transparent" then look
    /// alike, which in a compositor is the one confusion that matters.
    #[test]
    fn a_translucent_swatch_keeps_its_alpha_over_the_slash_underlay() {
        let colors = Colors::light();
        let translucent = hsla_from_hsv(0.6, 0.7, 0.8, 0.4);
        let layers = swatch_layers(Some(translucent), &colors);

        assert_eq!(
            layers.underlay,
            pattern_slash(colors.border, SLASH_WIDTH, SLASH_INTERVAL),
            "the swatch lost the underlay a translucent colour shows through"
        );
        assert_eq!(
            layers.color.expect("a colour").alpha,
            0.4,
            "the swatch flattened the alpha away"
        );
        assert_eq!(layers.ground, colors.background);

        // No value yet: the underlay still paints, so an empty swatch reads as
        // empty rather than as black.
        let empty = swatch_layers(None, &colors);
        assert_eq!(empty.color, None);
        assert_eq!(empty.underlay, layers.underlay);
    }

    // -----------------------------------------------------------------------
    // What only a window can answer: that Tab reaches the face, and that an
    // arrow press there travels the same road a drag does — out through
    // `ColorPickerEvent::Change`, which is where the consumer's debounce is.
    // -----------------------------------------------------------------------

    use gpui::{Context, Entity, Modifiers, Point, Render, TestAppContext, VisualTestContext};
    use gpui_base::{ColorPickerEvent, ColorPickerState};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// The middle of the face, as the harness below lays the popup out.
    const FACE_CENTRE: Point<Pixels> = Point {
        x: px(70.0),
        y: px(94.0),
    };

    /// A seed whose face point is `(0.5, 0.8)`, so 25 arrow presses fit along
    /// the horizontal axis without the clamp taking part.
    fn seed() -> Hsla {
        hsla_from_hsv(0.5, 0.5, 0.8, 1.0)
    }

    struct PickerHarness {
        state: Entity<ColorPickerState>,
    }

    impl Render for PickerHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("harness")
                .tab_group()
                .size(px(400.0))
                .child(ColorPicker::new(&self.state))
        }
    }

    /// An open picker with the face focused, and every `Change` it emits.
    fn open_picker(
        cx: &mut TestAppContext,
    ) -> (
        &mut VisualTestContext,
        Entity<ColorPickerState>,
        Rc<RefCell<Vec<Hsla>>>,
    ) {
        cx.update(gpui_base::init);
        let (view, cx) = cx.add_window_view(|window, cx| PickerHarness {
            state: cx.new(|cx| ColorPickerState::new(window, cx).default_value(seed())),
        });
        let state = view.read_with(cx, |harness, _| harness.state.clone());

        let changes: Rc<RefCell<Vec<Hsla>>> = Rc::default();
        cx.update(|_, cx| {
            let seen = changes.clone();
            cx.subscribe(&state, move |_, event: &ColorPickerEvent, _| {
                let ColorPickerEvent::Change(color) = event;
                if let Some(color) = color {
                    seen.borrow_mut().push(*color);
                }
            })
            .detach();
        });

        cx.update(|window, cx| {
            state.update(cx, |state, cx| state.set_open(true, cx));
            window.draw(cx).clear(cx);
        });
        // The popover takes focus when it opens; the first Tab stop inside it
        // is the face, which is the plan's "Tab into the face" in one step.
        cx.update(|window, cx| window.focus_next(cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        changes.borrow_mut().clear();

        (cx, state, changes)
    }

    /// Press a key **through the keymap**, the way a real keystroke arrives.
    ///
    /// Not `simulate_event(KeyDownEvent)`: that hands the event straight to
    /// the listeners and skips binding resolution, so it cannot see a binding
    /// swallowing the key — which is exactly the failure the running
    /// application showed.
    fn press(cx: &mut VisualTestContext, key: &str) {
        cx.simulate_keystrokes(key);
    }

    /// **The keyboard half of the one value road.** A face that wrote its
    /// slider states directly — the obvious shortcut, since the state hands
    /// them over — would move the colour on screen and emit nothing, so the
    /// consumer's debounce would never see the gesture and the edit would fold
    /// into whatever undo step came next. The assertion is the event.
    #[gpui::test]
    fn an_arrow_on_the_face_emits_the_change_a_drag_would(cx: &mut TestAppContext) {
        let (cx, state, changes) = open_picker(cx);

        press(cx, "right");

        let emitted = changes.borrow().clone();
        assert_eq!(
            emitted.len(),
            1,
            "one arrow press must emit exactly one Change: {emitted:?}"
        );
        let value = state
            .read_with(cx, |state, _| state.value())
            .expect("a value");
        assert_eq!(emitted[0], value, "the event carried the committed colour");

        let moved = face_point(value);
        let before = face_point(seed());
        assert!(
            (moved.saturation - (before.saturation + NUDGE_STEP)).abs() < 1e-4,
            "saturation went from {} to {}",
            before.saturation,
            moved.saturation
        );
        assert!(
            (moved.value - before.value).abs() < 1e-4,
            "the vertical axis moved too"
        );
        assert!(
            (hue_fraction(value) - hue_fraction(seed())).abs() < 1e-4,
            "the face changed the hue"
        );
    }

    /// Shift on the same key, through the same handler: ten steps, one event.
    #[gpui::test]
    fn shift_and_an_arrow_move_ten_steps_in_one_event(cx: &mut TestAppContext) {
        let (cx, state, changes) = open_picker(cx);

        press(cx, "shift-right");

        assert_eq!(changes.borrow().len(), 1, "one press, one Change");
        let value = state
            .read_with(cx, |state, _| state.value())
            .expect("a value");
        assert!(
            (face_point(value).saturation - (face_point(seed()).saturation + NUDGE_STEP_COARSE))
                .abs()
                < 1e-4
        );
    }

    /// **The pointer half of the road, and the click-to-focus that goes with
    /// it.** Clicking a surface must both write a colour *and* leave that
    /// surface holding the keyboard, so the obvious gesture — click roughly,
    /// then nudge — works. It was broken once already: the surface's own
    /// mouse-down handler called `stop_propagation`, which silences gpui's
    /// focus-on-mouse-down (registered first, dispatched last), so a clicked
    /// face picked a colour and then ignored every arrow.
    ///
    /// The coordinate is the face's centre as this harness lays it out: the
    /// picker sits at the window's top-left corner and the popover anchors
    /// under its trigger, so it is stable, and a layout change that moved the
    /// face would fail here rather than silently stop testing anything —
    /// which is what the first assertion is for.
    #[gpui::test]
    fn clicking_a_surface_writes_a_colour_and_takes_the_keyboard(cx: &mut TestAppContext) {
        let (cx, state, changes) = open_picker(cx);

        cx.simulate_click(FACE_CENTRE, Modifiers::default());

        assert_eq!(
            changes.borrow().len(),
            1,
            "the click missed the face: no Change was emitted"
        );
        let clicked = state
            .read_with(cx, |state, _| state.value())
            .expect("a value");
        let point = face_point(clicked);
        assert!(
            (point.saturation - 0.5).abs() < 0.2 && (point.value - 0.5).abs() < 0.2,
            "the click landed off the middle of the face: {point:?}"
        );

        // And the arrows now work without a Tab, which is the half that the
        // `stop_propagation` regression broke.
        press(cx, "right");
        assert_eq!(changes.borrow().len(), 2, "the clicked face has no focus");
        assert!(
            face_point(
                state
                    .read_with(cx, |state, _| state.value())
                    .expect("a value")
            )
            .saturation
                > point.saturation
        );
    }

    /// A held arrow is what the consumer's debounce has to survive: 25 presses
    /// are 25 `Change`s, every one carrying the value already applied, which
    /// is exactly the shape a slider drag has. Coalescing them into one undo
    /// step is the consumer's job and this pins the input to it.
    #[gpui::test]
    fn a_held_arrow_emits_one_change_per_press_and_stops_at_the_edge(cx: &mut TestAppContext) {
        let (cx, state, changes) = open_picker(cx);

        for _ in 0..25 {
            press(cx, "right");
        }

        assert_eq!(
            changes.borrow().len(),
            25,
            "a held arrow must keep writing while the axis has room"
        );
        let value = state
            .read_with(cx, |state, _| state.value())
            .expect("a value");
        assert!(
            (face_point(value).saturation - 0.75).abs() < 1e-3,
            "25 presses of 1% from 0.5 landed on {}",
            face_point(value).saturation
        );

        // Drive it into the edge, then keep pressing: against the end of its
        // axis a surface goes quiet rather than re-emitting the value it
        // already has (UX invariant 3), so the consumer's quiet period can
        // actually end while the key is still down.
        for _ in 0..5 {
            press(cx, "shift-right");
        }
        let value = state
            .read_with(cx, |state, _| state.value())
            .expect("a value");
        assert!(
            (face_point(value).saturation - 1.0).abs() < 1e-4,
            "the axis did not reach its end"
        );

        changes.borrow_mut().clear();
        for _ in 0..5 {
            press(cx, "right");
        }
        assert!(
            changes.borrow().is_empty(),
            "the edge kept emitting: {:?}",
            changes.borrow()
        );
    }

    /// **The regression this closes.** `Interactivity::on_drag_move` fires in
    /// the capture phase for every painted listener of the payload's type
    /// *without consulting its hitbox*, so a drag on one surface reached the
    /// other two as well. All three read the same `ColorPickerState`, so
    /// naming only the picker in the payload told them apart from another
    /// picker's drag and not from each other — and each one then mapped a
    /// pointer that was never on it onto its own axes. Dragging the alpha rail
    /// was what the user saw: below the face, brightness clamps to zero, so
    /// the marker pinned itself to the bottom edge while they changed opacity.
    ///
    /// The drag under test is the **hue strip's**, because the alpha rail's
    /// rectangle is not usable from a test: no locale is loaded, so its label
    /// is the raw `color_picker.alpha` key, which is wide enough to push the
    /// `flex_1` rail down to 2px and out past the popup's clip. The strip is
    /// the same mechanism from the other side — its own axis must move, and
    /// the face's two and the alpha must not.
    #[gpui::test]
    fn a_drag_on_one_surface_moves_nothing_but_its_own_axis(cx: &mut TestAppContext) {
        let (cx, state, _changes) = open_picker(cx);
        let value = |cx: &mut VisualTestContext| {
            state
                .read_with(cx, |state, _| state.value())
                .expect("a value")
        };
        let before = value(cx);

        let strip = cx
            .debug_bounds(surface_name(PickerSurface::Hue))
            .expect("the hue strip is drawn");
        let start = strip.center();
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        // The first move is the one that starts the drag: the capture-phase
        // handler runs before `active_drag` is set, so it writes nothing. The
        // second is the move a user would notice.
        let mut at = start;
        for offset in [4.0, 30.0] {
            at = Point {
                x: start.x,
                y: start.y - px(offset),
            };
            cx.simulate_mouse_move(at, Some(MouseButton::Left), Modifiers::none());
        }
        cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();

        let after = value(cx);
        assert!(
            (hue_fraction(after) - hue_fraction(before)).abs() > 1e-3,
            "the drag never reached the hue strip: {} to {}",
            hue_fraction(before),
            hue_fraction(after)
        );
        let (moved, held) = (face_point(after), face_point(before));
        assert!(
            (moved.saturation - held.saturation).abs() < 1e-4
                && (moved.value - held.value).abs() < 1e-4,
            "the hue drag moved the face: {held:?} to {moved:?}"
        );
        assert_eq!(
            after.alpha, before.alpha,
            "the hue drag moved the alpha rail"
        );
    }

    /// One rule, both palettes: nothing here asks which mode is in force.
    #[test]
    fn both_palettes_take_their_colours_from_their_own_tokens() {
        for colors in [Colors::light(), Colors::dark()] {
            let layers = swatch_layers(Some(gpui::red()), &colors);
            assert_eq!(layers.ground, colors.background);
            assert_eq!(
                layers.underlay,
                pattern_slash(colors.border, SLASH_WIDTH, SLASH_INTERVAL)
            );
        }
    }
}
