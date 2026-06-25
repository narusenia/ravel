// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Concrete panel shells.
//!
//! Each panel's interactive content is implemented in its own follow-up task.
//! This module currently provides the Properties inspector shell; other panels
//! are hosted as empty frames driven by [`crate::panel::PanelKind`] until their
//! tasks land.

pub mod properties;
pub mod timeline;
