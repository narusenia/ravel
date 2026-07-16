// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

pub mod bezier;
pub mod painting;
pub mod port_colors;
pub mod viewport;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EdgeStyle {
    #[default]
    Bezier,
    Straight,
    Step,
}
