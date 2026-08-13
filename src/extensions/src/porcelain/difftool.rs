//! `git difftool` — show changes using an external diff tool.
//!
//! `difftool` is a front-end that computes a diff and then hands each changed
//! path to an *external program*. Upstream this is done by spawning `git diff`
//! with `GIT_EXTERNAL_DIFF=git-difftool--helper`; git then materialises the
//! pre-/post-image of every changed path into temp files and invokes the helper
//! once per file (the seven-positional `GIT_EXTERNAL_DIFF` convention). This
//! module reproduces that behaviour directly: it runs this binary's own ported
//! `git diff --raw -z` to enumerate the exact changed-path set (the same list
//! `git difftool` would show), materialises `LOCAL`/`REMOTE` for each path the
//! way `prepare_temp_file()` does, and launches the resolved tool with
//! `LOCAL`/`REMOTE`/`MERGED`/`BASE` in scope — a faithful in-process rendering
//! of `run_file_diff()` + `git-difftool--helper.sh`'s `launch_merge_tool`.
//!
//! Because the diagnostics a user sees come out of that `git diff` child, the
//! order here is git's: the child runs **first**, so an option `difftool` does
//! not know (`PARSE_OPT_KEEP_UNKNOWN_OPT` forwards it verbatim) is rejected by
//! `diff`'s own parser with `diff`'s own exit code, before any tool is resolved.
//! Tool resolution likewise happens where the script does it — the *name* up
//! front, its command only inside `launch_merge_tool`, after the prompt.
//!
//! What is ported (checked against git 2.55.0's `builtin/difftool.c`,
//! `git-difftool--helper.sh` and `git-mergetool--lib.sh`):
//!
//!   * `-h` → the usage block on **stdout**, exit 129, before repository setup.
//!   * `--tool-help` → delegated to the `mergetool` sibling's
//!     `show_tool_help("diff")`, before repository setup.
//!   * value-taking option with no value → the parse-options `requires a value`
//!     diagnostic on stderr, exit 129, before repository setup.
//!   * no repository/worktree → `fatal: difftool requires worktree or
//!     --no-index`, exit 128; a bare repository → `fatal: this operation must be
//!     run in a work tree`, exit 128.
//!   * `--tool=` / `--extcmd=` empty value → `fatal: no <tool> given for
//!     --tool=<tool>` / `fatal: no <cmd> given for --extcmd=<cmd>`, exit 128,
//!     after the worktree check.
//!   * `die_for_incompatible_opt3`: `--gui`, `--tool` and `--extcmd` are mutually
//!     exclusive (exit 128).
//!   * **The file-diff launch** (`run_file_diff`): each changed path from
//!     `git diff --raw -z` has its pre-image staged into a temp file and its
//!     post-image staged (or the live work-tree file borrowed), and the tool is
//!     launched per file with the `\nViewing (c/t): 'path'` / `Launch '…' [Y/n]? `
//!     prompt (skipped by `-y`/`--no-prompt`, forced by `--prompt`), the
//!     `eval $cmd '"$LOCAL"' '"$REMOTE"'` (extcmd) or `( eval $cmd )` (user tool)
//!     invocation, and the `status >= 126` / `GIT_DIFFTOOL_TRUST_EXIT_CODE` early
//!     exits — each of which makes the helper exit non-zero, which `git diff`
//!     turns into `fatal: external diff died, stopping at <path>` and exit 128.
//!     An empty diff launches nothing and exits 0, matching git.
//!   * **Unmerged paths.** `run_diff_files()` prints a *combined* diff for them
//!     and never queues a filepair, so `GIT_EXTERNAL_DIFF` is not consulted: the
//!     combined diff goes to stdout and no tool is launched for that path.
//!   * **`--dir-diff` (`-d`)** (`run_dir_diff`): both temp trees are staged
//!     (index side checked out into `left/`, work-tree side symlinked — or copied
//!     under `--no-symlinks` — into `right/`, with submodule and symlink standin
//!     files), the tool is run once on the two directories, and files it modified
//!     are copied back into the work tree.
//!   * **Tool selection** (`get_merge_tool`): `--tool`/`GIT_DIFF_TOOL`, then
//!     `diff.tool`/`merge.tool` (`diff.guitool`/`merge.guitool` first under
//!     `--gui`/`difftool.guiDefault`), then `guess_merge_tool`'s candidate list
//!     with its guidance on stderr. `initialize_merge_tool`'s and
//!     `get_merge_tool_path`'s rejections are reproduced verbatim.
//!   * `git difftool --no-index <a> <b>`: an inaccessible path → `error: Could
//!     not access '<path>'`, exit 1; an identical pair → exit 0; a differing
//!     regular-file pair under `-x<cmd>`/`--extcmd=` launches the command on the
//!     two files directly (git's `--no-index` external-diff path).
//!
//! What bails, honestly, because the substrate is not in the vendored crates:
//! a *catalogue* tool's `diff_cmd`. `vimdiff`, `meld` and friends keep theirs in
//! a `mergetools/` shell script under `$(git --exec-path)`, and nothing under
//! `src/ported` carries that database, so reaching a launch for one of them
//! bails rather than run the wrong program. Selecting such a tool, prompting for
//! it, and every diagnostic that precedes the launch are reproduced.
//!
//! Known approximations of the ported path: staged temp files hold the blob
//! bytes verbatim (no smudge filter — the same floor `add.rs` records), a
//! work-tree symlink is handed to the per-file tool as the link itself rather
//! than a temp holding its `readlink` text, and a dirty work-tree submodule's
//! standin omits the `-dirty` suffix (its committed `HEAD` is still shown).
//! Upstream also runs the helper as a *fresh process per path*, so
//! `guess_merge_tool`'s guidance is repeated once per changed file; fusing the
//! helper into this process emits it once. `run_dir_diff`'s copy-back decides
//! "the tool changed this file" by re-hashing it rather than by writing
//! `wtindex` out and running `update-index --really-refresh` + `diff-files`
//! against it, which is what those two children reduce to on a freshly written
//! index with no stat cache.

use anyhow::{anyhow, Result};
use gix::bstr::ByteSlice;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus, Stdio};

/// Stock git's `difftool` usage block, byte-for-byte (813 bytes, git 2.55.0),
/// including the trailing blank line. Printed on `-h` (stdout).
const USAGE: &str = concat!(
    "usage: git difftool [<options>] [<commit> [<commit>]] [--] [<path>...]\n",
    "\n",
    "\x20   -g, --[no-]gui        use `diff.guitool` instead of `diff.tool`\n",
    "\x20   -d, --[no-]dir-diff   perform a full-directory diff\n",
    "\x20   -y, --no-prompt       do not prompt before launching a diff tool\n",
    "\x20   --[no-]symlinks       use symlinks in dir-diff mode\n",
    "\x20   -t, --[no-]tool <tool>\n",
    "\x20                         use the specified diff tool\n",
    "\x20   --[no-]tool-help      print a list of diff tools that may be used with `--tool`\n",
    "\x20   --[no-]trust-exit-code\n",
    "\x20                         make 'git-difftool' exit when an invoked diff tool returns a non-zero exit code\n",
    "\x20   -x, --[no-]extcmd <command>\n",
    "\x20                         specify a custom command for viewing diffs\n",
    "\x20   --no-index            passed to `diff`\n",
    "\x20   --index               opposite of --no-index\n",
    "\n",
);

/// The options that take a separate value argument, as `(long, short)`.
const VALUE_OPTS: [(&str, char); 2] = [("tool", 't'), ("extcmd", 'x')];

