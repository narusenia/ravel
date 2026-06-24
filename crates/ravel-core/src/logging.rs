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

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

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
    let env_filter =
        EnvFilter::try_from_env(env_key).unwrap_or_else(|_| EnvFilter::new("info"));

    let stderr_layer = fmt::layer()
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
