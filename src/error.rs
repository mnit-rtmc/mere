// error.rs    Directory mirroring service
//
// Copyright (c) 2019  Minnesota Department of Transportation
//
use ssh2;
use std::error::Error as _;
use std::{fmt, io};
use std::path::PathBuf;
use std::sync::mpsc::{SendError, RecvError, TryRecvError};

/// Error enum
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    MpscSend(SendError<PathBuf>),
    MpscRecv(RecvError),
    MpscTryRecv(TryRecvError),
    Notify(notify::Error),
    Ssh(ssh2::Error),
    InvalidArgs(),
}

/// Custom Result
pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.source() {
            Some(src) => fmt::Display::fmt(src, f),
            None => {
                match self {
                    Error::InvalidArgs() => f.write_str("invlaid args"),
                    _ => unreachable!(),
                }
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::MpscSend(e) => Some(e),
            Error::MpscRecv(e) => Some(e),
            Error::MpscTryRecv(e) => Some(e),
            Error::Notify(e) => Some(e),
            Error::Ssh(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<notify::Error> for Error {
    fn from(e: notify::Error) -> Self {
        Error::Notify(e)
    }
}

impl From<ssh2::Error> for Error {
    fn from(e: ssh2::Error) -> Self {
        Error::Ssh(e)
    }
}

impl From<SendError<PathBuf>> for Error {
    fn from(e: SendError<PathBuf>) -> Self {
        Error::MpscSend(e)
    }
}

impl From<RecvError> for Error {
    fn from(e: RecvError) -> Self {
        Error::MpscRecv(e)
    }
}

impl From<TryRecvError> for Error {
    fn from(e: TryRecvError) -> Self {
        Error::MpscTryRecv(e)
    }
}