/// What the parsed command line asks for.
struct Opts {
    /// `--tool-help` was given.
    tool_help: bool,
    /// `--tool=`/`-t` value, if any. `Some("")` means an explicitly empty value.
    tool: Option<String>,
    /// `--extcmd=`/`-x` value, if any. `Some("")` means an explicitly empty value.
    extcmd: Option<String>,
    /// `--no-index` was given (`difftool` then diffs two paths outside any repo).
    no_index: bool,
    /// `-d`/`--dir-diff` was given (its negation clears it).
    dir_diff: bool,
    /// `dt_options.symlinks`, initialised from `has_symlinks` (true on unix) and
    /// overridden by `--symlinks`/`--no-symlinks`. Only dir-diff reads it.
    symlinks: bool,
    /// git's tri-state `use_gui_tool` (`-1` unset / `0` / `1`): `-g`/`--gui` and
    /// `--no-gui` pin it (the C then exports `GIT_MERGETOOL_GUI`), while an unset
    /// value leaves `gui_mode` to consult `difftool.guiDefault`. Steers the tool
    /// config key order (`diff.guitool` first) and the `--tool`/`--extcmd`
    /// incompatibility, which only an explicit `--gui` triggers.
    gui: Option<bool>,
    /// `-y`/`--no-prompt` → `Some(false)`, `--prompt` → `Some(true)`, unset →
    /// `None` (the `difftool.prompt`/`mergetool.prompt` default applies).
    prompt: Option<bool>,
    /// `--trust-exit-code` → `Some(true)`, `--no-trust-exit-code` → `Some(false)`,
    /// unset → `None` (the `difftool.trustExitCode` config default applies).
    trust: Option<bool>,
    /// Every argument that is not one of `difftool`'s own options — revisions,
    /// pathspecs, `--`, and the `git diff` options it forwards. Passed verbatim to
    /// the `git diff --raw -z` child, exactly as git's `PARSE_OPT_KEEP_UNKNOWN_OPT
    /// | PARSE_OPT_KEEP_DASHDASH` leaves them in `argv`.
    forward: Vec<String>,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            tool_help: false,
            tool: None,
            extcmd: None,
            no_index: false,
            dir_diff: false,
            // `dt_options.symlinks = dt_options.has_symlinks`, which `init_db`
            // leaves at 1 on every platform this port builds for.
            symlinks: true,
            gui: None,
            prompt: None,
            trust: None,
            forward: Vec::new(),
        }
    }
}

/// `git difftool` — validate arguments, then launch a diff tool for the changed
/// paths (`run_file_diff`) or for two staged trees (`run_dir_diff`).
///
/// See the module documentation for the exact set of invocations that are
/// reproduced and for the substrate the bailing path would need.
pub fn difftool(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0. `difftool`'s own positionals are
    // revisions and paths, so a leading literal `difftool` is unambiguous only
    // as the verb; strip exactly one.
    let args = match args.first().map(String::as_str) {
        Some("difftool") => &args[1..],
        _ => args,
    };

    // Phase 1 — parse_options. `-h`, and every "requires a value" diagnostic,
    // are emitted here, before git looks at the repository at all.
    let mut opts = Opts::default();
    let mut end_of_opts = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();

        if end_of_opts {
            opts.forward.push(a.to_owned());
            i += 1;
            continue;
        }

        match a {
            "--" => {
                end_of_opts = true;
                // `PARSE_OPT_KEEP_DASHDASH`: git forwards `--` itself to `diff`.
                opts.forward.push(a.to_owned());
            }
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--tool-help" => opts.tool_help = true,
            "--no-tool-help" => opts.tool_help = false,
            "-d" | "--dir-diff" => opts.dir_diff = true,
            "--no-dir-diff" => opts.dir_diff = false,
            "-y" | "--no-prompt" => opts.prompt = Some(false),
            "--prompt" => opts.prompt = Some(true),
            "-g" | "--gui" => opts.gui = Some(true),
            "--no-gui" => opts.gui = Some(false),
            "--symlinks" => opts.symlinks = true,
            "--no-symlinks" => opts.symlinks = false,
            "--trust-exit-code" => opts.trust = Some(true),
            "--no-trust-exit-code" => opts.trust = Some(false),
            "--no-index" => opts.no_index = true,
            "--index" => opts.no_index = false,
            "--no-tool" => opts.tool = None,
            "--no-extcmd" => opts.extcmd = None,

            // `--tool <v>` / `--extcmd <v>`: a separate value argument.
            _ if VALUE_OPTS.iter().any(|(l, _)| a.strip_prefix("--") == Some(*l)) => {
                let name = &a[2..];
                let short = short_for(name);
                let Some(v) = args.get(i + 1) else {
                    return Ok(usage_error(&format!("option `{name}' requires a value")));
                };
                store_value(&mut opts, short, v.clone());
                i += 1;
            }
            // `--tool=<v>` / `--extcmd=<v>`, including an empty `<v>`; the
            // emptiness is diagnosed later, after the worktree check.
            _ if VALUE_OPTS
                .iter()
                .any(|(l, _)| a.starts_with(&format!("--{l}="))) =>
            {
                let (name, v) = a[2..].split_once('=').unwrap_or((&a[2..], ""));
                store_value(&mut opts, short_for(name), v.to_owned());
            }

            // Any other long option is unknown to `difftool` and forwarded to
            // `git diff` verbatim (`PARSE_OPT_KEEP_UNKNOWN_OPT`).
            _ if a.starts_with("--") => opts.forward.push(a.to_owned()),

            // A clustered short group. If every letter is one of `difftool`'s own
            // switches it is consumed here; otherwise the whole token is unknown
            // and forwarded to `git diff`.
            _ if a.len() > 1 && a.starts_with('-') && is_difftool_cluster(&a[1..]) => {
                let mut chars = a[1..].chars();
                while let Some(c) = chars.next() {
                    match c {
                        'h' => {
                            print!("{USAGE}");
                            return Ok(ExitCode::from(129));
                        }
                        'y' => opts.prompt = Some(false),
                        'g' => opts.gui = Some(true),
                        'd' => opts.dir_diff = true,
                        't' | 'x' => {
                            // The value is the rest of the cluster if non-empty,
                            // otherwise the next argument.
                            let rest: String = chars.by_ref().collect();
                            if rest.is_empty() {
                                let Some(v) = args.get(i + 1) else {
                                    return Ok(usage_error(&format!(
                                        "switch `{c}' requires a value"
                                    )));
                                };
                                store_value(&mut opts, c, v.clone());
                                i += 1;
                            } else {
                                store_value(&mut opts, c, rest);
                            }
                        }
                        _ => unreachable!("is_difftool_cluster gate"),
                    }
                }
            }

            // Revisions, pathspecs, `-`, and unknown short clusters: forwarded.
            _ => opts.forward.push(a.to_owned()),
        }
        i += 1;
    }

    // `if (tool_help) return print_tool_help();` — the C spawns `git mergetool
    // --tool-help=diff`. Answered before repository setup, so it works outside a
    // repository and in a bare one. Delegate to the `mergetool` sibling rather
    // than re-roll the tool database.
    if opts.tool_help {
        return super::mergetool::mergetool(&["--tool-help=diff".to_owned()]);
    }

    // `--no-index` compares two filesystem paths directly and needs no
    // repository, so it is answered before repository setup.
    if opts.no_index {
        if opts.dir_diff {
            eprintln!("fatal: options '--dir-diff' and '--no-index' cannot be used together");
            return Ok(ExitCode::from(128));
        }
        if let Some(code) = incompatible_opt3(&opts) {
            return Ok(code);
        }
        if let Some(code) = empty_value_fatal(&opts) {
            return Ok(code);
        }
        return no_index(&opts);
    }

    // Phase 2 — repository setup. Both diagnostics are git's own, exit 128.
    let repo = match gix::discover(".") {
        Ok(repo) => repo,
        Err(_) => {
            eprintln!("fatal: difftool requires worktree or --no-index");
            return Ok(ExitCode::from(128));
        }
    };
    if repo.workdir().is_none() {
        eprintln!("fatal: this operation must be run in a work tree");
        return Ok(ExitCode::from(128));
    }

    // Phase 3 — `die_for_incompatible_opt3` (C step 4) then the empty-value
    // checks (C steps 5–6), all performed after worktree setup and before the
    // `git diff` child is built.
    if let Some(code) = incompatible_opt3(&opts) {
        return Ok(code);
    }
    if let Some(code) = empty_value_fatal(&opts) {
        return Ok(code);
    }

    // Phase 4 — the launch.
    if opts.dir_diff {
        run_dir_diff(&repo, &opts)
    } else {
        run_file_diff(&repo, &opts)
    }
}

// ---------------------------------------------------------------------------
// Tool selection (`get_merge_tool` and friends, diff mode)
// ---------------------------------------------------------------------------

