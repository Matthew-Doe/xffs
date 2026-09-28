use clap::Parser;
use std::path::PathBuf;
#[derive(Parser)]
#[command(about = "Mount an XFFS image in the foreground (read-only by default)")]
struct Args {
    image: PathBuf,
    mountpoint: PathBuf,
    #[arg(long)]
    noexec: bool,
    #[arg(long)]
    rw: bool,
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a = Args::parse();
    if !std::path::Path::new("/dev/fuse").exists() {
        return Err(
            "unmet prerequisite: /dev/fuse is unavailable; use xffs-check for unmounted validation"
                .into(),
        );
    }
    let uid = nix::unistd::getuid().as_raw();
    let gid = nix::unistd::getgid().as_raw();
    let adapter = if a.rw {
        xffs_fuse::Adapter::new_writable(
            xffs_core::ReadWriteFs::open(&a.image)?,
            uid,
            gid,
            a.noexec,
        )
    } else {
        let fs = xffs_core::ReadOnlyFs::open(&a.image)?;
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
