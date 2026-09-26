use std::{
    fs::{File, OpenOptions},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use xffs_core::{BlockDevice, DeviceError};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
pub struct TempImage(pub PathBuf);
impl TempImage {
    pub fn new(capacity: u64) -> Self {
        loop {
            let id = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("xffs-{}-{id}.img", std::process::id()));
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => {
                    file.set_len(capacity).unwrap();
                    return Self(path);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("create temporary image: {e}"),
            }
        }
    }
    pub fn file(&self) -> File {
        OpenOptions::new().write(true).open(&self.0).unwrap()
    }
}
impl Drop for TempImage {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).unwrap();
    }
}

/// Run unchanged against a newly zeroed 32-byte device of either backend.
pub fn writable_contract(device: &mut impl BlockDevice) {
    assert_eq!(device.capacity_bytes(), 32);
    let mut all = [1; 32];
    device.read_at(0, &mut all).unwrap();
    assert_eq!(all, [0; 32]);
    device.write_at(3, b"abcdef").unwrap();
    device.write_at(5, b"XY").unwrap();
    let mut part = [0; 6];
    device.read_at(3, &mut part).unwrap();
    assert_eq!(&part, b"abXYef");
    device.flush().unwrap();
    device.flush().unwrap();
    device.read_at(3, &mut part).unwrap();
    assert_eq!(&part, b"abXYef");
    for offset in [0, 1, 31, 32] {
        device.read_at(offset, &mut []).unwrap();
        device.write_at(offset, &[]).unwrap();
    }
    for (offset, length) in [(33, 0), (32, 1), (31, 2), (u64::MAX, 2), (u64::MAX, 0)] {
        let mut destination = vec![99; length];
        assert!(matches!(
            device.read_at(offset, &mut destination),
            Err(DeviceError::InvalidRange { .. })
        ));
        assert_eq!(destination, vec![99; length]);
        assert!(matches!(
            device.write_at(offset, &vec![99; length]),
            Err(DeviceError::InvalidRange { .. })
        ));
    }
    device.read_at(3, &mut part).unwrap();
    assert_eq!(&part, b"abXYef");
    device.write_at(31, &[42]).unwrap();
    device.flush().unwrap();
    let mut last = [0];
    device.read_at(31, &mut last).unwrap();
    assert_eq!(last, [42]);
}

pub fn readonly_contract(device: &mut impl BlockDevice) {
    let mut data = [0; 1];
    device.read_at(0, &mut data).unwrap();
    assert!(matches!(
        device.write_at(0, b"x"),
        Err(DeviceError::ReadOnly)
    ));
    assert!(matches!(
        device.write_at(0, &[]),
        Err(DeviceError::ReadOnly)
    ));
    assert!(matches!(
        device.write_at(u64::MAX, b"x"),
        Err(DeviceError::InvalidRange { .. })
    ));
    device.flush().unwrap();
}
