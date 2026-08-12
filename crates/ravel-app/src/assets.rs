// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Embedded asset source: Ravel's own icons with a fallback to the
//! gpui-component icon set.
//!
//! Ravel icons are vendored Lucide SVGs (ISC licensed, see
//! `assets/icons/LICENSE`) and small project-specific glyphs under
//! `assets/icons/`. Only icons that are actually used are embedded. The
//! `ui-design-impl` skill documents the vendoring procedure.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};
use gpui_component::IconNamed;
use ravel_core::registry::NodeCategory;
use ravel_ui::panel::PanelKind;
use rust_embed::RustEmbed;

/// Ravel-vendored icons, embedded at compile time.
#[derive(RustEmbed)]
#[folder = "../../assets"]
#[include = "icons/**/*.svg"]
struct RavelEmbed;

/// Serves Ravel icons first, then falls back to the gpui-component asset set
/// so built-in widget icons (chevrons, checks, …) resolve too.
pub struct RavelAssets;

impl AssetSource for RavelAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(file) = RavelEmbed::get(path) {
            return Ok(Some(file.data));
        }
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut entries: Vec<SharedString> = RavelEmbed::iter()
            .filter(|p| p.starts_with(path))
            .map(|p| p.to_string().into())
            .collect();
        entries.extend(gpui_component_assets::Assets.list(path)?);
        Ok(entries)
    }
}

