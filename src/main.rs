// main.rs    Directory mirroring service
//
// Copyright (C)  2018-2020  Minnesota Department of Transportation
//
#![forbid(unsafe_code)]

mod mere;

use anyhow::Context;
use gumdrop::Options;
use std::env;
use std::net::ToSocketAddrs;

/// Mere version from cargo manifest
const VERSION: &str = env!("CARGO_PKG_VERSION");

// Mere program options
#[derive(Debug, Options)]
struct MereOptions {
    /// Print help message
    help: bool,

    /// Destination host
    #[options(required, short = "d")]
    destination: String,

    /// One or more source directories to mirror
    #[options(required, short = "s")]
    sources: Vec<String>,
}

/// Main function
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("mere v{}", VERSION);
    let opts = MereOptions::parse_args_default_or_exit();
    env_logger::builder().format_timestamp(None).init();
    let addrs = socket_addr(&opts.destination)?;
    Ok(mere::mirror_files(&addrs, &opts.sources)?)
}

/// Validate destination host to parse as socket address
fn socket_addr(dest: &str) -> anyhow::Result<String> {
    let mut addr = dest.to_string();
    if addr.to_socket_addrs().is_err() {
        addr.push_str(":22");
        addr.to_socket_addrs()
            .with_context(|| format!("Invalid destination — {}", dest))?;
    }
    Ok(addr)
}
