// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Locale keys for node type labels, descriptions, and parameter docs.
//!
//! `ravel-ui` has no i18n dependency: it builds the *keys* and decides when
//! a node's stored label is a user rename, while the GPUI host (`ravel-app`)
//! translates them — the same split as `properties.field.*` (see
//! `read_only_value` there). The strings live in `assets/locales/*.toml`
//! under `[node."<type_key>"]`; a type without a key falls back to its
//! `type_key`, so adding a node type never breaks the UI before its locale
//! entry lands.

use ravel_core::graph::Node;
use ravel_core::registry::NodeRegistry;

/// Locale key of a node type's display label: `node.<type_key>.label`.
pub fn label_key(type_key: &str) -> String {
    format!("node.{type_key}.label")
}

/// Locale key of a node type's description: `node.<type_key>.description`.
pub fn description_key(type_key: &str) -> String {
    format!("node.{type_key}.description")
}

/// Locale key of one parameter's description:
/// `node.<type_key>.params.<param>`.
pub fn param_key(type_key: &str, param: &str) -> String {
    format!("node.{type_key}.params.{param}")
}

/// Locale key of one parameter group's heading:
/// `node.<type_key>.group.<group>`.
///
/// Only a group the **type** declares gets a key. An In node's instance
/// groups are the user's own text (`NETIF-2` parameters have no type to
/// declare them), so those titles are literal and never come through here.
pub fn group_key(type_key: &str, group: &str) -> String {
    format!("node.{type_key}.group.{group}")
}

/// The node's stored label when the user renamed it away from the template
/// default. `NodeTemplate::create_node` seeds `metadata.label` with the
/// template's label, so an instance whose label still matches its template
/// counts as unrenamed (and localizes); any other stored label is the
/// user's own text and always wins over the locale entry.
pub fn user_label<'a>(node: &'a Node, registry: &NodeRegistry) -> Option<&'a str> {
    let label = node.metadata.label.as_deref()?;
    let is_template_default = registry
        .get(&node.type_key)
        .is_some_and(|template| template.label == label);
    (!is_template_default).then_some(label)
}

/// Display label of `node` as literal text or a locale key: the user's
/// rename when there is one, the [`label_key`] for a registered type (the
/// host translates it), else the bare `type_key`.
pub fn label_or_key(node: &Node, registry: &NodeRegistry) -> String {
    if let Some(label) = user_label(node, registry) {
        return label.to_string();
    }
    if registry.get(&node.type_key).is_some() {
        return label_key(&node.type_key);
    }
    node.type_key.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::id::NodeId;
    use ravel_core::registry::builtin::register_builtins;

    fn registry() -> NodeRegistry {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        reg
    }

    #[test]
    fn key_format_matches_the_locale_tables() {
        assert_eq!(label_key("field.noise"), "node.field.noise.label");
        assert_eq!(description_key("blur"), "node.blur.description");
        assert_eq!(
            param_key("field.noise", "frequency"),
            "node.field.noise.params.frequency"
        );
    }

    #[test]
    fn a_label_matching_the_template_default_is_not_a_user_label() {
        let registry = registry();
        let node = registry
            .create_node("blur", NodeId::new(1))
            .expect("blur is registered");
        assert_eq!(node.metadata.label.as_deref(), Some("Blur"));
        assert_eq!(user_label(&node, &registry), None);
        assert_eq!(label_or_key(&node, &registry), "node.blur.label");
    }

    #[test]
    fn a_renamed_node_keeps_its_own_label() {
        let registry = registry();
        let mut node = registry
            .create_node("blur", NodeId::new(1))
            .expect("blur is registered");
        node.metadata.label = Some("Soft edge".to_string());
        assert_eq!(user_label(&node, &registry), Some("Soft edge"));
        assert_eq!(label_or_key(&node, &registry), "Soft edge");
    }

    #[test]
    fn an_unknown_type_falls_back_to_the_type_key() {
        let registry = registry();
        let node = Node::new(NodeId::new(1), "plugin.custom");
        assert_eq!(label_or_key(&node, &registry), "plugin.custom");
    }

    /// Every built-in template must have a `label` in both shipped locales;
    /// the scan follows the registry, so a new template fails here until its
    /// locale entries land.
    #[test]
    fn all_builtin_templates_have_a_label_in_every_locale() {
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);
        let templates: Vec<_> = registry.all_templates().collect();
        assert!(!templates.is_empty(), "registry scan must see templates");

        for locale in ["en", "ja"] {
            let nodes = node_catalog(locale);
            for template in &templates {
                let entry = nodes
                    .get(&template.type_key)
                    .and_then(toml::Value::as_table);
                assert!(
                    entry
                        .and_then(|table| table.get("label"))
                        .and_then(toml::Value::as_str)
                        .is_some(),
                    "{locale}.toml has no label for node type {:?}",
                    template.type_key
                );
            }
        }
    }

    /// `params.<name>` keys must name real parameters of a *registered*
    /// template — a typo'd type key or parameter name would silently never
    /// display.
    #[test]
    fn node_param_locale_keys_name_real_parameters() {
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);

        for locale in ["en", "ja"] {
            let nodes = node_catalog(locale);
            for (type_key, entry) in &nodes {
                let Some(params) = entry.get("params").and_then(toml::Value::as_table) else {
                    continue;
                };
                let template = registry.get(type_key).unwrap_or_else(|| {
                    panic!("{locale}.toml documents unknown node type {type_key:?}")
                });
                for key in params.keys() {
                    assert!(
                        template.default_params.iter().any(|p| &p.key == key),
                        "{locale}.toml documents param {key:?} that {type_key:?} does not have"
                    );
                }
            }
        }
    }

    /// The `[node]` table of a shipped locale catalog.
    fn node_catalog(locale: &str) -> toml::Table {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/locales/");
        let text =
            std::fs::read_to_string(format!("{path}{locale}.toml")).expect("locale file not found");
        let catalog: toml::Table = text.parse().expect("locale file is invalid TOML");
        catalog
            .get("node")
            .and_then(toml::Value::as_table)
            .expect("locale file has no [node] tables")
            .clone()
    }
}
