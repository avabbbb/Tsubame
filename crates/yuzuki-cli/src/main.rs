use std::io::Write;
use std::process::ExitCode;

fn main() -> ExitCode {
    let outcome = yuzuki_cli::run(std::env::args_os());
    let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    ExitCode::from(outcome.code)
}
