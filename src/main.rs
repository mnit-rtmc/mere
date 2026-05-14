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

    /// key file (default `id_rsa`)
    #[argh(option, short = 'k', default = "String::from(\"id_rsa\")")]
    key_file: String,

    /// watch paths for changes using inotify
    #[argh(switch, short = 'w')]
    watch: bool,
}

impl Args {
    /// Mirror files to another host
    fn mirror_files(self) -> Result<()> {
        let dest = socket_addr(&self.destination)?;
        let mut mirror = Mirror::new(dest, self.key_file);
        for path in self.path {
            mirror.add_path(path.into());
        }
        if self.watch {
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

/// Main function
fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::builder().format_timestamp(None).init();
    println!("mere v{VERSION}");
    let args: Args = argh::from_env();
    Ok(args.mirror_files()?)
}
