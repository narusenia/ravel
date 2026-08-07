// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Every way `ravel-cli` can refuse or fail, and the exit code each one
//! reports.
//!
//! # Two texts, on purpose
//!
//! [`CliError`] carries a `thiserror` `Display` that is the **diagnostic**:
//! English, unlocalized, and what goes into the log and into the `detail`
//! field of the machine-readable output. [`CliError::localized`] is the
//! **user's** sentence, assembled from the locale catalogs, and is what
//! reaches the terminal. Keeping them apart is what lets a script grep a
//! stable string while a reader gets their own language.
//!
//! # Exit codes
//!
//! The codes are a contract with whatever drives the CLI, so they are
//! constants with names rather than integers spelled at the throw site. `2`
//! is `clap`'s own code for a bad command line and is left alone; everything
//! else is Ravel's.

use std::path::PathBuf;

use ravel_core::exposed::ExposedType;
use ravel_core::exposed::apply::ExposedApplyError;
use ravel_core::media::MediaError;
use ravel_core::media::encode::UnavailableReason;
use ravel_core::runtime::RenderError;
use ravel_i18n::t;
use thiserror::Error;

/// Success.
pub const EXIT_OK: u8 = 0;
/// Something the CLI cannot classify: a worker that vanished, a poisoned
/// lock. Always a bug rather than a user mistake.
pub const EXIT_INTERNAL: u8 = 1;
/// The command line itself is wrong. `clap`'s own code for a parse failure,
/// reused for arguments that only the document can invalidate (an unknown
/// composition, a range that covers nothing).
pub const EXIT_USAGE: u8 = 2;
/// The project could not be read.
pub const EXIT_LOAD: u8 = 3;
/// A `--param` names something undeclared, or its value does not fit the
/// declared type.
pub const EXIT_PARAM: u8 = 4;
/// The requested output format cannot be written here.
pub const EXIT_CODEC: u8 = 5;
/// Output files already exist and `--overwrite` was not given.
pub const EXIT_OUTPUT_EXISTS: u8 = 6;
/// Compiling or evaluating the composition failed.
pub const EXIT_EVAL: u8 = 7;
/// The encoder refused a frame or could not close its output.
pub const EXIT_ENCODE: u8 = 8;
/// The render was interrupted. Not `130`: Windows has no `128 + signal`
/// convention, and this code has to mean the same thing on every platform.
pub const EXIT_CANCELLED: u8 = 9;

/// Why the CLI stopped.
#[derive(Debug, Error)]
pub enum CliError {
    #[error("cannot read the project {path}: {detail}")]
    Load { path: PathBuf, detail: String },

    #[error("--param must be written NAME=VALUE, not {raw:?}")]
    ParamSyntax { raw: String },

    #[error("--param {name:?} takes {declared}, and {raw:?} is not one")]
    ParamValue {
        name: String,
        declared: ExposedType,
        raw: String,
    },

