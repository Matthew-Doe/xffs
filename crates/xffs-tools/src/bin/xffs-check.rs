use clap::Parser;
#[derive(Parser)]
struct Args {
    image: std::path::PathBuf,
    #[arg(long)]
    device: bool,
    #[arg(long, default_value_t = 128)]
    memory_mib: usize,
}
fn main() -> xffs_tools::ToolResult<()> {
    let a = Args::parse();
    let device = xffs_tools::open_target(&a.image, a.device, xffs_core::AccessMode::ReadOnly)?;
    let fs = xffs_core::ReadOnlyFs::from_device(
        device,
        xffs_core::reader::OpenOptions {
            memory_limit: a
                .memory_mib
                .checked_mul(1024 * 1024)
                .ok_or("memory limit overflow")?,
        },
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
