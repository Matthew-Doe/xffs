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
        #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u16).range(1..=2))]
        format_revision: u16,
    },
}
fn main() -> xffs_tools::ToolResult<()> {
    let Args {
        command:
            Command::CreateDemo {
                image,
                scenario,
                format_revision,
            },
    } = Args::parse();
    xffs_tools::create_image_revision(
        &image,
        64 * 1024 * 1024,
        xffs_tools::DEMO_UUID,
        None,
        Some(scenario),
        xffs_core::format::FormatRevision::decode(format_revision)?,
    )?;
    println!("Created {} ({scenario:?})", image.display());
    Ok(())
}
