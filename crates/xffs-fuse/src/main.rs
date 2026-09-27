use clap::Parser;
use std::path::PathBuf;
#[derive(Parser)]
#[command(about = "Mount an XFFS image read-only in the foreground")]
struct Args {
    image: PathBuf,
    mountpoint: PathBuf,
    #[arg(long)]
    noexec: bool,
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a = Args::parse();
    if !std::path::Path::new("/dev/fuse").exists() {
        return Err(
            "unmet prerequisite: /dev/fuse is unavailable; use xffs-check for unmounted validation"
                .into(),
        );
    }
    let fs = xffs_core::ReadOnlyFs::open(&a.image)?;
    for d in fs.diagnostics() {
        eprintln!("{d}");
    }
    let adapter = xffs_fuse::Adapter::new(
        fs,
        nix::unistd::getuid().as_raw(),
        nix::unistd::getgid().as_raw(),
        a.noexec,
    );
    fuser::mount2(adapter, &a.mountpoint, &xffs_fuse::mount_config(a.noexec))?;
    Ok(())
}