/// Which program the run will launch, resolved the way `git-difftool--helper`
/// resolves it before its loop starts.
pub(super) enum Tool {
    /// `use_ext_cmd`: `GIT_DIFFTOOL_EXTCMD` / `--extcmd`, run as
    /// `eval $cmd '"$LOCAL"' '"$REMOTE"'`.
    ExtCmd(String),
    /// A tool name from `GIT_DIFF_TOOL`/`--tool` or `get_merge_tool`. Its command
    /// is only resolved when a launch actually happens, because
    /// `initialize_merge_tool` runs inside `launch_merge_tool`, after the prompt.
    Named(String),
    /// `guess_merge_tool` found nothing on `$PATH`: `merge_tool` stays empty and
    /// every later `initialize_merge_tool ""` fails.
    Unset,
}

impl Tool {
    /// The name the `Launch '<…>' [Y/n]?` prompt shows — `$GIT_DIFFTOOL_EXTCMD`
    /// under `use_ext_cmd`, else `$merge_tool` (empty when the guess failed).
    pub(super) fn label(&self) -> &str {
        match self {
            Tool::ExtCmd(c) => c,
            Tool::Named(t) => t,
            Tool::Unset => "",
        }
    }
}

/// `git-difftool--helper`'s prologue: `use_ext_cmd` wins, else `GIT_DIFF_TOOL`,
/// else `get_merge_tool` — whose `guess_merge_tool` guidance goes to stderr
/// before anything is launched.
pub(super) fn select_tool(
    config: &gix::config::File,
    extcmd: Option<&str>,
    named: Option<&str>,
    gui: Option<bool>,
) -> Result<Tool> {
    if let Some(cmd) = extcmd.filter(|c| !c.is_empty()) {
        return Ok(Tool::ExtCmd(cmd.to_owned()));
    }
    if let Some(t) = named.filter(|t| !t.is_empty()) {
        return Ok(Tool::Named(t.to_owned()));
    }

    // `gui_mode`: with `GIT_MERGETOOL_GUI` unset, `get_gui_default` reads
    // `difftool.guiDefault`.
    let gui = gui.unwrap_or_else(|| super::mergetool::gui_default(config, "difftool.guiDefault"));
    let selection = super::mergetool::select_tool(config, gui, true);
    if !selection.guidance.is_empty() {
        let mut e = std::io::stderr().lock();
        e.write_all(&selection.guidance)?;
        e.flush()?;
    }
    Ok(match selection.tool {
        Some(t) => Tool::Named(t),
        None => Tool::Unset,
    })
}

/// The resolved diff command and how to invoke it.
struct DiffCmd {
    /// The shell command text (`eval`'d in a child `sh`).
    text: String,
    /// Whether git appends `'"$LOCAL"' '"$REMOTE"'` after it (the `--extcmd`
    /// convention) or not (a user tool's `difftool.<tool>.cmd`, which references
    /// `$LOCAL`/`$REMOTE` itself).
    append: bool,
    /// `get_merge_tool_path`'s answer — `difftool.<tool>.path`, else
    /// `mergetool.<tool>.path`, else the tool name. `run_merge_tool` assigns it
    /// to `merge_tool_path` before `diff_cmd`, so a `.cmd` can spell it.
    /// `--extcmd` never reaches `get_merge_tool_path`, so it stays empty there.
    tool_path: String,
}

/// `initialize_merge_tool "$merge_tool" || exit 1` followed by `run_merge_tool`'s
/// `get_merge_tool_path` — everything `launch_merge_tool` does after the prompt
/// and before the command runs.
///
/// `Ok(None)` is one of those two steps failing: its `error:`/message is already
/// on stderr and the helper exits 1 without launching anything. `Err` is the one
/// substrate gap — a catalogue tool whose `diff_cmd` lives in a `mergetools/`
/// shell script this port does not carry.
fn tool_command(config: &gix::config::File, tool: &Tool) -> Result<Option<DiffCmd>> {
    let name = match tool {
        Tool::ExtCmd(cmd) => {
            return Ok(Some(DiffCmd {
                text: cmd.clone(),
                append: true,
                tool_path: String::new(),
            }))
        }
        Tool::Named(t) => t.as_str(),
        Tool::Unset => "",
    };

    // `initialize_merge_tool` → `setup_tool`.
    let cmd = super::mergetool::user_tool_cmd(config, name, true);
    if let Err(msg) = super::mergetool::setup_tool(name, cmd.is_some(), true) {
        eprintln!("{msg}");
        return Ok(None);
    }

    // `run_merge_tool` → `get_merge_tool_path`. `valid_tool` already passed above,
    // so only the availability probe can still reject the tool.
    let tool_path = super::mergetool::merge_tool_path(config, name, true);
    if cmd.is_none() && !super::mergetool::is_available(&tool_path) {
        eprintln!("The diff tool {name} is not available as '{tool_path}'");
        return Ok(None);
    }

    match cmd {
        Some(text) => Ok(Some(DiffCmd { text, append: false, tool_path })),
        None => crate::git_fatal!(
            "the built-in diff tool {name:?} keeps its diff_cmd in a mergetools/ shell script \
             under $(git --exec-path), which is not present in the vendored crates \
             (ported: -x/--extcmd, any tool with a difftool.<tool>.cmd or \
             mergetool.<tool>.cmd, and every diagnostic that precedes the launch)"
        ),
    }
}

// ---------------------------------------------------------------------------
// `run_file_diff`
// ---------------------------------------------------------------------------

/// `run_file_diff` + `git-difftool--helper.sh`: enumerate the changed paths with
/// this binary's own `git diff --raw -z`, then launch the resolved tool once per
/// path with the pre-/post-image staged the way `prepare_temp_file` stages them.
fn run_file_diff(repo: &gix::Repository, opts: &Opts) -> Result<ExitCode> {
    let snapshot = repo.config_snapshot();
    let config = snapshot.plumbing();

    // `should_prompt`, and `GIT_DIFFTOOL_TRUST_EXIT_CODE` from
    // `difftool.trustExitCode` / `--trust-exit-code`.
    let prompt = should_prompt(opts.prompt, Some(config));
    let trust = opts
        .trust
        .unwrap_or_else(|| snapshot.boolean("difftool.trustExitCode") == Some(true));

    // `setup_work_tree`: every raw path is relative to the work-tree root, so run
    // (and access work-tree files) from there.
    let workdir = absolute_workdir(repo)?;
    std::env::set_current_dir(&workdir)?;

    // The `git diff` child runs before anything else: its parse errors, and its
    // exit code, are what the user sees for an argument `difftool` forwarded.
    let records = match raw_diff(repo, &workdir, &opts.forward)? {
        Ok(records) => records,
        Err(code) => return Ok(code),
    };

    // An unmerged index entry makes `run_diff_files()` print a combined diff and
    // `continue`, so neither it nor the regular pair for the same path is ever
    // handed to `GIT_EXTERNAL_DIFF`.
    let unmerged: Vec<&[u8]> = records
        .iter()
        .filter(|r| r.status.starts_with('U'))
        .map(|r| r.path.as_slice())
        .collect();
    let mut combined = combined_diffs(&workdir, &opts.forward, unmerged.len())?.into_iter();

    // `q->nr` — what `GIT_DIFF_PATH_TOTAL` reports — counts only the queued pairs.
    let launches: Vec<&RawRecord> = records
        .iter()
        .filter(|r| !unmerged.iter().any(|p| *p == r.path.as_slice()))
        .collect();
    let total = launches.len();

    // The tool *name* is resolved up front, as the helper's prologue does; its
    // command waits until a launch is actually reached.
    let tool = select_tool(config, opts.extcmd.as_deref(), opts.tool.as_deref(), opts.gui)?;

    let mut stdout = std::io::stdout();
    let mut launched = 0usize;
    let tmpdir = (total > 0).then(mktemp_dir).transpose()?;

    let result = (|| -> Result<ExitCode> {
        for rec in &records {
            // Combined diffs are emitted in the same path order the launches are,
            // so the k-th unmerged record takes the k-th chunk.
            if rec.status.starts_with('U') {
                stdout.write_all(&combined.next().unwrap_or_default())?;
                continue;
            }
            if unmerged.iter().any(|p| *p == rec.path.as_slice()) {
                continue;
            }

            launched += 1;
            let merged = String::from_utf8_lossy(&rec.path).into_owned();
            let tmpdir = tmpdir.as_deref().expect("total > 0 when a record launches");

            // `prepare_temp_file` for each side: `/dev/null` for an absent side, a
            // staged blob (or submodule standin) for a recorded object, or the live
            // work-tree file for the unstaged side.
            let local = materialize_side(repo, tmpdir, &rec.path, &rec.mode_a, &rec.oid_a, "left")?;
            let remote =
                materialize_side(repo, tmpdir, &rec.path, &rec.mode_b, &rec.oid_b, "right")?;

            // `launch_merge_tool`: prompt (unless suppressed), then resolve and
            // eval the tool.
            let launched_tool = if prompt {
                write!(
                    stdout,
                    "\nViewing ({launched}/{total}): '{merged}'\nLaunch '{}' [Y/n]? ",
                    tool.label()
                )?;
                stdout.flush()?;
                match read_reply()? {
                    // `read ans || return` — a failed read leaves `$?` nonzero and
                    // launches nothing.
                    None => Launched::Status(1),
                    // `test "$ans" = n` — skip this file, `$?` is 0.
                    Some(ans) if ans == "n" => Launched::Status(0),
                    Some(_) => launch_file_tool(config, &tool, &local, &remote, &merged)?,
                }
            } else {
                launch_file_tool(config, &tool, &local, &remote, &merged)?
            };

            // The helper leaves non-zero on an unusable tool (`exit 1`), on
            // command-not-found/not-executable/signal (`status >= 126`), and,
            // under `--trust-exit-code`, on any failure — and every one of those
            // is a failed `run_command` that `run_external_diff` turns into a die.
            let dies = match launched_tool {
                Launched::HelperExit(_) => true,
                Launched::Status(status) => status >= 126 || (status != 0 && trust),
            };
            if dies {
                eprintln!("fatal: external diff died, stopping at {merged}");
                return Ok(ExitCode::from(128));
            }
        }
        Ok(ExitCode::SUCCESS)
    })();

    if let Some(dir) = &tmpdir {
        let _ = std::fs::remove_dir_all(dir);
    }
    result
}

