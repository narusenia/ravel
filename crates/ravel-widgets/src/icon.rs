// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ravel's icon.
//!
//! An icon is an SVG at a size, and that is the whole of it: no state, no
//! interaction, no theme of its own. It takes the ambient text colour unless
//! the caller names one, so an icon inside a [`Button`](crate::button::Button)
//! is the same colour as the label beside it without either of them saying so.
//!
//! **Ravel names its own icons.** [`IconPath`] is the trait an icon-name enum
//! implements, and `ravel-app`'s `RavelIcon` is the one that matters; this
//! crate deliberately depends on no icon *asset* crate, because which files
//! exist is a property of the host's `AssetSource`, not of the widget layer.

use gpui::{
    App, Hsla, IntoElement, Pixels, RenderOnce, SharedString, StyleRefinement, Styled, Svg, Window,
    svg,
};

use crate::tokens::{DEFAULT_ICON_SIZE, Density};

/// An icon name that knows which file it is.
///
/// The path is relative to the asset source (`icons/chevron-down.svg`), which
/// is what makes one name work in the application, in `examples/gallery`, and
/// in a test that installs neither.
pub trait IconPath {
    /// The asset path of this icon.
    fn icon_path(self) -> SharedString;
}

/// The shape-named glyphs: chevrons, a plus, an ellipsis.
///
/// `ravel-app`'s `RavelIcon` names icons after what they *mean* in Ravel —
/// `NodeSubnet`, `SafeAreas` — because that is what a panel asks for. Most of
/// these are the opposite: a widget needs "the chevron that points down" with
/// no opinion about why, and naming them `PropertiesGroupExpanded` would invent
/// meaning the drawing does not have.
///
/// The four exceptions are the ones the parameter editors in this crate draw
/// themselves — the three interpolation modes and the fit-to-view action. They
/// carry meaning because the widget that draws them carries the meaning too:
/// [`crate::param_curve_editor`] *is* where a keyframe's interpolation is
/// chosen, so there is no host to ask.
///
/// They are the set the borrowed `gpui_component::IconName` was used for, and
/// **no new SVG rides along**: every path here already resolves through
/// `ravel-app`'s `RavelAssets`, from Ravel's own `assets/icons/` where it has
/// one and from `gpui-kit-assets` (Apache-2.0, compatible with Ravel's
/// `Apache-2.0 OR MIT`) otherwise. Naming a path is not depending on the
/// bytes: which files exist stays the host `AssetSource`'s business, and
/// `ravel-app`'s `every_ui_glyph_resolves_through_the_asset_source` is what
/// holds the two ends together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiIcon {
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    ChevronUp,
    Copy,
    Delete,
    Ellipsis,
    ExternalLink,
    FolderClosed,
    Frame,
    InterpolationBezier,
    InterpolationLinear,
    InterpolationStep,
    Network,
    Plus,
    Settings,
    TriangleAlert,
    ZoomFit,
}

impl UiIcon {
    /// Every glyph, for the tests and the gallery.
    pub const ALL: [Self; 18] = [
        Self::ChevronDown,
        Self::ChevronLeft,
        Self::ChevronRight,
        Self::ChevronUp,
        Self::Copy,
        Self::Delete,
        Self::Ellipsis,
        Self::ExternalLink,
        Self::FolderClosed,
        Self::Frame,
        Self::InterpolationBezier,
        Self::InterpolationLinear,
        Self::InterpolationStep,
        Self::Network,
        Self::Plus,
        Self::Settings,
        Self::TriangleAlert,
        Self::ZoomFit,
    ];

    /// The asset path of this glyph.
    pub fn icon_path(self) -> SharedString {
        match self {
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::ChevronLeft => "icons/chevron-left.svg",
            Self::ChevronRight => "icons/chevron-right.svg",
            Self::ChevronUp => "icons/chevron-up.svg",
            Self::Copy => "icons/copy.svg",
            Self::Delete => "icons/delete.svg",
            Self::Ellipsis => "icons/ellipsis.svg",
            Self::ExternalLink => "icons/external-link.svg",
            Self::FolderClosed => "icons/folder-closed.svg",
            Self::Frame => "icons/frame.svg",
            Self::InterpolationBezier => "icons/interpolation-bezier.svg",
            Self::InterpolationLinear => "icons/interpolation-linear.svg",
            Self::InterpolationStep => "icons/interpolation-step.svg",
            Self::Network => "icons/network.svg",
            Self::Plus => "icons/plus.svg",
            Self::Settings => "icons/settings.svg",
            Self::TriangleAlert => "icons/triangle-alert.svg",
            Self::ZoomFit => "icons/maximize.svg",
        }
        .into()
    }
}

impl IconPath for UiIcon {
    fn icon_path(self) -> SharedString {
        UiIcon::icon_path(self)
    }
}

