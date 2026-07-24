//! `git zrepl` — an interactive line console over the zvcs verbs.
//!
//! Each line is run exactly as `git <line>` would be (superset verbs and
//! porcelain alike), so it doubles as a live ledger/daemon console:
//! `zjobs`, `zjob 3`, `zrepos`, `zreindex`, `zdaemon status`, `zsync`, … Type
//! `quit`/`exit` (or press Ctrl-D) to leave.
//!
//! On a terminal it opens with a stats banner (logo + live verb/repo counts,
//! [`crate::superset::banner`]) and edits the line with [`reedline`]: real cursor
//! motion, Tab completion of the command word against every dispatchable verb
//! (superset + porcelain) and of the second word against a verb's subcommands
//! (`zdaemon <Tab>` → start/stop/…), Ctrl-C to abandon the current line, Ctrl-D to quit,
//! and history persisted to `~/.zvcs/repl_history` across sessions. Emacs or vi
//! keys per `zvcs.replvimode`; the Tab/Shift+Tab menu bindings attach to whichever
//! insert keymap is active. Piped/non-tty stdin falls back to a raw line reader
//! (no banner, no completion), so `echo 'zrepos' | git zrepl` and heredocs stay
//! scriptable.

use anyhow::Result;
use std::borrow::Cow;
use std::io::{BufRead, IsTerminal};
use std::process::ExitCode;

use reedline::{
    default_emacs_keybindings, default_vi_insert_keybindings, default_vi_normal_keybindings,
    ColumnarMenu, Completer, EditMode, Emacs, FileBackedHistory, KeyCode, KeyModifiers, Keybindings,
    MenuBuilder, Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus,
    PromptViMode, Reedline, ReedlineEvent, ReedlineMenu, Signal, Span, Suggestion, Vi,
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

/// Every verb `git <line>` accepts — the superset (`z*`) verbs plus every
/// git-compat porcelain command — sorted and deduped, for Tab completion of the
/// command word. Sourced from the dispatch tables so the set never drifts.
fn all_verbs() -> Vec<String> {
    let mut v: Vec<String> = crate::dispatch::SUPERSET_VERBS
        .iter()
        .chain(crate::dispatch::PORCELAIN_VERBS.iter())
        .map(|s| (*s).to_string())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Byte index `start` of the word under the cursor and that word's text, for
/// prefix matching. Word boundaries are whitespace only (git verbs carry no
/// sigils). Ported from strykelang's `completion_word_start`, minus the sigil
/// snapping stryke needs for `$`/`@`/`%` variables.
fn completion_word_start(line: &str, pos: usize) -> (usize, &str) {
    let pos = pos.min(line.len());
    let before = line.get(..pos).unwrap_or("");
    let start = before
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    (start, line.get(start..pos).unwrap_or(""))
}

/// The fixed subcommand vocabulary of a verb, for second-token completion, or
/// `&[]` for verbs whose next token is free-form (a name, path, refspec, id).
/// Mirrors each verb's own subcommand parser — the source of truth is the named
/// file — the way a zsh `_git` completer enumerates subcommands. Only the
/// documented primary spellings are offered (aliases like `run`/`reload` still
/// work when typed, they just don't clutter the menu).
fn verb_subcommands(verb: &str) -> &'static [&'static str] {
    match verb {
        "zdaemon" => &["start", "stop", "restart", "status", "info", "ping", "log"], // superset/zdaemon.rs
        "zhook" => &["set", "unset", "show", "list", "test"],                        // superset/zhook.rs
        "zworktree" => &["add", "list", "remove"],                                   // superset/zworktree.rs
        "zjob" => &["stop", "restart"],                                              // superset/ledger.rs
        "ztrigger" => &["list", "rm", "test"],                                       // superset/trigger.rs
        "zwatch" => &["list", "rm"],                                                 // superset/trigger.rs
        _ => &[],
    }
}

/// Tab completion for `git zrepl`: the command word completes against the full
/// verb set; the second word completes against that verb's fixed subcommands
/// (e.g. `zdaemon <Tab>` → start/stop/restart/…). Deeper positions and
/// free-form-argument verbs yield nothing — there is no fixed vocabulary there.
struct ZreplCompleter {
    verbs: Vec<String>,
}

/// Build sorted, prefix-filtered suggestions over `candidates` replacing `span`.
fn suggestions<'a>(
    candidates: impl Iterator<Item = &'a str>,
    prefix: &str,
    span: Span,
) -> Vec<Suggestion> {
    let mut out: Vec<Suggestion> = candidates
        .filter(|c| c.starts_with(prefix))
        .map(|c| Suggestion {
            value: c.to_string(),
            description: None,
            style: None,
            extra: None,
            span,
            append_whitespace: true,
            display_override: None,
            match_indices: None,
        })
        .collect();
    out.sort_by(|a, b| a.value.cmp(&b.value));
    out
}

