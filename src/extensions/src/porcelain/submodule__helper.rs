//! `git submodule--helper` — the internal dispatcher behind `git submodule`.
//!
//! In git 2.55.0 this is a builtin whose only job is `parse_options` with
//! `PARSE_OPT_SUBCOMMAND`: it owns no options of its own, and every subcommand
//! it names is the same C function `git submodule` reaches. Fourteen
//! subcommands are registered (verified by probing `<cmd> -h` against git
//! 2.55.0 on Darwin): `clone`, `add`, `update`, `foreach`, `init`, `status`,
//! `sync`, `deinit`, `summary`, `push-check`, `absorbgitdirs`, `set-url`,
//! `set-branch`, `create-branch`, plus `gitdir`, `get-default-remote` and
//! `migrate-gitdir-configs`.
//!
//! Ported, byte-for-byte against git 2.55.0:
//!
//!   * **The whole dispatcher.** No arguments →
//!     ``error: need a subcommand`` + usage on stderr, exit 129. Unknown word →
//!     ``error: unknown subcommand: `X'``. Unknown `--long` →
//!     ``error: unknown option `X'``. Unknown `-x` →
//!     ``error: unknown switch `x'``. `-h` (including as the first letter of a
//!     cluster, e.g. `-hx`) → the usage block on **stdout**, exit 129. `--` and
//!     `--end-of-options` terminate option scanning without naming a
//!     subcommand, so both land on ``error: need a subcommand``. The usage
//!     block is `usage: git submodule--helper <command>\n\n` in every case.
//!
//!   * **`gitdir <name>`** — git's `submodule_name_to_gitdir` in its default
//!     shape: `repo_git_path(r, "modules/%s", name)`, i.e. the git directory as
//!     git's own setup resolved it, `/modules/`, then the name verbatim (no
//!     validation: `../evil` and `a/b` pass through unchanged). Wrong argument
//!     count → `usage: git submodule--helper gitdir <name>` on stderr (one
//!     line, no trailing blank), exit 129. The git-directory spelling is
//!     reproduced rather than taken from gitoxide, because git prints the
//!     *relative* `.git` when it discovered the repository by walking up, and
//!     `gix` always hands back an absolute path: `.git` for a repository whose
//!     `.git` is a real directory, the value of `GIT_DIR` verbatim when that is
//!     set, the resolved absolute path for a `.git` gitfile or linked worktree,
//!     and `.` (which `cleanup_path` then elides, yielding `modules/<name>`)
//!     for a bare repository entered at its top level.
//!
//!   * **`get-default-remote <path>`** — git's `repo_get_default_remote` run
//!     against the repository at `<path>`: the branch's `branch.<name>.remote`
//!     when `HEAD` is a symref into `refs/heads/`, otherwise `origin`. A
//!     detached, unborn or remote-less `HEAD` therefore all print `origin`.
//!     A path that is not a repository →
//!     `fatal: could not get a repository handle for submodule '<prefix+path>'`
//!     and exit 128, with the path reported relative to the superproject root
//!     exactly as git's `prefix_path` renders it. Wrong argument count → the
//!     `usage_with_options` block (usage line plus a blank line) on stderr,
//!     exit 129.
//!
//!   * **`foreach`** parses `module_foreach`'s own option table here and then
//!     calls the porcelain's body directly. It is the one shared subcommand that
//!     cannot be forwarded verbatim, because the two entry points scan their
//!     arguments differently: `module_foreach` calls `parse_options(..., 0)`,
//!     which **permutes** — an option after the command is still an option —
//!     while `git-submodule.sh`'s `cmd_foreach` loop stops at the first
//!     non-option and re-invokes the helper behind an explicit `--`. So
//!     `submodule--helper foreach does-not-exist -q` is a quiet one-argument
//!     command (the shell form, no `Entering` line) where
//!     `submodule foreach does-not-exist -q` is a noisy two-argument one. The
//!     usage block, the abbreviation matching (`--qu` is `--quiet`, `--no-` is
//!     ambiguous), `--help-all`'s hidden `--super-prefix`, and which of those
//!     land on stdout rather than stderr all follow parse-options.
//!
//!   * **`status`**, **`init`**, **`summary`**, **`sync`**,
//!     **`update`**, **`deinit`**, **`absorbgitdirs`**, **`set-branch`** and
//!     **`set-url`** delegate to [`super::submodule::subcommand`], which
//!     implements them. Each is registered in builtin/submodule--helper.c's
//!     `OPT_SUBCOMMAND` table against the very same C function (`module_status`,
//!     `module_init`, `module_summary`, `module_sync`,
//!     `module_update`, `module_deinit`, `absorb_git_dirs`,
//!     `module_set_branch`, `module_set_url`) that `git submodule <name>`
//!     dispatches to, so forwarding `[<name>, <tail>...]` into the porcelain
//!     module reproduces the helper. `status`/`init` were confirmed to emit
//!     identical bytes here (including the `../sm` display path from a
//!     subdirectory).
//!
//!     The forward deliberately targets `subcommand` rather than
//!     `submodule`: the porcelain entry point also reproduces
//!     `git-submodule.sh:29`'s `GIT_PROTOCOL_FROM_USER=0` export, and the helper
//!     has no such export — which is why `git submodule--helper update --remote`
//!     fetches over a `file` url where `git submodule update --remote` dies
//!     `transport 'file' not allowed`.
//!
//!   * **`add`** parses `module_add`'s own option table here — the porcelain
//!     wrapper forwards its arguments unvalidated except for a missing
//!     `<repository>`, so the two disagree on the error: a wrong operand count
//!     is `usage_with_options` (the add usage block, exit 129) for the helper
//!     and the `git-submodule.sh` usage block (exit 1) for `git submodule add`.
//!     Past those checks the work is the porcelain's.
//!
//!   * **`push-check <superproject-head> <remote> [<refspec>...]`** is ported
//!     whole: the operand count, `refs_resolve_refdup(..., "HEAD", 0, ...)` and
//!     the detached-HEAD test it feeds, the "the remote must be configured" rule
//!     that stops a submodule pushing to the superproject's own url, and the
//!     per-refspec left-hand-side check — `count_refspec_match()` over every ref
//!     under `refs/`, with `refname_match()`'s six rules and its weak/strong
//!     distinction, and `HEAD`'s special case against the superproject's branch.
//!
//! Not ported — each bails naming the missing substrate rather than guessing,
//! but only *after* reproducing the argument checks that come first:
//!
//!   * `clone` — needs transport plus worktree materialisation for a submodule
//!     that `update` has not already planned. `module_clone`'s option table and
//!     its `if (argc || !clone_data.url || !clone_data.path ||
//!     !*(clone_data.path)) usage_with_options(...)` are reproduced, so a bare
//!     `submodule--helper clone` prints git's usage block and exits 129.
//!   * `create-branch` — `git branch` inside a submodule with `--track`
//!     bookkeeping. `if (argc != 3) usage_with_options(usage, options)` is
//!     reproduced, usage block and exit 129 included.
//!   * `migrate-gitdir-configs` — the `extensions.submodulePathConfig`
//!     migration (rewrites `core.repositoryformatversion`, sets
//!     `submodule.<name>.gitdir` per module, relocates git dirs).
//!
//! `gitdir` additionally bails when `extensions.submodulePathConfig` is
//! enabled: that path reads `submodule.<name>.gitdir` and runs git's
//! `validate_submodule_git_dir` containment check, neither of which any
//! vendored crate under `src/ported` implements. (`gix` may also refuse to open
//! such a repository outright, since the extension is unknown to it.)

