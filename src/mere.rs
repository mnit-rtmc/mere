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
            let wd = inotify
                .add_watch(&dir, mask)
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
                self.add_change_event(event);
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
            more |= self.add_change_event(event);
        }
        Ok(more)
    }

    /// Add a change from a given inotify event
    fn add_change_event(&mut self, event: Event<&OsStr>) -> bool {
        trace!("add_change_event: {:?}", event);
        let dir = self.dirs.get(&event.wd);
        if let (Some(dir), Some(name)) = (dir, event.name) {
            let mut pb = dir.clone();
            pb.push(name);
            self.add_change_path(pb);
            return true;
        }
        debug!("ignored event: {:?}", event);
        false
    }

    /// Add a pending PathBuf change
    fn add_change_path(&mut self, path: PathBuf) {
        if is_path_valid(&path) {
            debug!("adding path: {:?}", path);
            self.changes.insert(path);
        } else {
            debug!("ignoring path: {:?}", path);
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
            self.mirror_pending(&sftp)?;
        }
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
        let mut remote = sftp
            .readdir(dir)
            .with_context(|| format!("sftp readdir {:?}", dir))?;
        for entry in std::fs::read_dir(dir)
            .with_context(|| format!("read_dir {:?}", dir))?
        {
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
            rm_file(sftp, path.as_path())?;
        }
        Ok(())
    }

    /// Mirror pending changes.
    ///
    /// * `sftp` Sftp instance.
    fn mirror_pending(&mut self, sftp: &Sftp) -> Result<()> {
        trace!("mirror_pending");
        for path in self.changes.drain() {
            if is_file(&path) {
                try_copy_file(sftp, &path)?;
            } else {
                rm_file(sftp, &path)?;
            }
        }
        Ok(())
    }
}

/// Check if a path is valid
fn is_path_valid(path: &Path) -> bool {
    path.is_absolute() && !is_path_hidden(path) && !is_path_temp(path)
}

/// Check whether a file path is hidden
fn is_path_hidden(path: &Path) -> bool {
    match path.file_name() {
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
fn is_path_temp(path: &Path) -> bool {
    match path.extension() {
        Some(e) => match e.to_str() {
            Some(se) => se.ends_with('~'),
            _ => true,
        },
        None => false,
    }
}

/// Check if a file exists
fn is_file(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(metadata) => metadata.is_file(),
        Err(_) => false,
    }
}

/// Create a new SSH session
///
/// * `host` Host name (and port) to connect.
fn create_session(host: &str) -> Result<Session> {
    trace!("create_session {}", host);
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
    trace!("authenticate_session {}", username);
    // First, try using key with no pass-phrase.  If that doesn't work,
    // try using agent auth -- maybe we're running interactively
    authenticate_pubkey(session, username)
        .or_else(|_| authenticate_agent(session, username))
        .with_context(|| {
            format!("authentication failed for user {}", username)
        })?;
    Ok(())
}

/// Authenticate an SSH session using public key.
///
/// * `session` SSH session.
/// * `username` User to authenticate.
fn authenticate_pubkey(session: &Session, username: &str) -> Result<()> {
    let mut key_file = PathBuf::new();
    key_file.push("/home");
    key_file.push(username);
    key_file.push(".ssh");
    key_file.push("id_rsa");
    session.userauth_pubkey_file(username, None, &key_file, None)?;
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
    copy_file(sftp, path).with_context(|| format!("copy failed {:?}", path))?;
    info!("copied {:?} in {:?}", path, t.elapsed());
    Ok(())
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

/// Copy one file with sftp.
///
/// * `sftp` Sftp instance.
/// * `path` Path to file.
fn copy_file(sftp: &Sftp, path: &Path) -> Result<()> {
    trace!("copy_file {:?}", path);
    let temp = temp_file(path);
    let src = File::open(path)?;
    let metadata = src.metadata()?;
    let len = metadata.len();
    // Mask off higher mode bits to avoid a "file corrupt" error
    let mode = (metadata.permissions().mode() & 0o7777) as i32;
    let dst = sftp.open_mode(
        temp.as_path(),
        OpenFlags::TRUNCATE,
        mode,
        OpenType::File,
    )?;
    let mut src = io::BufReader::with_capacity(CAPACITY, src);
    let mut dst = io::BufWriter::with_capacity(CAPACITY, dst);
    let copied = io::copy(&mut src, &mut dst)?;
    drop(dst);
    if copied == len {
        sftp.rename(temp.as_path(), path, rename_flags())?;
        Ok(())
    } else {
        Err(anyhow!("copy length wrong: {} != {}", copied, len))
    }
}

/// Remove a remote file.
///
/// * `sftp` Sftp instance.
/// * `path` Path to file.
fn rm_file(sftp: &Sftp, path: &Path) -> Result<()> {
    trace!("rm_file {:?}", path);
    let t = Instant::now();
    sftp.unlink(path)
        .with_context(|| format!("remove failed {:?}", path))?;
    info!("removed {:?} in {:?}", path, t.elapsed());
    Ok(())
}

/// Mirror files to another host.
///
/// * `dest` Destination host.
/// * `sources` Source directories to mirror.
pub fn mirror_files(dest: &str, sources: &[String]) -> Result<()> {
    let username = whoami::username();
    info!("Mirroring to {} as user {}", dest, username);
    let mut pc = PendingChanges::new(dest, sources)?;
    loop {
        pc.wait_events()?;
        pc.handle_session(&username)?;
    }
}
