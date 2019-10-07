// mere.rs    Directory mirroring service
//
// Copyright (C) 2018-2019  Minnesota Department of Transportation
//
use crate::error::{Result, Error::ScpLength};
use inotify::{Event, EventMask, Inotify, WatchDescriptor, WatchMask};
use log::{debug, error, info, trace};
use ssh2::Session;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::File;
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

/// Pending changes to mirror
struct PendingChanges {
    /// Mirror destination host:port
    host: String,
    /// Inotify watches
    inotify: Inotify,
    /// Map of watch descriptors to paths
    dirs: HashMap<WatchDescriptor, PathBuf>,
    /// Set of pending changes
    changes: HashSet<PathBuf>,
}

/// Get the inotify watch mask
fn watch_mask() -> WatchMask {
    let mut mask = WatchMask::CLOSE_WRITE;
    mask.insert(WatchMask::DELETE);
    mask.insert(WatchMask::MOVE);
    mask
}

impl PendingChanges {
    /// Create new pending changes
    ///
    /// * `host` Host name (and port) to connect.
    /// * `directories` Slice of directories.
    fn new(host: &str, directories: &[String]) -> Result<Self> {
        let host = host.to_string();
        let mut inotify = Inotify::init()?;
        let changes = HashSet::new();
        let mask = watch_mask();
        let mut dirs = HashMap::new();
        for dir in directories {
            info!("  Directory {:}", dir);
            let wd = inotify.add_watch(dir, mask)?;
            dirs.insert(wd, dir.into());
        }
        Ok(PendingChanges { host, inotify, dirs, changes })
    }
    /// Wait for watch events
    fn wait_events(&mut self) -> Result<()> {
        trace!("waiting for watch events");
        let mut buffer = [0; 1024];
        while self.changes.is_empty() {
            let events = self.inotify.read_events_blocking(&mut buffer)?;
            for event in events {
                self.add_change(event);
            }
        }
        // Check for more events until there are none
        loop {
            thread::sleep(Duration::from_millis(50));
            if !self.check_more_events(&mut buffer)? {
                break;
            }
        }
        Ok(())
    }
    /// Check for more watch events
    fn check_more_events(&mut self, mut buffer: &mut [u8]) -> Result<bool> {
        let mut more = false;
        let events = self.inotify.read_events(&mut buffer)?;
        for event in events {
            more |= self.add_change(event);
        }
        Ok(more)
    }
    /// Add a pending change
    fn add_change(&mut self, event: Event<&OsStr>) -> bool {
        trace!("notify event: {:?}", event);
        let dir = self.dirs.get(&event.wd);
        match (dir, event.name) {
            (Some(dir), Some(p)) => {
                if event.mask.contains(EventMask::CREATE) ||
                   event.mask.contains(EventMask::CLOSE_WRITE) ||
                   event.mask.contains(EventMask::DELETE) ||
                   event.mask.contains(EventMask::MOVED_FROM) ||
                   event.mask.contains(EventMask::MOVED_TO)
                {
                    let mut pb = dir.clone();
                    pb.push(p);
                    self.add_path(pb);
                    return true;
                }
            }
            _ => (),
        }
        trace!("ignored event: {:?}", event);
        false
    }
    /// Add a pending PathBuf change
    fn add_path(&mut self, p: PathBuf) {
        if check_path(p.as_ref()) {
            debug!("adding path: {:?}", p);
            self.changes.insert(p);
        } else {
            debug!("ignoring path: {:?}", p);
        }
    }
    /// Handle an SSH session.
    ///
    /// * `username` Name of user to use for authentication.
    fn handle_session(&mut self, username: &str) -> Result<()> {
        match create_session(&self.host) {
            Ok(session) => {
                authenticate_session(&session, username)?;
                self.mirror_session(&session)?;
            },
            Err(e) => {
                error!("{}, host: {}", e, self.host);
                debug!("waiting for 10 seconds to try again");
                thread::sleep(Duration::from_secs(10));
            },
        }
        Ok(())
    }
    /// Mirror files for one session.
    ///
    /// * `session` SSH session.
    fn mirror_session(&mut self, session: &Session) -> Result<()> {
        loop {
            self.wait_events()?;
            if let Err(_) = self.mirror_pending(session) {
                break;
            }
        }
        Ok(())
    }
    /// Mirror pending changes.
    ///
    /// * `session` SSH session.
    fn mirror_pending(&mut self, session: &Session) -> Result<()> {
        for p in self.changes.drain() {
            if check_file(p.as_ref()) {
                try_scp_file(session, &p)?;
            } else {
                try_rm_file(session, &p)?;
            }
        }
        Ok(())
    }
}

/// Check if a path is valid
fn check_path(p: &Path) -> bool {
    p.is_absolute() && !check_path_hidden(p) && !check_path_temp(p)
}

/// Check whether a file path is hidden
fn check_path_hidden(p: &Path) -> bool {
    match p.file_name() {
        Some(n) => {
            match n.to_str() {
                Some(sn) => check_hidden(sn),
                _ => true,
            }
        }
        None => true,
    }
}

/// For some reason, vim creates temporary files called 4913
const VIM_TEMP: &str = "4913";

/// Check whether a file name is hidden
fn check_hidden(sn: &str) -> bool {
    sn.starts_with(".") || sn == VIM_TEMP
}

/// Check whether a file path is temporary
fn check_path_temp(p: &Path) -> bool {
    match p.extension() {
        Some(e) => {
            match e.to_str() {
                Some(se) => se.ends_with("~"),
                _ => true,
            }
        }
        None => false,
    }
}

/// Check if a file exists
fn check_file(p: &Path) -> bool {
    match std::fs::metadata(p) {
        Ok(metadata) => metadata.is_file(),
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
        Ok(())
    } else {
        Err(ScpLength(c, len))
    }
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
    Ok(())
}

/// Mirror files to another host.
///
/// * `host` Destination host.
/// * `directories` Slice of absolute directory names.
pub fn mirror_files(host: &str, directories: &[String]) -> Result<()> {
    let username = whoami::username();
    info!("Mirroring to {:} as user {:}", host, username);
    let mut pc = PendingChanges::new(host, directories)?;
    loop {
        pc.wait_events()?;
        pc.handle_session(&username)?;
    }
}