/// Ravel-specific icons (vendored Lucide icons and project glyphs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RavelIcon {
    Outliner,
    NodeGraph,
    Timeline,
    Viewer,
    Dopesheet,
    Properties,
    MediaBin,
    CurveEditor,
    Waveform,
    Vectorscope,
    Histogram,
    Parade,
    TextEditor,
    ShaderEditor,
    LuaConsole,
    RenderQueue,
    /// Viewer toolbar: fit the composition to the panel.
    ZoomFit,
    /// Viewer toolbar: 100% (one comp pixel per screen pixel).
    ZoomActualSize,
    /// Viewer toolbar: proportional grid overlay.
    GridOverlay,
    /// Viewer toolbar: action/title safe-area overlay.
    SafeAreas,
    /// Timeline transport: jump to the first frame.
    SkipBack,
    /// Timeline transport: step one frame backward.
    StepBack,
    /// Timeline transport: start playback.
    Play,
    /// Timeline transport: pause playback.
    Pause,
    /// Timeline transport: stop playback.
    Stop,
    /// Timeline transport: step one frame forward.
    StepForward,
    /// Timeline transport: jump to the final frame.
    SkipForward,
    /// Timeline zoom: fit the entire composition duration.
    TimelineFit,
    /// Timeline view switcher: layer bar view.
    TimelineBars,
    /// Curve graph: Bezier interpolation.
    InterpolationBezier,
    /// Curve graph: linear interpolation.
    InterpolationLinear,
    /// Curve graph: step interpolation.
    InterpolationStep,
    /// Keyframe toggle: no key at the current frame (hollow ◇).
    Diamond,
    /// Keyframe toggle: a key sits at the current frame (filled ◆).
    DiamondFilled,
    /// Port toggle: parameter not exposed as an input port (○).
    Circle,
    /// Port toggle: parameter exposed, no connection (◎).
    CircleDot,
    /// Port toggle: parameter exposed and driven by an edge (●).
    CircleFilled,
    /// Declaration toggle: parameter not part of the project's external
    /// contract (□).
    Square,
    /// Declaration toggle: parameter declared as a project input (■).
    SquareFilled,
    /// Tool toolbar: select tool (V).
    ToolSelect,
    /// Tool toolbar: pen tool (P).
    ToolPen,
    /// Tool toolbar: rectangle tool (R).
    ToolRect,
    /// Tool toolbar: ellipse tool (E).
    ToolEllipse,
    /// Tool toolbar: hand / pan tool (H).
    ToolHand,
    /// Tool toolbar: zoom tool (Z).
    ToolZoom,
    /// MediaBin row fallback: a still-image asset.
    MediaStill,
    /// Detached window title bar: keep the window above the others.
    AlwaysOnTop,
    /// Node header/menu: `constant`.
    NodeConstant,
    /// Node header/menu: `constant.color`.
    NodeConstantColor,
    /// Node header/menu: `constant.vec2`.
    NodeConstantVec2,
    /// Node header/menu: `constant.vec3`.
    NodeConstantVec3,
    /// Node header/menu: `constant.vec4`.
    NodeConstantVec4,
    /// Node header/menu: `media`.
    NodeMedia,
    /// Node header/menu: `layer.ref`.
    NodeLayerRef,
    /// Node header/menu: `subnet`.
    NodeSubnet,
    /// Node header/menu: `merge`.
    NodeMerge,
    /// Node header/menu: `math.scalar`.
    NodeMathScalar,
    /// Node header/menu: `math.remap`.
    NodeMathRemap,
    /// Node header/menu: `math.curve`.
    NodeMathCurve,
    /// Node header/menu: `vector.construct.vec2`.
    NodeVectorConstructVec2,
    /// Node header/menu: `vector.construct.vec3`.
    NodeVectorConstructVec3,
    /// Node header/menu: `vector.construct.vec4`.
    NodeVectorConstructVec4,
    /// Node header/menu: `geometry.transform`.
    NodeGeometryTransform,
    /// Node header/menu: `geometry.merge`.
    NodeGeometryMerge,
    /// Node header/menu: `geometry.connect`.
    NodeGeometryConnect,
    /// Node header/menu: `scene.add`.
    NodeSceneAdd,
    /// Node header/menu: `scene.merge`.
    NodeSceneMerge,
    /// Node header/menu: `scene.camera`.
    NodeSceneCamera,
    /// Node header/menu: `blur`.
    NodeBlur,
    /// Node header/menu: `transform`.
    NodeTransform,
    /// Node header/menu: `color_correct`.
    NodeColorCorrect,
    /// Node header/menu: `rasterize`.
    NodeRasterize,
    /// Node header/menu: `shape.rect`.
    NodeShapeRect,
    /// Node header/menu: `shape.ellipse`.
    NodeShapeEllipse,
    /// Node header/menu: `shape.polygon`.
    NodeShapePolygon,
    /// Node header/menu: `shape.star`.
    NodeShapeStar,
    /// Node header/menu: `shape.line`.
    NodeShapeLine,
    /// Node header/menu: `shape.grid`.
    NodeShapeGrid,
    /// Node header/menu: `shape.custom_path`.
    NodeShapeCustomPath,
    /// Node header/menu: `scatter.grid`.
    NodeScatterGrid,
    /// Node header/menu: `scatter.circular`.
    NodeScatterCircular,
    /// Node header/menu: `scatter.path_array`.
    NodeScatterPathArray,
    /// Node header/menu: `scatter.scatter`.
    NodeScatterScatter,
    /// Node header/menu: `attribute.set`.
    NodeAttributeSet,
    /// Node header/menu: `attribute.promote`.
    NodeAttributePromote,
    /// Node header/menu: `attribute.transfer`.
    NodeAttributeTransfer,
    /// Node header/menu: `attribute.path_sample`.
    NodeAttributePathSample,
    /// Node header/menu: `attribute.curveu`.
    NodeAttributeCurveU,
    /// Node header/menu: `field.noise`.
    NodeFieldNoise,
    /// Node header/menu: `field.falloff`.
    NodeFieldFalloff,
    /// Node header/menu: `field.curve_remap`.
    NodeFieldCurveRemap,
    /// Node header/menu: `field.expression`.
    NodeFieldExpression,
    /// A parameter driven by an expression (Properties row badge). Shares the
    /// `field.expression` glyph: both mean "this value comes from a formula".
    Expression,
    /// Node header/menu: `field.add`.
    NodeFieldAdd,
    /// Node header/menu: `field.multiply`.
    NodeFieldMultiply,
    /// Node header/menu: `field.max`.
    NodeFieldMax,
    /// Node header/menu: `field.blend`.
    NodeFieldBlend,
    /// Node header/menu: `field.attribute`.
    NodeFieldAttribute,
    /// Node header/menu: `field.apply`.
    NodeFieldApply,
    /// Category fallback: `NodeCategory::Geometry`.
    CategoryGeometry,
    /// Category fallback: `NodeCategory::Scene`.
    CategoryScene,
    /// Category fallback: `NodeCategory::Field`.
    CategoryField,
    /// Category fallback: `NodeCategory::Image`.
    CategoryImage,
    /// Category fallback: `NodeCategory::Color`.
    CategoryColor,
    /// Category fallback: `NodeCategory::Time`.
    CategoryTime,
    /// Category fallback: `NodeCategory::Utility`.
    CategoryUtility,
}