/// What one `launch_merge_tool` attempt did.
pub(super) enum Launched {
    /// The tool ran; this is the `$?` the script records.
    Status(i32),
    /// `initialize_merge_tool "$merge_tool" || exit 1`, or `run_merge_tool`'s
    /// `get_merge_tool_path … || exit` — the helper leaves *immediately* with
    /// this status rather than moving on to the next path.
    HelperExit(i32),
}

/// One iteration of `launch_merge_tool`'s tail: resolve the command, then run it.
pub(super) fn launch_file_tool(
    config: &gix::config::File,
    tool: &Tool,
    local: &Path,
    remote: &Path,
    merged: &str,
) -> Result<Launched> {
    match tool_command(config, tool)? {
        Some(cmd) => Ok(Launched::Status(run_cmd(
            &cmd.text,
            local,
            remote,
            merged,
            cmd.append,
            &cmd.tool_path,
        )?)),
        None => Ok(Launched::HelperExit(1)),
    }
}

/// The helper's dir-diff branch: `LOCAL="$1"; REMOTE="$2";
/// initialize_merge_tool … run_merge_tool "$merge_tool" false`. There is no
/// `MERGED` here, so `$BASE` is empty and no prompt is shown.
pub(super) fn launch_dir_tool(
    config: &gix::config::File,
    tool: &Tool,
    ldir: &Path,
    rdir: &Path,
) -> Result<Launched> {
    launch_file_tool(config, tool, ldir, rdir, "")
}

/// `strvec_push(&child.args, "diff"); … "--raw" "-z"` plus the forwarded
/// revisions/pathspecs, run as a child of this binary.
///
/// `--abbrev=<hexsz>` stands in for git's `--no-abbrev` (this binary's `diff`
/// clamps `--abbrev` to the full hash width), giving full object ids to
/// materialise from. `Err(code)` carries the child's own failure: its stderr has
/// already been forwarded and its exit code is `difftool`'s, exactly as
/// `run_command(child)` makes it.
fn raw_diff(
    repo: &gix::Repository,
    workdir: &Path,
    forward: &[String],
) -> Result<std::result::Result<Vec<RawRecord>, ExitCode>> {
    let hexlen = repo.object_hash().len_in_hex();
    let abbrev = format!("--abbrev={hexlen}");
    let out = Command::new(current_exe()?)
        .current_dir(workdir)
        .args(["diff", "--raw", "-z", abbrev.as_str()])
        .args(forward)
        .output()?;
    if !out.status.success() {
        std::io::stderr().write_all(&out.stderr)?;
        return Ok(Err(ExitCode::from(out.status.code().unwrap_or(1) as u8)));
    }
    Ok(Ok(parse_raw(&out.stdout)?))
}

/// The combined diffs `run_diff_files()` prints for unmerged paths, one chunk per
/// path, in the order the raw enumeration reports them.
///
/// `--diff-filter=U` keeps exactly the unmerged entries, and each of their
/// combined diffs starts with a `diff --cc `/`diff --combined ` header at column
/// zero (every content line in a combined diff carries a one- or two-character
/// prefix, so no body line can be mistaken for one).
fn combined_diffs(workdir: &Path, forward: &[String], count: usize) -> Result<Vec<Vec<u8>>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let out = Command::new(current_exe()?)
        .current_dir(workdir)
        .args(["diff", "--diff-filter=U"])
        .args(forward)
        .output()?;
    if !out.status.success() {
        std::io::stderr().write_all(&out.stderr)?;
        crate::git_fatal!("could not obtain the combined diff of the unmerged paths");
    }

    let mut chunks: Vec<Vec<u8>> = Vec::new();
    for line in out.stdout.split_inclusive(|&b| b == b'\n') {
        let header = line.starts_with(b"diff --cc ") || line.starts_with(b"diff --combined ");
        if header || chunks.is_empty() {
            chunks.push(Vec::new());
        }
        chunks.last_mut().expect("pushed above").extend_from_slice(line);
    }
    // A count mismatch would misalign every later chunk; emitting the whole
    // buffer at the first unmerged path keeps the bytes but not the interleaving.
    if chunks.len() != count {
        let mut all = vec![out.stdout];
        all.resize(count, Vec::new());
        return Ok(all);
    }
    Ok(chunks)
}

// ---------------------------------------------------------------------------
// `run_dir_diff`
// ---------------------------------------------------------------------------

/// One work-tree file staged into `right/` from the work tree rather than from an
/// object: git's `wtindex`, which the copy-back loop walks.
struct WtEntry {
    path: Vec<u8>,
    /// The blob id `use_wt_file` hashed out of the work-tree file, i.e. what
    /// `right/<path>` held before the tool ran.
    oid: gix::ObjectId,
}

/// Port of `run_dir_diff`: stage `left/` and `right/` under one temp directory,
/// run the tool once on the pair, then copy back whatever it changed.
fn run_dir_diff(repo: &gix::Repository, opts: &Opts) -> Result<ExitCode> {
    let workdir = absolute_workdir(repo)?;
    std::env::set_current_dir(&workdir)?;

    let records = match raw_diff(repo, &workdir, &opts.forward)? {
        Ok(records) => records,
        Err(code) => return Ok(code),
    };

    let tmpdir = mktemp_dir()?;
    let ldir = tmpdir.join("left");
    let rdir = tmpdir.join("right");
    std::fs::create_dir_all(&ldir)?;
    std::fs::create_dir_all(&rdir)?;

    let outcome = dir_diff_inner(repo, opts, &workdir, &ldir, &rdir, &records);

    // `if (err) { warning("temporary files exist in '%s'."); … ret = 1; } else
    // remove_dir_recursively(&tmpdir, 0);` — the `err` arm fires only when a
    // copy-back was refused because both copies had changed.
    match &outcome {
        Ok((_, true)) => {
            eprintln!("warning: temporary files exist in '{}'.", tmpdir.display());
            eprintln!("warning: you may want to cleanup or recover these.");
            return Ok(ExitCode::from(1));
        }
        _ => {
            let _ = std::fs::remove_dir_all(&tmpdir);
        }
    }
    let (ret, _) = outcome?;
    if ret != 0 {
        eprintln!("warning: failed: {ret}");
    }
    // `return (ret < 0) ? 1 : ret;`
    Ok(ExitCode::from(if ret < 0 { 1 } else { ret as u8 }))
}

