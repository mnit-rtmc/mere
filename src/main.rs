// main.rs    Directory mirroring service
//
// Copyright (C)  2018-2020  Minnesota Department of Transportation
//
#![forbid(unsafe_code)]

mod mere;

use gumdrop::Options;
use std::env;

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
    source: Vec<String>,
}

/// Main function
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("mere v{}", VERSION);
    let opts = MereOptions::parse_args_default_or_exit();
    env_logger::builder().format_timestamp(None).init();
    Ok(mere::mirror_files(&opts.destination, &opts.source)?)
}
