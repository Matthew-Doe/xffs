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
    #[arg(long, default_value_t = 128)]
    memory_mib: usize,
}
fn main() -> xffs_tools::ToolResult<()> {
    let a = Args::parse();
    let device = xffs_tools::open_target_identified(
        &a.image,
        a.device,
        xffs_core::AccessMode::ReadOnly,
        a.expect_serial.as_deref(),
        a.expect_disk_sequence,
    )?;
    let fs = xffs_core::ReadOnlyFs::from_device(
        device,
        xffs_core::reader::OpenOptions::with_memory_mib(a.memory_mib)?,
    )?;
    for message in fs.diagnostics() {
        println!("{message}");
    }
    println!(
        "Recovered view valid: {:?}; working-memory charge {} bytes",
        fs.statfs(),
        fs.memory_used()
    );
    Ok(())
}
