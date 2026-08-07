// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Entry point for `ravel-cli`. Everything it does lives in the library
//! beside it; this is the argument parse, the two global initializations,
//! and the exit code.

use clap::Parser;
use ravel_cli::args::Cli;

fn main() -> std::process::ExitCode {
    let _ = ravel_core::logging::init_logging("RAVEL_LOG", None);
    // Before the first `t!`: an error message is the most likely thing this
    // process will ever print.
    ravel_cli::init_locale();

    // `clap` exits with its own code 2 for a malformed command line, which
    // is the "arguments are wrong" class the plan asks for.
    let cli = Cli::parse();
    std::process::ExitCode::from(ravel_cli::run(cli))
}
