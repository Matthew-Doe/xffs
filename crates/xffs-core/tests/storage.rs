#[path = "../../../tests/support/mod.rs"]
mod support;
use support::{TempImage, readonly_contract, writable_contract};
use xffs_core::{
    AccessMode::{ReadOnly, ReadWrite},
    BlockDevice, DeviceError, ImageDevice, Operation, validate_range,
};

#[test]
fn image_contract_and_host_flush_reopen() {
    let image = TempImage::new(32);
    {
        let mut device = ImageDevice::open(&image.0, ReadWrite).unwrap();
        writable_contract(&mut device);
    }
    let mut device = ImageDevice::open(&image.0, ReadOnly).unwrap();
    let mut bytes = [0; 6];
    device.read_at(3, &mut bytes).unwrap();
    assert_eq!(&bytes, b"abXYef");
    readonly_contract(&mut device);
    assert_eq!(std::fs::metadata(&image.0).unwrap().len(), 32);
}

#[test]
fn ranges_include_overflow_and_zero_capacity() {
    assert!(validate_range(u64::MAX, u64::MAX, 0).is_ok());
    assert!(validate_range(u64::MAX, u64::MAX, 1).is_err());
    assert!(validate_range(0, 0, 0).is_ok());
    assert!(validate_range(0, 0, 1).is_err());
    let image = TempImage::new(0);
    let mut device = ImageDevice::open(&image.0, ReadWrite).unwrap();
    device.write_at(0, &[]).unwrap();
    device.read_at(0, &mut []).unwrap();
    assert!(device.write_at(0, b"x").is_err());
}

#[test]
fn existing_regular_files_only() {
    let image = TempImage::new(8);
    let absent = image.0.with_extension("missing");
    assert!(matches!(
        ImageDevice::open(&absent, ReadWrite),
        Err(DeviceError::Io { .. })
    ));
    assert!(!absent.exists());
    assert!(matches!(
        ImageDevice::open(std::env::temp_dir(), ReadOnly),
        Err(DeviceError::UnsupportedFileType)
    ));
    #[cfg(unix)]
    assert!(matches!(
        ImageDevice::open("/dev/null", ReadWrite),
        Err(DeviceError::UnsupportedFileType)
    ));
}

// A concurrent process spawn can briefly inherit flock descriptors before exec.
// Serialize the drop assertion with the subprocess test.
static PROCESS_LOCK_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn locks_are_shared_or_exclusive_and_released_on_drop() {
    let _guard = PROCESS_LOCK_TEST.lock().unwrap();
    let image = TempImage::new(8);
    let first = ImageDevice::open(&image.0, ReadOnly).unwrap();
    let second = ImageDevice::open(&image.0, ReadOnly).unwrap();
    assert!(matches!(
        ImageDevice::open(&image.0, ReadWrite),
        Err(DeviceError::LockContention)
    ));
    drop((first, second));
    let writer = ImageDevice::open(&image.0, ReadWrite).unwrap();
    for access in [ReadOnly, ReadWrite] {
        assert!(matches!(
            ImageDevice::open(&image.0, access),
            Err(DeviceError::LockContention)
        ));
    }
    drop(writer);
    ImageDevice::open(&image.0, ReadWrite).unwrap();
}

#[test]
fn lock_child() {
    let Some(path) = std::env::var_os("XFFS_LOCK_TEST_PATH") else {
        return;
    };
    let shared = std::env::var_os("XFFS_LOCK_TEST_SHARED").is_some();
    let result = ImageDevice::open(&path, ReadOnly);
    if shared {
        assert!(result.is_ok());
    } else {
        assert!(matches!(result, Err(DeviceError::LockContention)));
    }
    assert!(matches!(
        ImageDevice::open(&path, ReadWrite),
        Err(DeviceError::LockContention)
    ));
}

#[test]
fn contention_from_separate_process() {
    let _guard = PROCESS_LOCK_TEST.lock().unwrap();
    let image = TempImage::new(8);
    for access in [ReadOnly, ReadWrite] {
        let holder = ImageDevice::open(&image.0, access).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "lock_child", "--nocapture"])
            .env("XFFS_LOCK_TEST_PATH", &image.0)
            .env_remove("XFFS_LOCK_TEST_SHARED");
        if access == ReadOnly {
            child.env("XFFS_LOCK_TEST_SHARED", "1");
        }
        let output = child.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        drop(holder);
    }
}

#[test]
fn unexpected_external_truncation_reports_partial_eof() {
    let image = TempImage::new(8);
    let mut device = ImageDevice::open(&image.0, ReadWrite).unwrap();
    // Unsupported external modification, deliberately exercised for error reporting.
    image.file().set_len(3).unwrap();
    let mut bytes = [9; 8];
    match device.read_at(0, &mut bytes).unwrap_err() {
        DeviceError::Io {
            operation,
            offset,
            transferred,
            source,
        } => {
            assert_eq!(operation, Operation::Read);
            assert_eq!(offset, Some(0));
            assert_eq!(transferred, Some(3));
            assert_eq!(source.kind(), std::io::ErrorKind::UnexpectedEof);
        }
        error => panic!("{error:?}"),
    }
    assert_eq!(bytes, [0, 0, 0, 9, 9, 9, 9, 9]);
}
