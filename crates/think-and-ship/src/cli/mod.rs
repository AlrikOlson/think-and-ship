//! CLI subcommand handlers.

use anyhow::Result;

const UNIMPLEMENTED: &str = "think-and-ship: command not yet implemented.";

pub fn serve(_http: Option<String>) -> Result<()> {
    println!("{UNIMPLEMENTED}");
    Ok(())
}

pub fn init(_with_claude_md: bool, _full: bool) -> Result<()> {
    println!("{UNIMPLEMENTED}");
    Ok(())
}

pub fn doctor() -> Result<()> {
    println!("{UNIMPLEMENTED}");
    Ok(())
}

pub fn status() -> Result<()> {
    println!("{UNIMPLEMENTED}");
    Ok(())
}

pub fn export(_format: &str) -> Result<()> {
    println!("{UNIMPLEMENTED}");
    Ok(())
}