impl RavelIcon {
    pub fn for_panel(kind: PanelKind) -> Self {
        match kind {
            PanelKind::Outliner => Self::Outliner,
            PanelKind::NodeGraph => Self::NodeGraph,
            PanelKind::Timeline => Self::Timeline,
            PanelKind::Viewer => Self::Viewer,
            PanelKind::Dopesheet => Self::Dopesheet,
            PanelKind::Properties => Self::Properties,
            PanelKind::MediaBin => Self::MediaBin,
            PanelKind::CurveEditor => Self::CurveEditor,
            PanelKind::Waveform => Self::Waveform,
            PanelKind::Vectorscope => Self::Vectorscope,
            PanelKind::Histogram => Self::Histogram,
            PanelKind::Parade => Self::Parade,
            PanelKind::TextEditor => Self::TextEditor,
            PanelKind::ShaderEditor => Self::ShaderEditor,
            PanelKind::LuaConsole => Self::LuaConsole,
            PanelKind::RenderQueue => Self::RenderQueue,
        }
    }

    pub fn for_tool(tool: ravel_ui::ToolKind) -> Self {
        match tool {
            ravel_ui::ToolKind::Select => Self::ToolSelect,
            ravel_ui::ToolKind::Pen => Self::ToolPen,
            ravel_ui::ToolKind::Rect => Self::ToolRect,
            ravel_ui::ToolKind::Ellipse => Self::ToolEllipse,
            ravel_ui::ToolKind::Hand => Self::ToolHand,
            ravel_ui::ToolKind::Zoom => Self::ToolZoom,
        }
    }

    /// Default icon of a node category; the fallback for node types without
    /// their own entry in [`RavelIcon::for_node_type`].
    pub fn for_category(category: NodeCategory) -> Self {
        match category {
            NodeCategory::Geometry => Self::CategoryGeometry,
            NodeCategory::Scene => Self::CategoryScene,
            NodeCategory::Field => Self::CategoryField,
            NodeCategory::Image => Self::CategoryImage,
            NodeCategory::Color => Self::CategoryColor,
            NodeCategory::Time => Self::CategoryTime,
            NodeCategory::Utility => Self::CategoryUtility,
        }
    }

