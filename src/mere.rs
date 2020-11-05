// mere.rs    Directory mirroring service
//
// Copyright (C) 2018-2020  Minnesota Department of Transportation
//
use anyhow::{anyhow, Context, Result};
use inotify::{Event, Inotify, WatchDescriptor, WatchMask};
use log::{debug, info, trace};
use ssh2::{FileStat, OpenFlags, OpenType, RenameFlags, Session, Sftp};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::{File, Metadata};
use std::io;
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

/// Use 64 KB buffers
const CAPACITY: usize = 64 * 1024;

/// Mirror paths to destination
struct Mirror {
    /// Destination host:port
    destination: String,
    /// Paths to mirror
    paths: HashSet<PathBuf>,
    /// User name
    username: String,
}

/// Watcher for mirroring
struct Watcher {
    /// Inotify watches
    inotify: Inotify,
    /// Map of watch descriptors to paths
    watches: HashMap<WatchDescriptor, PathBuf>,
}

/// Get the inotify watch mask
fn watch_mask() -> WatchMask {
    let mut mask = WatchMask::CLOSE_WRITE;
    mask.insert(WatchMask::DELETE);
    mask.insert(WatchMask::MOVE);
    mask
}

impl Mirror {
    /// Create a new mirror.
    ///
    /// * `destination` Destination host and port.
    fn new(destination: &str) -> Result<Self> {
        let destination = destination.to_string();
        let paths = HashSet::new();
        let username = whoami::username();
        info!("Mirroring to {} as user {}", destination, username);
        Ok(Mirror {
            destination,
            paths,
            username,
        })
    }

    /// Add a path to be mirrored
    fn add_path(&mut self, path: PathBuf) -> Result<bool> {
        let path = std::fs::canonicalize(&path)
            .with_context(|| format!("Invalid path {:?}", path))?;
        if is_path_valid(&path) {
            debug!("adding path: {:?}", path);
            self.paths.insert(path);
            Ok(true)
        } else {
            debug!("ignoring path: {:?}", path);
            Ok(false)
        }
    }

    /// Copy all paths
    fn copy_all(&mut self) -> Result<()> {
        trace!("copy_all");
        let session = self.session().map_err(|e| {
            debug!("waiting for 10 seconds to try again");
            thread::sleep(Duration::from_secs(10));
            e
        })?;
        self.mirror_session(&session)?;
        Ok(())
    }

    /// Get or create a session
    fn session(&self) -> Result<Session> {
        let session = create_session(&self.destination)?;
        authenticate_session(&session, &self.username)?;
        Ok(session)
    }

    /// Mirror files for one session.
    ///
    /// * `session` SSH session.
    fn mirror_session(&mut self, session: &Session) -> Result<()> {
        trace!("mirror_session");
        let sftp = session.sftp()?;
        self.mirror_all(&sftp)?;
        Ok(())
    }

    /// Mirror all paths.
    ///
    /// * `sftp` Sftp instance.
    fn mirror_all(&mut self, sftp: &Sftp) -> Result<()> {
        trace!("mirror_all");
        for path in self.paths.drain() {
            if let Ok(metadata) = std::fs::metadata(&path) {
                if metadata.is_dir() {
                    mirror_directory(sftp, &path)?;
                } else if metadata.is_file() {
                    mirror_file(sftp, &path)?;
                } else {
                    rm_file(sftp, &path)?;
                }
            }
        }
        Ok(())
    }
}

impl Watcher {
    /// Create a new watcher.
    fn new(mirror: &Mirror) -> Result<Self> {
        let mut inotify = Inotify::init()?;
        let mask = watch_mask();
        let mut watches = HashMap::new();
        for path in &mirror.paths {
            let wd = inotify
                .add_watch(&path, mask)
                .with_context(|| format!("Could not add watch {:?}", path))?;
            watches.insert(wd, path.clone());
        }
        Ok(Watcher { inotify, watches })
    }

    /// Wait for watch events
    fn wait_events(&mut self, mirror: &mut Mirror) -> Result<()> {
        trace!("wait_events");
        while mirror.paths.is_empty() {
            let mut buffer = [0; 1024];
            let events = self.inotify.read_events_blocking(&mut buffer)?;
            for event in events {
                if let Some(path) = self.event_path(event) {
                    mirror.add_path(path)?;
                }
            }
        }
        // Check for more events until there are none
        loop {
            thread::sleep(Duration::from_millis(50));
            if !self.check_more_events(mirror)? {
                break;
            }
        }
        Ok(())
    }