/// The body of `run_dir_diff` between the two temp directories being created and
/// the cleanup: stage both trees, run the tool, copy back. Returns the tool's
/// status and git's `err` flag (a path modified on both sides).
fn dir_diff_inner(
    repo: &gix::Repository,
    opts: &Opts,
    workdir: &Path,
    ldir: &Path,
    rdir: &Path,
    records: &[RawRecord],
) -> Result<(i32, bool)> {
    // `struct pair_entry`: a standin file's left and right contents, keyed by path
    // and written to both trees after the enumeration loop.
    let mut standins: Vec<(Vec<u8>, Option<String>, Option<String>)> = Vec::new();
    let mut add_standin = |path: &[u8], text: String, right: bool| {
        let slot = match standins.iter_mut().find(|(p, _, _)| p == path) {
            Some(slot) => slot,
            None => {
                standins.push((path.to_vec(), None, None));
                standins.last_mut().expect("pushed above")
            }
        };
        if right {
            slot.2 = Some(text);
        } else {
            slot.1 = Some(text);
        }
    };

    let mut wt_dups: Vec<Vec<u8>> = Vec::new();
    let mut wtindex: Vec<WtEntry> = Vec::new();

    for rec in records {
        if rec.combined {
            crate::git_fatal!(
                "combined diff formats ('-c' and '--cc') are not supported in\n\
                 directory diff mode ('-d' and '--dir-diff')."
            );
        }
        let src = rec.path.as_slice();
        let dst = rec.dst.as_deref().unwrap_or(src);

        // Submodules: a standin naming the recorded commit on each side.
        if rec.mode_a == "160000" || rec.mode_b == "160000" {
            add_standin(src, format!("Subproject commit {}", rec.oid_a), false);
            let mut right = format!("Subproject commit {}", rec.oid_b);
            if rec.oid_a == rec.oid_b {
                right.push_str("-dirty");
            }
            add_standin(dst, right, true);
            continue;
        }

        // Symlinks: git shows the link text, not the target's contents.
        if rec.mode_a == "120000" {
            add_standin(src, symlink_text(repo, workdir, &rec.oid_a, src)?, false);
        }
        if rec.mode_b == "120000" {
            add_standin(dst, symlink_text(repo, workdir, &rec.oid_b, dst)?, true);
        }

        // `if (lmode && status != 'C')` — the left tree always comes from objects.
        if rec.mode_a != "000000" && !rec.status.starts_with('C') {
            checkout_path(repo, ldir, src, &rec.mode_a, &rec.oid_a)?;
        }

        if rec.mode_b == "000000" || rec.mode_b == "120000" {
            continue;
        }
        // `working_tree_dups`: a path reached twice stages only once.
        if wt_dups.iter().any(|p| p == dst) {
            continue;
        }
        wt_dups.push(dst.to_vec());

        let mut roid = rec.oid_b.clone();
        if !use_wt_file(repo, workdir, dst, &mut roid)? {
            checkout_path(repo, rdir, dst, &rec.mode_b, &roid)?;
        } else if !roid.bytes().all(|b| b == b'0') {
            // A work-tree change is not in the index, so it is tracked separately
            // and either symlinked or copied into the right tree.
            wtindex.push(WtEntry {
                path: dst.to_vec(),
                oid: gix::ObjectId::from_hex(roid.as_bytes())?,
            });
            let target = workdir.join(bytes_path(dst));
            let link = rdir.join(bytes_path(dst));
            if let Some(parent) = link.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if opts.symlinks {
                std::os::unix::fs::symlink(&target, &link)?;
            } else {
                std::fs::copy(&target, &link)?;
            }
        }
    }

    // `if (!i) goto finish;` — nothing changed, so no tool is run.
    if records.is_empty() {
        return Ok((0, false));
    }

    // `write_standin_files`: each side gets a file only when that side has text.
    for (path, left, right) in &standins {
        if let Some(text) = left {
            write_standin(ldir, path, text)?;
        }
        if let Some(text) = right {
            write_standin(rdir, path, text)?;
        }
    }

    // The tool is run once on the two directories. git passes them with a
    // trailing separator and execs `extcmd` directly — no shell, unlike the
    // per-file launch.
    let lpath = with_trailing_slash(ldir);
    let rpath = with_trailing_slash(rdir);
    let ret = match opts.extcmd.as_deref().filter(|c| !c.is_empty()) {
        Some(extcmd) => match Command::new(extcmd).arg(&lpath).arg(&rpath).status() {
            Ok(status) => wait_status(status),
            Err(e) => {
                // `start_command` splits these: a bare name that `prepare_cmd`
                // cannot find on `$PATH` never forks and gets `error: cannot run
                // <cmd>`, while a name with a slash forks and the child's failed
                // `execve` is reported by `child_err_spew` as `fatal: cannot exec
                // '<cmd>'`. Either way `run_command` reports -1 to the caller.
                let reason = e.to_string();
                let reason = reason.split(" (os error ").next().unwrap_or(&reason);
                if extcmd.contains('/') {
                    eprintln!("fatal: cannot exec '{extcmd}': {reason}");
                } else {
                    eprintln!("error: cannot run {extcmd}: {reason}");
                }
                -1
            }
        },
        None => {
            // `strvec_push(&cmd.args, "difftool--helper"); cmd.git_cmd = 1;
            // setenv("GIT_DIFFTOOL_DIRDIFF", "true", 1);`
            let status = Command::new(current_exe()?)
                .arg("difftool--helper")
                .arg(&lpath)
                .arg(&rpath)
                .env("GIT_DIFFTOOL_DIRDIFF", "true")
                .env(
                    "GIT_DIFFTOOL_TRUST_EXIT_CODE",
                    if opts.trust.unwrap_or(false) { "true" } else { "false" },
                )
                .status()?;
            wait_status(status)
        }
    };

    let err = copy_back(opts, workdir, rdir, &wtindex)?;
    Ok((ret, err))
}

/// The tail of `run_dir_diff`: copy back every work-tree file the tool changed.
///
/// git decides "changed" by writing `wtindex` out and running
/// `update-index --really-refresh` + `diff-files --name-only` against it, once
/// for the work tree and once for the right-hand tree. Both reduce to "does this
/// file still hash to the id we recorded", which is what is computed here — the
/// stat cache the two children would consult is empty in a freshly written index,
/// so they compare contents too.
///
/// Returns git's `err` flag: some path had changed on both sides, so its
/// work-tree copy was left alone and the temp tree is kept for recovery.
fn copy_back(opts: &Opts, workdir: &Path, rdir: &Path, wtindex: &[WtEntry]) -> Result<bool> {
    let mut err = false;
    for entry in wtindex {
        let rel = bytes_path(&entry.path);
        let tmp = rdir.join(rel);
        let Ok(meta) = std::fs::symlink_metadata(&tmp) else {
            continue;
        };
        // Under `--symlinks` the tool edited the work-tree file through the link,
        // so there is nothing to copy; anything that is not a regular file is
        // skipped outright.
        if (opts.symlinks && meta.file_type().is_symlink()) || !meta.is_file() {
            continue;
        }

        let wt = workdir.join(rel);
        let tmp_modified = blob_id_of(&tmp)?.is_none_or(|id| id != entry.oid);
        if !tmp_modified {
            continue;
        }
        if blob_id_of(&wt)?.is_none_or(|id| id != entry.oid) {
            eprintln!(
                "warning: both files modified: '{}' and '{}'.",
                wt.display(),
                tmp.display()
            );
            eprintln!("warning: working tree file has been left.");
            eprintln!("warning: ");
            err = true;
            continue;
        }
        if let Err(e) = std::fs::remove_file(&wt).and_then(|()| std::fs::copy(&tmp, &wt).map(|_| ()))
        {
            eprintln!(
                "warning: could not copy '{}' to '{}': {e}",
                tmp.display(),
                wt.display()
            );
        }
    }
    Ok(err)
}

/// The blob id of a file's current contents, or `None` when it cannot be read.
fn blob_id_of(path: &Path) -> Result<Option<gix::ObjectId>> {
    let Ok(data) = std::fs::read(path) else {
        return Ok(None);
    };
    Ok(Some(gix::objs::compute_hash(
        gix::hash::Kind::Sha1,
        gix::object::Kind::Blob,
        &data,
    )?))
}

