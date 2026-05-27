//! `deliberate-mcp` binary stub.
//!
//! The reasoning trace server has merged into the unified `think-and-ship`
//! binary. This stub exists so existing installations fail loudly with a
//! migration pointer instead of silently launching an out-of-date server.

fn main() {
    eprintln!("deliberate-mcp has merged into the `think-and-ship` server.");
    eprintln!();
    eprintln!("  Install:        cargo install think-and-ship");
    eprintln!("                  npm i -g think-and-ship");
    eprintln!();
    eprintln!("  MCP config:     command = `think-and-ship`, args = [\"serve\"]");
    eprintln!();
    eprintln!("Tool names: prefer `think_*` (canonical). The old `deliberate_*`");
    eprintln!("names remain wired as deprecated aliases through v0.2.x.");
    eprintln!();
    eprintln!("See https://github.com/AlrikOlson/think-and-ship for details.");
    std::process::exit(1);
}
