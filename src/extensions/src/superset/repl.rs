//! `git zrepl` — an interactive line console over the zvcs verbs.
//!
//! Each line is run exactly as `git <line>` would be (superset verbs and
//! porcelain alike), so it doubles as a live ledger/daemon console:
//! `zjobs`, `zjob 3`, `zrepos`, `zreindex`, `zdaemon status`, `zsync`, … Type
//! `quit`/`exit` (or send EOF) to leave. The prompt is written to stderr and
//! only when stdin is a terminal, so piped input stays scriptable.

use anyhow::Result;
use std::io::{BufRead, IsTerminal, Write};
use std::process::ExitCode;

pub fn zrepl(_args: &[String]) -> Result<ExitCode> {
    let stdin = std::io::stdin();
    let interactive = stdin.is_terminal();
    let mut lines = stdin.lock().lines();

    loop {
        if interactive {
            eprint!("zvcs> ");
            let _ = std::io::stderr().flush();
        }
        let Some(line) = lines.next() else { break }; // EOF
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "quit" || line == "exit" {
            break;
        }

        let parts: Vec<String> = line.split_whitespace().map(String::from).collect();
        let (sub, rest) = parts.split_first().expect("non-empty checked above");
        match crate::dispatch::run(sub, rest) {
            Ok(_) => {}
            Err(e) => eprintln!("zvcs: {sub}: {e:#}"),
        }
    }
    Ok(ExitCode::SUCCESS)
}