/// `use_wt_file`: whether the work-tree file can stand in for the recorded
/// post-image. A null id adopts the file's own hash (an unstaged change), a
/// recorded id has to match it, and a symlink never qualifies.
fn use_wt_file(
    repo: &gix::Repository,
    workdir: &Path,
    name: &[u8],
    oid: &mut String,
) -> Result<bool> {
    let path = workdir.join(bytes_path(name));
    let Ok(meta) = std::fs::symlink_metadata(&path) else {
        return Ok(false);
    };
    if meta.file_type().is_symlink() {
        return Ok(false);
    }
    let Ok(data) = std::fs::read(&path) else {
        return Ok(false);
    };
    let wt_oid =
        gix::objs::compute_hash(repo.object_hash(), gix::object::Kind::Blob, &data)?.to_string();
    if oid.bytes().all(|b| b == b'0') {
        *oid = wt_oid;
        return Ok(true);
    }
    Ok(*oid == wt_oid)
}

/// `get_symlink`: the link's text, read from the object when one is recorded and
/// from the work tree otherwise.
fn symlink_text(
    repo: &gix::Repository,
    workdir: &Path,
    oid_hex: &str,
    path: &[u8],
) -> Result<String> {
    if oid_hex.bytes().all(|b| b == b'0') {
        let target = std::fs::read_link(workdir.join(bytes_path(path)))
            .map_err(|_| anyhow!("could not read symlink {}", path.as_bstr()))?;
        return Ok(target.to_string_lossy().into_owned());
    }
    let oid = gix::ObjectId::from_hex(oid_hex.as_bytes())?;
    let object = repo.find_object(oid)?;
    Ok(String::from_utf8_lossy(&object.data).into_owned())
}

/// `write_file_in_directory`: replace `<dir>/<path>` with `<content>`, creating
/// the leading directories git's `ensure_leading_directories` would.
fn write_standin(dir: &Path, path: &[u8], content: &str) -> Result<()> {
    let dest = dir.join(bytes_path(path));
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(&dest);
    std::fs::write(dest, content)?;
    Ok(())
}

