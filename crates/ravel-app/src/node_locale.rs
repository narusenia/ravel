// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Resolution of the node locale keys built by [`ravel_ui::node_locale`].
//!
//! This is the single place that turns `node.<type_key>.*` keys into display
//! text. Every UI surface that names a node — the node editor canvas and its
//! add-node menu, the Properties inspector, the Outliner — goes through here
//! (directly, or via the key `ravel-ui` emits and `read_only_value`
//! translates). A type without a locale entry falls back to its `type_key`,
//! so a newly registered node type renders fine before its strings land.

use ravel_core::graph::Node;
use ravel_core::registry::NodeRegistry;

/// Translates `key`, falling back to `fallback` when no locale defines it
/// (`t!` returns the key itself for a miss).
fn translate_or(key: String, fallback: &str) -> String {
    let value = ravel_i18n::translate(&key);
    if value == key {
        fallback.to_string()
    } else {
        value
    }
}

/// Localized display label of a node type, falling back to the raw
/// `type_key` when the key is missing.
pub fn type_label(type_key: &str) -> String {
    translate_or(ravel_ui::node_locale::label_key(type_key), type_key)
}

/// Localized display label of a node instance: the user's rename when the
/// stored label differs from the template default, else the type label.
pub fn display_label(node: &Node, registry: &NodeRegistry) -> String {
    if let Some(label) = ravel_ui::node_locale::user_label(node, registry) {
        return label.to_string();
    }
    type_label(&node.type_key)
}

/// Localized description of a node type. `None` when the type has none —
/// descriptions are optional, so absence is not an error and callers skip
/// the section rather than showing a fallback.
pub fn description(type_key: &str) -> Option<String> {
    let key = ravel_ui::node_locale::description_key(type_key);
    let value = ravel_i18n::translate(&key);
    (value != key).then_some(value)
}

/// Localized description of one parameter of a node type. `None` when the
/// parameter has none.
pub fn param_description(type_key: &str, param: &str) -> Option<String> {
    let key = ravel_ui::node_locale::param_key(type_key, param);
    let value = ravel_i18n::translate(&key);
    (value != key).then_some(value)
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

    /// An unknown type has no locale entry in any catalog, so the fallback
    /// holds whether or not the test process initialized i18n.
    #[test]
    fn an_unknown_type_key_falls_back_to_the_type_key() {
        assert_eq!(type_label("plugin.unknown"), "plugin.unknown");
        let node = Node::new(NodeId::new(1), "plugin.unknown");
        assert_eq!(display_label(&node, &registry()), "plugin.unknown");
        assert_eq!(description("plugin.unknown"), None);
        assert_eq!(param_description("plugin.unknown", "strength"), None);
    }

    #[test]
    fn a_user_rename_wins_over_the_locale_entry() {
        let registry = registry();
        let mut node = registry
            .create_node("blur", NodeId::new(1))
            .expect("blur is registered");
        node.metadata.label = Some("My Blur".to_string());
        assert_eq!(display_label(&node, &registry), "My Blur");
    }
}
