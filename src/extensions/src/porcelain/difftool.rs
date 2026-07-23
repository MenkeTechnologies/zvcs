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
//! What is ported (checked against git 2.39-era `builtin/difftool.c` and
//! `git-mergetool--lib.sh`):
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
//!   * **The file-diff launch** (`run_file_diff`): for a `-x<cmd>`/`--extcmd=`
//!     command, or a `-t<tool>`/`--tool=`/`diff.tool`/`diff.guitool` tool that has
//!     a `difftool.<tool>.cmd` (or `mergetool.<tool>.cmd`) config, each changed
//!     path from `git diff --raw -z` has its pre-image staged into a temp file and
//!     its post-image staged (or the live work-tree file borrowed) and the tool is
//!     launched per file with the `\nViewing (c/t): 'path'` / `Launch '…' [Y/n]? `
//!     prompt (skipped by `-y`/`--no-prompt`, forced by `--prompt`), the
//!     `eval $cmd '"$LOCAL"' '"$REMOTE"'` (extcmd) or `( eval $cmd )` (user tool)
//!     invocation, the `status >= 126` early exit, and
//!     `difftool.trustExitCode`/`--trust-exit-code`. An empty diff launches
//!     nothing and exits 0, matching git.
//!   * `git difftool --no-index <a> <b>`: an inaccessible path → `error: Could
//!     not access '<path>'`, exit 1; an identical pair → exit 0; a differing
//!     regular-file pair under `-x<cmd>`/`--extcmd=` launches the command on the
//!     two files directly (git's `--no-index` external-diff path).
//!
//! What bails, honestly, because the substrate is not in the vendored crates:
//!
//!   1. **Built-in tools and tool guessing.** A tool with no `difftool.<tool>.cmd`
//!      (`vimdiff`, `meld`, …) has its `diff_cmd` in a `mergetools/` shell script
//!      under `$(git --exec-path)`, and picking a tool with neither `--tool` nor
//!      `diff.tool`/`merge.tool` runs `guess_merge_tool` over that same catalogue.
//!      Nothing under `src/ported` carries the `mergetools/` database, so these
//!      paths bail rather than run the wrong program.
//!   2. **`--dir-diff` (`-d`).** `run_dir_diff` stages two whole temp trees, then
//!      copies files the tool modified back into the work tree. Skipping that
//!      copy-back would silently drop the user's edits, so dir-diff bails rather
//!      than launch a partial implementation that loses data.
//!
//! Known approximations of the ported path: a work-tree symlink is handed to the
//! tool as the link itself rather than a temp holding its `readlink` text, and a
//! dirty work-tree submodule's standin omits the `-dirty` suffix (its committed
//! `HEAD` is still shown).

use anyhow::{anyhow, bail, Result};
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
#[derive(Default)]
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
    /// `-g`/`--gui` was given (its negation clears it). Steers the tool config
    /// key order (`diff.guitool` first).
    gui: bool,
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

/// `git difftool` — validate arguments, then launch a diff tool for each changed
/// path (`run_file_diff`).
///
/// See the module documentation for the exact set of invocations that are
/// reproduced and for the substrate the bailing paths would need.
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
            "-g" | "--gui" => opts.gui = true,
            "--no-gui" => opts.gui = false,
            "--symlinks" | "--no-symlinks" => {}
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
                        'g' => opts.gui = true,
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

    // `--dir-diff` needs the two-temp-tree staging *and* the copy-back of files
    // the tool modified; skipping the copy-back would lose the user's edits, so
    // it bails rather than run a data-losing partial (see module docs).
    if opts.dir_diff {
        bail!(
            "--dir-diff (`-d`) needs run_dir_diff's two temp trees plus the copy-back of \
             tool-modified files into the work tree; a partial implementation would silently \
             drop those edits, so it is not launched \
             (ported: the per-file launch for -x/--extcmd and difftool.<tool>.cmd tools)"
        );
    }

    // Phase 4 — the file-diff launch.
    run_file_diff(&repo, &opts)
}

