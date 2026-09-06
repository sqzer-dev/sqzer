//! `sqzer` binary. Argument parsing lands with the first codec.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("sqzer {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    eprintln!("sqzer: nothing implemented yet, see docs/adr/0001-system-design.md");
    ExitCode::from(3)
}