/// An SVG icon at a fixed size.
#[derive(IntoElement)]
pub struct Icon {
    base: Svg,
    path: SharedString,
}

impl Icon {
    /// The icon `name` refers to, at the default density's size.
    pub fn new(icon: impl Into<Icon>) -> Self {
        icon.into()
    }

    /// An icon at an asset path this crate has no name for.
    pub fn from_path(path: impl Into<SharedString>) -> Self {
        Self {
            base: svg(),
            path: path.into(),
        }
    }

    /// Draw the icon at the size of one density step.
    pub fn density(self, density: Density) -> Self {
        self.size(density.icon_size())
    }

    /// Draw the icon at an explicit size.
    ///
    /// Shadows [`Styled::size`] for a `Pixels` argument, which is the only
    /// thing that ever makes sense for an icon; `size_4()` and friends still
    /// reach the style refinement and still win over the density default.
    pub fn size(mut self, size: Pixels) -> Self {
        self.base = Styled::size(self.base, size);
        self
    }
}

impl<T: IconPath> From<T> for Icon {
    fn from(name: T) -> Self {
        Self::from_path(name.icon_path())
    }
}

impl Styled for Icon {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl Icon {
    /// The `Svg` this icon paints as, given the text colour in force around
    /// it.
    ///
    /// `ambient` is not a nicety: `Svg::paint` draws nothing at all when the
    /// computed style carries no text colour — the `zip` in
    /// `gpui/src/elements/svg.rs` — and it does not inherit the surrounding
    /// text style. Without the fallback an icon nobody coloured is invisible
    /// rather than merely off-palette, which is why this is a named step with
    /// a test on it instead of a line inside `render`.
    fn into_svg(mut self, ambient: Hsla) -> Svg {
        let color = self.style().text.color.unwrap_or(ambient);
        let sized = self.style().size.width.is_some() || self.style().size.height.is_some();

        let mut base = self.base.flex_none().text_color(color);
        if !sized {
            base = Styled::size(base, DEFAULT_ICON_SIZE);
        }
        base.path(self.path)
    }
}

impl RenderOnce for Icon {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.into_svg(window.text_style().color)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::COMPACT_ICON_SIZE;
    use gpui::{Length, px, rems};

    #[derive(Clone, Copy)]
    enum TestIcon {
        ChevronDown,
    }

    impl IconPath for TestIcon {
        fn icon_path(self) -> SharedString {
            match self {
                Self::ChevronDown => "icons/chevron-down.svg".into(),
            }
        }
    }

    fn width_of(mut icon: Icon) -> Option<Length> {
        icon.style().size.width
    }

    fn ambient() -> Hsla {
        crate::tokens::Colors::light().muted_foreground
    }

    #[test]
    fn a_name_becomes_its_asset_path() {
        assert_eq!(
            Icon::new(TestIcon::ChevronDown).path,
            "icons/chevron-down.svg"
        );
        assert_eq!(Icon::from_path("icons/plus.svg").path, "icons/plus.svg");
        // Nothing is decided until it is drawn: an icon that was given no size
        // carries none, so the density default can still fill it in.
        assert_eq!(width_of(Icon::new(TestIcon::ChevronDown)), None);
    }

    #[test]
    fn density_and_an_explicit_size_both_win_over_the_default() {
        assert_eq!(
            width_of(Icon::new(TestIcon::ChevronDown).density(Density::Compact)),
            Some(COMPACT_ICON_SIZE.into()),
        );
        assert_eq!(
            width_of(Icon::new(TestIcon::ChevronDown).density(Density::Default)),
            Some(DEFAULT_ICON_SIZE.into()),
        );
        assert_eq!(
            width_of(Icon::new(TestIcon::ChevronDown).size(px(9.0))),
            Some(px(9.0).into()),
        );
        // And `Styled`'s own setters are not shadowed away: a call site that
        // already said `.size_3()` keeps its own 0.75rem rather than being
        // silently replaced by the density default.
        assert_eq!(
            width_of(Icon::new(TestIcon::ChevronDown).size_3()),
            Some(rems(0.75).into()),
        );
    }

    /// The regression this guards is a blank icon, not a wrong colour: drop the
    /// fallback and `Svg::paint` skips the draw entirely.
    #[test]
    fn an_uncoloured_icon_takes_the_ambient_text_colour() {
        let mut painted = Icon::new(TestIcon::ChevronDown).into_svg(ambient());
        assert_eq!(painted.style().text.color, Some(ambient()));
        assert_eq!(painted.style().size.width, Some(DEFAULT_ICON_SIZE.into()));
    }

    #[test]
    fn an_explicit_colour_survives_the_fallback() {
        let named = crate::tokens::Colors::light().primary;
        let mut painted = Icon::new(TestIcon::ChevronDown)
            .text_color(named)
            .into_svg(ambient());
        assert_eq!(painted.style().text.color, Some(named));
    }
}