impl Completer for ZreplCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let (start, prefix) = completion_word_start(line, pos);
        let span = Span::new(start, pos);
        let before = line.get(..start).unwrap_or("").trim();

        // First token → complete the verb name.
        if before.is_empty() {
            return suggestions(self.verbs.iter().map(String::as_str), prefix, span);
        }
        // Second token, and the first is exactly one verb → complete its
        // subcommands. `before` with no interior whitespace is that lone verb.
        if !before.contains(char::is_whitespace) {
            return suggestions(verb_subcommands(before).iter().copied(), prefix, span);
        }
        // Deeper positions have no fixed vocabulary.
        Vec::new()
    }
}

/// Bind Tab to pop / advance the completion menu, Shift+Tab to step back —
/// shared so the bindings live on the emacs map AND the vi insert map. Ported
/// from strykelang's `install_menu_bindings`.
fn install_menu_bindings(keybindings: &mut Keybindings) {
    keybindings.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu("completion_menu".to_string()),
            ReedlineEvent::MenuNext,
        ]),
    );
    keybindings.add_binding(KeyModifiers::SHIFT, KeyCode::BackTab, ReedlineEvent::MenuPrevious);
    keybindings.add_binding(KeyModifiers::NONE, KeyCode::BackTab, ReedlineEvent::MenuPrevious);
}

/// Interactive stdin: a reedline editor with persistent history, verb-name Tab
/// completion, and full line editing. Ctrl-C abandons the current line; Ctrl-D
/// ends the session.
fn run_interactive() -> Result<ExitCode> {
    // The startup banner (zvcs logo + live verb/system/repo stats), colored on a
    // tty unless NO_COLOR is set, plus a one-line usage hint.
    let colored = std::env::var_os("NO_COLOR").is_none();
    crate::superset::banner::print_banner(colored);
    let (dim, reset) = if colored { ("\x1b[2m", "\x1b[0m") } else { ("", "") };
    println!();
    println!("{dim}  type `exit` or Ctrl-D to leave the console — Tab completes verbs{reset}");
    println!();

    let completer = Box::new(ZreplCompleter { verbs: all_verbs() });
    let menu = ColumnarMenu::default()
        .with_name("completion_menu")
        .with_columns(4)
        .with_column_padding(2);

    // `zvcs.replvimode` (repo config, or global when run outside a repo) swaps the
    // default emacs keybindings for vi. Menu (Tab) bindings attach to whichever
    // insert-mode keymap is active so completion behaves the same in either mode;
    // vi normal-mode keys come from reedline's defaults, untouched. The prompt
    // indicator reflects the vi mode (`:` normal, `>` insert); emacs stays `>`.
    let edit_mode: Box<dyn EditMode> = if crate::config::config_bool("zvcs.replvimode").unwrap_or(false) {
        let mut insert_kb = default_vi_insert_keybindings();
        install_menu_bindings(&mut insert_kb);
        Box::new(Vi::new(insert_kb, default_vi_normal_keybindings()))
    } else {
        let mut kb = default_emacs_keybindings();
        install_menu_bindings(&mut kb);
        Box::new(Emacs::new(kb))
    };

    let mut editor = Reedline::create()
        .with_completer(completer)
        .with_menu(ReedlineMenu::EngineCompleter(Box::new(menu)))
        .with_edit_mode(edit_mode);

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

    fn render_prompt_indicator(&self, mode: PromptEditMode) -> Cow<'_, str> {
        // In vi mode show the sub-mode; emacs and everything else stay `> `.
        match mode {
            PromptEditMode::Vi(PromptViMode::Normal) => Cow::Borrowed(": "),
            _ => Cow::Borrowed("> "),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn values(line: &str) -> Vec<String> {
        let mut c = ZreplCompleter { verbs: all_verbs() };
        c.complete(line, line.len()).into_iter().map(|s| s.value).collect()
    }

    #[test]
    fn first_token_completes_verbs() {
        let v = values("zsn");
        assert!(v.contains(&"zsnapshot".to_string()));
        assert!(v.contains(&"zsnapshots".to_string()));
        // A porcelain verb is reachable too (both dispatch tables feed the set).
        assert!(values("stat").contains(&"status".to_string()));
    }

    #[test]
    fn second_token_completes_zdaemon_subcommands() {
        // The reported bug: `zdaemon <Tab>` offered nothing. Every documented
        // subcommand must appear, and a prefix must narrow to it.
        let all = values("zdaemon ");
        for sub in ["start", "stop", "restart", "status", "info", "ping", "log"] {
            assert!(all.contains(&sub.to_string()), "zdaemon missing `{sub}`: {all:?}");
        }
        assert_eq!(values("zdaemon sta"), vec!["start", "status"]);
    }

    #[test]
    fn other_subcommand_verbs_and_freeform_verbs() {
        assert!(values("zhook ").contains(&"unset".to_string()));
        assert!(values("zworktree ").contains(&"remove".to_string()));
        assert_eq!(values("zjob "), vec!["restart", "stop"]);
        // A free-form-arg verb offers no second-token vocabulary.
        assert!(values("zsnapshot ").is_empty());
        // Deeper than the subcommand offers nothing.
        assert!(values("zdaemon status ").is_empty());
    }
}
