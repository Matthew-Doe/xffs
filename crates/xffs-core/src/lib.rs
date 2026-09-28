//! Byte-addressed storage. See `docs/storage-contract.md` for durability semantics.
#![forbid(unsafe_code)]

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{DeviceInfo, LinuxBlockDevice};

mod image;
pub use image::ImageDevice;

use std::{fmt, io};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessMode {
    ReadOnly,
    ReadWrite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Open,
    Metadata,
    Lock,
    Read,
    Write,
    Flush,
}

#[derive(Debug)]
pub enum DeviceError {
    InvalidRange {
        offset: u64,
        length: usize,
        capacity: u64,
    },
    ReadOnly,
    DeviceRemoved,
    IdentityChanged,
    UnsafeTopology(String),
    UnsupportedGeometry,
    LockContention,
    UnsupportedFileType,
    Io {
        operation: Operation,
        offset: Option<u64>,
        transferred: Option<usize>,
        source: io::Error,
    },
    InjectedFault {
        operation: Operation,
        offset: Option<u64>,
        transferred: usize,
    },
    InvalidScenario {
        reason: &'static str,
    },
    ResourceLimit {
        resource: &'static str,
        limit: u64,
    },
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRange {
                offset,
                length,
                capacity,
            } => write!(
                f,
                "range at {offset} of length {length} exceeds capacity {capacity}"
            ),
            Self::DeviceRemoved => f.write_str("block device was removed"),
            Self::IdentityChanged => f.write_str("block device identity changed"),
            Self::UnsafeTopology(reason) => write!(f, "unsafe block-device topology: {reason}"),
            Self::UnsupportedGeometry => f.write_str("unsupported block-device geometry"),
            Self::ReadOnly => f.write_str("device is read-only"),
            Self::LockContention => f.write_str("device lock is held by another handle"),
            Self::UnsupportedFileType => {
                f.write_str("only existing regular image files are supported")
            }
            Self::Io {
                operation,
                offset,
                transferred,
                source,
            } => write!(
                f,
                "{operation:?} at {offset:?}, transferred {transferred:?}: {source}"
            ),
            Self::InjectedFault {
                operation,
                offset,
                transferred,
            } => write!(
                f,
                "injected {operation:?} failure at {offset:?} after {transferred} bytes"
            ),
            Self::InvalidScenario { reason } => write!(f, "invalid simulation scenario: {reason}"),
            Self::ResourceLimit { resource, limit } => {
                write!(f, "simulator {resource} limit ({limit}) reached")
            }
        }
    }
}

impl std::error::Error for DeviceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Validate before I/O, including empty requests and arithmetic overflow.
pub fn validate_range(capacity: u64, offset: u64, length: usize) -> Result<(), DeviceError> {
    if u64::try_from(length)
        .ok()
        .and_then(|n| offset.checked_add(n))
        .is_some_and(|end| end <= capacity)
    {
        Ok(())
    } else {
        Err(DeviceError::InvalidRange {
            offset,
            length,
            capacity,
        })
    }
}

/// Exact transfers; a successful write is visible but need not be durable.
/// A successful flush makes preceding writes durable under the backend contract.
pub trait BlockDevice {
    fn capacity_bytes(&self) -> u64;
    fn read_at(&mut self, offset: u64, destination: &mut [u8]) -> Result<(), DeviceError>;
    fn write_at(&mut self, offset: u64, source: &[u8]) -> Result<(), DeviceError>;
    fn flush(&mut self) -> Result<(), DeviceError>;
}

pub mod format;
pub mod names;

pub mod reader;
pub use reader::ReadOnlyFs;

pub mod writer;
pub use writer::ReadWriteFs;
