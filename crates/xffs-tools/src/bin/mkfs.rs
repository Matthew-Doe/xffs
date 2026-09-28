use clap::Parser;
use std::path::PathBuf;
use xffs_core::{
    AccessMode, LinuxBlockDevice,
    format::{FormatRevision, VolumeLayout},
};
#[derive(Parser)]
struct Args {
    image: PathBuf,
    #[arg(long, required_unless_present = "device", conflicts_with = "device")]
    size_mib: Option<u64>,
    #[arg(long)]
    uuid: uuid::Uuid,
    #[arg(long)]
    inodes: Option<u64>,
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u16).range(1..=2))]
    format_revision: u16,
    #[arg(long, requires_all = ["erase", "expect_serial"])]
    device: bool,
    #[arg(long, requires = "device")]
    erase: bool,
    #[arg(long, requires = "device")]
    expect_serial: Option<String>,
}
fn main() -> xffs_tools::ToolResult<()> {
    let a = Args::parse();
    if a.device {
        if a.format_revision != 2 {
            return Err("physical devices require format revision 2".into());
        }
        let mut device = LinuxBlockDevice::open(&a.image, AccessMode::ReadWrite)?;
        xffs_tools::validate_serial(device.info(), a.expect_serial.as_deref().unwrap())?;
        let info = device.info().clone();
        let layout = VolumeLayout::new(info.capacity, a.inodes)?;
        println!(
            "Erasing {}: {info:?}\nLayout: {layout:?}",
            a.image.display()
        );
        device.verify_identity()?;
        xffs_tools::format_empty(
            &mut device,
            *a.uuid.as_bytes(),
            a.inodes,
            FormatRevision::Two,
        )?;
        drop(device);
        xffs_tools::refresh_partition_view(&a.image, &info)?;
        println!(
            "Formatted {}; kernel partition view verified",
            a.image.display()
        );
    } else {
        let bytes = a
            .size_mib
            .unwrap()
            .checked_mul(1024 * 1024)
            .ok_or("size overflow")?;
        xffs_tools::create_image_revision(
            &a.image,
            bytes,
            *a.uuid.as_bytes(),
            a.inodes,
            None,
            FormatRevision::decode(a.format_revision)?,
        )?;
        println!("Created {}", a.image.display());
    }
    Ok(())
}
