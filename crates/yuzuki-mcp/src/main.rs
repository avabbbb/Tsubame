//! yuzuki-mcp: MCP stdio server for the YUZUKI Desktop. stdout carries only
//! JSON-RPC; diagnostics go to stderr.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use yuzuki_client::DiscoverOptions;
use yuzuki_mcp::Server;

const USAGE: &str = "usage: yuzuki-mcp [--control-file <path>] [--launch]";

fn main() -> ExitCode {
    let mut opts = DiscoverOptions::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--control-file" => match args.next() {
                Some(path) => opts.control_file = Some(PathBuf::from(path)),
                None => return usage("--control-file needs a path"),
            },
            "--launch" => opts.launch = true,
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => return usage(&format!("unknown argument {other}")),
        }
    }
    let server = Server::new(opts);
    let mut stdout = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(e) => {
                eprintln!("yuzuki-mcp: stdin: {e}");
                return ExitCode::FAILURE;
            }
        };
        if let Some(reply) = server.handle_line(&line) {
            if let Err(e) = writeln!(stdout, "{reply}").and_then(|()| stdout.flush()) {
                eprintln!("yuzuki-mcp: stdout: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

fn usage(message: &str) -> ExitCode {
    eprintln!("yuzuki-mcp: {message}\n{USAGE}");
    ExitCode::from(2)
}