    /// Check for more watch events
    fn check_more_events(&mut self, mirror: &mut Mirror) -> Result<bool> {
        trace!("check_more_events");
        let mut buffer = [0; 1024];
        let mut more = false;
        let events = self.inotify.read_events(&mut buffer)?;
        for event in events {
            if let Some(path) = self.event_path(event) {
                more |= mirror.add_path(path)?;
            }
        }
        Ok(more)
    }

    /// Get path from an inotify event
    fn event_path(&self, event: Event<&OsStr>) -> Option<PathBuf> {
        trace!("event_path: {:?}", event);
        let path = self.watches.get(&event.wd);
        if let (Some(path), Some(name)) = (path, event.name) {
            let mut path = path.clone();
            path.push(name);
            return Some(path);
        }
        debug!("ignored event: {:?}", event);
        None
    }
}

/// Check if a path is valid
fn is_path_valid(path: &Path) -> bool {
    path.is_absolute() && !is_path_hidden(path) && !is_path_temp(path)
}

/// For some reason, vim creates temporary files called 4913
const VIM_TEMP: &str = "4913";

/// Check whether a file path is hidden
fn is_path_hidden(path: &Path) -> bool {
    path.file_name().map_or(true, |n| {
        n.to_str()
            .map_or(true, |sn| sn.starts_with('.') || sn == VIM_TEMP)
    })
}

/// Check whether a file path is temporary
fn is_path_temp(path: &Path) -> bool {
    path.extension()
        .map_or(false, |e| e.to_str().map_or(true, |se| se.ends_with('~')))
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

/// Mirror one directory to destination host
fn mirror_directory(sftp: &Sftp, dir: &Path) -> Result<()> {
    trace!("mirror_directory: {:?}", dir);
    let mut remote = sftp
        .readdir(dir)
        .with_context(|| format!("sftp readdir {:?}", dir))?;
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("read_dir {:?}", dir))?
    {
        if let Ok(entry) = entry {
            if let Ok(metadata) = entry.metadata() {
                let path = entry.path();
                let pos = remote.iter().position(|p| (*p).0 == path);
                let rfile = pos.map(|i| remote.swap_remove(i));
                if should_mirror(&metadata, rfile) {
                    mirror_file(sftp, &path)?;
                }
            }
        }
    }
    // remove files which are not in source directory
    for (path, _) in remote {
        rm_file(sftp, &path)?;
    }
    Ok(())
}

/// Check if a file should be mirrored
fn should_mirror(
    metadata: &Metadata,
    rfile: Option<(PathBuf, FileStat)>,
) -> bool {
    rfile.is_none() || {
        let rstat = rfile.unwrap().1; // can't be none
        let rlen = rstat.size.unwrap_or(0);
        metadata.is_file() && metadata.len() != rlen
    }
}

/// Mirror a file.
///
/// * `sftp` Sftp instance.
/// * `path` Path to file.
fn mirror_file(sftp: &Sftp, path: &Path) -> Result<()> {
    let t = Instant::now();
    copy_file(sftp, path).with_context(|| format!("copy failed {:?}", path))?;
    info!("copied {:?} in {:?}", path, t.elapsed());
    Ok(())
}

/// Create a temp file path
fn temp_file(path: &Path) -> PathBuf {
    let mut temp = PathBuf::new();
    temp.push(path.parent().unwrap());
    temp.push(".mere~");
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
    sftp.unlink(path)
        .with_context(|| format!("remove failed {:?}", path))?;
    info!("removed {:?}", path);
    Ok(())
}

/// Mirror files to another host.
///
/// * `dest` Destination host.
/// * `sources` Source directories to mirror.
pub fn mirror_files(dest: &str, sources: &[String]) -> Result<()> {
    trace!("mirror_files");
    let mut mirror = Mirror::new(dest)?;
    for dir in sources {
        mirror.add_path(dir.into())?;
    }
    let mut watcher = Watcher::new(&mirror)?;
    mirror.copy_all()?;
    loop {
        watcher.wait_events(&mut mirror)?;
        mirror.copy_all()?;
    }
}