/// The resolved diff command and how to invoke it.
struct DiffCmd {
    /// The shell command text (`eval`'d in a child `sh`).
    text: String,
    /// Whether git appends `'"$LOCAL"' '"$REMOTE"'` after it (the `--extcmd`
    /// convention) or not (a user tool's `difftool.<tool>.cmd`, which references
    /// `$LOCAL`/`$REMOTE` itself).
    append: bool,
    /// The name shown in the `Launch '<label>' [Y/n]?` prompt.
    label: String,
}

/// `run_file_diff` + `git-difftool--helper.sh`: enumerate the changed paths with
/// this binary's own `git diff --raw -z`, then launch the resolved tool once per
/// path with the pre-/post-image staged the way `prepare_temp_file` stages them.
fn run_file_diff(repo: &gix::Repository, opts: &Opts) -> Result<ExitCode> {
    let snapshot = repo.config_snapshot();
    // Resolve the tool/extcmd first — its failure (a built-in tool, or nothing
    // configured) is the honest floor and must win before any temp is created.
    let cmd = resolve_command(&snapshot, opts)?;

    // `should_prompt`, and `GIT_DIFFTOOL_TRUST_EXIT_CODE` from
    // `difftool.trustExitCode` / `--trust-exit-code`.
    let prompt = should_prompt(opts.prompt, Some(&snapshot));
    let trust = opts
        .trust
        .unwrap_or_else(|| snapshot.boolean("difftool.trustExitCode") == Some(true));

    // `setup_work_tree`: every raw path is relative to the work-tree root, so run
    // (and access work-tree files) from there.
    let workdir = repo.workdir().expect("work tree checked by caller").to_path_buf();
    std::env::set_current_dir(&workdir)?;

    // `strvec_push(&child.args, "diff"); … "--raw" "-z"` plus the forwarded
    // revisions/pathspecs. `--abbrev=<hexsz>` stands in for git's `--no-abbrev`
    // (this binary's `diff` clamps `--abbrev` to the full hash width), giving full
    // object ids to materialise from.
    let hexlen = repo.object_hash().len_in_hex();
    let abbrev = format!("--abbrev={hexlen}");
    let exe = std::env::current_exe()
        .map_err(|e| anyhow!("cannot locate the running executable: {e}"))?;
    let out = Command::new(&exe)
        .current_dir(&workdir)
        .args(["diff", "--raw", "-z", abbrev.as_str()])
        .args(&opts.forward)
        .output()?;
    if !out.status.success() {
        // `die("could not obtain raw diff")` — surface the child's own diagnostic
        // and exit code rather than guessing at the diff.
        std::io::stderr().write_all(&out.stderr)?;
        return Ok(ExitCode::from(out.status.code().unwrap_or(1) as u8));
    }

    let records = parse_raw(&out.stdout)?;
    let total = records.len();
    if total == 0 {
        // Nothing changed: git launches no tool and exits 0.
        return Ok(ExitCode::SUCCESS);
    }

    // A per-invocation staging directory for pre-/post-image temp files.
    let tmpdir = mktemp_dir()?;

    let mut stdout = std::io::stdout();
    let result = (|| -> Result<ExitCode> {
        for (idx, rec) in records.iter().enumerate() {
            let counter = idx + 1;
            let merged = String::from_utf8_lossy(&rec.path).into_owned();

            // `prepare_temp_file` for each side: `/dev/null` for an absent side, a
            // staged blob (or submodule standin) for a recorded object, or the live
            // work-tree file for the unstaged side.
            let local = materialize_side(repo, &tmpdir, &rec.path, &rec.mode_a, &rec.oid_a, "left")?;
            let remote = materialize_side(repo, &tmpdir, &rec.path, &rec.mode_b, &rec.oid_b, "right")?;

            // `launch_merge_tool`: prompt (unless suppressed), then eval the tool.
            let status = if prompt {
                write!(
                    stdout,
                    "\nViewing ({counter}/{total}): '{merged}'\nLaunch '{}' [Y/n]? ",
                    cmd.label
                )?;
                stdout.flush()?;
                match read_reply()? {
                    // `read ans || return` — a failed read leaves `$?` nonzero and
                    // launches nothing.
                    None => 1,
                    // `test "$ans" = n` — skip this file, `$?` is 0.
                    Some(ans) if ans == "n" => 0,
                    Some(_) => run_cmd(&cmd.text, &local, &remote, &merged, cmd.append)?,
                }
            } else {
                run_cmd(&cmd.text, &local, &remote, &merged, cmd.append)?
            };

            // Command not found (127), not executable (126) or death by signal.
            if status >= 126 {
                return Ok(ExitCode::from(status as u8));
            }
            if status != 0 && trust {
                return Ok(ExitCode::from(status as u8));
            }
        }
        Ok(ExitCode::SUCCESS)
    })();

    let _ = std::fs::remove_dir_all(&tmpdir);
    result
}

