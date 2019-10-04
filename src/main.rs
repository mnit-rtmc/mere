// mere.rs
//
// Copyright (C)  2018-2019  Minnesota Department of Transportation
//
#![forbid(unsafe_code)]

use log::{error, info};
use std::env;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use whoami;

mod error;
mod mere;

/// Main function
fn main() {
    env_logger::builder().format_timestamp(None).init();
    let args: Vec<String> = env::args().into_iter().collect();
    if args.len() > 2 {
        mirror_files(&args[1], &args[2..]);
    } else {
        error!("Usage: {:} [host] [directory 0] … [directory N]", args[0]);
    }
}

/// Mirror files to another host.
///
/// * `host` Destination host.
/// * `directories` Slice of absolute directory names.
fn mirror_files(host: &str, directories: &[String]) {
    let username = whoami::username();
    info!("Mirroring to {:} as user {:}", host, username);
    for dir in directories {
        info!("  Directory {:}", dir);
    }
    let (tx, rx) = channel();
    let join_handle = mere::start_thread(&host, &username, rx);
    // FIXME: use fsnotifier to send paths to channel
    let mut n = PathBuf::new();
    n.push("/home");
    n.push(username);
    n.push("test.txt");
    tx.send(n).unwrap();
    match join_handle.join() {
        Ok(Ok(())) => (),
        Ok(Err(e)) => error!("mere: {:?}", e),
        Err(e) => error!("mere panic: {:?}", e),
    }
}
