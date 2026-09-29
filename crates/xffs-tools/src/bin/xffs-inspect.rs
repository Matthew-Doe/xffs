use clap::Parser;
#[derive(Parser)]
struct Args {
    image: std::path::PathBuf,
    #[arg(long)]
    device: bool,
    #[arg(long, requires = "device")]
    expect_serial: Option<String>,
    #[arg(long, requires = "device")]
    expect_disk_sequence: Option<u64>,
}
fn main() -> xffs_tools::ToolResult<()> {
    let a = Args::parse();
    let d = xffs_tools::open_target_identified(
        &a.image,
        a.device,
        xffs_core::AccessMode::ReadOnly,
        a.expect_serial.as_deref(),
        a.expect_disk_sequence,
    )?;
    println!("{}", xffs_tools::inspect_device(d)?);
    Ok(())
}
