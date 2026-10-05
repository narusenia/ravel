// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What a reserved attribute reads as when its column is absent.
//!
//! The readers (`rasterize`, [`InstanceColumns`](super::InstanceColumns)) treat
//! a missing reserved column as a particular value, which is rarely the
//! column type's zero. Anything that has to *invent* the rows of a column that
//! one side of a concatenation lacks (`geometry.merge`, [`expand_instances`],
//! `field.apply`'s `create_if_missing`) therefore asks here instead of
//! writing zeros, so the side that lacked the column is drawn the way it was
//! drawn without it. A name that is not reserved, or whose column carries a
//! type the reserved name does not, keeps the typed zero.
//!
//! [`expand_instances`]: super::ops::expand_instances

use super::ops::{AttributeValue, broadcast_value};
use super::{AttributeArray, AttributeType, Domain, Geometry, InstanceTransform, names};
use crate::types::{Color, Vec2, Vec3, Vec4};

/// Opacity of an element with no `alpha`.
pub const DEFAULT_ALPHA: f32 = 1.0;
/// Radius of a sprite point with no `pscale`, in composition pixels.
pub const DEFAULT_PSCALE: f32 = 2.0;
/// Whether an element with no `fill` is filled: the `rasterize` template's
/// `fill`.
pub const DEFAULT_FILL: bool = true;
/// Stroke width of an element with no `stroke_width`: the `rasterize`
/// template's `stroke_width`.
pub const DEFAULT_STROKE_WIDTH: f32 = 0.0;
/// The colour a colour reads as when nothing else decides it: the `rasterize`
/// template's `color`, and the neutral tint of an instance with no `Cd`.
pub const DEFAULT_COLOR: Color = Color::WHITE;

/// How a reserved attribute reads on a domain when its column is absent.
#[derive(Clone, Debug, PartialEq)]
pub enum Absent {
    /// A constant: `alpha` = 1, `scale` = (1, 1), `pscale` = 2, Instance `Cd`
    /// = white, ...
    Value(AttributeValue),
    /// Inherited from the drawing context (the `rasterize` parameters, an
    /// enclosing instance, the element's own fill): Primitive / Point `Cd`,
    /// `fill`, `stroke_width`, `stroke_color`. A column cannot say "no
    /// opinion", so filling one materialises the `rasterize` template's
    /// defaults ([`absent_column`]).
    Inherited,
}

/// What reserved attribute `name` reads as on `domain` when its column is
/// absent. `None` when the name has no reading of its own there: not
/// reserved, reserved for another domain, or without a reader (`P`, `index`,
/// `id`, 3D names, Detail names, the `field.attribute` sim columns).
pub fn absent(domain: Domain, name: &str) -> Option<Absent> {
    use Absent::{Inherited, Value};
    use AttributeValue as V;
    use Domain::{Instance, Point, Primitive};
    Some(match (domain, name) {
        (Instance, names::SOURCE_INDEX) => Value(V::I32(0)),
        (Instance, names::ROT | names::SHEAR) => Value(V::F32(0.0)),
        (Instance, names::SCALE) => Value(V::Vec2(InstanceTransform::IDENTITY.scale)),
        (Instance, names::CD) => Value(V::Color(DEFAULT_COLOR)),
        (Point | Primitive, names::CD) => Inherited,
        (Point | Primitive | Instance, names::ALPHA) => Value(V::F32(DEFAULT_ALPHA)),
        (Point, names::PSCALE) => Value(V::F32(DEFAULT_PSCALE)),
        (Primitive | Instance, names::FILL | names::STROKE_WIDTH) => Inherited,
        (Point | Primitive | Instance, names::STROKE_COLOR) => Inherited,
        (Primitive, names::STROKE_ALIGN) => Value(V::I32(names::STROKE_ALIGN_CENTER)),
        (Point, names::IN_TAN | names::OUT_TAN) => Value(V::Vec2(Vec2(0.0, 0.0))),
        _ => return None,
    })
}

