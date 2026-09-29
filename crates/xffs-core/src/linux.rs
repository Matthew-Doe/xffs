//! Exclusive, whole-disk Linux backend. Never reopens a device after removal.
use crate::image::{io_error, read_exact, retry, write_exact};
use crate::{AccessMode, BlockDevice, DeviceError, Operation, validate_range};
use rustix::fs::{Mode, OFlags};
use std::{
    fs::{self, File, TryLockError},
    io::{Seek, SeekFrom},
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub capacity: u64,
    pub logical_sector: u32,
    pub physical_sector: u32,
    pub major: u32,
    pub minor: u32,
    pub serial: Option<String>,
    pub usb: bool,
    pub removable: bool,
    pub disk_sequence: u64,
}

#[derive(Debug)]
pub struct LinuxBlockDevice {
    file: File,
    sysfs: PathBuf,
    info: DeviceInfo,
    access: AccessMode,
}
fn metadata_error(e: std::io::Error) -> DeviceError {
    io_error(Operation::Metadata, None, None, e)
}
fn value(path: &Path) -> Result<String, DeviceError> {
    fs::read_to_string(path)
        .map(|s| s.trim().to_owned())
        .map_err(metadata_error)
}
fn number(path: &Path) -> Result<u64, DeviceError> {
    value(path)?
        .parse()
        .map_err(|_| DeviceError::UnsupportedGeometry)
}
fn optional(path: &Path) -> Result<Option<String>, DeviceError> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s.trim().to_owned())),
        // Some SCSI sysfs serial attributes exist but return ENXIO when the
        // transport supplies no serial; the USB ancestor can still have one.
        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound
                || e.raw_os_error() == Some(rustix::io::Errno::NXIO.raw_os_error()) =>
        {
            Ok(None)
        }
        Err(e) => Err(metadata_error(e)),
    }
}
fn identity(sysfs: &Path, major: u32, minor: u32) -> Result<DeviceInfo, DeviceError> {
    let canonical = fs::canonicalize(sysfs).map_err(metadata_error)?;
    if sysfs.join("partition").exists() {
        return Err(DeviceError::UnsafeTopology(
            "partition targets are not supported".into(),
        ));
    }
    let mut serial = optional(&sysfs.join("device/serial"))?;
    let mut usb = false;
    for ancestor in canonical.ancestors() {
        if let Ok(subsystem) = fs::read_link(ancestor.join("subsystem"))
            && subsystem.file_name().is_some_and(|s| s == "usb")
        {
            usb = true;
            if serial.is_none() {
                serial = optional(&ancestor.join("serial"))?;
            }
        }
    }
    Ok(DeviceInfo {
        capacity: number(&sysfs.join("size"))?
            .checked_mul(512)
            .ok_or(DeviceError::UnsupportedGeometry)?,
        logical_sector: number(&sysfs.join("queue/logical_block_size"))?
            .try_into()
            .map_err(|_| DeviceError::UnsupportedGeometry)?,
        physical_sector: number(&sysfs.join("queue/physical_block_size"))?
            .try_into()
            .map_err(|_| DeviceError::UnsupportedGeometry)?,
        major,
        minor,
        serial,
        usb,
        removable: number(&sysfs.join("removable"))? != 0,
        disk_sequence: number(&sysfs.join("diskseq"))?,
    })
}
fn members(sysfs: &Path) -> Result<Vec<PathBuf>, DeviceError> {
    let mut paths = vec![sysfs.to_owned()];
    for entry in fs::read_dir(sysfs).map_err(metadata_error)? {
        let path = entry.map_err(metadata_error)?.path();
        if path.join("partition").exists() {
            paths.push(path);
        }
    }
    Ok(paths)
}
fn mount_ids(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .filter_map(|line| line.split_whitespace().nth(2))
}
fn topology(sysfs: &Path) -> Result<(), DeviceError> {
    let paths = members(sysfs)?;
    let ids = paths
        .iter()
        .map(|p| value(&p.join("dev")))
        .collect::<Result<Vec<_>, _>>()?;
    for path in paths {
        if fs::read_dir(path.join("holders"))
            .map_err(metadata_error)?
            .next()
            .is_some()
        {
            return Err(DeviceError::UnsafeTopology("active holders".into()));
        }
    }
    let mounts = fs::read_to_string("/proc/self/mountinfo").map_err(metadata_error)?;
    if mount_ids(&mounts).any(|id| ids.iter().any(|candidate| candidate == id)) {
        return Err(DeviceError::UnsafeTopology(
            "target or child is mounted".into(),
        ));
    }
    // Source-device checks also cover filesystems such as btrfs with anonymous st_dev.
    for line in mounts.lines() {
        if let Some(after) = line.split(" - ").nth(1)
            && let Some(source) = after.split_whitespace().nth(1)
            && source.starts_with("/dev/")
            && let Ok(meta) = fs::metadata(source)
        {
            let id = format!(
                "{}:{}",
                rustix::fs::major(meta.rdev()),
                rustix::fs::minor(meta.rdev())
            );
            if ids.contains(&id) {
                return Err(DeviceError::UnsafeTopology(
                    "mounted filesystem source".into(),
                ));
            }
        }
    }
    // Refuse every member of mounted multi-device btrfs volumes, not just the source.
    if let Ok(volumes) = fs::read_dir("/sys/fs/btrfs") {
        for volume in volumes {
            let volume = volume.map_err(metadata_error)?.path();
            if let Ok(devices) = fs::read_dir(volume.join("devices")) {
                for device in devices {
                    let id = value(&device.map_err(metadata_error)?.path().join("dev"))?;
                    if ids.contains(&id) {
                        return Err(DeviceError::UnsafeTopology("active btrfs member".into()));
                    }
                }
            }
        }
    }
    let swaps = fs::read_to_string("/proc/swaps").map_err(metadata_error)?;
    for line in swaps.lines().skip(1) {
        if let Some(path) = line.split_whitespace().next() {
            let meta = fs::metadata(path).map_err(metadata_error)?;
            let dev = if meta.file_type().is_block_device() {
                meta.rdev()
            } else {
                meta.dev()
            };
            let id = format!("{}:{}", rustix::fs::major(dev), rustix::fs::minor(dev));
            if ids.contains(&id) {
                return Err(DeviceError::UnsafeTopology("active swap".into()));
            }
        }
    }
    Ok(())
}
impl LinuxBlockDevice {
    pub fn open(path: impl AsRef<Path>, access: AccessMode) -> Result<Self, DeviceError> {
        let path = path.as_ref();
        let meta = fs::metadata(path).map_err(metadata_error)?;
        if !meta.file_type().is_block_device() {
            return Err(DeviceError::UnsupportedFileType);
        }
        let major = rustix::fs::major(meta.rdev());
        let minor = rustix::fs::minor(meta.rdev());
        let sysfs = PathBuf::from(format!("/sys/dev/block/{major}:{minor}"));
        let info = identity(&sysfs, major, minor)?;
        topology(&sysfs)?;
        let flags = OFlags::EXCL
            | OFlags::CLOEXEC
            | OFlags::NONBLOCK
            | if access == AccessMode::ReadOnly {
                OFlags::RDONLY
            } else {
                OFlags::RDWR
            };
        let fd = rustix::fs::open(path, flags, Mode::empty()).map_err(|e| {
            if e == rustix::io::Errno::BUSY {
                DeviceError::LockContention
            } else {
                io_error(Operation::Open, None, None, e.into())
            }
        })?;
        let mut file = File::from(fd);
        let opened = file.metadata().map_err(metadata_error)?;
        if !opened.file_type().is_block_device() || opened.rdev() != meta.rdev() {
            return Err(DeviceError::IdentityChanged);
        }
        match retry(|| {
            file.try_lock().map_err(|e| match e {
                TryLockError::WouldBlock => std::io::ErrorKind::WouldBlock.into(),
                TryLockError::Error(e) => e,
            })
        }) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(DeviceError::LockContention);
            }
            Err(e) => return Err(io_error(Operation::Lock, None, None, e)),
        }
        topology(&sysfs)?;
        if identity(&sysfs, major, minor)? != info {
            return Err(DeviceError::IdentityChanged);
        }
        let capacity = file.seek(SeekFrom::End(0)).map_err(metadata_error)?;
        let logical = rustix::fs::ioctl_blksszget(&file).map_err(|e| metadata_error(e.into()))?;
        let physical = rustix::fs::ioctl_blkpbszget(&file).map_err(|e| metadata_error(e.into()))?;
        if capacity != info.capacity
            || logical != info.logical_sector
            || physical != info.physical_sector
        {
            return Err(DeviceError::IdentityChanged);
        }
        if !matches!(logical, 512 | 4096) || capacity == 0 || capacity % 4096 != 0 {
            return Err(DeviceError::UnsupportedGeometry);
        }
        Ok(Self {
            file,
            sysfs,
            info,
            access,
        })
    }
    pub fn info(&self) -> &DeviceInfo {
        &self.info
    }
    /// Bind an externally selected target to this claimed descriptor before use.
    pub fn require_identity(
        &self,
        serial: Option<&str>,
        disk_sequence: Option<u64>,
    ) -> Result<(), DeviceError> {
        if serial.is_some_and(|expected| {
            expected.is_empty() || self.info.serial.as_deref() != Some(expected)
        }) || disk_sequence.is_some_and(|expected| expected != self.info.disk_sequence)
        {
            return Err(DeviceError::IdentityChanged);
        }
        Ok(())
    }
    pub fn verify_identity(&mut self) -> Result<(), DeviceError> {
        if !self.sysfs.exists() {
            return Err(DeviceError::DeviceRemoved);
        }
        if number(&self.sysfs.join("diskseq"))? != self.info.disk_sequence
            || number(&self.sysfs.join("size"))?.checked_mul(512) != Some(self.info.capacity)
        {
            return Err(DeviceError::IdentityChanged);
        }
        let capacity = self.file.seek(SeekFrom::End(0)).map_err(metadata_error)?;
        if capacity != self.info.capacity {
            return Err(DeviceError::DeviceRemoved);
        }
        Ok(())
    }
    fn seek(&mut self, offset: u64, operation: Operation) -> Result<(), DeviceError> {
        retry(|| self.file.seek(SeekFrom::Start(offset)))
            .map(|_| ())
            .map_err(|e| io_error(operation, Some(offset), Some(0), e))
    }
}
impl BlockDevice for LinuxBlockDevice {
    fn capacity_bytes(&self) -> u64 {
        self.info.capacity
    }
    fn read_at(&mut self, offset: u64, destination: &mut [u8]) -> Result<(), DeviceError> {
        validate_range(self.info.capacity, offset, destination.len())?;
        self.verify_identity()?;
        self.seek(offset, Operation::Read)?;
        read_exact(&mut self.file, offset, destination)
    }
    fn write_at(&mut self, offset: u64, source: &[u8]) -> Result<(), DeviceError> {
        validate_range(self.info.capacity, offset, source.len())?;
        if self.access == AccessMode::ReadOnly {
            return Err(DeviceError::ReadOnly);
        }
        self.verify_identity()?;
        self.seek(offset, Operation::Write)?;
        write_exact(&mut self.file, offset, source)
    }
    fn flush(&mut self) -> Result<(), DeviceError> {
        self.verify_identity()?;
        if self.access == AccessMode::ReadOnly {
            return Ok(());
        }
        retry(|| self.file.sync_all()).map_err(|e| io_error(Operation::Flush, None, None, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_mount_device_field_only() {
        assert_eq!(
            mount_ids("42 1 8:17 / /x rw - ext4 /dev/sdb1 rw\n43 1 0:1 / /y rw - tmpfs tmpfs rw")
                .collect::<Vec<_>>(),
            ["8:17", "0:1"]
        );
    }
    #[test]
    fn refuses_regular_files() {
        assert!(matches!(
            LinuxBlockDevice::open("Cargo.toml", AccessMode::ReadOnly),
            Err(DeviceError::UnsupportedFileType)
        ));
    }
    #[test]
    fn retained_descriptor_detects_disappearance_and_changed_sequence() {
        let dir = std::env::temp_dir().join(format!("xffs-identity-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let backing = dir.join("backing");
        let file = File::create_new(&backing).unwrap();
        file.set_len(4096).unwrap();
        fs::write(dir.join("diskseq"), "9").unwrap();
        fs::write(dir.join("size"), "8").unwrap();
        let mut device = LinuxBlockDevice {
            file,
            sysfs: dir.clone(),
            access: AccessMode::ReadWrite,
            info: DeviceInfo {
                capacity: 4096,
                logical_sector: 512,
                physical_sector: 512,
                major: 7,
                minor: 0,
                serial: None,
                usb: false,
                removable: false,
                disk_sequence: 9,
            },
        };
        device.verify_identity().unwrap();
        device.require_identity(None, Some(9)).unwrap();
        assert!(matches!(
            device.require_identity(Some("wrong"), None),
            Err(DeviceError::IdentityChanged)
        ));
        assert!(matches!(
            device.require_identity(None, Some(10)),
            Err(DeviceError::IdentityChanged)
        ));
        fs::write(dir.join("diskseq"), "10").unwrap();
        assert!(matches!(
            device.verify_identity(),
            Err(DeviceError::IdentityChanged)
        ));
        fs::remove_dir_all(&dir).unwrap();
        assert!(matches!(
            device.verify_identity(),
            Err(DeviceError::DeviceRemoved)
        ));
        assert!(matches!(
            device.write_at(0, b"no"),
            Err(DeviceError::DeviceRemoved)
        ));
    }
}
