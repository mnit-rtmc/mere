// mere.rs    Directory mirroring service
//
// Copyright (C) 2018-2019  Minnesota Department of Transportation
//
use crate::error::Result;
use crate::error::Error::MpscTryRecv;
use log::{debug, error, info, trace};
use notify::DebouncedEvent;
use ssh2::Session;
use std::collections::VecDeque;
use std::fs::File;
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

/// Mirror change
#[derive(PartialEq, Eq, Hash)]
enum MirrorChange {
    FileCopy(PathBuf),
    FileRemove(PathBuf),
}

/// Pending changes to mirror
struct PendingChanges {
    changes: VecDeque<MirrorChange>,
}

impl PendingChanges {
    /// Create new pending changes
    fn new() -> Self {
        let changes = VecDeque::new();
        PendingChanges { changes }
    }
    /// Wait to receive paths from channel.
    ///
    /// * `rx` Channel receiver for path names.
    fn wait_receive(&mut self, rx: &Receiver<DebouncedEvent>) -> Result<()> {
        trace!("waiting to receive paths");
        while self.changes.is_empty() {
            self.add_change(rx.recv()?);
        }
        loop {
            match rx.try_recv() {
                Err(TryRecvError::Empty) => break,
                Err(e) => return Err(MpscTryRecv(e)),
                Ok(event) => self.add_change(event),
            }
        }
        Ok(())
    }
    /// Add a pending change
    fn add_change(&mut self, event: DebouncedEvent) {
        trace!("notify event: {:?}", event);
        match event {
            DebouncedEvent::Create(p) => self.add_copy(p),
            DebouncedEvent::Remove(p) => self.add_remove(p),
            DebouncedEvent::Rename(src, dst) => {
                self.add_remove(src);
                self.add_copy(dst);
            },
            e => trace!("ignored event: {:?}", e),
        }
    }
    /// Add a pending FileCopy change
    fn add_copy(&mut self, p: PathBuf) {
        if check_path(&p) && check_file(&p) {
            debug!("copy: {:?}", p);
            self.changes.push_back(MirrorChange::FileCopy(p));
        } else {
            debug!("ignoring copy: {:?}", p);
        }
    }
    /// Add a pending FileRemove change
    fn add_remove(&mut self, p: PathBuf) {
        if check_path(&p) {
            debug!("remove: {:?}", p);
            self.changes.push_back(MirrorChange::FileRemove(p));
        } else {
            debug!("ignoring remove: {:?}", p);
        }
    }
    /// Mirror all pending changes.
    ///
    /// * `session` SSH session.
    fn mirror_all(&mut self, session: &Session) -> Result<()> {
        while let Some(c) = self.changes.pop_front() {
            match c {
                MirrorChange::FileCopy(p) => try_scp_file(session, &p)?,
                MirrorChange::FileRemove(p) => try_rm_file(session, &p)?,
            }
        }
        Ok(())
    }
}

/// Check if a path is valid
fn check_path(p: &PathBuf) -> bool {
    p.is_absolute() && !check_path_hidden(p) && !check_path_temp(p)
}

/// Check whether a file path is hidden
fn check_path_hidden(p: &PathBuf) -> bool {
    match p.file_name() {
        Some(n) => {
            match n.to_str() {
                Some(sn) => sn.starts_with("."),
                _ => true,
            }
        }
        None => true,
    }
}

/// Check whether a file path is temporary
fn check_path_temp(p: &PathBuf) -> bool {
    match p.extension() {
        Some(e) => {
            match e.to_str() {
                Some(se) => se.ends_with("~"),
                _ => true,
            }
        }
        None => true,
    }
}

/// Check if a file exists
fn check_file(p: &PathBuf) -> bool {
    match std::fs::metadata(p) {
        Ok(metadata) => metadata.is_file() && metadata.len() > 0,
        Err(_) => false,
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
/// * `pc` Set of pending changes to mirror.
fn mirror_from_channel(session: &Session, rx: &Receiver<DebouncedEvent>,
    pc: &mut PendingChanges) -> Result<()>
{
    loop {
        pc.wait_receive(rx)?;
        if let Err(_) = pc.mirror_all(session) {
            break;
        }
    }
    Ok(())
}

/// Try to scp a file
///
/// * `session` SSH session.
/// * `p` Path to file.
fn try_scp_file(session: &Session, p: &PathBuf) -> Result<()> {
    let t = Instant::now();
    let r = scp_file(session, &p);
    match &r {
        Ok(_) => info!("copied {:?} in {:?}", p, t.elapsed()),
        Err(e) => error!("{}, copying {:?}", e, p),
    }
    r
}

/// Mirror one file with scp.
///
/// * `session` SSH session.
/// * `p` Path to file.
fn scp_file(session: &Session, p: &PathBuf) -> Result<()> {
    let mut fi = File::open(&p)?;
    let metadata = fi.metadata()?;
    let len = metadata.len();
    // Mask off higher mode bits to prevent scp_send
    // from returning a "file corrupt" error
    let mode = (metadata.permissions().mode() & 0o7777) as i32;
    debug!("copying {:?} with len: {} and mode {:o}", p, len, mode);
    let mut fo = session.scp_send(p.as_path(), mode, len, None)?;
    let c = std::io::copy(&mut fi, &mut fo)?;
    if c == len {
        debug!("copied {:?}", p);
    } else {
        error!("{:?}: length mismatch {} != {}", p, c, len);
    }
    Ok(())
}

/// Try to remove one file.
///
/// * `session` SSH session.
/// * `p` Path to file.
fn try_rm_file(session: &Session, p: &PathBuf) -> Result<()> {
    let t = Instant::now();
    let r = rm_file(session, &p);
    match &r {
        Ok(_) => info!("removed {:?} in {:?}", p, t.elapsed()),
        Err(e) => error!("{}, removing {:?}", e, p),
    }
    r
}

/// Remove one file.
///
/// * `session` SSH session.
/// * `p` Path to file.
fn rm_file(session: &Session, p: &PathBuf) -> Result<()> {
    debug!("removing {:?}", p);
    let mut channel = session.channel_session()?;
    let mut cmd = String::new();
    cmd.push_str("rm -f ");
    cmd.push_str(p.to_str().unwrap());
    channel.exec(&cmd)?;
    debug!("removed {:?}", p);
    Ok(())
}

/// Start mirror session.
///
/// * `host` Host name (and port) to connect.
/// * `username` Name of user to use for authentication.
/// * `rx` Channel receiver for path names.
/// * `pc` Set of pending changes to mirror.
fn start_session(host: &str, username: &str, rx: &Receiver<DebouncedEvent>,
    mut pc: &mut PendingChanges) -> Result<()>
{
    match create_session(host) {
        Ok(session) => {
            authenticate_session(&session, username)?;
            mirror_from_channel(&session, rx, &mut pc)?;
        },
        Err(e) => {
            error!("{}, host: {}", e, host);
            debug!("waiting for 10 seconds to try again");
            thread::sleep(Duration::from_secs(10));
        },
    }
    Ok(())
}

/// Mirror files received from channel.
///
/// * `host` Host name (and port) to connect.
/// * `username` Name of user to use for authentication.
/// * `rx` Channel receiver for path names.
pub fn mirror_files(host: &str, username: &str, rx: Receiver<DebouncedEvent>)
    -> Result<()>
{
    debug!("mirroring started for {}", host);
    let mut pc = PendingChanges::new();
    loop {
        pc.wait_receive(&rx)?;
        start_session(&host, username, &rx, &mut pc)?;
    }
}
