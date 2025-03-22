// main.rs    Directory mirroring service
//
// Copyright (C)  2018-2025  Minnesota Department of Transportation
//
#![forbid(unsafe_code)]

mod mere;

use crate::mere::{Mirror, Watcher};
use anyhow::{Context, Result};
use argh::FromArgs;
use std::env;
use std::net::ToSocketAddrs;

/// Mere version from cargo manifest
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Command-line arguments
#[derive(Debug, FromArgs)]
struct Args {
    /// destination <host> or <host>:<port>
    #[argh(option, short = 'd')]
    destination: String,

    /// directory or file path (can be used multiple times)
    #[argh(option, short = 'p')]
    path: Vec<String>,

    /// watch paths for changes using inotify
    #[argh(switch, short = 'w')]
    watch: bool,
}

/// Main function
fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::builder().format_timestamp(None).init();
    println!("mere v{VERSION}");
    let args: Args = argh::from_env();
    let dest = socket_addr(&args.destination)?;
    Ok(mirror_files(args.watch, &dest, &args.path)?)
}

/// Validate destination host to parse as socket address
fn socket_addr(dest: &str) -> anyhow::Result<String> {
    let mut addr = dest.to_string();
    if addr.to_socket_addrs().is_err() {
        addr.push_str(":22");
        addr.to_socket_addrs()
            .with_context(|| format!("Invalid destination {dest:?}"))?;
    }
    Ok(addr)
}

/// Mirror files to another host.
fn mirror_files(watch: bool, dest: &str, paths: &[String]) -> Result<()> {
    let mut mirror = Mirror::new(dest);
    for path in paths {
        mirror.add_path(path.into());
    }
    if watch {
        let mut watcher = Watcher::new(&mirror)?;
        mirror.copy_all()?;
        loop {
            watcher.wait_events(&mut mirror)?;
            mirror.copy_all()?;
        }
    } else {
        mirror.copy_all()
    }
}
