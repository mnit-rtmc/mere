// main.rs    Directory mirroring service
//
// Copyright (C)  2018-2020  Minnesota Department of Transportation
//
#![forbid(unsafe_code)]

use log::{error, info};
use std::env;

mod error;
mod mere;

/// Mere version from cargo manifest
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Main function
fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::builder().format_timestamp(None).init();
    info!("mere v{}", VERSION);
    let args: Vec<String> = env::args().into_iter().collect();
    if args.len() > 2 {
        Ok(mere::mirror_files(&args[1], &args[2..])?)
    } else {
        error!("Usage: {:} [host] [directory 0] … [directory N]", args[0]);
        Err(Box::new(error::Error::InvalidArgs()))
    }
}
