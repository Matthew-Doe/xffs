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
}
fn main() -> xffs_tools::ToolResult<()> {
    let a = Args::parse();
    let bytes = a.size_mib.checked_mul(1024 * 1024).ok_or("size overflow")?;
    xffs_tools::create_image(&a.image, bytes, *a.uuid.as_bytes(), a.inodes, None)?;
    println!("Created {}", a.image.display());
    Ok(())
}