impl AttributeValue {
    /// The type's zero: what a column the reserved names do not speak for is
    /// filled with.
    pub fn zero(attr_type: AttributeType) -> Self {
        match attr_type {
            AttributeType::F32 => Self::F32(0.0),
            AttributeType::Vec2 => Self::Vec2(Vec2(0.0, 0.0)),
            AttributeType::Vec3 => Self::Vec3(Vec3(0.0, 0.0, 0.0)),
            AttributeType::Vec4 => Self::Vec4(Vec4(0.0, 0.0, 0.0, 0.0)),
            AttributeType::Color => Self::Color(Color::TRANSPARENT),
            AttributeType::I32 => Self::I32(0),
            AttributeType::Bool => Self::Bool(false),
            AttributeType::Str => Self::Str(String::new()),
        }
    }

    /// The type of a column holding this value.
    pub fn attr_type(&self) -> AttributeType {
        match self {
            Self::F32(_) => AttributeType::F32,
            Self::Vec2(_) => AttributeType::Vec2,
            Self::Vec3(_) => AttributeType::Vec3,
            Self::Vec4(_) => AttributeType::Vec4,
            Self::Color(_) => AttributeType::Color,
            Self::I32(_) => AttributeType::I32,
            Self::Bool(_) => AttributeType::Bool,
            Self::Str(_) => AttributeType::Str,
        }
    }
}

/// The value of one absent row of `name` on `domain` for a column of
/// `attr_type`, without looking at any geometry: the constant for a
/// [`Absent::Value`], the `rasterize` template's default for an
/// [`Absent::Inherited`] one, the type's zero otherwise (also when
/// `attr_type` is not the reserved type).
///
/// What a caller with no [`Geometry`] to read (a piece's provenance row)
/// fills with. The `stroke_color` of a row follows that row's `Cd` instead,
/// which only the caller can see; this answers white.
pub fn absent_value(domain: Domain, name: &str, attr_type: AttributeType) -> AttributeValue {
    let zero = || AttributeValue::zero(attr_type);
    match absent(domain, name) {
        Some(Absent::Value(value)) if value.attr_type() == attr_type => value,
        Some(Absent::Inherited) => match (name, attr_type) {
            (names::FILL, AttributeType::Bool) => AttributeValue::Bool(DEFAULT_FILL),
            (names::STROKE_WIDTH, AttributeType::F32) => AttributeValue::F32(DEFAULT_STROKE_WIDTH),
            (names::CD | names::STROKE_COLOR, AttributeType::Color) => {
                AttributeValue::Color(DEFAULT_COLOR)
            }
            _ => zero(),
        },
        _ => zero(),
    }
}

/// The column `geometry` would resolve `name` on `domain` to if it carried
/// one, `len` rows long and of type `attr_type`. Typed zero for an unreserved
/// name or a type the reserved name does not have.
///
/// Takes the geometry because two inherited columns follow another column of
/// the same side: a `stroke_color` is the row's fill colour (the same domain's
/// `Cd`, white when that is absent too), and a Point `Cd` is the stroke colour
/// of the path the point belongs to (Primitive `stroke_color`, else Primitive
/// `Cd`, else white; a point on no path is white). Resolving them at the
/// moment of filling is deliberate: the filled column does not follow a later
/// change of `Cd`, where an absent one would have.
pub fn absent_column(
    geometry: &Geometry,
    domain: Domain,
    name: &str,
    attr_type: AttributeType,
    len: usize,
) -> AttributeArray {
    if attr_type == AttributeType::Color
        && matches!(absent(domain, name), Some(Absent::Inherited))
        && let Some(colors) = derived_colors(geometry, domain, name, len)
    {
        return AttributeArray::Color(colors);
    }
    broadcast_value(&absent_value(domain, name, attr_type), len)
}

