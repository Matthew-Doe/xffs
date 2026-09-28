use clap::Parser;
#[derive(Parser)]
struct Args {
    image: std::path::PathBuf,
    #[arg(long)]
    device: bool,
}
fn main() -> xffs_tools::ToolResult<()> {
    let a = Args::parse();
    let d = xffs_tools::open_target(&a.image, a.device, xffs_core::AccessMode::ReadOnly)?;
    println!("{}", xffs_tools::inspect_device(d)?);
    Ok(())
}
