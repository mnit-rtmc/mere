// mere.rs
//
// Copyright (C) 2018-2019  Minnesota Department of Transportation
//
use crate::error::Result;
use log::{debug, error, info};
use ssh2::Session;
use std::collections::HashSet;
use std::fs::File;
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// A set of paths to mirror
struct PathSet {
    set : HashSet<PathBuf>,
}

impl PathSet {
    /// Create a new PathSet
    fn new() -> Self {
        let set = HashSet::new();
        PathSet { set }
    }
    /// Wait to receive paths from channel.
    ///
    /// * `rx` Channel receiver for path names.
    fn wait_receive(&mut self, rx: &Receiver<PathBuf>) -> Result<()> {
        if self.set.is_empty() {
            debug!("waiting to receive paths");
            let p = rx.recv()?;
            debug!("received path: {:?}", p);
            self.set.insert(p);
        }
        loop {
            let p = rx.try_recv();
            if let Err(TryRecvError::Empty) = p {
                break;
            };
            debug!("received path: {:?}", p);
            self.set.insert(p?);
        }
        Ok(())
    }
}

/// Create a new SSH session
///
/// * `host` Host name (and port) to connect.
fn create_session(host: &str) -> Result<Session> {
    debug!("creating session for {}", host);
    let mut session = Session::new()?;
    session.set_tcp_stream(TcpStream::connect(host)?);
    session.handshake()?;
    Ok(session)
}

/// Authenticate an SSH session.
///
/// * `session` SSH session.
/// * `username` User to authenticate.
fn authenticate_session(session: &Session, username: &str) -> Result<()> {
    debug!("authenticating user {}", username);
    // First, try using key with no pass-phrase.  If that doesn't work,
    // try using agent auth -- maybe we're running interactively
    authenticate_pubkey(session, username)
        .or_else(|_| authenticate_agent(session, username))
        .or_else(|e| {
            error!("authentication failed for user {}", username);
            Err(e)
        })
}

/// Authenticate an SSH session using public key.
///
/// * `session` SSH session.
/// * `username` User to authenticate.
fn authenticate_pubkey(session: &Session, username: &str) -> Result<()> {
    let mut key = PathBuf::new();
    key.push("/home");
    key.push(username);
    key.push(".ssh");
    key.push("id_rsa");
    session.userauth_pubkey_file(username, None, &key, None)?;
    debug!("authenticated {} using pubkey", username);
    Ok(())
}

/// Authenticate an SSH session using agent.
///
/// * `session` SSH session.
/// * `username` User to authenticate.
fn authenticate_agent(session: &Session, username: &str) -> Result<()> {
    session.userauth_agent(username)?;
    debug!("authenticated {} using agent", username);
    Ok(())
}

/// Mirror files received from channel.
///
/// * `session` SSH session.
/// * `rx` Channel receiver for path names.
/// * `ps` Set of path names to mirror.
fn mirror_from_channel(session: &Session, rx: &Receiver<PathBuf>,
    mut ps: &mut PathSet) -> Result<()>
{
    loop {
        ps.wait_receive(rx)?;
        if let Err(_) = mirror_all(session, &mut ps) {
            break;
        }
    }
    Ok(())
}

/// Mirror all files in a path set.
///
/// * `session` SSH session.
/// * `ps` Set of path names to mirror.
fn mirror_all(session: &Session, ps: &mut PathSet) -> Result<()> {
    for p in ps.set.iter() {
        let t = Instant::now();
        match mirror_file(session, &p) {
            Ok(action) => info!("{} {:?} in {:?}", action, p, t.elapsed()),
            Err(e) => {
                error!("{}, file {:?}", e, p);
                return Err(e);
            },
        }
    }
    // All copied successfully
    ps.set.clear();
    Ok(())
}

/// Mirror one file.
///
/// * `session` SSH session.
/// * `p` Path to file.
fn mirror_file(session: &Session, p: &PathBuf) -> Result<&'static str> {
    let fi = File::open(&p);
    match fi {
        Ok(f)  => scp_file(session, p, f),
        Err(_) => rm_file(session, p),
    }
}

/// Mirror one file with scp.
///
/// * `session` SSH session.
/// * `p` Path to file.
fn scp_file(session: &Session, p: &PathBuf, mut fi: File)
    -> Result<&'static str>
{
    let metadata = fi.metadata()?;
    let len = metadata.len();
    // Mask off higher mode bits to prevent scp_send
    // from returning a [-28] "file corrupt" error
    let mode = (metadata.permissions().mode() & 0o7777) as i32;
    debug!("copying {:?} with len: {} and mode {:o}", p, len, mode);
    let mut fo = session.scp_send(p.as_path(), mode, len, None)?;
    let c = std::io::copy(&mut fi, &mut fo)?;
    if c == len {
        debug!("copied {:?}", p);
    } else {
        error!("{:?}: length mismatch {} != {}", p, c, len);
    }
    Ok("copied")
}

/// Remove one file.
///
/// * `session` SSH session.
/// * `p` Path to file.
fn rm_file(session: &Session, p: &PathBuf) -> Result<&'static str> {
    debug!("removing {:?}", p);
    let mut channel = session.channel_session()?;
    let mut cmd = String::new();
    cmd.push_str("rm -f ");
    cmd.push_str(p.to_str().unwrap());
    channel.exec(&cmd)?;
    debug!("removed {:?}", p);
    Ok("removed")
}

/// Start mirror session.
///
/// * `host` Host name (and port) to connect.
/// * `username` Name of user to use for authentication.
/// * `rx` Channel receiver for path names.
/// * `ps` Set of path names to mirror.
fn start_session(host: &str, username: &str, rx: &Receiver<PathBuf>,
    mut ps: &mut PathSet) -> Result<()>
{
    match create_session(host) {
        Ok(session) => {
            authenticate_session(&session, username)?;
            mirror_from_channel(&session, rx, &mut ps)?;
        },
        Err(e) => {
            error!("{}, host: {}", e, host);
            debug!("waiting for 10 seconds to try again");
            thread::sleep(Duration::from_secs(10));
        },
    }
    Ok(())
}

/// Mirror thread entry point.
///
/// * `host` Host name (and port) to connect.
/// * `username` Name of user to use for authentication.
/// * `rx` Channel receiver for path names.
fn mirror_thread(host: &str, username: &str, rx: Receiver<PathBuf>)
    -> Result<()>
{
    debug!("mirror thread started for {}", host);
    let mut ps = PathSet::new();
    loop {
        ps.wait_receive(&rx)?;
        start_session(&host, username, &rx, &mut ps)?;
    }
}

/// Start mirroring thread.
///
/// * `host` Host name (and port) to connect.
/// * `username` Name of user to use for authentication.
/// * `rx` Channel receiver for path names.
pub fn start_thread(host: &str, username: &str, rx: Receiver<PathBuf>)
    -> JoinHandle<Result<()>>
{
    let host = host.to_string();
    let username = username.to_string();
    thread::spawn(move || { mirror_thread(&host, &username, rx) })
}
