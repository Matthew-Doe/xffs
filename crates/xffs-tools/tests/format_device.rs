use xffs_core::{
    BlockDevice, DeviceError, Operation, ReadOnlyFs,
    format::{FormatRevision, Superblock},
    reader::OpenOptions,
};
use xffs_tools::format_empty;
struct Memory {
    bytes: Vec<u8>,
    remaining: usize,
    operations: usize,
    metadata_flushed: bool,
    flushes: usize,
}
impl Memory {
    fn new(remaining: usize) -> Self {
        Self {
            bytes: vec![0xcc; 16 * 1024 * 1024],
            remaining,
            operations: 0,
            metadata_flushed: false,
            flushes: 0,
        }
    }
    fn step(&mut self, operation: Operation) -> Result<(), DeviceError> {
        self.operations += 1;
        if self.remaining == 0 {
            return Err(DeviceError::InjectedFault {
                operation,
                offset: None,
                transferred: 0,
            });
        }
        self.remaining -= 1;
        Ok(())
    }
}
impl BlockDevice for Memory {
    fn capacity_bytes(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn read_at(&mut self, off: u64, out: &mut [u8]) -> Result<(), DeviceError> {
        out.copy_from_slice(&self.bytes[off as usize..off as usize + out.len()]);
        Ok(())
    }
    fn write_at(&mut self, off: u64, data: &[u8]) -> Result<(), DeviceError> {
        if Superblock::decode(data, off / 4096, self.capacity_bytes()).is_ok() {
            assert!(self.metadata_flushed, "published before metadata flush");
        }
        let result = self.step(Operation::Write);
        let count = if result.is_err() {
            data.len() / 2
        } else {
            data.len()
        };
        self.bytes[off as usize..off as usize + count].copy_from_slice(&data[..count]);
        result
    }
    fn flush(&mut self) -> Result<(), DeviceError> {
        self.step(Operation::Flush)?;
        self.flushes += 1;
        self.metadata_flushed = self.flushes >= 2;
        Ok(())
    }
}
#[test]
fn every_interrupted_write_and_flush_leaves_no_partial_filesystem() {
    let mut completed = Memory::new(usize::MAX);
    format_empty(&mut completed, [7; 16], Some(256), FormatRevision::Two).unwrap();
    let operations = completed.operations;
    let fs = ReadOnlyFs::from_device(completed, OpenOptions::default()).unwrap();
    assert_eq!(fs.superblock().revision, FormatRevision::Two);
    for stop in 0..operations {
        let mut d = Memory::new(stop);
        assert!(format_empty(&mut d, [7; 16], Some(256), FormatRevision::Two).is_err());
        let published = [0, d.capacity_bytes() - 4096].into_iter().any(|off| {
            Superblock::decode(
                &d.bytes[off as usize..off as usize + 4096],
                off / 4096,
                d.capacity_bytes(),
            )
            .is_ok()
        });
        if published {
            ReadOnlyFs::from_device(d, OpenOptions::default()).unwrap();
        }
    }
}
#[test]
fn invalid_geometry_does_not_mutate() {
    let mut d = Memory::new(usize::MAX);
    assert!(format_empty(&mut d, [0; 16], Some(u64::MAX), FormatRevision::Two).is_err());
    assert_eq!(d.operations, 0);
}
#[test]
fn serial_requires_exact_nonempty_match() {
    let mut info = xffs_core::DeviceInfo {
        capacity: 0,
        logical_sector: 512,
        physical_sector: 512,
        major: 8,
        minor: 16,
        serial: Some("0085199340190280".into()),
        usb: true,
        removable: true,
        disk_sequence: 1,
    };
    assert!(xffs_tools::validate_serial(&info, "wrong").is_err());
    assert!(xffs_tools::validate_serial(&info, "0085199340190280").is_ok());
    info.serial = None;
    assert!(xffs_tools::validate_serial(&info, "").is_err());
}

#[test]
#[ignore = "requires root and disposable Linux loop devices"]
fn loop_format_check_and_writable_reopen() {
    use std::{fs, process::Command};
    use xffs_core::{AccessMode, LinuxBlockDevice, ReadWriteFs, format::ROOT};
    for sector in [512, 4096] {
        let image = std::env::temp_dir().join(format!(
            "xffs-format-loop-{}-{sector}.img",
            std::process::id()
        ));
        fs::File::create_new(&image)
            .unwrap()
            .set_len(32 * 1024 * 1024)
            .unwrap();
        let output = Command::new("losetup")
            .args(["--find", "--show", "--sector-size", &sector.to_string()])
            .arg(&image)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let path = String::from_utf8(output.stdout).unwrap().trim().to_owned();
        struct Cleanup(String, std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = Command::new("losetup").args(["-d", &self.0]).status();
                let _ = fs::remove_file(&self.1);
            }
        }
        let _cleanup = Cleanup(path.clone(), image);
        let mut d = LinuxBlockDevice::open(&path, AccessMode::ReadWrite).unwrap();
        let info = d.info().clone();
        format_empty(&mut d, [8; 16], Some(256), FormatRevision::Two).unwrap();
        drop(d);
        xffs_tools::refresh_partition_view(std::path::Path::new(&path), &info).unwrap();
        let d = LinuxBlockDevice::open(&path, AccessMode::ReadWrite).unwrap();
        let mut fs = ReadWriteFs::from_device(d, OpenOptions::default()).unwrap();
        let id = fs.create(ROOT, b"loop.txt", false).unwrap();
        fs.write_file(id, 0, b"loop persistence").unwrap();
        fs.sync().unwrap();
        drop(fs);
        let d = LinuxBlockDevice::open(&path, AccessMode::ReadOnly).unwrap();
        let mut fs = ReadOnlyFs::from_device(d, OpenOptions::default()).unwrap();
        let id = fs.lookup(ROOT, b"loop.txt").unwrap();
        assert_eq!(fs.read_file(id, 0, 16).unwrap(), b"loop persistence");
    }
}
