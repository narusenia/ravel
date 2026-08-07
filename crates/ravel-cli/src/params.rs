// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `--param NAME=VALUE` into the values
//! [`apply`](ravel_core::exposed::apply::apply) takes.
//!
//! # What this module does not do
//!
//! It does not validate. The contract lives in `ravel-core`: an undeclared
//! name is [`ExposedApplyError::Undeclared`], a value of the wrong shape is
//! [`ExposedApplyError::TypeMismatch`], and a non-finite number is
//! [`ExposedApplyError::NonFiniteValue`] — each reported before the document
//! is touched. Re-implementing any of that here would give a caller two
//! answers to the same question.
//!
//! What is genuinely the CLI's is that a command line carries **text**. A
//! declaration says `scale` is a float, so `--param scale=2` has to become
//! `ExposedValue::Float(2.0)` rather than an integer — which means the
//! declared type, not the literal's syntax, drives the parse. Two
//! consequences worth stating:
//!
//! * a value that cannot be read as the declared type is refused **here**,
//!   as [`CliError::ParamValue`], and `ExposedApplyError::TypeMismatch`
//!   consequently never fires for a command line. Both are the same class to
//!   the caller — exit code [`crate::error::EXIT_PARAM`] — which is what the
//!   plan's "a type mismatch fails before the render starts" asks for;
//! * a name with no declaration has no type to parse against. Rather than
//!   deciding it is unknown, this hands the raw text through as a string and
//!   lets `apply` say so: it checks undeclared names *before* types, so the
//!   error a user sees is `Undeclared` and comes from the one authority on
//!   what the project declares.
//!
//! [`ExposedApplyError::Undeclared`]: ravel_core::exposed::ExposedApplyError::Undeclared
//! [`ExposedApplyError::TypeMismatch`]: ravel_core::exposed::ExposedApplyError::TypeMismatch
//! [`ExposedApplyError::NonFiniteValue`]: ravel_core::exposed::ExposedApplyError::NonFiniteValue

use std::collections::HashMap;

use ravel_core::composition::AssetPath;
use ravel_core::exposed::listing::ExposedListing;
use ravel_core::exposed::{ExposedType, ExposedValue};
use ravel_core::types::{Color, Vec2, Vec3, Vec4};

use crate::error::CliError;

/// Parse every `NAME=VALUE` in `raw` against the declarations in `listing`.
///
/// A repeated name keeps the last occurrence, which is what a shell user
/// expects from appending an override to a command line.
pub fn parse(
    raw: &[String],
    listing: &ExposedListing,
) -> Result<HashMap<String, ExposedValue>, CliError> {
    let mut values = HashMap::with_capacity(raw.len());
    for entry in raw {
        let (name, text) = entry
            .split_once('=')
            .ok_or_else(|| CliError::ParamSyntax { raw: entry.clone() })?;
        let name = name.trim();
        if name.is_empty() {
            return Err(CliError::ParamSyntax { raw: entry.clone() });
        }
        let declared = listing
            .parameters
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| entry.value_type);
        let value = match declared {
            Some(value_type) => parse_value(name, value_type, text)?,
            // No declaration, so no type to read the text as. `apply`
            // rejects the name before it looks at any value.
            None => ExposedValue::String(text.to_string()),
        };
        values.insert(name.to_string(), value);
    }
    Ok(values)
}

/// Read `text` as the declared type.
fn parse_value(name: &str, declared: ExposedType, text: &str) -> Result<ExposedValue, CliError> {
    let bad = || CliError::ParamValue {
        name: name.to_string(),
        declared,
        raw: text.to_string(),
    };
    let value = match declared {
        ExposedType::Float => ExposedValue::Float(text.trim().parse().map_err(|_| bad())?),
        ExposedType::Int => ExposedValue::Int(text.trim().parse().map_err(|_| bad())?),
        ExposedType::Bool => ExposedValue::Bool(parse_bool(text).ok_or_else(bad)?),
        // Not trimmed: leading and trailing spaces are part of a caption.
        ExposedType::String => ExposedValue::String(text.to_string()),
        ExposedType::Vec2 => {
            let [x, y] = components(text).ok_or_else(bad)?;
            ExposedValue::Vec2(Vec2(x, y))
        }
        ExposedType::Vec3 => {
            let [x, y, z] = components(text).ok_or_else(bad)?;
            ExposedValue::Vec3(Vec3(x, y, z))
        }
        ExposedType::Vec4 => {
            let [x, y, z, w] = components(text).ok_or_else(bad)?;
            ExposedValue::Vec4(Vec4(x, y, z, w))
        }
        // Four components, alpha included. An implicit opaque alpha would be
        // a convenience that silently disagrees with `list params`, which
        // prints every colour default as four numbers.
        ExposedType::Color => {
            let [r, g, b, a] = components(text).ok_or_else(bad)?;
            ExposedValue::Color(Color::new(r, g, b, a))
        }
        ExposedType::Media => {
            if text.trim().is_empty() {
                return Err(bad());
            }
            ExposedValue::Media(AssetPath::parse(text.trim()))
        }
    };
    Ok(value)
}

