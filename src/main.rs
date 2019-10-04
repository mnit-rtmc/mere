// main.rs    Directory mirroring service
//
// Copyright (C)  2018-2019  Minnesota Department of Transportation
//
#![forbid(unsafe_code)]

use log::{error, info};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::env;
use std::sync::mpsc::channel;
use std::time::Duration;
use whoami;

mod error;
mod mere;

/// Mere version from cargo manifest
const VERSION: &'static str = env!("CARGO_PKG_VERSION");

/// Main function
fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::builder().format_timestamp(None).init();
    info!("mere v{}", VERSION);
    let args: Vec<String> = env::args().into_iter().collect();
    if args.len() > 2 {
        Ok(mirror_files(&args[1], &args[2..])?)
    } else {
        error!("Usage: {:} [host] [directory 0] … [directory N]", args[0]);
        Err(Box::new(error::Error::InvalidArgs()))
    }
}

/// Mirror files to another host.
///
/// * `host` Destination host.
/// * `directories` Slice of absolute directory names.
fn mirror_files(host: &str, directories: &[String]) -> error::Result<()> {
    let username = whoami::username();
    info!("Mirroring to {:} as user {:}", host, username);
    for dir in directories {
        info!("  Directory {:}", dir);
    }
    let (tx, rx) = channel();
    let mut watcher: RecommendedWatcher = Watcher::new(tx,
        Duration::from_secs(1))?;
    for dir in directories {
        watcher.watch(dir, RecursiveMode::NonRecursive)?;
    }
    mere::mirror_files(&host, &username, rx)
}