/// `get_merge_tool`/`get_merge_tool_cmd` for diff mode: resolve the command to
/// launch, or bail with the substrate a built-in tool / a guess would need.
fn resolve_command(snapshot: &gix::config::Snapshot<'_>, opts: &Opts) -> Result<DiffCmd> {
    // `use_ext_cmd`: a non-empty `--extcmd` wins outright and appends the paths.
    if let Some(x) = opts.extcmd.as_deref().filter(|v| !v.is_empty()) {
        return Ok(DiffCmd { text: x.to_owned(), append: true, label: x.to_owned() });
    }

    // The tool name: `--tool` (git's `GIT_DIFF_TOOL`) wins, else
    // `get_configured_merge_tool`'s diff-mode key order.
    let tool = match opts.tool.as_deref().filter(|v| !v.is_empty()) {
        Some(t) => t.to_owned(),
        None => {
            let keys: &[&str] = if opts.gui {
                &["diff.guitool", "merge.guitool", "diff.tool", "merge.tool"]
            } else {
                &["diff.tool", "merge.tool"]
            };
            match keys.iter().find_map(|k| {
                snapshot.string(*k).map(|v| v.to_str_lossy().into_owned()).filter(|v| !v.is_empty())
            }) {
                Some(t) => t,
                None => bail!(
                    "no diff tool configured: without --tool/--extcmd or diff.tool/merge.tool, git \
                     runs guess_merge_tool over the mergetools/ catalogue under $(git --exec-path), \
                     which is not present in the vendored crates \
                     (ported: -x/--extcmd and any tool with difftool.<tool>.cmd)"
                ),
            }
        }
    };

    // `get_merge_tool_cmd` (diff mode): `difftool.<tool>.cmd` then
    // `mergetool.<tool>.cmd`. A non-empty value is a user-defined tool, run as
    // `( eval $cmd )` without appended paths.
    let cmd = snapshot
        .string(&format!("difftool.{tool}.cmd"))
        .or_else(|| snapshot.string(&format!("mergetool.{tool}.cmd")))
        .map(|v| v.to_str_lossy().into_owned())
        .filter(|v| !v.is_empty());
    match cmd {
        Some(c) => Ok(DiffCmd { text: c, append: false, label: tool }),
        None => bail!(
            "built-in diff tool {tool:?} has no difftool.{tool}.cmd/mergetool.{tool}.cmd config; \
             its diff_cmd lives in a mergetools/ shell script under $(git --exec-path), which is not \
             present in the vendored crates \
             (ported: -x/--extcmd and any tool with a configured .cmd)"
        ),
    }
}

/// A parsed `git diff --raw -z` record: the two modes, the two full object-id
/// hexes (all-zero for an absent/work-tree side) and the path.
struct RawRecord {
    mode_a: String,
    mode_b: String,
    oid_a: String,
    oid_b: String,
    path: Vec<u8>,
}