    #[error("the supplied parameters were refused: {0}")]
    ParamRejected(#[from] ExposedApplyError),

    #[error("the project has no composition called {0:?}")]
    UnknownComposition(String),

    #[error("the project has no composition to render")]
    NoComposition,

    #[error("--range must be written START-END or START, not {raw:?}")]
    BadRange { raw: String },

    #[error("frame range {start}-{end} covers no frames")]
    EmptyRange { start: u64, end: u64 },

    #[error("the output file name is not usable: {0}")]
    OutputName(#[from] MediaError),

    #[error("{format} cannot be written in this build on this machine")]
    CodecUnavailable {
        format: &'static str,
        reason: UnavailableReason,
    },

    #[error("{format} is available here but Ravel has no writer for it yet")]
    CodecNoWriter { format: &'static str },

    #[error("{total} output file(s) already exist, starting with {first}")]
    OutputExists {
        first: PathBuf,
        total: usize,
        /// Up to [`ravel_core::runtime::render::CONFLICT_SAMPLE`] of the
        /// conflicting paths, for the machine-readable output.
        sample: Vec<PathBuf>,
    },

    #[error("rendering failed: {0}")]
    Eval(String),

    #[error("writing the output failed: {0}")]
    Encode(String),

    #[error("the render was interrupted")]
    Cancelled,

    #[error("no usable GPU adapter: {0}")]
    Gpu(String),

    #[error("internal failure: {0}")]
    Internal(String),
}

impl CliError {
    /// The process exit code this failure reports.
    pub fn code(&self) -> u8 {
        match self {
            Self::Load { .. } => EXIT_LOAD,
            Self::ParamSyntax { .. } | Self::ParamValue { .. } | Self::ParamRejected(_) => {
                EXIT_PARAM
            }
            Self::UnknownComposition(_)
            | Self::NoComposition
            | Self::BadRange { .. }
            | Self::EmptyRange { .. }
            | Self::OutputName(_) => EXIT_USAGE,
            Self::CodecUnavailable { .. } | Self::CodecNoWriter { .. } => EXIT_CODEC,
            Self::OutputExists { .. } => EXIT_OUTPUT_EXISTS,
            Self::Eval(_) => EXIT_EVAL,
            Self::Encode(_) => EXIT_ENCODE,
            Self::Cancelled => EXIT_CANCELLED,
            Self::Gpu(_) | Self::Internal(_) => EXIT_INTERNAL,
        }
    }

    /// A stable identifier for the machine-readable output, so a caller can
    /// branch on the failure without matching a localized sentence or an
    /// exit code that groups several causes.
    pub fn id(&self) -> &'static str {
        match self {
            Self::Load { .. } => "load-failed",
            Self::ParamSyntax { .. } => "param-syntax",
            Self::ParamValue { .. } => "param-type",
            Self::ParamRejected(_) => "param-rejected",
            Self::UnknownComposition(_) => "unknown-composition",
            Self::NoComposition => "no-composition",
            Self::BadRange { .. } => "bad-range",
            Self::EmptyRange { .. } => "empty-range",
            Self::OutputName(_) => "bad-output-name",
            Self::CodecUnavailable { .. } => "codec-unavailable",
            Self::CodecNoWriter { .. } => "codec-no-writer",
            Self::OutputExists { .. } => "output-exists",
            Self::Eval(_) => "eval-failed",
            Self::Encode(_) => "encode-failed",
            Self::Cancelled => "cancelled",
            Self::Gpu(_) => "no-gpu",
            Self::Internal(_) => "internal",
        }
    }

    /// The sentence a person reads, in the active locale.
    pub fn localized(&self) -> String {
        match self {
            Self::Load { path, detail } => t!("cli.error.load")
                .replace("{path}", &path.display().to_string())
                .replace("{detail}", detail),
            Self::ParamSyntax { raw } => t!("cli.error.param_syntax").replace("{raw}", raw),
            Self::ParamValue {
                name,
                declared,
                raw,
            } => t!("cli.error.param_value")
                .replace("{name}", name)
                .replace("{type}", &declared.to_string())
                .replace("{raw}", raw),
            Self::ParamRejected(error) => {
                t!("cli.error.param_rejected").replace("{detail}", &error.to_string())
            }
            Self::UnknownComposition(name) => {
                t!("cli.error.unknown_composition").replace("{name}", name)
            }
            Self::NoComposition => t!("cli.error.no_composition"),
            Self::BadRange { raw } => t!("cli.error.bad_range").replace("{raw}", raw),
            Self::EmptyRange { start, end } => t!("cli.error.empty_range")
                .replace("{start}", &start.to_string())
                .replace("{end}", &end.to_string()),
            Self::OutputName(error) => {
                t!("cli.error.output_name").replace("{detail}", &error.to_string())
            }
            Self::CodecUnavailable { format, reason } => t!("cli.error.codec_unavailable")
                .replace("{format}", format)
                .replace("{reason}", &localized_reason(reason)),
            Self::CodecNoWriter { format } => {
                t!("cli.error.codec_no_writer").replace("{format}", format)
            }
            Self::OutputExists { first, total, .. } => t!("cli.error.output_exists")
                .replace("{count}", &total.to_string())
                .replace("{first}", &first.display().to_string()),
            Self::Eval(detail) => t!("cli.error.eval").replace("{detail}", detail),
            Self::Encode(detail) => t!("cli.error.encode").replace("{detail}", detail),
            Self::Cancelled => t!("cli.error.cancelled"),
            Self::Gpu(detail) => t!("cli.error.gpu").replace("{detail}", detail),
            Self::Internal(detail) => t!("cli.error.internal").replace("{detail}", detail),
        }
    }
}

/// Turn a structured [`UnavailableReason`] into the phrase that completes
/// `cli.error.codec_unavailable`.
///
/// `ravel-core` deliberately keeps the reasons free of prose
/// (`media::encode`), so this is the one place the CLI puts words on them.
pub fn localized_reason(reason: &UnavailableReason) -> String {
    match reason {
        UnavailableReason::FfmpegNotLinked => t!("cli.codec.reason.ffmpeg_not_linked"),
        UnavailableReason::FfmpegEncoderMissing { candidates } => {
            t!("cli.codec.reason.ffmpeg_encoder_missing")
                .replace("{candidates}", &candidates.join(", "))
        }
        UnavailableReason::PlatformApiUnavailable { api } => {
            t!("cli.codec.reason.platform_api_unavailable").replace("{api}", api.id())
        }
        UnavailableReason::NoPlatformRouteOnThisOs => t!("cli.codec.reason.no_platform_route"),
        UnavailableReason::NotOffered => t!("cli.codec.reason.not_offered"),
    }
}

/// Classify a worker failure. The queue reports one error type for causes the
/// CLI has to keep apart — an existing output is the user's to resolve, an
/// evaluation failure is the project's, an encoder failure is the machine's.
impl From<RenderError> for CliError {
    fn from(error: RenderError) -> Self {
        match error {
            RenderError::OutputExists { sample, total } => CliError::OutputExists {
                first: sample.first().cloned().unwrap_or_default(),
                total,
                sample,
            },
            RenderError::CompositionNotFound(comp) => {
                CliError::UnknownComposition(comp.raw().to_string())
            }
            RenderError::EmptyRange { start, end } => CliError::EmptyRange { start, end },
            RenderError::Encode(error) => CliError::Encode(error.to_string()),
            RenderError::WorkerGone => CliError::Internal(RenderError::WorkerGone.to_string()),
            other => CliError::Eval(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every failure gets a code, and the codes stay apart where the plan
    /// asks them to (`docs/implementation/render-export-plan.md`, unit 3).
    #[test]
    fn each_failure_class_reports_its_own_code() {
        assert_eq!(
            CliError::Load {
                path: "p".into(),
                detail: "x".into()
            }
            .code(),
            EXIT_LOAD
        );
        assert_eq!(CliError::ParamSyntax { raw: "x".into() }.code(), EXIT_PARAM);
        assert_eq!(
            CliError::ParamRejected(ExposedApplyError::Undeclared("x".into())).code(),
            EXIT_PARAM
        );
        assert_eq!(CliError::UnknownComposition("x".into()).code(), EXIT_USAGE);
        assert_eq!(CliError::CodecNoWriter { format: "vp9" }.code(), EXIT_CODEC);
        assert_eq!(
            CliError::OutputExists {
                first: "a.png".into(),
                total: 3,
                sample: vec!["a.png".into()]
            }
            .code(),
            EXIT_OUTPUT_EXISTS
        );
        assert_eq!(CliError::Eval("x".into()).code(), EXIT_EVAL);
        assert_eq!(CliError::Encode("x".into()).code(), EXIT_ENCODE);
        assert_eq!(CliError::Cancelled.code(), EXIT_CANCELLED);
        assert_eq!(CliError::Internal("x".into()).code(), EXIT_INTERNAL);
    }

    /// A worker failure has to land on the class the user can act on, not on
    /// one bucket for "the render did not work".
    #[test]
    fn worker_failures_keep_their_classes_apart() {
        let exists = CliError::from(RenderError::OutputExists {
            sample: vec!["frame_0000.png".into()],
            total: 10,
        });
        assert_eq!(exists.code(), EXIT_OUTPUT_EXISTS);
        assert_eq!(exists.id(), "output-exists");

        let encode = CliError::from(RenderError::Encode(MediaError::EncodeError("disk".into())));
        assert_eq!(encode.code(), EXIT_ENCODE);

        let eval = CliError::from(RenderError::NotAFrame { frame: 4 });
        assert_eq!(eval.code(), EXIT_EVAL);

        assert_eq!(
            CliError::from(RenderError::WorkerGone).code(),
            EXIT_INTERNAL
        );
    }
}
