// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Structured logging via `tracing`.
//!
//! Log level is controlled by the `RAVEL_LOG` environment variable using
//! `tracing_subscriber`'s `EnvFilter` syntax (e.g. `RAVEL_LOG=info`,
//! `RAVEL_LOG=ravel_core=debug,warn`). Falls back to `info` when unset.
//!
//! In release builds, logs are also written to rotating files under the
//! application log directory.
//!
//! Console output goes to **stderr**, never stdout: `ravel-cli` puts its
//! machine-readable output on stdout, and a consumer of that stream must
//! never have to filter diagnostics out of it.

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;

/// Guard that must be held alive for the lifetime of the application to keep
/// the non-blocking file writer flushing. Drop it to flush and close the log
/// file.
pub struct LogGuard {
    _file_guard: Option<WorkerGuard>,
}

/// Initialize the global tracing subscriber.
///
/// * `env_key` — environment variable name for the filter directive
///   (e.g. `"RAVEL_LOG"`).
/// * `log_dir` — if `Some`, log files are written to this directory with
///   daily rotation. Pass `None` to skip file logging (e.g. in tests).
///
/// Returns `Ok(LogGuard)` on success. Returns `Err` if a global subscriber
/// was already installed (safe to ignore in tests).
pub fn init_logging(
    env_key: &str,
    log_dir: Option<&std::path::Path>,
) -> Result<LogGuard, anyhow::Error> {
    let env_filter = EnvFilter::try_from_env(env_key).unwrap_or_else(|_| EnvFilter::new("info"));

    // `with_writer` is not optional here: `fmt::layer()` defaults to
    // **stdout**, and a diagnostic on stdout is indistinguishable from
    // output. `ravel-cli` writes its machine-readable progress and its
    // listings there, so a log line landing in the same stream turns valid
    // JSON into something no consumer can parse.
    let stderr_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_thread_names(true)
        .compact();

    let (file_layer, file_guard) = if let Some(dir) = log_dir {
        let file_appender = tracing_appender::rolling::daily(dir, "ravel.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        let layer = fmt::layer()
            .with_writer(non_blocking)
            .with_ansi(false)
            .json();
        (Some(layer), Some(guard))
    } else {
        (None, None)
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stderr_layer)
        .with(file_layer)
        .try_init()?;

    Ok(LogGuard {
        _file_guard: file_guard,
    })
}