/// Parse `git diff --raw -z` output: `:m1 m2 oid1 oid2 STATUS\0path\0` records
/// concatenated. This binary's `diff` performs no rename/copy detection, so every
/// record carries exactly one path and every status is one of `A`/`D`/`M`/`T`/`U`.
fn parse_raw(buf: &[u8]) -> Result<Vec<RawRecord>> {
    let mut fields = buf.split(|&b| b == 0);
    let mut out = Vec::new();
    loop {
        let Some(header) = fields.next() else { break };
        if header.is_empty() {
            // Trailing empty field after the final NUL.
            break;
        }
        let Some(path) = fields.next() else {
            bail!("malformed raw diff: header with no path field");
        };
        // `:m1 m2 oid1 oid2 STATUS`.
        let header = std::str::from_utf8(header)
            .map_err(|_| anyhow!("malformed raw diff header (non-utf8)"))?;
        let body = header.strip_prefix(':').unwrap_or(header);
        let parts: Vec<&str> = body.split(' ').collect();
        let [m1, m2, oid1, oid2, _status] = parts.as_slice() else {
            bail!("malformed raw diff header: {header:?}");
        };
        out.push(RawRecord {
            mode_a: (*m1).to_owned(),
            mode_b: (*m2).to_owned(),
            oid_a: (*oid1).to_owned(),
            oid_b: (*oid2).to_owned(),
            path: path.to_owned(),
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
    let rel = Path::new(std::ffi::OsStr::from_bytes(path));
    let dest = tmpdir.join(side).join(rel);
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
fn run_cmd(text: &str, local: &Path, remote: &Path, merged: &str, append: bool) -> Result<i32> {
    // `--extcmd`: `export BASE; eval $GIT_DIFFTOOL_EXTCMD '"$LOCAL"' '"$REMOTE"'`.
    const EXTCMD: &str = r#"LOCAL="$1"
REMOTE="$2"
MERGED="$3"
BASE="$3"
export BASE
eval $4 '"$LOCAL"' '"$REMOTE"'"#;
    // User tool: `run_diff_cmd` → `( eval $merge_tool_cmd )` with GIT_PREFIX set.
    const TOOL: &str = r#"LOCAL="$1"
REMOTE="$2"
MERGED="$3"
BASE="$3"
export BASE
GIT_PREFIX="${GIT_PREFIX:-.}"
export GIT_PREFIX
( eval $4 )"#;

    let script = if append { EXTCMD } else { TOOL };
    let status = Command::new("sh")
        .arg("-c")
        .arg(script)
        .arg("sh")
        .arg(local)
        .arg(remote)
        .arg(merged)
        .arg(text)
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
                        None => return Ok(ExitCode::from(1)),
                        Some(ans) if ans == "n" => return Ok(ExitCode::SUCCESS),
                        Some(_) => {}
                    }
                }
                let status = run_cmd(x, Path::new(a), Path::new(b), b, true)?;
                if status >= 126 {
                    return Ok(ExitCode::from(status as u8));
                }
                let trust = opts.trust.unwrap_or(false);
                if status != 0 && trust {
                    return Ok(ExitCode::from(status as u8));
                }
                return Ok(ExitCode::SUCCESS);
            }
        }
        bail!(
            "--no-index: {a:?} and {b:?} differ; launching a built-in tool needs the mergetools/ \
             catalogue and a directory pair needs --no-index's recursive walk, neither present in \
             the vendored crates (ported: an identical pair, an inaccessible path, and a differing \
             regular-file pair under -x/--extcmd)"
        );
    }
    bail!(
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
/// prompt). Reads config from the given repository snapshot, or the global files
/// (`--no-index` runs outside a repository).
fn should_prompt(flag: Option<bool>, snapshot: Option<&gix::config::Snapshot<'_>>) -> bool {
    match flag {
        Some(v) => v,
        None => config_bool("difftool.prompt", snapshot)
            .or_else(|| config_bool("mergetool.prompt", snapshot))
            .unwrap_or(true),
    }
}

/// A boolean config value from the repository snapshot when available, else the
/// system/global files (mirroring the `difftool--helper` sibling).
fn config_bool(key: &str, snapshot: Option<&gix::config::Snapshot<'_>>) -> Option<bool> {
    if let Some(snap) = snapshot {
        if let Some(v) = snap.boolean(key) {
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
fn read_reply() -> Result<Option<String>> {
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
    let dir = std::env::temp_dir().join(format!("git-difftool-{}", std::process::id()));
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
    if opts.gui {
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
fn wait_status(status: ExitStatus) -> i32 {
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
