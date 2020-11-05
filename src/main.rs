// main.rs    Directory mirroring service
//
// Copyright (C)  2018-2020  Minnesota Department of Transportation
//
#![forbid(unsafe_code)]

mod mere;

use crate::mere::{Mirror, Watcher};
use anyhow::{Context, Result};
use gumdrop::Options;
use std::env;
use std::net::ToSocketAddrs;

/// Mere version from cargo manifest
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A directory mirroring tool
#[derive(Debug, Options)]
struct MereOptions {
    /// Print help message
    help: bool,

    /// Destination: <host_name> or <host_name>:<port>
    #[options(required, short = "d")]
    destination: String,

    /// One or more source directories to mirror
    #[options(required, short = "s")]
    sources: Vec<String>,

    /// Watch directories for changes using inotify
    watch: bool,
}

/// Main function
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("mere v{}", VERSION);
    let opts = MereOptions::parse_args_default_or_exit();
    env_logger::builder().format_timestamp(None).init();
    let dest = socket_addr(&opts.destination)?;
    Ok(mirror_files(opts.watch, &dest, &opts.sources)?)
}

/// Validate destination host to parse as socket address
fn socket_addr(dest: &str) -> anyhow::Result<String> {
    let mut addr = dest.to_string();
    if addr.to_socket_addrs().is_err() {
        addr.push_str(":22");
        addr.to_socket_addrs()
            .with_context(|| format!("Invalid destination {:?}", dest))?;
    }
    Ok(addr)
}

/// Mirror files to another host.
fn mirror_files(watch: bool, dest: &str, sources: &[String]) -> Result<()> {
    let mut mirror = Mirror::new(dest)?;
    for dir in sources {
        mirror.add_path(dir.into())?;
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
