use clap::{Parser, Subcommand};
use std::path::PathBuf;
#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    CreateDemo {
        image: PathBuf,
        #[arg(long, value_enum, default_value = "clean")]
        scenario: xffs_tools::Scenario,
    },
}
fn main() -> xffs_tools::ToolResult<()> {
    let Args {
        command: Command::CreateDemo { image, scenario },
    } = Args::parse();
    xffs_tools::create_image(
        &image,
        64 * 1024 * 1024,
        xffs_tools::DEMO_UUID,
        None,
        Some(scenario),
    )?;
    println!("Created {} ({scenario:?})", image.display());
    Ok(())
}
