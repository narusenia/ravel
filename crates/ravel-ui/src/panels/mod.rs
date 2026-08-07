// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Concrete panel shells.
//!
//! Each panel's interactive content is implemented in its own follow-up task.
//! This module currently provides the Properties inspector shell; other panels
//! are hosted as empty frames driven by [`crate::panel::PanelKind`] until their
//! tasks land.

pub mod layer_selection;
pub mod media_bin;
pub mod outliner;
pub mod properties;
pub mod render_queue;
pub mod timeline;
pub mod viewer;
