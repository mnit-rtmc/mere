// error.rs    Directory mirroring service
//
// Copyright (c) 2019  Minnesota Department of Transportation
//
use std::error::Error as _;
use std::{fmt, io};

/// Error enum
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Ssh(ssh2::Error),
    InvalidArgs(),
    ScpLength(),
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
                    Error::ScpLength() => f.write_str("scp length"),
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

impl From<ssh2::Error> for Error {
    fn from(e: ssh2::Error) -> Self {
        Error::Ssh(e)
    }
}
