#![cfg(target_os = "linux")]
use std::{fs, process::Command};
use xffs_core::{AccessMode, BlockDevice, DeviceError, LinuxBlockDevice};

struct Loop {
    path: String,
    image: std::path::PathBuf,
}
impl Drop for Loop {
    fn drop(&mut self) {
        let _ = Command::new("losetup").args(["-d", &self.path]).status();
        let _ = fs::remove_file(&self.image);
    }
}
fn attach(sector: u32) -> Loop {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let image = std::env::temp_dir().join(format!(
        "xffs-loop-{}-{sector}-{id}.img",
        std::process::id()
    ));
    fs::File::create_new(&image)
        .unwrap()
        .set_len(32 * 1024 * 1024)
        .unwrap();
    let result = Command::new("losetup")
        .args(["--find", "--show", "--sector-size", &sector.to_string()])
        .arg(&image)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    Loop {
        path: String::from_utf8(result.stdout).unwrap().trim().into(),
        image,
    }
}
/// Explicitly invoke as root; allocates only newly created disposable images.
#[test]
#[ignore = "requires root and disposable Linux loop devices"]
fn loop_contract_and_exclusion() {
    for sector in [512, 4096] {
        let target = attach(sector);
        let mut d = LinuxBlockDevice::open(&target.path, AccessMode::ReadWrite).unwrap();
        assert_eq!(d.info().logical_sector, sector);
        assert_eq!(d.capacity_bytes(), 32 * 1024 * 1024);
        assert!(matches!(
            LinuxBlockDevice::open(&target.path, AccessMode::ReadOnly),
            Err(DeviceError::LockContention)
        ));
        d.write_at(13, b"unaligned").unwrap();
        d.write_at(15, b"ABC").unwrap();
        d.flush().unwrap();
        let mut out = [0; 9];
        d.read_at(13, &mut out).unwrap();
        assert_eq!(&out, b"unABCgned");
        assert!(matches!(
            d.read_at(u64::MAX, &mut out),
            Err(DeviceError::InvalidRange { .. })
        ));
        drop(d);
        let mut d = LinuxBlockDevice::open(&target.path, AccessMode::ReadOnly).unwrap();
        assert!(matches!(d.write_at(0, &[]), Err(DeviceError::ReadOnly)));
        assert!(LinuxBlockDevice::open(&target.path, AccessMode::ReadOnly).is_err());
        d.read_at(13, &mut out).unwrap();
        assert_eq!(&out, b"unABCgned");
    }
}

#[test]
#[ignore = "requires root, sfdisk, ext4 tools, and disposable loop devices"]
fn mounted_child_is_refused() {
    use std::io::Write;
    use std::process::Stdio;
    let target = attach(512);
    let mut partitioner = Command::new("sfdisk")
        .arg(&target.path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    partitioner
        .stdin
        .take()
        .unwrap()
        .write_all(b"label: dos\n,16M,L\n")
        .unwrap();
    assert!(partitioner.wait().unwrap().success());
    let child = format!("{}p1", target.path);
    if !std::path::Path::new(&child).exists() {
        assert!(
            Command::new("partx")
                .args(["--add", &target.path])
                .status()
                .unwrap()
                .success()
        );
    }
    assert!(
        Command::new("mkfs.ext4")
            .args(["-q", "-F", &child])
            .status()
            .unwrap()
            .success()
    );
    let mount = std::env::temp_dir().join(format!("xffs-child-mount-{}", std::process::id()));
    fs::create_dir(&mount).unwrap();
    struct Mount(std::path::PathBuf);
    impl Drop for Mount {
        fn drop(&mut self) {
            let _ = Command::new("umount").arg(&self.0).status();
            let _ = fs::remove_dir(&self.0);
        }
    }
    let _mount = Mount(mount.clone());
    assert!(
        Command::new("mount")
            .arg(&child)
            .arg(&mount)
            .status()
            .unwrap()
            .success()
    );
    assert!(matches!(
        LinuxBlockDevice::open(&target.path, AccessMode::ReadWrite),
        Err(DeviceError::UnsafeTopology(_))
    ));
    assert!(matches!(
        LinuxBlockDevice::open(&child, AccessMode::ReadOnly),
        Err(DeviceError::UnsafeTopology(_))
    ));
}

#[test]
#[ignore = "requires root and disposable Linux loop devices"]
fn capacity_change_faults_the_retained_descriptor() {
    let target = attach(512);
    let mut d = LinuxBlockDevice::open(&target.path, AccessMode::ReadWrite).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&target.image)
        .unwrap()
        .set_len(16 * 1024 * 1024)
        .unwrap();
    assert!(
        Command::new("losetup")
            .args(["--set-capacity", &target.path])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(d.capacity_bytes(), 32 * 1024 * 1024);
    assert!(matches!(
        d.read_at(0, &mut [0; 1]),
        Err(DeviceError::IdentityChanged)
    ));
    assert!(matches!(
        d.write_at(0, b"no"),
        Err(DeviceError::IdentityChanged)
    ));
    assert!(matches!(d.flush(), Err(DeviceError::IdentityChanged)));
}