    /// Icon of a node type, keyed on its template `type_key`.
    ///
    /// The category is passed in rather than derived from the `type_key`
    /// prefix because the taxonomy does not follow the prefixes (`blur`,
    /// `transform`, and `color_correct` are unprefixed Image/Color nodes and
    /// `layer.ref` is Utility): every caller — the header painter, the
    /// add-node menus, the Outliner — already holds the template's category.
    /// An unknown `type_key` (user-defined or future node) falls back to its
    /// category's default icon, or to the generic node icon when the category
    /// is unknown too, so a missing entry never breaks the build or the
    /// drawing.
    pub fn for_node_type(type_key: &str, category: Option<NodeCategory>) -> Self {
        match type_key {
            "constant" => Self::NodeConstant,
            "constant.color" => Self::NodeConstantColor,
            "constant.vec2" => Self::NodeConstantVec2,
            "constant.vec3" => Self::NodeConstantVec3,
            "constant.vec4" => Self::NodeConstantVec4,
            "media" => Self::NodeMedia,
            "layer.ref" => Self::NodeLayerRef,
            "subnet" => Self::NodeSubnet,
            "merge" => Self::NodeMerge,
            "math.scalar" => Self::NodeMathScalar,
            "math.remap" => Self::NodeMathRemap,
            "math.curve" => Self::NodeMathCurve,
            "vector.construct.vec2" => Self::NodeVectorConstructVec2,
            "vector.construct.vec3" => Self::NodeVectorConstructVec3,
            "vector.construct.vec4" => Self::NodeVectorConstructVec4,
            "geometry.transform" => Self::NodeGeometryTransform,
            "geometry.merge" => Self::NodeGeometryMerge,
            "geometry.connect" => Self::NodeGeometryConnect,
            "scene.add" => Self::NodeSceneAdd,
            "scene.merge" => Self::NodeSceneMerge,
            "scene.camera" => Self::NodeSceneCamera,
            "blur" => Self::NodeBlur,
            "transform" => Self::NodeTransform,
            "color_correct" => Self::NodeColorCorrect,
            "rasterize" => Self::NodeRasterize,
            "shape.rect" => Self::NodeShapeRect,
            "shape.ellipse" => Self::NodeShapeEllipse,
            "shape.polygon" => Self::NodeShapePolygon,
            "shape.star" => Self::NodeShapeStar,
            "shape.line" => Self::NodeShapeLine,
            "shape.grid" => Self::NodeShapeGrid,
            "shape.custom_path" => Self::NodeShapeCustomPath,
            "scatter.grid" => Self::NodeScatterGrid,
            "scatter.circular" => Self::NodeScatterCircular,
            "scatter.path_array" => Self::NodeScatterPathArray,
            "scatter.scatter" => Self::NodeScatterScatter,
            "attribute.set" => Self::NodeAttributeSet,
            "attribute.promote" => Self::NodeAttributePromote,
            "attribute.transfer" => Self::NodeAttributeTransfer,
            "attribute.path_sample" => Self::NodeAttributePathSample,
            "attribute.curveu" => Self::NodeAttributeCurveU,
            "field.noise" => Self::NodeFieldNoise,
            "field.falloff" => Self::NodeFieldFalloff,
            "field.curve_remap" => Self::NodeFieldCurveRemap,
            "field.expression" => Self::NodeFieldExpression,
            "field.add" => Self::NodeFieldAdd,
            "field.multiply" => Self::NodeFieldMultiply,
            "field.max" => Self::NodeFieldMax,
            "field.blend" => Self::NodeFieldBlend,
            "field.attribute" => Self::NodeFieldAttribute,
            "field.apply" => Self::NodeFieldApply,
            _ => category.map(Self::for_category).unwrap_or(Self::NodeGraph),
        }
    }
}