/// `true` / `false`, and the `1` / `0` a script is likely to hand over.
fn parse_bool(text: &str) -> Option<bool> {
    match text.trim() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

/// Exactly `N` comma-separated finite-or-not floats. The count is fixed by
/// the declared type, so `--param offset=1,2,3` for a `vec2` is a mistake
/// rather than a truncation.
fn components<const N: usize>(text: &str) -> Option<[f32; N]> {
    let parts: Vec<&str> = text.split(',').collect();
    if parts.len() != N {
        return None;
    }
    let mut out = [0.0f32; N];
    for (slot, part) in out.iter_mut().zip(parts) {
        *slot = part.trim().parse().ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::exposed::listing::ExposedListingEntry;

    fn listing(entries: &[(&str, ExposedType, ExposedValue)]) -> ExposedListing {
        ExposedListing {
            parameters: entries
                .iter()
                .map(|(name, value_type, default)| ExposedListingEntry {
                    name: (*name).to_string(),
                    value_type: *value_type,
                    default: default.clone(),
                    description: String::new(),
                    resolved: true,
                })
                .collect(),
        }
    }

    #[test]
    fn a_value_is_read_as_the_declared_type() {
        let listing = listing(&[
            ("scale", ExposedType::Float, ExposedValue::Float(1.0)),
            ("count", ExposedType::Int, ExposedValue::Int(1)),
            ("loop_it", ExposedType::Bool, ExposedValue::Bool(false)),
            (
                "tint",
                ExposedType::Color,
                ExposedValue::Color(Color::new(0.0, 0.0, 0.0, 1.0)),
            ),
        ]);
        let values = parse(
            &[
                // An integer literal for a float declaration: the declared
                // type decides, not the literal's syntax.
                "scale=2".to_string(),
                "count=7".to_string(),
                "loop_it=true".to_string(),
                "tint=1,0.5,0.25,1".to_string(),
            ],
            &listing,
        )
        .expect("all four parse");

        assert_eq!(values["scale"], ExposedValue::Float(2.0));
        assert_eq!(values["count"], ExposedValue::Int(7));
        assert_eq!(values["loop_it"], ExposedValue::Bool(true));
        assert_eq!(
            values["tint"],
            ExposedValue::Color(Color::new(1.0, 0.5, 0.25, 1.0))
        );
    }

    #[test]
    fn a_value_that_is_not_the_declared_type_is_refused() {
        let listing = listing(&[("scale", ExposedType::Float, ExposedValue::Float(1.0))]);
        let error = parse(&["scale=large".to_string()], &listing).expect_err("refused");
        assert!(matches!(
            error,
            CliError::ParamValue {
                declared: ExposedType::Float,
                ..
            }
        ));
    }

    /// The component count comes from the declared type, so a vector with
    /// the wrong arity is refused instead of being padded or truncated.
    #[test]
    fn a_vector_needs_exactly_its_components() {
        let listing = listing(&[(
            "offset",
            ExposedType::Vec2,
            ExposedValue::Vec2(Vec2(0.0, 0.0)),
        )]);
        assert!(parse(&["offset=1,2".to_string()], &listing).is_ok());
        assert!(parse(&["offset=1,2,3".to_string()], &listing).is_err());
        assert!(parse(&["offset=1".to_string()], &listing).is_err());
    }

    /// An undeclared name is carried through as text so `apply` — the one
    /// authority on what a project declares — is the thing that refuses it.
    #[test]
    fn an_undeclared_name_is_carried_through_for_apply_to_refuse() {
        let listing = listing(&[("scale", ExposedType::Float, ExposedValue::Float(1.0))]);
        let values = parse(&["nosuch=1".to_string()], &listing).expect("parsing does not judge");
        assert_eq!(values["nosuch"], ExposedValue::String("1".to_string()));
    }

    #[test]
    fn a_missing_equals_sign_is_a_syntax_error() {
        let listing = listing(&[]);
        assert!(matches!(
            parse(&["scale".to_string()], &listing),
            Err(CliError::ParamSyntax { .. })
        ));
        assert!(matches!(
            parse(&["=1".to_string()], &listing),
            Err(CliError::ParamSyntax { .. })
        ));
    }

    /// A value may contain `=` — an expression, a query string in a path —
    /// so only the first one separates.
    #[test]
    fn only_the_first_equals_separates() {
        let listing = listing(&[(
            "caption",
            ExposedType::String,
            ExposedValue::String(String::new()),
        )]);
        let values = parse(&["caption=a=b".to_string()], &listing).expect("parses");
        assert_eq!(values["caption"], ExposedValue::String("a=b".to_string()));
    }

    #[test]
    fn a_repeated_name_keeps_the_last_value() {
        let listing = listing(&[("scale", ExposedType::Float, ExposedValue::Float(1.0))]);
        let values =
            parse(&["scale=1".to_string(), "scale=3".to_string()], &listing).expect("both parse");
        assert_eq!(values["scale"], ExposedValue::Float(3.0));
    }
}
