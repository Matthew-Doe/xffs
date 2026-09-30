//! Read-only verification of an existing physical format, not mkfs.
use clap::Parser;
use xffs_core::{AccessMode, BlockDevice, LinuxBlockDevice, ReadOnlyFs, reader::OpenOptions};
#[derive(Parser)]
struct Args {
    device: std::path::PathBuf,
    #[arg(long)]
    expect_serial: String,
    #[arg(long)]
    expect_disk_sequence: u64,
    #[arg(long)]
    expect_bytes: u64,
    #[arg(long)]
    expect_uuid: uuid::Uuid,
    #[arg(long)]
    expect_inodes: u64,
}
fn main() -> xffs_tools::ToolResult<()> {
    let a = Args::parse();
    let mut d = LinuxBlockDevice::open(&a.device, AccessMode::ReadOnly)?;
    d.require_identity(Some(&a.expect_serial), Some(a.expect_disk_sequence))?;
    if d.capacity_bytes() != a.expect_bytes {
        return Err("capacity mismatch".into());
    }
    xffs_tools::verify_existing_format(&mut d, *a.expect_uuid.as_bytes(), a.expect_inodes)?;
    let fs = ReadOnlyFs::from_device(d, OpenOptions::with_memory_mib(512)?)?;
    println!(
        "Existing revision-2 format verified: UUID {}, {} inodes; {:?}",
        a.expect_uuid,
        a.expect_inodes,
        fs.statfs()
    );
    Ok(())
}
