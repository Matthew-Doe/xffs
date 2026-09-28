use clap::Parser;
use std::path::PathBuf;
#[derive(Parser)]
struct Args {
    image: PathBuf,
    #[arg(long)]
    size_mib: u64,
    #[arg(long)]
    uuid: uuid::Uuid,
    #[arg(long)]
    inodes: Option<u64>,
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u16).range(1..=2))]
    format_revision: u16,
}
fn main() -> xffs_tools::ToolResult<()> {
    let a = Args::parse();
    let bytes = a.size_mib.checked_mul(1024 * 1024).ok_or("size overflow")?;
    xffs_tools::create_image_revision(
        &a.image,
        bytes,
        *a.uuid.as_bytes(),
        a.inodes,
        None,
        xffs_core::format::FormatRevision::decode(a.format_revision)?,
    )?;
    println!("Created {}", a.image.display());
    Ok(())
}