use anyhow::{bail, Result};
use std::process::ExitCode;

use gix::bstr::{BString, ByteSlice};
use gix::config::KeyRef;

/// The dispatcher's usage block: one line plus a blank line, 40 bytes.
const USAGE: &str = "usage: git submodule--helper <command>\n\n";

/// `git submodule--helper` — dispatch to a submodule subcommand.
///
/// Reproduces `parse_options`' `PARSE_OPT_SUBCOMMAND` behaviour exactly (this
/// builtin declares no options of its own), then routes to the four ported
/// subcommands; every other registered subcommand bails.
#[allow(non_snake_case)] // maps to git's `submodule--helper` subcommand
pub fn submodule__helper(args: &[String]) -> Result<ExitCode> {
    // Dispatch hands us the tail; tolerate the subcommand name at index 0 so
    // either calling convention behaves the same.
    let args = match args.first() {
        Some(a) if a == "submodule--helper" => &args[1..],
        _ => args,
    };

    let mut sub: Option<usize> = None;
    // Scans args left-to-right to find the subcommand token; the first hit returns,
    // so clippy sees "loop that never iterates twice" — the scan is intentional.
    #[allow(clippy::never_loop)]
    for (n, a) in args.iter().enumerate() {
        // `--`/`--end-of-options` stop option scanning; parse_options then has
        // no subcommand to run, which is the "need a subcommand" path.
        if a == "--" || a == "--end-of-options" {
            break;
        }
        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // placed after those two breaks and ahead of parse_long_opt(): the name
        // never abbreviates and never takes an `=<value>`, so `--help-a` and
        // `--help-all=x` still reach the unknown-option refusal below. The
        // builtin's table holds nothing but `OPT_SUBCOMMAND` entries — no
        // `PARSE_OPT_HIDDEN` one — so `USAGE_FULL` is the block `-h` prints.
        if a == "--help-all" {
            print!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
        if let Some(name) = a.strip_prefix("--") {
            eprintln!("error: unknown option `{name}'");
            eprint!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
        // `-` alone is not an option; it falls through as a subcommand name.
        if a.len() > 1 && a.starts_with('-') {
            // Short cluster: the first letter decides. `-h` wins immediately
            // (so `-hx` prints help), any other letter is reported and stops.
            let c = a[1..].chars().next().expect("len > 1");
            if c == 'h' {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            eprintln!("error: unknown switch `{c}'");
            eprint!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
        sub = Some(n);
        break;
    }

    let Some(n) = sub else {
        eprintln!("error: need a subcommand");
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    };
    let name = args[n].as_str();
    let tail = &args[n + 1..];

    match name {
        "gitdir" => gitdir(tail),
        "get-default-remote" => get_default_remote(tail),
        // Upstream these are literally the same C functions `git submodule`
        // dispatches to (`module_status`, `module_init`,
        // `module_summary`, `module_sync`, `module_update`, `module_deinit`,
        // `absorb_git_dirs`, `module_set_branch`, `module_set_url` — see the
        // `OPT_SUBCOMMAND` table in builtin/submodule--helper.c), so the
        // porcelain module owns the implementation and the helper forwards to
        // its shared subcommand table. It is deliberately *not* routed through
        // `submodule()`: that entry point also reproduces `git-submodule.sh`'s
        // `GIT_PROTOCOL_FROM_USER=0` export, which the helper does not have —
        // `git submodule--helper update --remote` fetches over `file` where
        // `git submodule update --remote` refuses.
        // `foreach` is the one shared subcommand whose *parse* differs between
        // the two entry points, so it cannot be forwarded verbatim — see
        // [`foreach`].
        "foreach" => foreach(tail),
        "status" | "init" | "summary" | "set-branch" | "sync" | "update" | "deinit"
        | "absorbgitdirs" | "set-url" => {
            let mut forwarded = Vec::with_capacity(tail.len() + 1);
            forwarded.push(name.to_string());
            forwarded.extend(tail.iter().cloned());
            super::submodule::subcommand(&forwarded)
        }
        // `module_add` is likewise the same C function `git submodule add`
        // reaches, but the two disagree before it: the porcelain wrapper does no
        // option parsing of its own and forwards everything, so a wrong operand
        // count here is `module_add`'s own `usage_with_options` (exit 129) while
        // the wrapper's is the `git-submodule.sh` usage block (exit 1).
        "add" => add(tail),
        "clone" => clone(tail),
        "push-check" => push_check(tail),
        "create-branch" => create_branch(tail),
        "migrate-gitdir-configs" => bail!(
            "unsupported subcommand \"migrate-gitdir-configs\": the extensions.submodulePathConfig migration is not ported"
        ),
        other => {
            eprintln!("error: unknown subcommand: `{other}'");
            eprint!("{USAGE}");
            Ok(ExitCode::from(129))
        }
    }
}

// --------------------------------------------------------------- foreach ----

/// The `usage_with_options` block `module_foreach` prints, captured from git
/// 2.55.0: the usage line, a blank line, its two options, and a trailing blank
/// line. It is *not* the `git-submodule.sh` usage the porcelain wrapper prints.
const FOREACH_USAGE: &str = "\
usage: git submodule foreach [--quiet] [--recursive] [--] <command>

    -q, --[no-]quiet      suppress output of entering each submodule command
    --[no-]recursive      recurse into nested submodules

";

/// `USAGE_FULL` — what `--help-all` prints, which is the block above plus the
/// `PARSE_OPT_HIDDEN` `--super-prefix`.
const FOREACH_USAGE_FULL: &str = "\
usage: git submodule foreach [--quiet] [--recursive] [--] <command>

    --[no-]super-prefix <prefix>
                          prefixed path to initial superproject
    -q, --[no-]quiet      suppress output of entering each submodule command
    --[no-]recursive      recurse into nested submodules

";

/// `git submodule--helper foreach [<options>] [--] <command>` — `module_foreach`
/// (builtin/submodule--helper.c:432).
///
/// The whole reason this is not forwarded to [`super::submodule::subcommand`] is
/// the option scan. `module_foreach` calls `parse_options(..., 0)`: with no
/// `PARSE_OPT_STOP_AT_NON_OPTION`, parse-options **permutes** — a non-option is
/// copied to `ctx->out` and the walk continues, so an option after the command
/// is still an option. `git submodule foreach` never sees that, because
/// `git-submodule.sh`'s `cmd_foreach` stops at the first non-option itself and
/// then re-invokes the helper with an explicit `--`.
///
/// The difference is observable in two ways at once, because
/// `runcommand_in_submodule_cb` branches on `info->argc == 1`:
///
/// ```text
/// git submodule--helper foreach does-not-exist -q
///   → -q permutes out, one argument is left, so the command runs through the
///     shell as `path='sm'; does-not-exist` and no `Entering` line prints
/// git submodule foreach does-not-exist -q
///   → `-q` stays in the command, two arguments are exec'd directly
/// ```
fn foreach(args: &[String]) -> Result<ExitCode> {
    /// `PARSE_OPT_UNKNOWN` reaching `parse_options`: the `error:` line, then the
    /// usage block, then exit 129.
    fn unknown(message: String) -> Result<ExitCode> {
        eprintln!("error: {message}");
        eprint!("{FOREACH_USAGE}");
        Ok(ExitCode::from(129))
    }
    /// `PARSE_OPT_ERROR`: the `error:` line and exit 129 with **no** usage block
    /// — `parse_options` exits on it before reaching `usage_with_options`.
    fn opt_error(message: String) -> Result<ExitCode> {
        eprintln!("error: {message}");
        Ok(ExitCode::from(129))
    }
    /// `show_usage:` — `usage_with_options_internal(…, USAGE_TO_STDOUT)`, so the
    /// block goes to **stdout** even when an `error:` line preceded it on stderr.
    fn help() -> Result<ExitCode> {
        print!("{FOREACH_USAGE}");
        Ok(ExitCode::from(129))
    }
    /// The same, for the ambiguous-abbreviation path: `parse_long_opt` prints
    /// its `error:` and returns `PARSE_OPT_HELP`, which is a `goto show_usage`.
    fn ambiguous_error(message: String) -> Result<ExitCode> {
        eprintln!("error: {message}");
        help()
    }

    /// `module_foreach_options[]`. `super-prefix` is `PARSE_OPT_HIDDEN`, which is
    /// why the usage block above lists only the other two; hidden options still
    /// parse and still take part in abbreviation matching.
    const LONG: [&str; 3] = ["super-prefix", "quiet", "recursive"];
    /// Which of them take a value (`OPT__SUPER_PREFIX` is an `OPT_STRING`; the
    /// other two are `OPT_COUNTUP`/`OPT_BOOL`, i.e. `PARSE_OPT_NOARG`).
    fn takes_value(name: &str) -> bool {
        name == "super-prefix"
    }

    let mut quiet = false;
    let mut recursive = false;
    let mut super_prefix: Option<String> = None;
    let mut command: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;

        // `if (*arg != '-' || !arg[1])`: a non-option — including a lone `-` —
        // is copied to `ctx->out` and the scan *continues*. This is the
        // permutation that separates the helper from the porcelain wrapper.
        if !a.starts_with('-') || a == "-" {
            command.push(a.to_string());
            continue;
        }
        // `--` and `--end-of-options` end the scan; the rest are operands.
        if a == "--" || a == "--end-of-options" {
            command.extend(args[i..].iter().cloned());
            break;
        }
        // A lone `-h` asks for help, but only when it is the *entire* command
        // line: `internal_help && ctx->total == 1`.
        if a == "-h" && args.len() == 1 {
            return help();
        }

        let Some(body) = a.strip_prefix("--") else {
            // Short clusters. `-q` is the only switch; `-h` anywhere in one
            // still shows the usage (`internal_help && *ctx->opt == 'h'`).
            if !a.is_ascii() {
                return unknown(format!("unknown non-ascii option in string: `{a}'"));
            }
            for c in a[1..].chars() {
                match c {
                    'q' => quiet = true,
                    'h' => return help(),
                    _ => return unknown(format!("unknown switch `{c}'")),
                }
            }
            continue;
        };

        // `internal_help` is on (flags `0`), and both of these are matched
        // literally, ahead of `parse_long_opt`, so neither abbreviates.
        if body == "help" {
            return help();
        }
        if body == "help-all" {
            print!("{FOREACH_USAGE_FULL}");
            return Ok(ExitCode::from(129));
        }

        // ---- `parse_long_opt`, including its abbreviation matching. ----
        let (head, inline) = match body.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (body, None),
        };
        // `skip_prefix(arg_start, "no-", …)` twice: one `no-` is the negation,
        // two means the option itself would have to be named `no-<something>`,
        // and none of these are.
        let (stem, unset, no_no) = match head.strip_prefix("no-") {
            Some(rest) => match rest.strip_prefix("no-") {
                Some(rest2) => (rest2, false, true),
                None => (rest, true, false),
            },
            None => (head, false, false),
        };

        let mut exact: Option<&str> = None;
        // `register_abbrev`'s two slots: the most recent prefix match, and the
        // earlier one it displaced (which is what makes the match ambiguous).
        let mut abbrev: Option<&str> = None;
        let mut ambiguous: Option<&str> = None;
        // `register_abbrev`: the newest prefix match displaces the previous one
        // into the `ambiguous` slot. git calls it from two independent places
        // per option, so one option can displace *itself* — which is why
        // `--no-` reports "could be --no-recursive or --no-recursive".
        let mut register = |name: &'static str| {
            if let Some(prev) = abbrev {
                ambiguous = Some(prev);
            }
            abbrev = Some(name);
        };
        if !no_no {
            for name in LONG {
                if stem == name {
                    exact = Some(name);
                    break;
                }
                // `!strncmp(long_name, arg_start, arg_end - arg_start)`.
                if name.starts_with(stem) {
                    register(name);
                }
                // "negated and abbreviated very much": `starts_with("no-", arg)`
                // asks whether `"no-"` starts with the *argument*, so `--n`,
                // `--no` and `--no-` register every negatable option a second
                // time — and are therefore always ambiguous.
                if "no-".starts_with(head) {
                    register(name);
                }
            }
        }
        if exact.is_none() {
            if let (Some(other), Some(one)) = (ambiguous, abbrev) {
                let no = if unset || "no-".starts_with(head) { "no-" } else { "" };
                return ambiguous_error(format!(
                    "ambiguous option: {body} (could be --{no}{other} or --{no}{one})"
                ));
            }
        }
        let Some(name) = exact.or(abbrev) else {
            return unknown(format!("unknown option `{body}'"));
        };

        if !takes_value(name) && inline.is_some() {
            return opt_error(format!("option `{name}' takes no value"));
        }
        match name {
            "quiet" => quiet = !unset,
            "recursive" => recursive = !unset,
            "super-prefix" if unset => super_prefix = None,
            "super-prefix" => {
                super_prefix = match inline {
                    Some(v) => Some(v.to_string()),
                    None => match args.get(i) {
                        Some(next) => {
                            i += 1;
                            Some(next.clone())
                        }
                        None => {
                            return opt_error("option `super-prefix' requires a value".to_string())
                        }
                    },
                };
            }
            _ => unreachable!("every name comes from LONG"),
        }
    }

    super::submodule::foreach_parsed(&command, quiet, recursive, super_prefix.as_deref())
}

// ------------------------------------------------------------------- add ----

/// The `usage:` block `module_add`'s `usage_with_options` prints, captured
/// byte-for-byte from git 2.55.0 (`git submodule--helper add`): the usage line, a
/// blank line, the nine options with their help text in column 27, and a
/// trailing blank line. Exit is 129.
const ADD_USAGE: &str = "\
usage: git submodule add [<options>] [--] <repository> [<path>]

    -b, --[no-]branch <branch>
                          branch of repository to add as submodule
    -f, --[no-]force      allow adding an otherwise ignored submodule path
    -q, --[no-]quiet      print only error messages
    --[no-]progress       force cloning progress
    --[no-]reference <repository>
                          reference repository
    --[no-]ref-format <format>
                          specify the reference format to use
    --[no-]dissociate     borrow the objects from reference repositories
    --[no-]name <name>    sets the submodule's name to the given string instead of defaulting to its path
    --[no-]depth <n>      depth for shallow clones

";

/// `git submodule--helper add [<options>] [--] <repository> [<path>]` — git's
/// `module_add` (submodule--helper.c:3642).
///
/// Only the two checks `module_add` performs before it starts working are here —
/// the writable-`.gitmodules` gate and the operand count — because they are the
/// two that differ from the porcelain wrapper, which forwards its arguments
/// unvalidated except for a missing `<repository>`. Everything past them is the
/// same C function `git submodule add` reaches, so it is delegated to the shared
/// subcommand table.
fn add(args: &[String]) -> Result<ExitCode> {
    /// `module_add`'s long options that take a value.
    const VALUED: &[&str] = &["branch", "reference", "ref-format", "name", "depth"];
    /// …and the ones that do not.
    const FLAGS: &[&str] = &["force", "quiet", "progress", "dissociate"];

    let mut operands = 0usize;
    let mut end_of_options = false;
    let mut i = 0;
    while let Some(a) = args.get(i) {
        i += 1;
        if end_of_options || !a.starts_with('-') || a == "-" {
            operands += 1;
            continue;
        }
        if a == "--" {
            end_of_options = true;
            continue;
        }
        if let Some(long) = a.strip_prefix("--") {
            // `--no-<name>` unsets; for a valued option it simply clears it.
            let (name, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            let name = name.strip_prefix("no-").unwrap_or(name);
            if FLAGS.contains(&name) {
                continue;
            }
            if VALUED.contains(&name) {
                if inline.is_none() && !long.starts_with("no-") {
                    if args.get(i).is_none() {
                        eprintln!("error: option `{name}' requires a value");
                        eprint!("{ADD_USAGE}");
                        return Ok(ExitCode::from(129));
                    }
                    i += 1;
                }
                continue;
            }
            eprintln!("error: unknown option `{long}'");
            eprint!("{ADD_USAGE}");
            return Ok(ExitCode::from(129));
        }
        // A short cluster: `-qf`, `-bmain`, `-b main`.
        let mut chars = a[1..].char_indices();
        while let Some((at, c)) = chars.next() {
            match c {
                'f' | 'q' => {}
                'b' => {
                    // The rest of the cluster is the value; an empty rest takes
                    // the next argument.
                    let rest = &a[1 + at + c.len_utf8()..];
                    if rest.is_empty() {
                        if args.get(i).is_none() {
                            eprintln!("error: switch `b' requires a value");
                            eprint!("{ADD_USAGE}");
                            return Ok(ExitCode::from(129));
                        }
                        i += 1;
                    }
                    break;
                }
                other => {
                    eprintln!("error: unknown switch `{other}'");
                    eprint!("{ADD_USAGE}");
                    return Ok(ExitCode::from(129));
                }
            }
        }
    }

    // `is_writing_gitmodules_ok()` runs *before* the operand count: `.gitmodules`
    // must be in the working tree, or absent from the index and HEAD alike.
    if !writing_gitmodules_ok()? {
        crate::git_fatal!("please make sure that the .gitmodules file is in the working tree");
    }

    if operands == 0 || operands > 2 {
        eprint!("{ADD_USAGE}");
        return Ok(ExitCode::from(129));
    }

    let mut forwarded = Vec::with_capacity(args.len() + 1);
    forwarded.push("add".to_string());
    forwarded.extend(args.iter().cloned());
    super::submodule::subcommand(&forwarded)
}

/// git's `is_writing_gitmodules_ok` (submodule.c): the worktree copy exists, or
/// there is no `.gitmodules` in the index nor in `HEAD` to be shadowed by one.
fn writing_gitmodules_ok() -> Result<bool> {
    let repo = crate::setup::discover()?;
    if let Some(workdir) = repo.workdir() {
        if workdir.join(".gitmodules").exists() {
            return Ok(true);
        }
    }
    let path = BString::from(".gitmodules");
    let in_index = repo
        .index_or_empty()?
        .entry_by_path(path.as_bstr())
        .is_some();
    let in_head = repo
        .head_commit()
        .ok()
        .and_then(|c| c.tree().ok())
        .and_then(|t| t.lookup_entry_by_path(".gitmodules").ok().flatten())
        .is_some();
    Ok(!in_index && !in_head)
}

/// The `usage_with_options` block `module_clone` prints, captured from git 2.55.0.
const CLONE_USAGE: &str = "\
usage: git submodule--helper clone [--prefix=<path>] [--quiet] [--reference <repository>] [--name <name>] [--depth <depth>] [--single-branch] [--filter <filter-spec>] --url <url> --path <path>

    --[no-]prefix <path>  alternative anchor for relative paths
    --[no-]path <path>    where the new submodule will be cloned to
    --[no-]name <string>  name of the new submodule
    --[no-]url <string>   url where to clone the submodule from
    --[no-]reference <repo>
                          reference repository
    --[no-]ref-format <format>
                          specify the reference format to use
    --[no-]dissociate     use --reference only while cloning
    --[no-]depth <n>      depth for shallow clones
    -q, --[no-]quiet      suppress output for cloning a submodule
    --[no-]progress       force cloning progress
    --[no-]require-init   disallow cloning into non-empty directory
    --[no-]single-branch  clone only one branch, HEAD or --branch
    --[no-]filter <args>  object filtering

";

/// The `usage_with_options` block `module_create_branch` prints, captured from git 2.55.0.
const CREATE_BRANCH_USAGE: &str = "\
usage: git submodule--helper create-branch [-f|--force] [--create-reflog] [-q|--quiet] [-t|--track] [-n|--dry-run] <name> <start-oid> <start-name>

    -q, --[no-]quiet      print only error messages
    -f, --[no-]force      force creation
    --[no-]create-reflog  create the branch's reflog
    -t, --[no-]track[=(direct|inherit)]
                          set branch tracking configuration
    -n, --[no-]dry-run    show whether the branch would be created

";

// ----------------------------------------------------------------- clone ----

/// `git submodule--helper clone` — validate the arguments `module_clone` validates.
///
/// ```c
/// if (argc || !clone_data.url || !clone_data.path || !*(clone_data.path))
///         usage_with_options(git_submodule_helper_usage, module_clone_options);
/// ```
///
/// (builtin/submodule--helper.c:2097-2099.) Everything past that check is
/// `clone_submodule()`, which needs transport plus worktree materialisation, so
/// only the refusal is reproduced here — but it is reproduced exactly, because
/// it is what a bare `git submodule--helper clone` prints.
fn clone(args: &[String]) -> Result<ExitCode> {
    /// `module_clone_options`' long options that take a value. `filter` comes
    /// from `OPT_PARSE_LIST_OBJECTS_FILTER`.
    const VALUED: &[&str] = &[
        "prefix",
        "path",
        "name",
        "url",
        "reference",
        "ref-format",
        "depth",
        "filter",
    ];
    /// …and its `OPT_BOOL`s, plus `OPT__QUIET`.
    const FLAGS: &[&str] = &[
        "dissociate",
        "quiet",
        "progress",
        "require-init",
        "single-branch",
    ];

    let usage = || {
        eprint!("{CLONE_USAGE}");
        Ok(ExitCode::from(129))
    };

    let mut operands = 0usize;
    let mut url: Option<String> = None;
    let mut path: Option<String> = None;
    let mut end_of_options = false;
    let mut i = 0;
    while let Some(a) = args.get(i) {
        i += 1;
        if end_of_options || !a.starts_with('-') || a == "-" {
            operands += 1;
            continue;
        }
        if a == "--" {
            end_of_options = true;
            continue;
        }
        if let Some(long) = a.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            let (name, negated) = match name.strip_prefix("no-") {
                Some(rest) => (rest, true),
                None => (name, false),
            };
            if FLAGS.contains(&name) {
                continue;
            }
            if VALUED.contains(&name) {
                let value = match (negated, inline) {
                    // `--no-<name>` clears the string rather than taking a value.
                    (true, _) => None,
                    (false, Some(v)) => Some(v),
                    (false, None) => match args.get(i) {
                        Some(next) => {
                            i += 1;
                            Some(next.clone())
                        }
                        None => {
                            eprintln!("error: option `{name}' requires a value");
                            return usage();
                        }
                    },
                };
                match name {
                    "url" => url = value,
                    "path" => path = value,
                    _ => {}
                }
                continue;
            }
            eprintln!("error: unknown option `{long}'");
            return usage();
        }
        // The only short option in the table is `-q`.
        for c in a[1..].chars() {
            if c != 'q' {
                eprintln!("error: unknown switch `{c}'");
                return usage();
            }
        }
    }

    if operands != 0 || url.is_none() || path.as_deref().unwrap_or("").is_empty() {
        return usage();
    }
    bail!("unsupported subcommand \"clone\": cloning a submodule needs transport plus worktree checkout")
}

// --------------------------------------------------------- create-branch ----

/// `git submodule--helper create-branch` — validate the operand count.
///
/// ```c
/// if (argc != 3)
///         usage_with_options(usage, options);
/// ```
///
/// (builtin/submodule--helper.c:3347-3348.) `create_branches_recursively()` is
/// the work, and it is not ported; the refusal above is, since that is what a
/// bare `git submodule--helper create-branch` prints.
fn create_branch(args: &[String]) -> Result<ExitCode> {
    /// The four `OPT_BOOL`-shaped entries in `module_create_branch`'s table.
    const FLAGS: &[&str] = &["quiet", "force", "create-reflog", "dry-run"];

    let usage = || {
        eprint!("{CREATE_BRANCH_USAGE}");
        Ok(ExitCode::from(129))
    };

    let mut operands = 0usize;
    let mut end_of_options = false;
    let mut i = 0;
    while let Some(a) = args.get(i) {
        i += 1;
        if end_of_options || !a.starts_with('-') || a == "-" {
            operands += 1;
            continue;
        }
        if a == "--" {
            end_of_options = true;
            continue;
        }
        if let Some(long) = a.strip_prefix("--") {
            let name = match long.split_once('=') {
                Some((n, _)) => n,
                None => long,
            };
            let name = name.strip_prefix("no-").unwrap_or(name);
            // `--track` is `PARSE_OPT_OPTARG`, so it never consumes the next
            // argument; only `--track=<mode>` carries a value.
            if FLAGS.contains(&name) || name == "track" {
                continue;
            }
            eprintln!("error: unknown option `{long}'");
            return usage();
        }
        for (at, c) in a[1..].char_indices() {
            match c {
                'q' | 'f' | 'n' => {}
                // A short `PARSE_OPT_OPTARG` takes the rest of the cluster as
                // its value when there is one, and never the next argument.
                't' => {
                    let _ = at;
                    break;
                }
                other => {
                    eprintln!("error: unknown switch `{other}'");
                    return usage();
                }
            }
        }
    }

    if operands != 3 {
        return usage();
    }
    bail!("unsupported subcommand \"create-branch\": creates a branch inside a submodule")
}

// ------------------------------------------------------------ push-check ----

/// `git submodule--helper push-check <superproject-head> <remote> [<refspec>...]`
///
/// Called by `git push --recurse-submodules=check`'s bookkeeping to refuse a
/// push whose submodule side would not land anywhere sensible. The whole of
/// `push_check()` (builtin/submodule--helper.c) is reproduced: the operand
/// count, the `HEAD` resolution, the "remote must be configured" rule that
/// stops a submodule pushing to the superproject's own url, and the per-refspec
/// left-hand-side check.
fn push_check(args: &[String]) -> Result<ExitCode> {
    // ```c
    // if (argc < 3)
    //         die("submodule--helper push-check requires at least 2 arguments");
    // ```
    //
    // `argc` counts the subcommand word, so this is "fewer than two operands".
    if args.len() < 2 {
        crate::git_fatal!("submodule--helper push-check requires at least 2 arguments");
    }
    let superproject_head = args[0].as_str();
    let remote_name = args[1].as_str();

    let repo = crate::setup::discover()?;

    // `refs_resolve_refdup(..., "HEAD", 0, ...)` hands back the name HEAD
    // resolves to; a detached HEAD resolves to itself, which is how git tells
    // the two apart. `flags` is 0, so an unborn branch still yields its name.
    let head = match repo.head_ref() {
        Ok(Some(r)) => r.name().as_bstr().to_str_lossy().into_owned(),
        Ok(None) => "HEAD".to_string(),
        Err(_) => match repo.find_reference("HEAD") {
            // A symref into a branch that does not exist yet still resolves by
            // name here; only a HEAD that cannot be read at all is the death.
            Ok(r) => match r.target().try_name() {
                Some(name) => name.as_bstr().to_str_lossy().into_owned(),
                None => "HEAD".to_string(),
            },
            Err(_) => crate::git_fatal!("Failed to resolve HEAD as a valid ref."),
        },
    };
    let detached_head = head == "HEAD";

    // ```c
    // remote = pushremote_get(argv[1]);
    // if (!remote || remote->origin == REMOTE_UNCONFIGURED)
    //         die("remote '%s' not configured", argv[1]);
    // ```
    //
    // `remote_get_1()` will happily turn an unknown name into a url alias, so
    // the `origin` field is what actually decides: `handle_config()` sets it to
    // `REMOTE_CONFIG` for any `remote.<name>.<key>` it reads, and nothing else
    // does. A bare name with no `remote.<name>.*` section is therefore
    // unconfigured no matter how url-shaped it looks.
    if !remote_is_configured(&repo, remote_name) {
        crate::git_fatal!("remote '{remote_name}' not configured");
    }

    // `if (argc > 2)`: the refspecs start at the third operand.
    if args.len() > 2 {
        let local_refs = local_heads(&repo)?;
        for spec in &args[2..] {
            let Some(item) = parse_push_refspec(spec) else {
                crate::git_fatal!("invalid refspec '{spec}'");
            };
            if item.pattern || item.matching {
                continue;
            }
            match count_refspec_match(&item.src, &local_refs) {
                1 => {}
                0 if item.src == "HEAD" => {
                    if detached_head || head != superproject_head {
                        crate::git_fatal!(
                            "HEAD does not match the named branch in the superproject"
                        );
                    }
                }
                _ => crate::git_fatal!("src refspec '{}' must name a ref", item.src),
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Is there a `remote.<name>.<anything>` in the configuration?
///
/// That is exactly what sets `remote->origin = REMOTE_CONFIG` in remote.c's
/// `handle_config()`, and `push_check()` reads nothing else off the remote.
fn remote_is_configured(repo: &gix::Repository, name: &str) -> bool {
    let config = repo.config_snapshot();
    let configured = config
        .sections_by_name("remote")
        .into_iter()
        .flatten()
        .any(|section| {
            section
                .header()
                .subsection_name()
                .is_some_and(|sub| sub == name)
                && section.value_names().next().is_some()
        });
    configured
}

/// `get_local_heads()`: every ref under `refs/`, minus the ones whose name is
/// malformed (`one_local_ref` drops those with `check_refname_format`).
///
/// The name is git's, not a description: `refs_for_each_ref()` walks all of
/// `refs/`, so remote-tracking refs and tags are in the list too — which is the
/// reason `count_refspec_match()` has a notion of a "weak" match at all.
fn local_heads(repo: &gix::Repository) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for reference in repo.references()?.all()? {
        let Ok(reference) = reference else { continue };
        let name = reference.name().as_bstr().to_str_lossy().into_owned();
        let Some(rest) = name.strip_prefix("refs/") else {
            continue;
        };
        if gix::validate::reference::name(rest.into()).is_err() {
            continue;
        }
        out.push(name);
    }
    Ok(out)
}

/// remote.c's `count_refspec_match()`, minus the matched-ref output parameter
/// that `push_check()` passes as `NULL`.
///
/// A match is "weak" when it is with a ref outside `refs/heads/` and
/// `refs/tags/` that the pattern did not name in full (or at least from
/// `refs/`); one strong match with any number of weak ones is still unique.
fn count_refspec_match(pattern: &str, refs: &[String]) -> usize {
    let patlen = pattern.len();
    let (mut weak_match, mut strong_match) = (0usize, 0usize);
    let mut matched = false;
    for name in refs {
        if refname_match(pattern, name) == 0 {
            continue;
        }
        let namelen = name.len();
        if namelen != patlen
            && patlen + 5 != namelen
            && !name.starts_with("refs/heads/")
            && !name.starts_with("refs/tags/")
        {
            weak_match += 1;
        } else {
            matched = true;
            strong_match += 1;
        }
    }
    match matched {
        true => strong_match,
        false => weak_match,
    }
}

/// refs.c's `refname_match()`: could `abbrev_name` have meant `full_name`?
///
/// ```c
/// static const char *ref_rev_parse_rules[] = {
///         "%.*s", "refs/%.*s", "refs/tags/%.*s", "refs/heads/%.*s",
///         "refs/remotes/%.*s", "refs/remotes/%.*s/HEAD", NULL
/// };
/// ```
///
/// (refs.c:622-630.) The return value is the rule's rank counted from the end,
/// so an earlier rule scores higher; `count_refspec_match()` only asks whether
/// it is non-zero.
fn refname_match(abbrev_name: &str, full_name: &str) -> usize {
    const RULES: [&str; 6] = [
        "{}",
        "refs/{}",
        "refs/tags/{}",
        "refs/heads/{}",
        "refs/remotes/{}",
        "refs/remotes/{}/HEAD",
    ];
    for (i, rule) in RULES.iter().enumerate() {
        if full_name == rule.replace("{}", abbrev_name) {
            return RULES.len() - i;
        }
    }
    0
}

/// The three fields of a `struct refspec_item` that `push_check()` reads.
struct PushRefspec {
    src: String,
    pattern: bool,
    matching: bool,
}

/// refspec.c's `parse_refspec(item, refspec, /* fetch */ 0)`.
///
/// `None` is its `return 0`, which `refspec_append()` turns into
/// `die("invalid refspec '%s'")`.
fn parse_push_refspec(spec: &str) -> Option<PushRefspec> {
    let mut lhs = spec;
    let mut negative = false;
    if let Some(rest) = lhs.strip_prefix('+') {
        lhs = rest;
    } else if let Some(rest) = lhs.strip_prefix('^') {
        negative = true;
        lhs = rest;
    }

    let colon = lhs.rfind(':');
    if negative && colon.is_some() {
        return None;
    }
    // `":"` (and `"+:"`) is the push-everything-matching spec.
    if colon == Some(0) && lhs.len() == 1 {
        return Some(PushRefspec {
            src: String::new(),
            pattern: false,
            matching: true,
        });
    }

    let (src, dst) = match colon {
        Some(at) => (&lhs[..at], Some(&lhs[at + 1..])),
        None => (lhs, None),
    };
    let mut is_glob = dst.is_some_and(|d| !d.is_empty() && d.contains('*'));
    if !src.is_empty() && src.contains('*') {
        if dst.is_some() && !is_glob {
            return None;
        }
        is_glob = true;
    } else if dst.is_some() && is_glob {
        return None;
    }

    // `if (llen == 1 && *lhs == '@') item->src = "HEAD";`
    let src = match src {
        "@" => "HEAD".to_string(),
        other => other.to_string(),
    };

    if negative {
        if src.is_empty() || looks_like_oid(&src) || !refname_ok(&src, is_glob) {
            return None;
        }
        return Some(PushRefspec {
            src,
            pattern: is_glob,
            matching: false,
        });
    }

    // Push LHS: empty means delete, a wildcard must still look like a ref, and
    // anything else "goes, for now".
    if is_glob && !src.is_empty() && !refname_ok(&src, true) {
        return None;
    }
    match dst {
        None => {
            if !refname_ok(&src, is_glob) {
                return None;
            }
        }
        Some("") => return None,
        Some(dst) => {
            if !refname_ok(dst, is_glob) {
                return None;
            }
        }
    }
    Some(PushRefspec {
        src,
        pattern: is_glob,
        matching: false,
    })
}

/// `check_refname_format(name, REFNAME_ALLOW_ONELEVEL | [REFNAME_REFSPEC_PATTERN])`
/// as far as a refspec cares: one `*` is allowed under the pattern flag, and a
/// single-level name is always allowed.
fn refname_ok(name: &str, pattern: bool) -> bool {
    let checked = match pattern {
        // The `*` stands in for a whole component; validation only has to see
        // that the rest of the name is well formed around it.
        true => name.replacen('*', "star", 1),
        false => name.to_string(),
    };
    if pattern && checked.contains('*') {
        return false;
    }
    gix::validate::reference::name_partial(checked.as_str().into()).is_ok()
}

/// `llen == the_hash_algo->hexsz && !get_oid_hex(item->src, &unused)`.
fn looks_like_oid(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

// ---------------------------------------------------------------- gitdir ----

/// `git submodule--helper gitdir <name>` — print the git directory that the
/// submodule `<name>` uses, i.e. `<git-dir>/modules/<name>`.
fn gitdir(args: &[String]) -> Result<ExitCode> {
    if args.len() != 1 {
        eprintln!("usage: git submodule--helper gitdir <name>");
        return Ok(ExitCode::from(129));
    }
    let name = args[0].as_str();

    let repo = crate::setup::discover()?;
    if repo
        .config_snapshot()
        .boolean("extensions.submodulePathConfig")
        .unwrap_or(false)
    {
        bail!(
            "extensions.submodulePathConfig is enabled: resolving `submodule.{name}.gitdir` and \
             git's validate_submodule_git_dir containment check are not ported"
        );
    }

    let mut path = git_dir_spelling(&repo)?;
    if !path.ends_with('/') {
        path.push('/');
    }
    path.push_str("modules/");
    path.push_str(name);
    println!("{}", cleanup_path(&path));
    Ok(ExitCode::SUCCESS)
}

/// How git's own setup would have spelled `$GIT_DIR` for this repository.
///
/// git prints this string verbatim, so the relative forms matter: see the
/// module docs for the four cases reproduced here.
fn git_dir_spelling(repo: &gix::Repository) -> Result<String> {
    // `setup_git_directory` takes `GIT_DIR` as given, without normalising it.
    if let Some(dir) = std::env::var_os("GIT_DIR") {
        let dir = dir.to_string_lossy().into_owned();
        if !dir.is_empty() {
            return Ok(dir);
        }
    }

    let git_dir = repo.git_dir();
    let real_git_dir = std::fs::canonicalize(git_dir).unwrap_or_else(|_| git_dir.to_owned());

    match repo.workdir() {
        Some(workdir) => {
            // Discovery walked up to a top level whose `.git` is a real
            // directory: git chdir'd there and kept the relative name.
            let dot_git = workdir.join(".git");
            let plain = dot_git.is_dir()
                && std::fs::canonicalize(&dot_git)
                    .map(|p| p == real_git_dir)
                    .unwrap_or(false);
            if plain {
                return Ok(".git".to_string());
            }
            // A `.git` gitfile or a linked worktree: git resolved it to an
            // absolute path before storing it.
            Ok(real_git_dir.to_string_lossy().into_owned())
        }
        None => {
            // Bare: git names it `.` when the cwd *is* the repository.
            let here = std::env::current_dir()
                .ok()
                .and_then(|p| std::fs::canonicalize(p).ok());
            if here.as_deref() == Some(real_git_dir.as_path()) {
                return Ok(".".to_string());
            }
            Ok(real_git_dir.to_string_lossy().into_owned())
        }
    }
}

/// git's `cleanup_path`: drop one leading `./`, then any slashes it left behind.
/// This is what turns `./modules/foo` into `modules/foo` in a bare repository.
fn cleanup_path(path: &str) -> &str {
    match path.strip_prefix("./") {
        Some(rest) => rest.trim_start_matches('/'),
        None => path,
    }
}

// ---------------------------------------------------- get-default-remote ----

/// `git submodule--helper get-default-remote <path>` — print the remote the
/// submodule at `<path>` would fetch from by default.
fn get_default_remote(args: &[String]) -> Result<ExitCode> {
    if args.len() != 1 {
        eprint!("usage: git submodule--helper get-default-remote <path>\n\n");
        return Ok(ExitCode::from(129));
    }
    let path = args[0].as_str();

    // `gix::open` does not walk upwards, matching `repo_submodule_init`, which
    // fails outright when `<path>` is not itself a repository.
    let Ok(sub) = gix::open(path) else {
        let repo = crate::setup::discover()?;
        let display = prefixed_path(&repo, path)?;
        eprintln!("fatal: could not get a repository handle for submodule '{display}'");
        return Ok(ExitCode::from(128));
    };

    // `repo_get_default_remote`: a symref into `refs/heads/` consults
    // `branch.<name>.remote`; everything else (detached HEAD) is `origin`.
    let head = sub.head()?;
    let branch = match head.referent_name() {
        Some(name) => {
            let full = name.as_bstr().to_str_lossy().into_owned();
            let Some(short) = full.strip_prefix("refs/heads/") else {
                crate::git_fatal!("HEAD of '{path}' points to {full}, which is not a branch");
            };
            Some(BString::from(short))
        }
        None => None,
    };
    drop(head);

    let remote = branch.and_then(|branch| {
        sub.config_snapshot().string(KeyRef {
            section_name: "branch",
            subsection_name: Some(branch.as_bstr()),
            value_name: "remote",
        })
    });

    match remote {
        Some(remote) => println!("{}", remote.to_str_lossy()),
        None => println!("origin"),
    }
    Ok(ExitCode::SUCCESS)
}

/// git's `prefix_path`: `<path>` re-expressed relative to the repository root
/// by prepending the current prefix and folding `.`/`..` lexically.
fn prefixed_path(repo: &gix::Repository, path: &str) -> Result<String> {
    let prefix = repo
        .prefix()?
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut parts: Vec<&str> = Vec::new();
    for component in prefix
        .split('/')
        .chain(path.split('/'))
        .filter(|c| !c.is_empty() && *c != ".")
    {
        if component == ".." {
            parts.pop();
        } else {
            parts.push(component);
        }
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the helper's two usage blocks to the bytes git 2.55.0 writes.
    ///
    /// `ADD_USAGE` is `module_add`'s `usage_with_options` output, captured from
    /// `git submodule--helper add` on git 2.55.0. Its shape is the part that
    /// silently rots: `parse_options` puts help text in column 27 and spills an
    /// option whose `-x, --[no-]name <arg>` header already reaches that column
    /// onto its own line — which is why `branch`, `reference`, `ref-format` are
    /// two-line entries and `force`, `quiet`, `progress`, `dissociate`, `name`,
    /// `depth` are one.
    #[test]
    fn usage_blocks_match_git() {
        assert_eq!(USAGE, "usage: git submodule--helper <command>\n\n");

        let lines: Vec<&str> = ADD_USAGE.split('\n').collect();
        assert_eq!(
            lines[0],
            "usage: git submodule add [<options>] [--] <repository> [<path>]"
        );
        assert_eq!(lines[1], "");
        assert_eq!(lines[2], "    -b, --[no-]branch <branch>");
        assert_eq!(
            lines[3],
            "                          branch of repository to add as submodule"
        );
        assert_eq!(
            lines[4],
            "    -f, --[no-]force      allow adding an otherwise ignored submodule path"
        );
        assert_eq!(
            lines[5],
            "    -q, --[no-]quiet      print only error messages"
        );
        assert_eq!(
            *lines.last().expect("non-empty"),
            "",
            "parse_options ends the block with a blank line"
        );
        assert!(ADD_USAGE.ends_with("depth for shallow clones\n\n"));
        // Every wrapped help line is indented to the same column as the inline
        // ones, so a mis-measured pad would show up as a mismatch here.
        for line in ADD_USAGE
            .lines()
            .filter(|l| l.starts_with("                "))
        {
            assert_eq!(
                line.len() - line.trim_start().len(),
                26,
                "help column drifted: {line:?}"
            );
        }
    }
}
