use clap::Parser;
#[derive(Parser)]
struct Args {
    image: std::path::PathBuf,
}
fn main() -> xffs_tools::ToolResult<()> {
    println!("{}", xffs_tools::inspect(&Args::parse().image)?);
    Ok(())
}