/// `checkout_path`: write the blob at `oid` to `<base>/<path>` with the mode's
/// executable bit. The bytes are written verbatim — no smudge filter, the same
/// floor `add.rs` records.
fn checkout_path(
    repo: &gix::Repository,
    base: &Path,
    path: &[u8],
    mode: &str,
    oid_hex: &str,
) -> Result<()> {
    let oid = gix::ObjectId::from_hex(oid_hex.as_bytes())
        .map_err(|e| anyhow!("bad object id {oid_hex:?} in raw diff: {e}"))?;
    let object = repo.find_object(oid)?;
    let dest = base.join(bytes_path(path));
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(&dest);
    std::fs::write(&dest, &object.data)?;
    if mode == "100755" {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// git hands the tool `<tmpdir>/left/` and `<tmpdir>/right/`, separator included.
fn with_trailing_slash(dir: &Path) -> std::ffi::OsString {
    let mut s = dir.as_os_str().to_owned();
    s.push("/");
    s
}

/// A raw-diff path as a filesystem path, without a lossy UTF-8 round trip.
fn bytes_path(path: &[u8]) -> &Path {
    Path::new(std::ffi::OsStr::from_bytes(path))
}

/// `absolute_path(repo_get_work_tree(repo))`, which `cmd_difftool` exports as
/// `GIT_WORK_TREE`. It has to be absolute *before* the process chdirs into it,
/// or a dir-diff symlink pointing at `<workdir>/<path>` would resolve relative to
/// the temp tree and become a loop.
fn absolute_workdir(repo: &gix::Repository) -> Result<PathBuf> {
    let workdir = repo.workdir().expect("work tree checked by caller");
    let joined = if workdir.is_absolute() {
        workdir.to_path_buf()
    } else {
        std::env::current_dir()?.join(workdir)
    };
    // `.` components would show up verbatim in the `both files modified` warning,
    // which quotes the joined path; `absolute_path()` does not leave any.
    Ok(joined
        .components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .collect())
}

/// The running executable, which stands in for git's `git_cmd = 1` children.
fn current_exe() -> Result<PathBuf> {
    std::env::current_exe().map_err(|e| anyhow!("cannot locate the running executable: {e}"))
}

// ---------------------------------------------------------------------------
// Raw diff parsing
// ---------------------------------------------------------------------------

/// A parsed `git diff --raw -z` record: the two modes, the two full object-id
/// hexes (all-zero for an absent/work-tree side), the status and the path (plus
/// the destination path a rename/copy carries).
struct RawRecord {
    mode_a: String,
    mode_b: String,
    oid_a: String,
    oid_b: String,
    status: String,
    path: Vec<u8>,
    dst: Option<Vec<u8>>,
    /// A `::`-prefixed header, i.e. a combined-diff record from `-c`/`--cc`.
    combined: bool,
}

/// Parse `git diff --raw -z` output: `:m1 m2 oid1 oid2 STATUS\0path[\0dst]\0`
/// records concatenated, with `C`/`R` statuses carrying the second path.
fn parse_raw(buf: &[u8]) -> Result<Vec<RawRecord>> {
    let mut fields = buf.split(|&b| b == 0);
    let mut out = Vec::new();
    while let Some(header) = fields.next() {
        if header.is_empty() {
            // Trailing empty field after the final NUL.
            break;
        }
        let Some(path) = fields.next() else {
            crate::git_fatal!("malformed raw diff: header with no path field");
        };
        // `:m1 m2 oid1 oid2 STATUS`.
        let header = std::str::from_utf8(header)
            .map_err(|_| anyhow!("malformed raw diff header (non-utf8)"))?;
        let combined = header.starts_with("::");
        let body = header.trim_start_matches(':');
        let parts: Vec<&str> = body.split(' ').collect();
        let [m1, m2, oid1, oid2, status] = parts.as_slice() else {
            crate::git_fatal!("malformed raw diff header: {header:?}");
        };
        let dst = if status.starts_with('C') || status.starts_with('R') {
            let Some(dst) = fields.next() else {
                crate::git_fatal!("malformed raw diff: {status} record with no destination path");
            };
            Some(dst.to_owned())
        } else {
            None
        };
        out.push(RawRecord {
            mode_a: (*m1).to_owned(),
            mode_b: (*m2).to_owned(),
            oid_a: (*oid1).to_owned(),
            oid_b: (*oid2).to_owned(),
            status: (*status).to_owned(),
            path: path.to_owned(),
            dst,
            combined,
        });
    }
    Ok(out)
}

/// `prepare_temp_file` for one side of one path.
///
///   * mode `000000` (`!DIFF_FILE_VALID`) → `/dev/null`.
///   * a gitlink (`160000`) → a temp holding `Subproject commit <hex>\n`; when the
///     side is the work tree (null id) the submodule's committed `HEAD` supplies
///     the hex.
///   * a recorded blob id → a temp holding the blob bytes.
///   * a null id on a non-gitlink side (the unstaged work-tree side) → the live
///     work-tree file itself, so tool edits land directly in the work tree.
fn materialize_side(
    repo: &gix::Repository,
    tmpdir: &Path,
    path: &[u8],
    mode: &str,
    oid_hex: &str,
    side: &str,
) -> Result<PathBuf> {
    if mode == "000000" {
        return Ok(PathBuf::from("/dev/null"));
    }
    let is_null = oid_hex.bytes().all(|b| b == b'0');

    // Gitlink: a "Subproject commit <hex>" standin, mirroring diff_populate_gitlink
    // and run_dir_diff's write_standin_files.
    if mode == "160000" {
        let hex = if is_null {
            // The work-tree submodule's committed HEAD.
            let abs = repo
                .workdir_path(gix::bstr::BStr::new(path))
                .ok_or_else(|| anyhow!("no work tree for submodule path"))?;
            let sub = gix::open(&abs).map_err(|e| {
                anyhow!("cannot open work-tree submodule for its HEAD standin: {e}")
            })?;
            sub.head_id()
                .map_err(|e| anyhow!("cannot resolve work-tree submodule HEAD: {e}"))?
                .detach()
                .to_string()
        } else {
            oid_hex.to_owned()
        };
        let content = format!("Subproject commit {hex}\n");
        return write_temp(tmpdir, side, path, content.as_bytes());
    }

    // The unstaged work-tree side: borrow the live file (git's reuse path).
    if is_null {
        return repo
            .workdir_path(gix::bstr::BStr::new(path))
            .ok_or_else(|| anyhow!("no work tree for path"));
    }

    // A recorded blob: stage its bytes into a temp file.
    let oid = gix::ObjectId::from_hex(oid_hex.as_bytes())
        .map_err(|e| anyhow!("bad object id {oid_hex:?} in raw diff: {e}"))?;
    let object = repo.find_object(oid)?;
    write_temp(tmpdir, side, path, &object.data)
}

/// Stage `content` at `<tmpdir>/<side>/<path>`, creating leading directories, so
/// the temp keeps the path's basename and extension (tools key syntax off it).
fn write_temp(tmpdir: &Path, side: &str, path: &[u8], content: &[u8]) -> Result<PathBuf> {
    let dest = tmpdir.join(side).join(bytes_path(path));
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&dest, content)?;
    Ok(dest)
}

/// `( eval $cmd )` (`append == false`, a user tool's `diff_cmd`) or
/// `eval $cmd '"$LOCAL"' '"$REMOTE"'` (`append == true`, `--extcmd`), with
/// `LOCAL`/`REMOTE`/`MERGED`/`BASE` in scope, run in a child `sh` to keep the
/// word-splitting and quoting identical, returning the `$?` a shell would see.
fn run_cmd(
    text: &str,
    local: &Path,
    remote: &Path,
    merged: &str,
    append: bool,
    tool_path: &str,
) -> Result<i32> {
    // `--extcmd`: `export BASE; eval $GIT_DIFFTOOL_EXTCMD '"$LOCAL"' '"$REMOTE"'`.
    const EXTCMD: &str = r#"LOCAL="$1"
REMOTE="$2"
MERGED="$3"
BASE="$3"
export BASE
eval $4 '"$LOCAL"' '"$REMOTE"'"#;
    // User tool: `run_diff_cmd` → `( eval $merge_tool_cmd )` with GIT_PREFIX set
    // and `merge_tool_path` assigned by the enclosing `run_merge_tool`, so a
    // `.cmd` that spells `$merge_tool_path` sees `difftool.<tool>.path`.
    const TOOL: &str = r#"LOCAL="$1"
REMOTE="$2"
MERGED="$3"
BASE="$3"
export BASE
merge_tool_path="$5"
GIT_PREFIX="${GIT_PREFIX:-.}"
export GIT_PREFIX
( eval $4 )"#;

    let script = if append { EXTCMD } else { TOOL };
    // `git-mergetool--lib.sh`, sourced by a `#!@SHELL_PATH@` script.
    let status = crate::external::shell()
        .arg("-c")
        .arg(script)
        .arg("sh")
        .arg(local)
        .arg(remote)
        .arg(merged)
        .arg(text)
        .arg(tool_path)
        .stdin(Stdio::inherit())
        .status()?;
    Ok(wait_status(status))
}

/// `--no-index`: compare two filesystem paths directly, the way
/// `git diff --no-index` does, with no repository involved.
///
///   * a path that cannot be `lstat`ed → `error: Could not access '<path>'`,
///     exit 1 (git checks the two paths in argv order);
///   * an identical pair → exit 0;
///   * a differing regular-file pair under `-x<cmd>`/`--extcmd=` → launch the
///     command on the two files directly (git's `--no-index` external-diff path).
///
/// A differing pair without an extcmd, a directory pair, or any path count other
/// than two still bails (a built-in tool needs the `mergetools/` catalogue; a
/// directory pair needs `--no-index`'s recursive walk; a non-2 count prints
/// `git diff --no-index`'s own usage block).
fn no_index(opts: &Opts) -> Result<ExitCode> {
    let paths: Vec<&str> = opts
        .forward
        .iter()
        .map(String::as_str)
        .filter(|a| !a.starts_with('-'))
        .collect();
    if let [a, b] = paths.as_slice() {
        let (a, b) = (*a, *b);
        // Accessibility, in argv order, using `lstat` so a broken symlink counts
        // as present (matching git, which does not follow the link here).
        for p in [a, b] {
            if std::fs::symlink_metadata(p).is_err() {
                eprintln!("error: Could not access '{p}'");
                return Ok(ExitCode::from(1));
            }
        }
        if paths_identical(a, b)? {
            return Ok(ExitCode::SUCCESS);
        }
        // A differing pair of regular files: launch `--extcmd` on them directly.
        let (ma, mb) = (std::fs::symlink_metadata(a)?, std::fs::symlink_metadata(b)?);
        if ma.is_file() && mb.is_file() {
            if let Some(x) = opts.extcmd.as_deref().filter(|v| !v.is_empty()) {
                let prompt = should_prompt(opts.prompt, None);
                if prompt {
                    print!("\nViewing (1/1): '{b}'\nLaunch '{x}' [Y/n]? ");
                    std::io::stdout().flush()?;
                    match read_reply()? {
                        None => {
                            eprintln!("fatal: external diff died, stopping at {b}");
                            return Ok(ExitCode::from(128));
                        }
                        Some(ans) if ans == "n" => return Ok(ExitCode::SUCCESS),
                        Some(_) => {}
                    }
                }
                let status = run_cmd(x, Path::new(a), Path::new(b), b, true, "")?;
                let trust = opts.trust.unwrap_or(false);
                if status >= 126 || (status != 0 && trust) {
                    eprintln!("fatal: external diff died, stopping at {b}");
                    return Ok(ExitCode::from(128));
                }
                return Ok(ExitCode::SUCCESS);
            }
        }
        crate::git_fatal!(
            "--no-index: {a:?} and {b:?} differ; launching a built-in tool needs the mergetools/ \
             catalogue and a directory pair needs --no-index's recursive walk, neither present in \
             the vendored crates (ported: an identical pair, an inaccessible path, and a differing \
             regular-file pair under -x/--extcmd)"
        );
    }
    crate::git_fatal!(
        "--no-index with {} path argument(s) prints `git diff --no-index`'s parse-options usage \
         block on stderr (exit 129); that block is `git diff`'s option surface, produced by its \
         parser rather than difftool's",
        paths.len()
    )
}

/// Whether two filesystem paths are diff-identical to `git diff --no-index`:
/// same type, same mode and same bytes.
fn paths_identical(a: &str, b: &str) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let (ma, mb) = (std::fs::symlink_metadata(a)?, std::fs::symlink_metadata(b)?);
    if ma.file_type().is_symlink() && mb.file_type().is_symlink() {
        return Ok(std::fs::read_link(a)? == std::fs::read_link(b)?);
    }
    if ma.is_file() && mb.is_file() {
        let exec_a = ma.permissions().mode() & 0o111 != 0;
        let exec_b = mb.permissions().mode() & 0o111 != 0;
        return Ok(exec_a == exec_b && std::fs::read(a)? == std::fs::read(b)?);
    }
    Ok(false)
}

/// `should_prompt`: the `difftool.prompt`/`mergetool.prompt` default (true),
/// overridden by `-y`/`--no-prompt` (never prompt) and `--prompt` (always
/// prompt). Reads the repository's merged configuration when there is one, else
/// the global files (`--no-index`, and the helper, also run outside a repository).
pub(super) fn should_prompt(flag: Option<bool>, config: Option<&gix::config::File>) -> bool {
    match flag {
        Some(v) => v,
        None => config_bool("difftool.prompt", config)
            .or_else(|| config_bool("mergetool.prompt", config))
            .unwrap_or(true),
    }
}

/// A boolean config value from the merged configuration when available, else the
/// system/global files.
///
/// Upstream reads these with `git config --bool`, whose failure on a malformed
/// value is swallowed by a `||` fallback; reading such a value as unset matches
/// that.
fn config_bool(key: &str, config: Option<&gix::config::File>) -> Option<bool> {
    if let Some(config) = config {
        if let Some(v) = config.boolean(key).ok().flatten() {
            return Some(v);
        }
    }
    gix::config::File::from_globals()
        .ok()
        .and_then(|f| f.boolean(key).ok().flatten())
}

