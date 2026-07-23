//! `git zrepl` — an interactive line console over the zvcs verbs.
//!
//! Each line is run exactly as `git <line>` would be (superset verbs and
//! porcelain alike), so it doubles as a live ledger/daemon console:
//! `zjobs`, `zjob 3`, `zrepos`, `zreindex`, `zdaemon status`, `zsync`, … Type
//! `quit`/`exit` (or press Ctrl-D) to leave.
//!
//! On a terminal the line is edited with [`reedline`]: real cursor motion,
//! Ctrl-C to abandon the current line, Ctrl-D to quit, and history persisted to
//! `~/.zvcs/repl_history` across sessions. Piped/non-tty stdin falls back to a
//! raw line reader, so `echo 'zrepos' | git zrepl` and heredocs stay scriptable.

use anyhow::Result;
use std::borrow::Cow;
use std::io::{BufRead, IsTerminal};
use std::process::ExitCode;

use reedline::{
    FileBackedHistory, Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus,
    Reedline, Signal,
};

pub fn zrepl(_args: &[String]) -> Result<ExitCode> {
    if std::io::stdin().is_terminal() {
        run_interactive()
    } else {
        run_piped()
    }
}

/// Run one console line as `git <line>` would. Returns `false` to leave the
/// console (`quit`/`exit`), `true` to keep looping. Blank lines are ignored.
fn run_one(line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() {
        return true;
    }
    if line == "quit" || line == "exit" {
        return false;
    }

    let parts: Vec<String> = line.split_whitespace().map(String::from).collect();
    let (sub, rest) = parts.split_first().expect("non-empty checked above");
    if let Err(e) = crate::dispatch::run(sub, rest) {
        eprintln!("zvcs: {sub}: {e:#}");
    }
    true
}

/// Piped / non-tty stdin: a raw line reader (reedline needs a terminal), so
/// scripted input pipes straight through.
fn run_piped() -> Result<ExitCode> {
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break }; // read error / EOF
        if !run_one(&line) {
            break;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Interactive stdin: a reedline editor with persistent history and full line
/// editing. Ctrl-C abandons the current line; Ctrl-D ends the session.
fn run_interactive() -> Result<ExitCode> {
    let mut editor = Reedline::create();
    // Persist history across sessions. A failure to open the file (e.g. a
    // read-only home) degrades to an in-memory editor rather than aborting the
    // console — history is a convenience, not a precondition.
    let history_path = crate::superset::zdaemon::zvcs_home().join("repl_history");
    if let Ok(history) = FileBackedHistory::with_file(1000, history_path) {
        editor = editor.with_history(Box::new(history));
    }
    let prompt = ZreplPrompt;

    loop {
        match editor.read_line(&prompt) {
            // A submitted line, or a host-command payload — both are verb text
            // to run. (`zrepl` wires no ExecuteHostCommand event, so HostCommand
            // is only reachable via a future keybinding; run it all the same.)
            Ok(Signal::Success(line)) | Ok(Signal::HostCommand(line)) => {
                if !run_one(&line) {
                    break;
                }
            }
            // Ctrl-C, or an external break: abandon the current line, keep going.
            Ok(Signal::CtrlC) | Ok(Signal::ExternalBreak(_)) => continue,
            // Ctrl-D: end the session (the console's EOF).
            Ok(Signal::CtrlD) => break,
            // `Signal` is #[non_exhaustive]; a future variant defaults to
            // abandoning the current line and keeping the session alive.
            Ok(_) => continue,
            // A terminal read error ends the console rather than busy-looping.
            Err(e) => {
                eprintln!("zvcs: zrepl: {e}");
                break;
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The `zvcs> ` prompt: a plain left label, no right prompt, no vi-mode noise.
struct ZreplPrompt;

impl Prompt for ZreplPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed("zvcs")
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("> ")
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("... ")
    }

    fn render_prompt_history_search_indicator(
        &self,
        search: PromptHistorySearch,
    ) -> Cow<'_, str> {
        let failing = match search.status {
            PromptHistorySearchStatus::Passing => "",
            PromptHistorySearchStatus::Failing => "failing ",
        };
        Cow::Owned(format!("({failing}reverse-search: {}) ", search.term))
    }
}
