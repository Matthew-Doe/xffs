use crate::{AccessMode, BlockDevice, DeviceError, Operation, validate_range};
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};

/// A fixed-capacity, locked handle to an existing regular file.
#[derive(Debug)]
pub struct ImageDevice {
    file: File,
    capacity: u64,
    access: AccessMode,
}

fn io_error(
    operation: Operation,
    offset: Option<u64>,
    transferred: Option<usize>,
    source: io::Error,
) -> DeviceError {
    DeviceError::Io {
        operation,
        offset,
        transferred,
        source,
    }
}

fn retry<T>(mut action: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    loop {
        match action() {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

impl ImageDevice {
    pub fn open(path: impl AsRef<Path>, access: AccessMode) -> Result<Self, DeviceError> {
        let path = path.as_ref();
        // Preflight avoids opening known special files (including blocking FIFOs).
        // Paths must not be concurrently replaced; recheck the actual handle below.
        if !retry(|| fs::metadata(path))
            .map_err(|e| io_error(Operation::Metadata, None, None, e))?
            .is_file()
        {
            return Err(DeviceError::UnsupportedFileType);
        }
        let file = retry(|| {
            OpenOptions::new()
                .read(true)
                .write(access == AccessMode::ReadWrite)
                .open(path)
        })
        .map_err(|e| io_error(Operation::Open, None, None, e))?;
        let metadata =
            retry(|| file.metadata()).map_err(|e| io_error(Operation::Metadata, None, None, e))?;
        if !metadata.is_file() {
            return Err(DeviceError::UnsupportedFileType);
        }
        loop {
            let result = match access {
                AccessMode::ReadOnly => file.try_lock_shared(),
                AccessMode::ReadWrite => file.try_lock(),
            };
            match result {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => return Err(DeviceError::LockContention),
                Err(TryLockError::Error(e)) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(TryLockError::Error(e)) => {
                    return Err(io_error(Operation::Lock, None, None, e));
                }
            }
        }
        // Capture size under the lock.
        let capacity = retry(|| file.metadata())
            .map_err(|e| io_error(Operation::Metadata, None, None, e))?
            .len();
        Ok(Self {
            file,
            capacity,
            access,
        })
    }

    fn seek(&mut self, offset: u64, operation: Operation) -> Result<(), DeviceError> {
        retry(|| self.file.seek(SeekFrom::Start(offset)))
            .map(|_| ())
            .map_err(|e| io_error(operation, Some(offset), Some(0), e))
    }
}

fn read_exact(
    reader: &mut impl Read,
    offset: u64,
    destination: &mut [u8],
) -> Result<(), DeviceError> {
    let mut transferred = 0;
    while transferred < destination.len() {
        let count = retry(|| reader.read(&mut destination[transferred..]))
            .map_err(|e| io_error(Operation::Read, Some(offset), Some(transferred), e))?;
        if count == 0 {
            return Err(io_error(
                Operation::Read,
                Some(offset),
                Some(transferred),
                io::ErrorKind::UnexpectedEof.into(),
            ));
        }
        transferred += count;
    }
    Ok(())
}

fn write_exact(writer: &mut impl Write, offset: u64, source: &[u8]) -> Result<(), DeviceError> {
    let mut transferred = 0;
    while transferred < source.len() {
        let count = retry(|| writer.write(&source[transferred..]))
            .map_err(|e| io_error(Operation::Write, Some(offset), Some(transferred), e))?;
        if count == 0 {
            return Err(io_error(
                Operation::Write,
                Some(offset),
                Some(transferred),
                io::ErrorKind::WriteZero.into(),
            ));
        }
        transferred += count;
    }
    Ok(())
}

impl BlockDevice for ImageDevice {
    fn capacity_bytes(&self) -> u64 {
        self.capacity
    }
    fn read_at(&mut self, offset: u64, destination: &mut [u8]) -> Result<(), DeviceError> {
        validate_range(self.capacity, offset, destination.len())?;
        if destination.is_empty() {
            return Ok(());
        }
        self.seek(offset, Operation::Read)?;
        read_exact(&mut self.file, offset, destination)
    }
    fn write_at(&mut self, offset: u64, source: &[u8]) -> Result<(), DeviceError> {
        validate_range(self.capacity, offset, source.len())?;
        if self.access == AccessMode::ReadOnly {
            return Err(DeviceError::ReadOnly);
        }
        if source.is_empty() {
            return Ok(());
        }
        self.seek(offset, Operation::Write)?;
        write_exact(&mut self.file, offset, source)
    }
    fn flush(&mut self) -> Result<(), DeviceError> {
        if self.access == AccessMode::ReadOnly {
            return Ok(());
        }
        retry(|| self.file.sync_all()).map_err(|e| io_error(Operation::Flush, None, None, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    struct ShortIo {
        steps: VecDeque<io::Result<usize>>,
        bytes: Vec<u8>,
    }
    impl Read for ShortIo {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let n = self.steps.pop_front().unwrap()?;
            out[..n].fill(7);
            Ok(n)
        }
    }
    impl Write for ShortIo {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            let n = self.steps.pop_front().unwrap()?;
            self.bytes.extend_from_slice(&input[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    fn scripted(steps: Vec<io::Result<usize>>) -> ShortIo {
        ShortIo {
            steps: steps.into(),
            bytes: vec![],
        }
    }
    fn check(error: DeviceError, operation: Operation, count: usize, kind: io::ErrorKind) {
        match error {
            DeviceError::Io {
                operation: op,
                offset,
                transferred,
                source,
            } => {
                assert_eq!(op, operation);
                assert_eq!(offset, Some(9));
                assert_eq!(transferred, Some(count));
                assert_eq!(source.kind(), kind);
            }
            other => panic!("{other:?}"),
        }
    }
    #[test]
    fn retries_interruptions_and_short_transfers() {
        let steps = || {
            vec![
                Err(io::ErrorKind::Interrupted.into()),
                Ok(1),
                Err(io::ErrorKind::Interrupted.into()),
                Ok(2),
            ]
        };
        let mut bytes = [0; 3];
        read_exact(&mut scripted(steps()), 9, &mut bytes).unwrap();
        assert_eq!(bytes, [7; 3]);
        let mut writer = scripted(steps());
        write_exact(&mut writer, 9, b"abc").unwrap();
        assert_eq!(writer.bytes, b"abc");
    }
    #[test]
    fn reports_eof_zero_progress_and_partial_errors() {
        for kind in [
            io::ErrorKind::UnexpectedEof,
            io::ErrorKind::PermissionDenied,
        ] {
            let end = if kind == io::ErrorKind::UnexpectedEof {
                Ok(0)
            } else {
                Err(kind.into())
            };
            let mut bytes = [0; 3];
            check(
                read_exact(&mut scripted(vec![Ok(1), end]), 9, &mut bytes).unwrap_err(),
                Operation::Read,
                1,
                kind,
            );
            assert_eq!(bytes, [7, 0, 0]);
        }
        for kind in [io::ErrorKind::WriteZero, io::ErrorKind::PermissionDenied] {
            let end = if kind == io::ErrorKind::WriteZero {
                Ok(0)
            } else {
                Err(kind.into())
            };
            let mut writer = scripted(vec![Ok(1), end]);
            check(
                write_exact(&mut writer, 9, b"abc").unwrap_err(),
                Operation::Write,
                1,
                kind,
            );
            assert_eq!(writer.bytes, b"a");
        }
    }
}
