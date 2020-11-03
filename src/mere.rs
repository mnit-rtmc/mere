// mere.rs    Directory mirroring service
//
// Copyright (C) 2018-2020  Minnesota Department of Transportation
//
use anyhow::{anyhow, Context, Result};
use inotify::{Event, Inotify, WatchDescriptor, WatchMask};
use log::{debug, error, info, trace};
use ssh2::{OpenFlags, OpenType, RenameFlags, Session, Sftp};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

/// Use 64 KB buffers
const CAPACITY: usize = 64 * 1024;

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
            let dir = std::fs::canonicalize(dir)
                .with_context(|| format!("Invalid directory \"{}\"", dir))?;
            info!("  Directory {:?}", dir);
            let wd = inotify.add_watch(&dir, mask)
                .with_context(|| format!("Could not add watch {:?}", dir))?;
            dirs.insert(wd, dir);
        }
        Ok(PendingChanges {
            host,
            inotify,
            dirs,
            changes,
        })
    }

    /// Wait for watch events
    fn wait_events(&mut self) -> Result<()> {
        trace!("wait_events");
        let mut buffer = [0; 1024];
        while self.changes.is_empty() {
            let events = self.inotify.read_events_blocking(&mut buffer)?;
            for event in events {
                self.add_pending_change(event);
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
        trace!("check_more_events");
        let mut more = false;
        let events = self.inotify.read_events(&mut buffer)?;
        for event in events {
            more |= self.add_pending_change(event);
        }
        Ok(more)
    }

    /// Add a pending change
    fn add_pending_change(&mut self, event: Event<&OsStr>) -> bool {
        trace!("add_pending_change: {:?}", event);
        let dir = self.dirs.get(&event.wd);
        if let (Some(dir), Some(p)) = (dir, event.name) {
            let mut pb = dir.clone();
            pb.push(p);
            self.add_path(pb);
            return true;
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
        trace!("handle_session: {}", username);
        match create_session(&self.host) {
            Ok(session) => {
                authenticate_session(&session, username)?;
                self.mirror_session(&session)?;
            }
            Err(e) => {
                error!("{}, host: {}", e, self.host);
                debug!("waiting for 10 seconds to try again");
                thread::sleep(Duration::from_secs(10));
            }
        }
        Ok(())
    }

    /// Mirror files for one session.
    ///
    /// * `session` SSH session.
    fn mirror_session(&mut self, session: &Session) -> Result<()> {
        trace!("mirror_session");
        let sftp = session.sftp()?;
        self.mirror_all(&sftp)?;
        loop {
            self.wait_events()?;
            if self.mirror_pending(&sftp).is_err() {
                break;
            }
        }
        Ok(())
    }

    /// Mirror all files to destination host
    fn mirror_all(&self, sftp: &Sftp) -> Result<()> {
        trace!("mirror_all");
        for dir in self.dirs.values() {
            self.mirror_directory(sftp, &dir)?;
        }
        Ok(())
    }

    /// Mirror one directory to destination host
    fn mirror_directory(&self, sftp: &Sftp, dir: &Path) -> Result<()> {
        trace!("mirror_directory: {:?}", dir);
        let mut remote = sftp.readdir(dir)?;
        for entry in std::fs::read_dir(dir)? {
            if let Ok(entry) = entry {
                let path = entry.path();
                let pos = remote.iter().position(|p| (*p).0 == path);
                let rfile = pos.map(|i| remote.swap_remove(i));
                let copy = rfile.is_none()
                    || match entry.metadata() {
                        Ok(metadata) => {
                            let rstat = rfile.unwrap().1; // can't be none
                            let rlen = rstat.size.unwrap_or(0);
                            metadata.is_file() && metadata.len() != rlen
                        }
                        Err(e) => {
                            error!("metadata error {:?} on {:?}", e, &path);
                            false
                        }
                    };
                if copy {
                    try_copy_file(sftp, path.as_path())?;
                }
            }
        }
        // remove files which are not in source directory
        for (path, _) in remote {
            try_rm_file(sftp, path.as_path())?;
        }
        Ok(())
    }

    /// Mirror pending changes.
    ///
    /// * `sftp` Sftp instance.
    fn mirror_pending(&mut self, sftp: &Sftp) -> Result<()> {
        trace!("mirror_pending");
        for p in self.changes.drain() {
            let path = p.as_path();
            if check_file(path) {
                try_copy_file(sftp, path)?;
            } else {
                try_rm_file(sftp, path)?;
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
        Some(n) => match n.to_str() {
            Some(sn) => check_hidden(sn),
            _ => true,
        },
        None => true,
    }
}

/// For some reason, vim creates temporary files called 4913
const VIM_TEMP: &str = "4913";

/// Check whether a file name is hidden
fn check_hidden(sn: &str) -> bool {
    sn.starts_with('.') || sn == VIM_TEMP
}

/// Check whether a file path is temporary
fn check_path_temp(p: &Path) -> bool {
    match p.extension() {
        Some(e) => match e.to_str() {
            Some(se) => se.ends_with('~'),
            _ => true,
        },
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
        .map_err(|e| {
            error!("authentication failed for user {}", username);
            e
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

/// Try to copy a file
///
/// * `sftp` Sftp instance.
/// * `path` Path to file.
fn try_copy_file(sftp: &Sftp, path: &Path) -> Result<()> {
    let t = Instant::now();
    let res = copy_file(sftp, path);
    match &res {
        Ok(_) => info!("copied {:?} in {:?}", path, t.elapsed()),
        Err(e) => error!("{}, copying {:?}", e, path),
    }
    res
}

/// Create a temp file path
fn temp_file(path: &Path) -> PathBuf {
    let mut temp = PathBuf::new();
    temp.push(path.parent().unwrap());
    temp.push(".mere.temp");
    temp
}

/// Get sftp rename flags
fn rename_flags() -> Option<RenameFlags> {
    let mut flags = RenameFlags::empty();
    flags.insert(RenameFlags::OVERWRITE);
    flags.insert(RenameFlags::ATOMIC);
    flags.insert(RenameFlags::NATIVE);
    Some(flags)
}

/// Mirror one file with sftp.
///
/// * `sftp` Sftp instance.
/// * `path` Path to file.
fn copy_file(sftp: &Sftp, path: &Path) -> Result<()> {
    let temp = temp_file(path);
    let src = File::open(path)?;
    let metadata = src.metadata()?;
    let len = metadata.len();
    // Mask off higher mode bits to avoid a "file corrupt" error
    let mode = (metadata.permissions().mode() & 0o7777) as i32;
    debug!("copying {:?} with len: {} and mode {:o}", path, len, mode);
    let dst = sftp.open_mode(
        temp.as_path(),
        OpenFlags::TRUNCATE,
        mode,
        OpenType::File,
    )?;
    let mut src = io::BufReader::with_capacity(CAPACITY, src);
    let mut dst = io::BufWriter::with_capacity(CAPACITY, dst);
    let c = io::copy(&mut src, &mut dst)?;
    drop(dst);
    if c == len {
        sftp.rename(temp.as_path(), path, rename_flags())?;
        Ok(())
    } else {
        Err(anyhow!("copy length wrong: {} vs {}", c, len))
    }
}

/// Try to remove one file.
///
/// * `sftp` Sftp instance.
/// * `path` Path to file.
fn try_rm_file(sftp: &Sftp, path: &Path) -> Result<()> {
    let t = Instant::now();
    let res = rm_file(sftp, path);
    match &res {
        Ok(_) => info!("removed {:?} in {:?}", path, t.elapsed()),
        Err(e) => error!("{}, removing {:?}", e, path),
    }
    res
}

/// Remove one file.
///
/// * `sftp` Sftp instance.
/// * `path` Path to file.
fn rm_file(sftp: &Sftp, path: &Path) -> Result<()> {
    debug!("removing {:?}", path);
    sftp.unlink(path)?;
    Ok(())
}

/// Mirror files to another host.
///
/// * `host` Destination host.
/// * `directories` Slice of absolute directory names.
pub fn mirror_files(host: &str, directories: &[String]) -> Result<()> {
    let username = whoami::username();
    info!("Mirroring to {} as user {}", host, username);
    let mut pc = PendingChanges::new(host, directories)?;
    loop {
        pc.wait_events()?;
        pc.handle_session(&username)?;
    }
}