impl IconNamed for RavelIcon {
    fn path(self) -> SharedString {
        match self {
            Self::Outliner => "icons/list-tree.svg",
            Self::NodeGraph => "icons/workflow.svg",
            Self::Timeline => "icons/layers.svg",
            Self::Viewer => "icons/monitor-play.svg",
            Self::Dopesheet => "icons/diamond.svg",
            Self::Properties => "icons/sliders-horizontal.svg",
            Self::MediaBin => "icons/clapperboard.svg",
            Self::CurveEditor => "icons/spline.svg",
            Self::Waveform => "icons/audio-waveform.svg",
            Self::Vectorscope => "icons/radar.svg",
            Self::Histogram => "icons/chart-column.svg",
            Self::Parade => "icons/chart-bar-big.svg",
            Self::TextEditor => "icons/type.svg",
            Self::ShaderEditor => "icons/braces.svg",
            Self::LuaConsole => "icons/terminal.svg",
            Self::RenderQueue => "icons/list-video.svg",
            Self::ZoomFit => "icons/maximize.svg",
            Self::ZoomActualSize => "icons/square-square.svg",
            Self::GridOverlay => "icons/grid-3x3.svg",
            Self::SafeAreas => "icons/frame.svg",
            Self::SkipBack => "icons/skip-back.svg",
            Self::StepBack => "icons/step-back.svg",
            Self::Play => "icons/play.svg",
            Self::Pause => "icons/pause.svg",
            Self::Stop => "icons/square.svg",
            Self::StepForward => "icons/step-forward.svg",
            Self::SkipForward => "icons/skip-forward.svg",
            Self::TimelineFit => "icons/maximize-2.svg",
            Self::TimelineBars => "icons/chart-no-axes-gantt.svg",
            Self::InterpolationBezier => "icons/interpolation-bezier.svg",
            Self::InterpolationLinear => "icons/interpolation-linear.svg",
            Self::InterpolationStep => "icons/interpolation-step.svg",
            Self::Diamond => "icons/diamond.svg",
            Self::DiamondFilled => "icons/diamond-filled.svg",
            Self::Circle => "icons/circle.svg",
            Self::CircleDot => "icons/circle-dot.svg",
            Self::CircleFilled => "icons/circle-filled.svg",
            Self::Square => "icons/square.svg",
            Self::SquareFilled => "icons/square-filled.svg",
            Self::ToolSelect => "icons/mouse-pointer.svg",
            Self::ToolPen => "icons/pen-tool.svg",
            Self::ToolRect => "icons/square.svg",
            Self::ToolEllipse => "icons/circle.svg",
            Self::ToolHand => "icons/hand.svg",
            Self::ToolZoom => "icons/zoom-in.svg",
            Self::MediaStill => "icons/image.svg",
            Self::AlwaysOnTop => "icons/pin.svg",
            Self::NodeConstant => "icons/equal.svg",
            Self::NodeConstantColor => "icons/palette.svg",
            Self::NodeConstantVec2 => "icons/move.svg",
            Self::NodeConstantVec3 => "icons/axis-3d.svg",
            Self::NodeConstantVec4 => "icons/boxes.svg",
            Self::NodeMedia => "icons/film.svg",
            Self::NodeLayerRef => "icons/layers.svg",
            Self::NodeSubnet => "icons/network.svg",
            Self::NodeMerge => "icons/merge.svg",
            Self::NodeMathScalar => "icons/calculator.svg",
            Self::NodeMathRemap => "icons/arrow-right-left.svg",
            Self::NodeMathCurve => "icons/spline.svg",
            Self::NodeVectorConstructVec2 => "icons/move.svg",
            Self::NodeVectorConstructVec3 => "icons/axis-3d.svg",
            Self::NodeVectorConstructVec4 => "icons/boxes.svg",
            Self::NodeGeometryTransform => "icons/move-3d.svg",
            Self::NodeGeometryMerge => "icons/combine.svg",
            Self::NodeGeometryConnect => "icons/network.svg",
            Self::NodeSceneAdd => "icons/box.svg",
            Self::NodeSceneMerge => "icons/group.svg",
            Self::NodeSceneCamera => "icons/video.svg",
            Self::NodeBlur => "icons/droplet.svg",
            Self::NodeTransform => "icons/scaling.svg",
            Self::NodeColorCorrect => "icons/contrast.svg",
            Self::NodeRasterize => "icons/image-down.svg",
            Self::NodeShapeRect => "icons/square.svg",
            Self::NodeShapeEllipse => "icons/circle.svg",
            Self::NodeShapePolygon => "icons/hexagon.svg",
            Self::NodeShapeStar => "icons/star.svg",
            Self::NodeShapeLine => "icons/interpolation-linear.svg",
            // Shares the lattice glyph with `scatter.grid`: both are grids,
            // and the vendored set has no second one.
            Self::NodeShapeGrid => "icons/grid-3x3.svg",
            Self::NodeShapeCustomPath => "icons/pen-tool.svg",
            Self::NodeScatterGrid => "icons/grid-3x3.svg",
            Self::NodeScatterCircular => "icons/circle-dashed.svg",
            Self::NodeScatterPathArray => "icons/waypoints.svg",
            Self::NodeScatterScatter => "icons/sparkles.svg",
            Self::NodeAttributeSet => "icons/tag.svg",
            Self::NodeAttributePromote => "icons/chevrons-up.svg",
            Self::NodeAttributeTransfer => "icons/replace.svg",
            Self::NodeAttributePathSample => "icons/route.svg",
            Self::NodeAttributeCurveU => "icons/interpolation-bezier.svg",
            Self::NodeFieldNoise => "icons/waves.svg",
            Self::NodeFieldFalloff => "icons/target.svg",
            Self::NodeFieldCurveRemap => "icons/spline.svg",
            Self::NodeFieldExpression => "icons/sigma.svg",
            Self::Expression => "icons/sigma.svg",
            Self::NodeFieldAdd => "icons/plus.svg",
            Self::NodeFieldMultiply => "icons/x.svg",
            Self::NodeFieldMax => "icons/arrow-up-to-line.svg",
            Self::NodeFieldBlend => "icons/blend.svg",
            Self::NodeFieldAttribute => "icons/hash.svg",
            Self::NodeFieldApply => "icons/paintbrush.svg",
            Self::CategoryGeometry => "icons/shapes.svg",
            Self::CategoryScene => "icons/orbit.svg",
            Self::CategoryField => "icons/activity.svg",
            Self::CategoryImage => "icons/image.svg",
            Self::CategoryColor => "icons/palette.svg",
            Self::CategoryTime => "icons/clock.svg",
            Self::CategoryUtility => "icons/wrench.svg",
        }
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_panel_icon_is_embedded() {
        for kind in PanelKind::ALL {
            let path = RavelIcon::for_panel(kind).path();
            assert!(
                RavelEmbed::get(path.as_ref()).is_some(),
                "missing embedded icon for {kind:?}: {path}"
            );
        }
    }

    /// Every built-in node template resolves to its own embedded icon —
    /// a typo'd `type_key` would silently fall back to the category
    /// default, so the test asserts the explicit entry, not just
    /// embeddability.
    #[test]
    fn every_node_template_icon_is_embedded() {
        let mut registry = ravel_core::registry::NodeRegistry::new();
        ravel_core::registry::builtin::register_builtins(&mut registry);
        for template in registry.all_templates() {
            let icon = RavelIcon::for_node_type(&template.type_key, Some(template.category));
            assert_ne!(
                icon,
                RavelIcon::for_category(template.category),
                "{} fell back to its category default — is the type_key in for_node_type?",
                template.type_key
            );
            let path = icon.path();
            assert!(
                RavelEmbed::get(path.as_ref()).is_some(),
                "missing embedded icon for {}: {path}",
                template.type_key
            );
        }
    }

    /// An unknown `type_key` (user-defined or future node) falls back to
    /// the category default, and to the generic node icon when even the
    /// category is unknown.
    #[test]
    fn unknown_node_type_falls_back_to_category_default() {
        assert_eq!(
            RavelIcon::for_node_type("user.my_node", Some(NodeCategory::Field)),
            RavelIcon::for_category(NodeCategory::Field)
        );
        assert_eq!(
            RavelIcon::for_node_type("user.my_node", Some(NodeCategory::Geometry)),
            RavelIcon::for_category(NodeCategory::Geometry)
        );
        assert_eq!(
            RavelIcon::for_node_type("user.my_node", None),
            RavelIcon::NodeGraph
        );
    }

    #[test]
    fn every_category_icon_is_embedded() {
        for category in [
            NodeCategory::Geometry,
            NodeCategory::Scene,
            NodeCategory::Field,
            NodeCategory::Image,
            NodeCategory::Color,
            NodeCategory::Time,
            NodeCategory::Utility,
        ] {
            let path = RavelIcon::for_category(category).path();
            assert!(
                RavelEmbed::get(path.as_ref()).is_some(),
                "missing embedded category icon for {category:?}: {path}"
            );
        }
    }

    #[test]
    fn fallback_serves_component_icons() {
        let loaded = RavelAssets
            .load(&gpui_component::IconName::ChevronDown.path())
            .unwrap();
        assert!(loaded.is_some(), "gpui-component fallback icons must load");
    }

    #[test]
    fn timeline_transport_icons_are_embedded() {
        for icon in [
            RavelIcon::SkipBack,
            RavelIcon::StepBack,
            RavelIcon::Play,
            RavelIcon::Pause,
            RavelIcon::Stop,
            RavelIcon::StepForward,
            RavelIcon::SkipForward,
            RavelIcon::TimelineFit,
            RavelIcon::TimelineBars,
            RavelIcon::InterpolationBezier,
            RavelIcon::InterpolationLinear,
            RavelIcon::InterpolationStep,
        ] {
            let path = icon.path();
            assert!(
                RavelEmbed::get(path.as_ref()).is_some(),
                "missing embedded timeline icon: {path}"
            );
        }
    }

    #[test]
    fn window_chrome_icons_are_embedded() {
        let path = RavelIcon::AlwaysOnTop.path();
        assert!(
            RavelEmbed::get(path.as_ref()).is_some(),
            "missing embedded window chrome icon: {path}"
        );
    }

    #[test]
    fn license_is_vendored_alongside_icons() {
        // ISC attribution must travel with the vendored SVGs.
        assert!(
            std::path::Path::new(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../assets/icons/LICENSE"
            ))
            .exists()
        );
    }
}
