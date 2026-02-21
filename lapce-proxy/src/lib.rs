#![allow(clippy::manual_clamp)]

pub mod buffer;
pub mod cli;
pub mod dispatch;
pub mod lsp;
pub mod project;
pub mod semgrep;
pub mod shell_env;
pub mod watcher;

use std::process::exit;

use anyhow::Result;
use clap::Parser;
use lapce_core::meta;
use lapce_rpc::file::PathObject;
use tracing::error;

#[derive(Parser)]
#[clap(name = "SourceDelve-proxy")]
#[clap(version = meta::VERSION)]
struct Cli {
    /// Paths to file(s) and/or folder(s) to open.
    /// When path is a file (that exists or not),
    /// it accepts `path:line:column` syntax
    /// to specify line and column at which it should open the file
    #[clap(value_parser = cli::parse_file_line_column)]
    #[clap(value_hint = clap::ValueHint::AnyPath)]
    paths: Vec<PathObject>,
}

/// Entry point for the `lapce-proxy` binary. When invoked from the command line,
/// it attempts to forward the requested paths to an already-running Lapce instance
/// via a local socket. If no running instance is found (connection fails), we exit
/// with code 1 -- the GUI process is the one that spawns the proxy internally.
pub fn mainloop() {
    let cli = Cli::parse();
    if let Err(e) = cli::try_open_in_existing_process(&cli.paths) {
        error!("failed to open path(s): {e}");
    };
    exit(1);
}

/// Ensures the directory containing the Lapce binary is on the PATH.
/// This is important so that plugins and language servers spawned by the proxy
/// can find the `lapce-proxy` binary (e.g., for CLI integration).
/// Prepends the exe directory to PATH only if it's not already present.
pub fn register_lapce_path() -> Result<()> {
    let exedir = std::env::current_exe()?
        .parent()
        .ok_or(anyhow::anyhow!("can't get parent dir of exe"))?
        .canonicalize()?;

    // Check if the exe directory is already on the PATH to avoid duplication
    let current_path = std::env::var("PATH")?;
    let paths = std::env::split_paths(&current_path);
    for path in paths {
        if exedir == path.canonicalize()? {
            return Ok(());
        }
    }
    // Prepend (not append) so our binary takes priority
    let paths = std::env::split_paths(&current_path);
    let paths = std::env::join_paths(std::iter::once(exedir).chain(paths))?;

    unsafe {
        std::env::set_var("PATH", paths);
    }

    Ok(())
}

/// HTTP GET with retry logic. Respects the `https_proxy` environment variable
/// for corporate/proxy environments. Retries up to 3 times on transient failures
/// before propagating the error, which helps with flaky network conditions during
/// plugin downloads.
pub fn get_url<T: reqwest::IntoUrl + Clone>(
    url: T,
    user_agent: Option<&str>,
) -> Result<reqwest::blocking::Response> {
    let mut builder = if let Ok(proxy) = std::env::var("https_proxy") {
        let proxy = reqwest::Proxy::all(proxy)?;
        reqwest::blocking::Client::builder()
            .proxy(proxy)
            .timeout(std::time::Duration::from_secs(10))
    } else {
        reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
    };
    if let Some(user_agent) = user_agent {
        builder = builder.user_agent(user_agent);
    }
    let client = builder.build()?;
    let mut try_time = 0;
    loop {
        let rs = client.get(url.clone()).send();
        if rs.is_ok() || try_time > 3 {
            return Ok(rs?);
        } else {
            try_time += 1;
        }
    }
}
