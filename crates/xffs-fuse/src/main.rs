use clap::Parser;
use std::path::PathBuf;
use xffs_core::{AccessMode, BlockDevice, ImageDevice, LinuxBlockDevice, reader::OpenOptions};
#[derive(Parser)]
#[command(about = "Mount XFFS in the foreground (read-only by default)")]
struct Args {
    image: PathBuf,
    mountpoint: PathBuf,
    #[arg(long)]
    noexec: bool,
    #[arg(long)]
    rw: bool,
    #[arg(long)]
    device: bool,
    #[arg(long, default_value_t = 128)]
    memory_mib: usize,
    #[arg(long, requires_all = ["gid", "device"])]
    uid: Option<u32>,
    #[arg(long, requires_all = ["uid", "device"])]
    gid: Option<u32>,
}
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn owner(
    current: (u32, u32),
    device: bool,
    explicit: Option<(u32, u32)>,
    sudo: Option<(u32, u32)>,
) -> Result<(u32, u32)> {
    if !device {
        return Ok(current);
    }
    let selected = explicit.or(sudo).unwrap_or(current);
    if current.0 == 0 {
        if selected.0 == 0 || selected.1 == 0 {
            return Err("root device mounts require a non-root invoking SUDO_UID/SUDO_GID or explicit --uid and --gid".into());
        }
    } else if selected != current {
        return Err("only root can select a different mounting uid/gid".into());
    }
    Ok(selected)
}
fn main() -> Result<()> {
    let a = Args::parse();
    let options = OpenOptions::with_memory_mib(a.memory_mib)?;
    if !std::path::Path::new("/dev/fuse").exists() {
        return Err(
            "unmet prerequisite: /dev/fuse is unavailable; use xffs-check for unmounted validation"
                .into(),
        );
    }
    let current = (
        nix::unistd::getuid().as_raw(),
        nix::unistd::getgid().as_raw(),
    );
    let sudo = std::env::var("SUDO_UID")
        .ok()
        .and_then(|s| s.parse().ok())
        .zip(std::env::var("SUDO_GID").ok().and_then(|s| s.parse().ok()));
    let (uid, gid) = owner(current, a.device, a.uid.zip(a.gid), sudo)?;
    let access = if a.rw {
        AccessMode::ReadWrite
    } else {
        AccessMode::ReadOnly
    };
    // Claim as root, then permanently drop privileges BEFORE recovery reads/writes.
    let device: Box<dyn BlockDevice + Send> = if a.device {
        let device = LinuxBlockDevice::open(&a.image, access)?;
        eprintln!("Claimed {}: {:?}", a.image.display(), device.info());
        Box::new(device)
    } else {
        Box::new(ImageDevice::open(&a.image, access)?)
    };
    if a.device && nix::unistd::geteuid().is_root() {
        nix::unistd::setgroups(&[])?;
        nix::unistd::setgid(nix::unistd::Gid::from_raw(gid))?;
        nix::unistd::setuid(nix::unistd::Uid::from_raw(uid))?;
        if nix::unistd::geteuid().as_raw() != uid
            || nix::unistd::getegid().as_raw() != gid
            || !nix::unistd::getgroups()?.is_empty()
        {
            return Err("privilege drop verification failed".into());
        }
    }
    let adapter = if a.rw {
        xffs_fuse::Adapter::new_writable(
            xffs_core::ReadWriteFs::from_device(device, options)?,
            uid,
            gid,
            a.noexec,
        )
    } else {
        let fs = xffs_core::ReadOnlyFs::from_device(device, options)?;
        for d in fs.diagnostics() {
            eprintln!("{d}");
        }
        xffs_fuse::Adapter::new(fs, uid, gid, a.noexec)
    };
    fuser::mount2(
        adapter,
        &a.mountpoint,
        &xffs_fuse::mount_config_writable(a.noexec, a.rw),
    )?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_root_requires_non_root_identity() {
        assert!(owner((0, 0), true, None, None).is_err());
        assert!(owner((0, 0), true, None, Some((0, 0))).is_err());
        assert_eq!(
            owner((0, 0), true, None, Some((1000, 1000))).unwrap(),
            (1000, 1000)
        );
        assert_eq!(
            owner((0, 0), true, Some((1001, 1002)), Some((1000, 1000))).unwrap(),
            (1001, 1002)
        );
        assert!(owner((1000, 1000), true, Some((1001, 1002)), None).is_err());
        assert_eq!(owner((0, 0), false, None, None).unwrap(), (0, 0));
    }
}