/// The colour column `name` follows on `geometry`, when it follows one.
pub(super) fn derived_colors(
    geometry: &Geometry,
    domain: Domain,
    name: &str,
    len: usize,
) -> Option<Vec<Color>> {
    let colors_of = |domain: Domain, name: &str| {
        let column = geometry.attribute_set(domain).get(name)?;
        column.as_color(name).ok().filter(|c| c.len() == len)
    };
    match (domain, name) {
        (Domain::Point, names::CD | names::STROKE_COLOR) => {
            if name == names::STROKE_COLOR
                && let Some(own) = colors_of(Domain::Point, names::CD)
            {
                return Some(own.to_vec());
            }
            Some(path_colors(geometry, len))
        }
        (Domain::Primitive | Domain::Instance, names::STROKE_COLOR) => {
            Some(match colors_of(domain, names::CD) {
                Some(own) => own.to_vec(),
                None => vec![DEFAULT_COLOR; len],
            })
        }
        _ => None,
    }
}

/// Per point: the stroke colour of the primitive that covers it (Primitive
/// `stroke_color` > Primitive `Cd` > white), white for a point on none.
fn path_colors(geometry: &Geometry, len: usize) -> Vec<Color> {
    let primitives = geometry.primitive_count();
    let column = |name: &str| {
        let colors = geometry.primitive_attrs().get(name)?.as_color(name).ok()?;
        (colors.len() == primitives).then_some(colors)
    };
    let stroke = column(names::STROKE_COLOR);
    let fill = column(names::CD);
    let mut colors = vec![DEFAULT_COLOR; len];
    for (index, primitive) in geometry.primitives().iter().enumerate() {
        let color = stroke
            .or(fill)
            .map_or(DEFAULT_COLOR, |colors| colors[index]);
        let verts = primitive.verts();
        let end = verts.end.min(len);
        let start = verts.start.min(end);
        colors[start..end].fill(color);
    }
    colors
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Color = Color::new(1.0, 0.0, 0.0, 1.0);
    const GREEN: Color = Color::new(0.0, 1.0, 0.0, 1.0);
    const BLUE: Color = Color::new(0.0, 0.0, 1.0, 1.0);

    fn value(domain: Domain, name: &str) -> AttributeValue {
        match absent(domain, name) {
            Some(Absent::Value(value)) => value,
            other => panic!("{name} on {domain:?}: expected a constant, got {other:?}"),
        }
    }

    /// Each expectation is what the reader does with a missing column
    /// (`rasterize`'s `unwrap_or`, `InstanceColumns::placement`,
    /// `DEFAULT_POINT_RADIUS`, `sample.rs`'s centre alignment, the tangent
    /// flattening's corner), written out from there rather than from
    /// `absent`'s output.
    #[test]
    fn a_constant_matches_what_the_reader_does_without_the_column() {
        assert_eq!(value(Domain::Point, "alpha"), AttributeValue::F32(1.0));
        assert_eq!(value(Domain::Primitive, "alpha"), AttributeValue::F32(1.0));
        assert_eq!(value(Domain::Instance, "alpha"), AttributeValue::F32(1.0));
        assert_eq!(
            value(Domain::Instance, "scale"),
            AttributeValue::Vec2(Vec2(1.0, 1.0))
        );
        assert_eq!(value(Domain::Point, "pscale"), AttributeValue::F32(2.0));
        assert_eq!(
            value(Domain::Instance, "Cd"),
            AttributeValue::Color(Color::new(1.0, 1.0, 1.0, 1.0))
        );
        for (domain, name, zero) in [
            (Domain::Instance, "rot", AttributeValue::F32(0.0)),
            (Domain::Instance, "shear", AttributeValue::F32(0.0)),
            (Domain::Instance, "source_index", AttributeValue::I32(0)),
            (Domain::Primitive, "stroke_align", AttributeValue::I32(0)),
            (
                Domain::Point,
                "in_tan",
                AttributeValue::Vec2(Vec2(0.0, 0.0)),
            ),
            (
                Domain::Point,
                "out_tan",
                AttributeValue::Vec2(Vec2(0.0, 0.0)),
            ),
        ] {
            assert_eq!(value(domain, name), zero, "{name}");
        }
    }

    #[test]
    fn the_inherited_attributes_are_marked_inherited() {
        for (domain, name) in [
            (Domain::Primitive, "Cd"),
            (Domain::Point, "Cd"),
            (Domain::Primitive, "fill"),
            (Domain::Instance, "fill"),
            (Domain::Primitive, "stroke_width"),
            (Domain::Instance, "stroke_width"),
            (Domain::Point, "stroke_color"),
            (Domain::Primitive, "stroke_color"),
            (Domain::Instance, "stroke_color"),
        ] {
            assert_eq!(
                absent(domain, name),
                Some(Absent::Inherited),
                "{name} on {domain:?}"
            );
        }
    }

    /// A reserved name nothing reads absent has no answer, and neither does
    /// a name reserved for another domain.
    #[test]
    fn a_name_without_a_reading_has_no_answer() {
        assert_eq!(absent(Domain::Instance, "pscale"), None);
        assert_eq!(absent(Domain::Point, "rot"), None);
        assert_eq!(absent(Domain::Point, "my_attribute"), None);
    }

    /// Reserved names that no reader gives an absent meaning to. Every other
    /// reserved name must answer on at least one domain, so adding a name to
    /// `names::ALL` without teaching `absent` fails here.
    const WITHOUT_READING: [&str; 20] = [
        names::P,
        names::ANCHOR,
        names::INDEX,
        names::ID,
        names::ORIENT,
        names::SCALE3,
        names::N,
        names::DASH,
        names::DASH_OFFSET,
        names::CAP,
        names::JOIN,
        names::AGE,
        names::LIFE,
        names::VELOCITY,
        names::U,
        names::CHAR_INDEX,
        names::WORD_INDEX,
        names::LINE_INDEX,
        names::CHAR_PROGRESS,
        names::ADVANCE,
    ];

    #[test]
    fn every_reserved_name_is_answered_or_listed_as_having_no_reading() {
        let domains = [
            Domain::Point,
            Domain::Primitive,
            Domain::Instance,
            Domain::Detail,
        ];
        for name in names::ALL {
            let answered = domains.iter().any(|domain| absent(*domain, name).is_some());
            assert_eq!(
                answered,
                !WITHOUT_READING.contains(&name),
                "{name}: answered = {answered}"
            );
        }
        // The exemption list only names reserved names.
        for name in WITHOUT_READING {
            assert!(names::ALL.contains(&name), "{name} is not reserved");
        }
    }

    fn two_path_geometry() -> Geometry {
        // Two 2-vertex paths, then one loose point.
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0); 5]);
        geometry.push_primitive(crate::geometry::Primitive::Path {
            verts: 0..2,
            closed: false,
        });
        geometry.push_primitive(crate::geometry::Primitive::Path {
            verts: 2..4,
            closed: false,
        });
        geometry
    }

    fn primitive_colors(geometry: &mut Geometry, name: &str, colors: Vec<Color>) {
        geometry
            .primitive_attrs_mut()
            .insert(name, AttributeArray::Color(colors))
            .unwrap();
    }

    #[test]
    fn a_primitive_stroke_colour_is_its_fill_colour() {
        let mut geometry = two_path_geometry();
        primitive_colors(&mut geometry, names::CD, vec![RED, BLUE]);
        assert_eq!(
            absent_column(
                &geometry,
                Domain::Primitive,
                names::STROKE_COLOR,
                AttributeType::Color,
                2
            ),
            AttributeArray::Color(vec![RED, BLUE])
        );
        // No Cd: white, which is what the fill reads as with no Cd either.
        let bare = two_path_geometry();
        assert_eq!(
            absent_column(
                &bare,
                Domain::Primitive,
                names::STROKE_COLOR,
                AttributeType::Color,
                2
            ),
            AttributeArray::Color(vec![Color::WHITE; 2])
        );
    }

    /// `vertex_stroke_colors`: a path's control points colour its stroke, so
    /// a filled point takes the colour the path strokes in (its
    /// `stroke_color`, else its `Cd`), and a point on no path takes the
    /// sprite's white.
    #[test]
    fn a_point_colour_is_the_stroke_colour_of_its_path() {
        let mut geometry = two_path_geometry();
        primitive_colors(&mut geometry, names::STROKE_COLOR, vec![GREEN, RED]);
        primitive_colors(&mut geometry, names::CD, vec![BLUE, BLUE]);
        let colors = |geometry: &Geometry, name| {
            absent_column(geometry, Domain::Point, name, AttributeType::Color, 5)
        };
        let expected = AttributeArray::Color(vec![GREEN, GREEN, RED, RED, Color::WHITE]);
        assert_eq!(colors(&geometry, names::CD), expected);
        assert_eq!(colors(&geometry, names::STROKE_COLOR), expected);

        // Without a stroke colour the path strokes in its fill colour.
        let mut fill_only = two_path_geometry();
        primitive_colors(&mut fill_only, names::CD, vec![RED, RED]);
        assert_eq!(
            colors(&fill_only, names::CD),
            AttributeArray::Color(vec![RED, RED, RED, RED, Color::WHITE])
        );
    }

    /// A Point `stroke_color` follows the Point's own `Cd` before the path
    /// does: that is the order `vertex_stroke_colors` asks in.
    #[test]
    fn a_point_stroke_colour_follows_the_points_own_colour() {
        let mut geometry = two_path_geometry();
        primitive_colors(&mut geometry, names::CD, vec![RED, RED]);
        geometry
            .points_mut()
            .insert(names::CD, AttributeArray::Color(vec![BLUE; 5]))
            .unwrap();
        assert_eq!(
            absent_column(
                &geometry,
                Domain::Point,
                names::STROKE_COLOR,
                AttributeType::Color,
                5
            ),
            AttributeArray::Color(vec![BLUE; 5])
        );
    }

    #[test]
    fn a_reserved_name_with_another_type_stays_the_typed_zero() {
        let geometry = Geometry::from_points(vec![Vec2(0.0, 0.0); 2]);
        assert_eq!(
            absent_column(&geometry, Domain::Point, names::CD, AttributeType::Vec3, 2),
            AttributeArray::Vec3(vec![Vec3(0.0, 0.0, 0.0); 2])
        );
        assert_eq!(
            absent_column(
                &geometry,
                Domain::Point,
                names::PSCALE,
                AttributeType::I32,
                2
            ),
            AttributeArray::I32(vec![0; 2])
        );
        // An unreserved name too.
        assert_eq!(
            absent_column(&geometry, Domain::Point, "mine", AttributeType::F32, 2),
            AttributeArray::F32(vec![0.0; 2])
        );
        assert_eq!(
            absent_column(&geometry, Domain::Point, "mine", AttributeType::Color, 1),
            AttributeArray::Color(vec![Color::TRANSPARENT])
        );
    }

    #[test]
    fn the_inherited_template_defaults_fill_a_column() {
        let geometry = two_path_geometry();
        let fill = |domain, name, ty| absent_column(&geometry, domain, name, ty, 2);
        assert_eq!(
            fill(Domain::Primitive, names::FILL, AttributeType::Bool),
            AttributeArray::Bool(vec![true; 2])
        );
        assert_eq!(
            fill(Domain::Primitive, names::STROKE_WIDTH, AttributeType::F32),
            AttributeArray::F32(vec![0.0; 2])
        );
        assert_eq!(
            fill(Domain::Primitive, names::CD, AttributeType::Color),
            AttributeArray::Color(vec![Color::WHITE; 2])
        );
        // The geometry's own set is untouched by asking.
        assert_eq!(geometry.primitive_attrs().element_count(), 0);
    }
}
