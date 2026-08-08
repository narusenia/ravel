// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

pub mod bezier;
pub mod hover_popover;
pub mod layout;
pub mod painting;
pub mod palette;
pub mod port_colors;
pub mod viewport;

/// The edge style in force. Defined in `ravel-ui` and re-exported here: it is
/// a persisted setting (`SettingsLayer::node_editor`) and `ravel-project`
/// cannot see this crate, but every drawing site names it through
/// `node_editor::EdgeStyle` as before.
pub use ravel_ui::node_editor::EdgeStyle;
