//! One cache benchmark's owned workload and process boundary.

mod evidence;
mod local;
mod workload;

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::{RailError, RailResult};

#[derive(Debug, Parser)]
#[command(
    name = "cargo rail-bench",
    version,
    about = "Compare native Cargo, Cargo-Rail and sccache on an isolated cache workload"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Operation>,
}

#[derive(Debug, Subcommand)]
enum Operation {
    /// Compare local caches on Unix; retain samples and correctness evidence.
    Local(local::Options),
    /// Prepare the bundled workload and prefetch locked dependencies without timing.
    Prepare {
        /// New workspace directory; its parent must exist.
        #[arg(long)]
        output: PathBuf,
        /// Shared bundled Git dependency directory; existing repositories must match the bundled revision.
        #[arg(long)]
        git_source: Option<PathBuf>,
        /// Use cached registry packages; the bundled local Git dependency is still prefetched.
        #[arg(long)]
        offline: bool,
    },
}

/// Execute the installed benchmark command from process arguments.
///
/// With no subcommand, run the local comparison with its default options.
///
/// Preparation creates the selected workload and Git source paths and prefetches
/// locked dependencies into the selected Cargo home. Existing workspaces are refused;
/// failed preparation is retained for diagnosis.
pub fn run(arguments: impl IntoIterator<Item = OsString>) -> RailResult<()> {
    let mut arguments = arguments.into_iter().collect::<Vec<_>>();
    if arguments.get(1).is_some_and(|argument| argument == "rail-bench") {
        arguments.remove(1);
    }
    match Cli::parse_from(arguments).command {
        None => run(["cargo-rail-bench", "local"].map(OsString::from)),
        Some(Operation::Local(options)) => local::run(options),
        Some(Operation::Prepare {
            output,
            git_source,
            offline,
        }) => {
            let git_source = match git_source {
                Some(path) => path,
                None => {
                    let mut name = output
                        .file_name()
                        .ok_or_else(|| RailError::message("workload output has no directory name"))?
                        .to_os_string();
                    name.push(".git-source");
                    output.with_file_name(name)
                }
            };
            let workspace = workload::materialize(&output, &git_source, offline).map_err(|error| {
                error.context(format!(
                    "preparing {}; any partial workload and Git source are retained",
                    output.display()
                ))
            })?;
            println!("Prepared cache benchmark workload: {}", workspace.display());
            Ok(())
        }
    }
}