/// One line of the user's reply, trimmed the way a POSIX `read ans` trims
/// (leading/trailing IFS whitespace). `None` marks the failing read at end of
/// input — including a final line with no terminating newline.
pub(super) fn read_reply() -> Result<Option<String>> {
    let mut buf = Vec::new();
    let mut stdin = std::io::stdin().lock();
    let mut byte = [0u8; 1];
    loop {
        match stdin.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => buf.push(byte[0]),
            Err(e) => return Err(e.into()),
        }
    }
    let s = String::from_utf8_lossy(&buf)
        .trim_matches(|c| c == ' ' || c == '\t')
        .to_owned();
    Ok(Some(s))
}

/// A per-invocation staging directory under the system temp location (git uses
/// `mkdtemp` on `$TMPDIR/git-difftool.XXXXXX`).
fn mktemp_dir() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("git-difftool.{}", std::process::id()));
    // A stale directory from a crashed prior run would poison the staging tree.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Whether a short-option cluster belongs to `difftool`: a run of its own
/// switches (`h`/`y`/`g`/`d`), optionally ending in a value switch (`t`/`x`)
/// whose stuck remainder is the value (`-tvimdiff`, `-yx meld`). A cluster with
/// any other letter before a value switch is unknown to `difftool` and forwarded
/// whole to `git diff`.
fn is_difftool_cluster(cluster: &str) -> bool {
    for c in cluster.chars() {
        match c {
            'h' | 'y' | 'g' | 'd' => continue,
            // A value switch consumes the rest of the cluster as its value.
            't' | 'x' => return true,
            _ => return false,
        }
    }
    true
}

/// The short letter a value-taking long option is spelled with (`tool` → `t`),
/// or `'\0'` for a name that is not one of them.
fn short_for(long: &str) -> char {
    VALUE_OPTS
        .iter()
        .find(|(l, _)| *l == long)
        .map(|(_, s)| *s)
        .unwrap_or('\0')
}

/// Record a `--tool`/`--extcmd` value, keyed by the option's short letter.
fn store_value(opts: &mut Opts, short: char, value: String) {
    match short {
        't' => opts.tool = Some(value),
        'x' => opts.extcmd = Some(value),
        _ => {}
    }
}

/// git's parse-options failure shape for `difftool`: `error: <msg>` on stderr,
/// exit 129. Unlike `-h`, no usage block follows.
fn usage_error(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::from(129)
}

/// `die_for_incompatible_opt3(use_gui_tool, "--gui", !!difftool_cmd, "--tool",
/// !!extcmd, "--extcmd")`: `--gui`, `--tool` and `--extcmd` are mutually
/// exclusive. `--tool`/`--extcmd` count as "set" whenever given, even with an
/// empty value (the C's `!!` pointer tests), so this fires before the empty-value
/// diagnostics. `None` when fewer than two are set. On stderr, exit 128.
fn incompatible_opt3(opts: &Opts) -> Option<ExitCode> {
    let mut set: Vec<&str> = Vec::new();
    if opts.gui == Some(true) {
        set.push("--gui");
    }
    if opts.tool.is_some() {
        set.push("--tool");
    }
    if opts.extcmd.is_some() {
        set.push("--extcmd");
    }
    match set.len() {
        3 => {
            eprintln!("fatal: options '--gui', '--tool', and '--extcmd' cannot be used together");
            Some(ExitCode::from(128))
        }
        2 => {
            eprintln!(
                "fatal: options '{}' and '{}' cannot be used together",
                set[0], set[1]
            );
            Some(ExitCode::from(128))
        }
        _ => None,
    }
}

/// The C's post-setup empty-value checks (steps 5–6): `if (difftool_cmd &&
/// !*difftool_cmd) die("no <tool> given for --tool=<tool>")` and the matching
/// `--extcmd` diagnostic, in that order. On stderr, exit 128.
fn empty_value_fatal(opts: &Opts) -> Option<ExitCode> {
    if opts.tool.as_deref() == Some("") {
        eprintln!("fatal: no <tool> given for --tool=<tool>");
        return Some(ExitCode::from(128));
    }
    if opts.extcmd.as_deref() == Some("") {
        eprintln!("fatal: no <cmd> given for --extcmd=<cmd>");
        return Some(ExitCode::from(128));
    }
    None
}

/// The `$?` a shell would see for a finished child: its exit code, or `128 + n`
/// when it died of signal `n`.
pub(super) fn wait_status(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        128 + status.signal().unwrap_or(0)
    }
    #[cfg(not(unix))]
    {
        128
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `git diff --raw -z` in a conflicted work tree emits *two* records for the
    /// same path — an unmerged `U` one and the ordinary work-tree comparison —
    /// which is what `run_file_diff` has to recognise so it prints the combined
    /// diff instead of launching a tool twice. Bytes taken from
    /// `git diff --raw --no-abbrev -z` on a repository with one conflict.
    #[test]
    fn parse_raw_reads_the_unmerged_pair_of_records() {
        const RAW: &[u8] = concat!(
            ":000000 100644 0000000000000000000000000000000000000000 ",
            "0000000000000000000000000000000000000000 U\0conflict.txt\0",
            ":100644 100644 b19a1e93bec1317dc6097229e12afaffbfa74dc2 ",
            "0000000000000000000000000000000000000000 M\0conflict.txt\0",
        )
        .as_bytes();

        let records = parse_raw(RAW).expect("well-formed raw diff");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].status, "U");
        assert_eq!(records[0].path, b"conflict.txt");
        assert!(records[0].dst.is_none());
        assert_eq!(records[1].status, "M");
        assert_eq!(records[1].path, b"conflict.txt");
        assert!(!records[0].combined && !records[1].combined);
    }

    /// A rename or copy carries a second path: the left tree is staged under the
    /// source name and the right tree under the destination.
    #[test]
    fn parse_raw_reads_the_destination_path_of_a_rename() {
        const RAW: &[u8] = concat!(
            ":100644 100644 b19a1e93bec1317dc6097229e12afaffbfa74dc2 ",
            "b19a1e93bec1317dc6097229e12afaffbfa74dc2 R100\0old.txt\0new.txt\0",
        )
        .as_bytes();

        let records = parse_raw(RAW).expect("well-formed raw diff");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, "R100");
        assert_eq!(records[0].path, b"old.txt");
        assert_eq!(records[0].dst.as_deref(), Some(&b"new.txt"[..]));
    }

    /// `-c`/`--cc` produces `::`-prefixed headers, which `run_dir_diff` refuses
    /// with git's own two-line diagnostic rather than mis-staging.
    #[test]
    fn parse_raw_flags_a_combined_header() {
        const RAW: &[u8] = concat!(
            "::100644 100644 100644 0000000000000000000000000000000000000000 ",
            "MM\0conflict.txt\0",
        )
        .as_bytes();

        let records = parse_raw(RAW).expect("well-formed raw diff");
        assert_eq!(records.len(), 1);
        assert!(records[0].combined);
    }

    /// `-x`/`--extcmd` and `--tool` are mutually exclusive with each other and
    /// with an explicit `--gui`. An empty value still counts as "given" (the C's
    /// `!!` pointer test), so this fires before the `no <tool> given` check, and
    /// `--no-gui` does not count at all.
    #[test]
    fn incompatible_opt3_counts_empty_values_as_given() {
        let with = |tool: Option<&str>, extcmd: Option<&str>, gui: Option<bool>| Opts {
            tool: tool.map(str::to_owned),
            extcmd: extcmd.map(str::to_owned),
            gui,
            ..Opts::default()
        };
        assert!(incompatible_opt3(&with(Some(""), None, None)).is_none());
        assert!(incompatible_opt3(&with(None, Some("true"), Some(false))).is_none());
        assert!(incompatible_opt3(&with(Some(""), Some(""), None)).is_some());
        assert!(incompatible_opt3(&with(Some("meld"), None, Some(true))).is_some());
    }

    /// A cluster is `difftool`'s only when every letter before a value switch is
    /// one of its own; anything else belongs to `git diff` and is forwarded whole.
    #[test]
    fn short_clusters_are_claimed_only_when_fully_understood() {
        assert!(is_difftool_cluster("yg"));
        assert!(is_difftool_cluster("tvimdiff"));
        assert!(is_difftool_cluster("yx"));
        // `-M` (rename detection) and `-w` are `diff`'s, not `difftool`'s.
        assert!(!is_difftool_cluster("M"));
        assert!(!is_difftool_cluster("wt"));
    }
}
