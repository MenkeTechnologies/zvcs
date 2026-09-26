use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::hash::ObjectId;

// ---------------------------------------------------------------------------
// usage blocks — one per `parse_options()` call in builtin/reflog.c.
//
// Every sub-command runs its own parser over its own `struct option options[]`,
// so once the sub-command word has been read `-h` is that sub-command's question
// and prints that sub-command's block: `git reflog expire -h` renders
// `reflog_expire_usage`, never `reflog_usage`. `--help-all` renders `USAGE_FULL`,
// which is the same block for all seven — no table here carries a
// `PARSE_OPT_HIDDEN` entry.
// ---------------------------------------------------------------------------

/// `cmd_reflog_show`'s block (builtin/reflog.c:40-43). Its table is `OPT_END()`
/// alone; everything else is forwarded to `cmd_log_reflog()`.
const SHOW_USAGE: &str = "\
usage: git reflog [show] [<log-options>] [<ref>]

";

/// `cmd_reflog_list`'s block (builtin/reflog.c:45-48), table `OPT_END()`.
const LIST_USAGE: &str = "\
usage: git reflog list

";

/// `cmd_reflog_exists`'s block (builtin/reflog.c:50-53), table `OPT_END()`.
const EXISTS_USAGE: &str = "\
usage: git reflog exists <ref>

";

/// `cmd_reflog_write`'s block (builtin/reflog.c:55-58), table `OPT_END()`.
const WRITE_USAGE: &str = "\
usage: git reflog write <ref> <old-oid> <new-oid> <message>

";

/// `cmd_reflog_delete`'s block over its table (builtin/reflog.c:310-321).
const DELETE_USAGE: &str = "\
usage: git reflog delete [--rewrite] [--updateref]
                         [--dry-run | -n] [--verbose] <ref>@{<specifier>}...

    -n, --[no-]dry-run    do not actually prune any entries
    --[no-]rewrite        rewrite the old SHA1 with the new SHA1 of the entry that now precedes it
    --[no-]updateref      update the reference to the value of the top reflog entry
    --[no-]verbose        print extra information on screen

";

/// `cmd_reflog_drop`'s block over its table (builtin/reflog.c:358-363).
const DROP_USAGE: &str = "\
usage: git reflog drop [--all [--single-worktree] | <refs>...]

    --[no-]all            drop the reflogs of all references
    --[no-]single-worktree
                          drop reflogs from the current worktree only

";

/// `cmd_reflog_expire`'s block over its table (builtin/reflog.c:190-213).
const EXPIRE_USAGE: &str = "\
usage: git reflog expire [--expire=<time>] [--expire-unreachable=<time>]
                         [--rewrite] [--updateref] [--stale-fix]
                         [--dry-run | -n] [--verbose] [--all [--single-worktree] | <refs>...]

    -n, --[no-]dry-run    do not actually prune any entries
    --[no-]rewrite        rewrite the old SHA1 with the new SHA1 of the entry that now precedes it
    --[no-]updateref      update the reference to the value of the top reflog entry
    --[no-]verbose        print extra information on screen
    --expire <timestamp>  prune entries older than the specified time
    --expire-unreachable <timestamp>
                          prune entries older than <time> that are not reachable from the current tip of the branch
    --[no-]stale-fix      prune any reflog entries that point to broken commits
    --[no-]all            process the reflogs of all references
    --[no-]single-worktree
                          limits processing to reflogs from the current worktree only

";

/// `usage_with_options()` over `builtin/reflog.c`'s subcommand table.
const USAGE: &str = r"usage: git reflog [show] [<log-options>] [<ref>]
   or: git reflog list
   or: git reflog exists <ref>
   or: git reflog write <ref> <old-oid> <new-oid> <message>
   or: git reflog delete [--rewrite] [--updateref]
                         [--dry-run | -n] [--verbose] <ref>@{<specifier>}...
   or: git reflog drop [--all [--single-worktree] | <refs>...]
   or: git reflog expire [--expire=<time>] [--expire-unreachable=<time>]
                         [--rewrite] [--updateref] [--stale-fix]
                         [--dry-run | -n] [--verbose] [--all [--single-worktree] | <refs>...]

";

/// `git reflog` — read the reference logs recorded under `$GIT_DIR/logs`.
///
/// Backed by gitoxide's `gix_ref` reflog reader (`Reference::log_iter()`), which
/// parses the raw `<old> <new> <sig>\t<message>` lines, plus a direct walk of the
/// log directory for the subcommands that are defined in terms of the files
/// themselves.
///
/// # Subcommands
///
///   * `git reflog [show] [<options>] [<ref>...]` — `show` is the default, and a
///     missing `<ref>` defaults to `HEAD`.
///   * `git reflog list` — every ref that has a reflog, in git's directory-tree
///     order (per-directory name sort).
///   * `git reflog exists <ref>` — exit 0 if `$GIT_DIR/logs/<ref>` is a file, else 1.
///   * `git reflog delete [--rewrite] [--updateref] [--dry-run] <ref>…` — drop
///     the named entries. The rest of the file is left byte-identical: the neighbours
///     keep the ids they recorded unless `--rewrite` closes the chain up, and the ref
///     only moves under `--updateref`. A selector past the end of a log is ignored.
///   * `git reflog expire [--expire=<t>] [--expire-unreachable=<t>] [--rewrite]
///     [--updateref] [--dry-run] [--verbose] [--all [--single-worktree] | <ref>…]` —
///     `should_expire_reflog_ent()`'s two tests: an entry goes when it is older than the
///     total cutoff, and also when it is older than the unreachable cutoff and neither of
///     its ids is reachable. What "reachable" means is
///     `reflog_expiry_prepare()`'s three regimes: every ref is a tip for `HEAD`
///     (`UE_HEAD`), the ref's own tip for anything else (`UE_NORMAL`), and nothing at all
///     once the unreachable cutoff is at or before the total one (`UE_ALWAYS`). The
///     cutoffs come from `--expire`/`--expire-unreachable`, then the first matching
///     `gc.<pattern>.reflogExpire[Unreachable]`, then `refs/stash`'s never-expire rule,
///     then `gc.reflogExpire[Unreachable]` and git's 90-day / 30-day defaults.
///     `--verbose` prints `keep`/`prune`/`would prune` per entry. `--all` covers every
///     worktree's logs unless `--single-worktree` narrows it. An emptied log is left as
///     an empty file, as git leaves it.
///   * `write` and `drop` bail — not ported.
///
/// # `show`
///
/// `cmd_reflog_show()` hands its argv to `cmd_log_reflog()`
/// (builtin/reflog.c:143-155), so `git reflog [show]` runs through
/// [`super::log`]'s `Flavor::Reflog` — the same walk, pretty-printer, `--notes`,
/// `log.decorate` and colour painting as `git log -g`. Only `-h`/`--help-all` are
/// answered here, by `cmd_reflog_show`'s own option table.
pub fn reflog(args: &[String]) -> Result<ExitCode> {
    // Tolerate the subcommand being present at index 0 regardless of how the
    // dispatcher slices argv.
    let args: &[String] = match args.first() {
        Some(a) if a == "reflog" => &args[1..],
        _ => args,
    };

    // `cmd_reflog`'s `parse_options(..., PARSE_OPT_SUBCOMMAND_OPTIONAL)` scans
    // leading options and stops at the first non-option, which becomes the
    // subcommand. So `-h` is this command's help exactly while it is the FIRST
    // token — the subcommand synopsis on stdout, exit 129. Once a subcommand has
    // been named, `-h` belongs to that subcommand's own parser instead.
    // `--help-all` answers the same way: parse_options_step() tests it with a
    // `strcmp()` of its own ahead of parse_long_opt(), and renders `USAGE_FULL`
    // — identical here because this option table has no `PARSE_OPT_HIDDEN`
    // entry. The compare is exact, which is why `--help-a` and `--help-all=x`
    // stay `unrecognized argument` reports.
    if args.first().is_some_and(|a| a == "-h" || a == "--help-all") {
        return Ok(super::show_usage(USAGE));
    }

    let (sub, rest): (&str, &[String]) = match args.first().map(String::as_str) {
        Some("show") => ("show", &args[1..]),
        Some("list") => ("list", &args[1..]),
        Some("exists") => ("exists", &args[1..]),
        Some("delete") => ("delete", &args[1..]),
        Some("expire") => ("expire", &args[1..]),
        Some("drop") => ("drop", &args[1..]),
        Some("write") => ("write", &args[1..]),
        // Anything else is a `<ref>` for the implicit `show`.
        _ => ("show", args),
    };

    let mut repo = crate::setup::discover()?;
    match sub {
        "show" => {
            // `cmd_reflog_show`'s table is empty, so no token is ever a value and
            // each is tested on its own. `PARSE_OPT_KEEP_DASHDASH` leaves the `--`
            // in argv for the revision parser but still breaks the option loop,
            // so nothing past it asks for help.
            if rest
                .iter()
                .take_while(|a| a.as_str() != "--")
                .any(|a| super::asks_for_help(a, ""))
            {
                return Ok(super::show_usage(SHOW_USAGE));
            }
            // `cmd_reflog_show()` is `cmd_log_reflog()` (builtin/reflog.c:154), so
            // the walk, the pretty-printer and its colours are `git log`'s own.
            drop(repo);
            super::log::reflog_show(rest)
        }
        "list" => list(&repo, rest),
        "exists" => exists(&repo, rest),
        "delete" => delete_entries(&repo, rest),
        "expire" => expire_entries(&repo, rest),
        "drop" => drop_reflogs(&repo, rest),
        "write" => write_reflog(&mut repo, rest),
        _ => unreachable!("subcommand set is closed above"),
    }
}


/// `%gd`'s short ref: `refs_shorten_unambiguous_ref(refs, ref, 0)`.
///
/// ```c
/// if (!commit_reflog->reflogs->short_ref)
///         commit_reflog->reflogs->short_ref
///                 = refs_shorten_unambiguous_ref(get_main_ref_store(the_repository),
///                                                commit_reflog->reflogs->ref,
///                                                0);
/// ```
/// (`reflog-walk.c:249-255`)
///
/// The reflog walker is the one caller that passes `strict = 0`, so a candidate
/// here only has to survive the rules *before* the one that produced it. It is
/// still not a prefix strip: `refs/remotes/origin/HEAD` shortens to `origin`
/// (rule 5 carries the `/HEAD` suffix), and `refs/heads/dup` alongside
/// `refs/tags/dup` stays `heads/dup`.
pub(crate) fn shorten_ref_unambiguous(repo: &gix::Repository, full: &str) -> String {
    crate::refname::shorten_unambiguous_str(repo, full, false)
}

// ---------------------------------------------------------------------------
// ref sets
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// option value parsing
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// rendering
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// diff output
// ---------------------------------------------------------------------------


// ---------------------------------------------------------------------------
// local timezone
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// list / exists
// ---------------------------------------------------------------------------

/// `parse_options()` over an `OPT_END()`-only table with no flags — the shape
/// `list`, `exists` and `write` share.
///
/// Everything dashed is `PARSE_OPT_UNKNOWN` (reported with its `=<value>` intact
/// and the block on stderr at 129) except the two help spellings, which reach the
/// same block on stdout. `--` and `--end-of-options` end the scan and are dropped,
/// leaving what follows for the caller to count.
fn scan_no_options<'a>(
    args: &'a [String],
    usage: &str,
) -> std::result::Result<Vec<&'a String>, ExitCode> {
    let mut operands = Vec::new();
    let mut literal = false;
    for a in args {
        if literal {
            operands.push(a);
            continue;
        }
        match a.as_str() {
            "--" | "--end-of-options" => literal = true,
            s if super::asks_for_help(s, "") => return Err(super::show_usage(usage)),
            s if s.starts_with('-') && s != "-" => {
                return Err(super::unknown_option(s, usage))
            }
            _ => operands.push(a),
        }
    }
    Ok(operands)
}

/// `git reflog list` — every ref under `$GIT_DIR/logs` that owns a log file.
fn list(repo: &gix::Repository, rest: &[String]) -> Result<ExitCode> {
    // `cmd_reflog_list` parses an empty table with no flags, so every dashed word
    // is `PARSE_OPT_UNKNOWN` and only then does the leftover count matter:
    // ``error(_("%s does not accept arguments: '%s'"), "list", argv[0])``, whose
    // -1 return reaches exit(3) as 255.
    match scan_no_options(rest, LIST_USAGE) {
        Err(code) => return Ok(code),
        Ok(operands) => {
            if let Some(a) = operands.first() {
                eprintln!("error: list does not accept arguments: '{a}'");
                return Ok(ExitCode::from(255));
            }
        }
    }
    if repo.git_dir() != repo.common_dir() {
        bail!("`reflog list` from a linked worktree is not supported");
    }

    let mut names: Vec<String> = Vec::new();
    collect_logs(&repo.git_dir().join("logs"), "", &mut names)?;

    let mut out = String::new();
    for name in names {
        out.push_str(&name);
        out.push('\n');
    }
    print!("{out}");
    Ok(ExitCode::SUCCESS)
}

/// `git reflog exists <ref>` — a literal test for `$GIT_DIR/logs/<ref>`.
fn exists(repo: &gix::Repository, rest: &[String]) -> Result<ExitCode> {
    let operands = match scan_no_options(rest, EXISTS_USAGE) {
        Ok(operands) => operands,
        Err(code) => return Ok(code),
    };
    // `if (!argc) usage_with_options(...)` — no `error:` line, and only the
    // *first* operand is read, so a second one is ignored rather than refused.
    let Some(name) = operands.first() else {
        eprint!("{EXISTS_USAGE}");
        return Ok(ExitCode::from(129));
    };

    // git validates with REFNAME_ALLOW_ONELEVEL, i.e. `master` is well-formed
    // even though it is not a full ref name — that is gitoxide's partial name.
    if <&gix::refs::PartialNameRef>::try_from(name.as_str()).is_err() {
        eprintln!("fatal: invalid ref format: {name}");
        return Ok(ExitCode::from(128));
    }

    let present = reflog_roots(repo)
        .iter()
        .any(|root| root.join(name).is_file());
    Ok(if present {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

/// The directories that hold reflog files. Normally one; a linked worktree keeps
/// its per-worktree logs (`HEAD`, `refs/bisect/*`) beside the shared ones.
fn reflog_roots(repo: &gix::Repository) -> Vec<PathBuf> {
    let git = repo.git_dir().join("logs");
    let common = repo.common_dir().join("logs");
    if git == common {
        vec![git]
    } else {
        vec![git, common]
    }
}

/// Append every log file below `dir` to `out` as a `/`-joined ref name, sorting
/// each directory's entries by name so the result matches git's tree walk (a
/// sub-directory is descended at its own sort position, not after its siblings).
fn collect_logs(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<()> {
    let read = match std::fs::read_dir(dir) {
        Ok(read) => read,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };

    let mut items: Vec<(String, bool)> = Vec::new();
    for entry in read {
        let entry = entry?;
        let is_dir = entry.file_type()?.is_dir();
        items.push((entry.file_name().to_string_lossy().into_owned(), is_dir));
    }
    items.sort();

    for (name, is_dir) in items {
        let full = format!("{prefix}{name}");
        if is_dir {
            collect_logs(&dir.join(&name), &format!("{full}/"), out)?;
        } else {
            out.push(full);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// delete / expire — the reflog write path
// ---------------------------------------------------------------------------

/// One line of a reflog file, kept whole so a rewrite is byte-exact.
///
/// git rewrites these files as text: `<old> <new> <who> <ts> <tz>\t<message>`. The two
/// ids and the timestamp are the only fields the write path reasons about, so the rest
/// of the line is carried through untouched.
struct RawLine {
    old: ObjectId,
    new: ObjectId,
    time: i64,
    bytes: Vec<u8>,
}

/// Parse one reflog line. `None` for a line git would not have written.
fn parse_raw_line(line: &[u8]) -> Option<RawLine> {
    let hexsz = line.iter().position(|b| *b == b' ')?;
    let old = ObjectId::from_hex(&line[..hexsz]).ok()?;
    let rest = &line[hexsz + 1..];
    let new_end = rest.iter().position(|b| *b == b' ')?;
    let new = ObjectId::from_hex(&rest[..new_end]).ok()?;
    // The committer block ends at the timestamp, which is the second-to-last
    // whitespace-separated field before the tab (`<name> <email> <secs> <tz>`).
    let head = match rest.iter().position(|b| *b == b'\t') {
        Some(tab) => &rest[..tab],
        None => rest,
    };
    let mut fields = head.rsplit(|b| *b == b' ');
    let _tz = fields.next()?;
    let secs = fields.next()?;
    let time: i64 = std::str::from_utf8(secs).ok()?.parse().ok()?;
    Some(RawLine {
        old,
        new,
        time,
        bytes: line.to_vec(),
    })
}

/// The `message` half of a reflog line, which is what
/// `should_expire_reflog_ent_verbose()` prints.
///
/// git's `message` still carries the line's own newline (the reflog file's
/// records are newline-terminated and it prints `"%s"` with no separator);
/// [`read_raw_log`] strips it, so it is put back here.
fn raw_line_message(line: &RawLine) -> Vec<u8> {
    let mut out = match line.bytes.iter().position(|b| *b == b'\t') {
        Some(tab) => line.bytes[tab + 1..].to_vec(),
        None => Vec::new(),
    };
    out.push(b'\n');
    out
}

/// `reflog_expire_config()` (reflog.c:35-83): the `gc.reflogExpire` /
/// `gc.reflogExpireUnreachable` defaults plus the `gc.<pattern>.reflog*` entries.
struct ExpireConfig {
    /// `opts->default_expire_total`.
    default_total: i64,
    /// `opts->default_expire_unreachable`.
    default_unreachable: i64,
    /// `opts->entries`, in configuration order — the first matching pattern wins.
    entries: Vec<(String, Option<i64>, Option<i64>)>,
}

impl ExpireConfig {
    fn read(repo: &gix::Repository, default_total: i64, default_unreachable: i64) -> Self {
        let mut out = ExpireConfig {
            default_total,
            default_unreachable,
            entries: Vec::new(),
        };
        let config = repo.config_snapshot();
        let Some(sections) = config.sections_by_name("gc") else {
            return out;
        };
        for section in sections {
            // `parse_config_key(var, "gc", &pattern, &pattern_len, &key)`: the subsection
            // is the pattern, and its absence names the defaults.
            let pattern = section
                .header()
                .subsection_name()
                .map(|s| s.to_str_lossy().into_owned());
            for (name, value) in [
                ("reflogExpire", REFLOG_EXPIRE_TOTAL),
                ("reflogExpireUnreachable", REFLOG_EXPIRE_UNREACH),
            ] {
                let Some(raw) = section.value(name) else { continue };
                // `git_config_expiry_date()` is `parse_expiry_date()` again.
                // `git_config_expiry_date()` failing makes `reflog_expire_config()`
                // return -1, which `repo_config()` reports through its own
                // `die()`; a value this port cannot read is left to the defaults
                // rather than invented.
                let Some(when) = expiry_date(&raw.to_str_lossy()) else {
                    continue;
                };
                match &pattern {
                    None => match value {
                        REFLOG_EXPIRE_TOTAL => out.default_total = when,
                        _ => out.default_unreachable = when,
                    },
                    Some(pattern) => {
                        let slot = match out.entries.iter().position(|(p, _, _)| p == pattern) {
                            Some(at) => at,
                            None => {
                                out.entries.push((pattern.clone(), None, None));
                                out.entries.len() - 1
                            }
                        };
                        match value {
                            REFLOG_EXPIRE_TOTAL => out.entries[slot].1 = Some(when),
                            _ => out.entries[slot].2 = Some(when),
                        }
                    }
                }
            }
        }
        out
    }

    /// `reflog_expire_options_set_refname()` (reflog.c:99-133) for one ref: what the
    /// command line did not pin is filled from the first matching pattern, from
    /// `refs/stash`'s never-expire rule, or from the defaults.
    fn for_ref(&self, refname: &str, cli_total: Option<i64>, cli_unreach: Option<i64>) -> (i64, i64) {
        if let (Some(total), Some(unreach)) = (cli_total, cli_unreach) {
            return (total, unreach);
        }
        let (total, unreach) = match self
            .entries
            .iter()
            .find(|(pattern, _, _)| glob_matches(pattern, refname))
        {
            Some((_, total, unreach)) => (total.unwrap_or(0), unreach.unwrap_or(0)),
            // `if (!strcmp(ref, "refs/stash")) { … = 0; … = 0; return; }` — the stash log
            // never expires unless the caller says otherwise.
            None if refname == "refs/stash" => (0, 0),
            None => (self.default_total, self.default_unreachable),
        };
        (cli_total.unwrap_or(total), cli_unreach.unwrap_or(unreach))
    }
}

/// `REFLOG_EXPIRE_TOTAL` / `REFLOG_EXPIRE_UNREACH`, as the two slots
/// `reflog_expire_config()` writes.
const REFLOG_EXPIRE_TOTAL: u8 = 1;
const REFLOG_EXPIRE_UNREACH: u8 = 2;

/// `parse_expiry_date()` (date.c) for a configuration value.
fn expiry_date(value: &str) -> Option<i64> {
    match value {
        "now" | "all" => Some(i64::MAX),
        "never" | "false" => Some(0),
        _ => {
            let (timestamp, error) = crate::date::approxidate_careful(value);
            (!error).then_some(timestamp)
        }
    }
}

/// `wildmatch(ent->pattern, ref, 0)`.
fn glob_matches(pattern: &str, refname: &str) -> bool {
    gix::glob::wildmatch(
        pattern.into(),
        refname.into(),
        gix::glob::wildmatch::Mode::empty(),
    )
}

/// The per-worktree `logs` directory of every linked worktree, with the id that
/// prefixes the ref names inside it.
///
/// A linked worktree's refs live at `$GIT_COMMON_DIR/worktrees/<id>/logs/<ref>`
/// and are named `worktrees/<id>/<ref>`, which is what `strbuf_worktree_ref()`
/// builds in `collect_reflog()`.
fn linked_worktree_log_roots(repo: &gix::Repository) -> Vec<(String, PathBuf)> {
    let dir = repo.common_dir().join("worktrees");
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = read
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().into_owned();
            // The current worktree's own logs were already collected by name.
            (e.path() != repo.git_dir()).then(|| (id, e.path().join("logs")))
        })
        .collect();
    out.sort();
    out
}

/// The file a ref's reflog lives in. `HEAD` (and the other per-worktree
/// pseudo-refs) belong to this worktree; everything else is shared.
pub(crate) fn log_file(repo: &gix::Repository, full_name: &str) -> PathBuf {
    // `worktrees/<id>/<ref>` is another worktree's private ref, whose store is
    // `$GIT_COMMON_DIR/worktrees/<id>` — the `logs/` goes *inside* it, not in front.
    if let Some(rest) = full_name.strip_prefix("worktrees/") {
        if let Some((id, ref_name)) = rest.split_once('/') {
            return repo
                .common_dir()
                .join("worktrees")
                .join(id)
                .join("logs")
                .join(ref_name);
        }
    }
    let root = if full_name.starts_with("refs/") {
        repo.common_dir()
    } else {
        repo.git_dir()
    };
    root.join("logs").join(full_name)
}

/// Read a reflog file as raw lines, oldest first. `None` when there is no log.
fn read_raw_log(path: &Path) -> Result<Option<Vec<RawLine>>> {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for line in data.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        match parse_raw_line(line) {
            Some(parsed) => out.push(parsed),
            None => crate::git_fatal!("bad reflog line in {}", path.display()),
        }
    }
    Ok(Some(out))
}

/// Write a reflog file back from its surviving lines. An empty survivor set leaves an
/// empty file rather than removing it, which is what `git reflog expire` leaves behind.
fn write_raw_log(path: &Path, lines: &[RawLine], rewrite: bool) -> Result<()> {
    let mut out: Vec<u8> = Vec::new();
    // ```c
    // if (cb->rewrite)
    //         ooid = &cb->last_kept_oid;
    // …
    // oidcpy(&cb->last_kept_oid, noid);
    // ```
    //
    // (`expire_reflog_ent()`, refs/files-backend.c.) `last_kept_oid` lives in a
    // zero-initialised `struct expire_reflog_policy_cb`, and the substitution is
    // unconditional under `--rewrite` — so it applies to the *first* surviving
    // entry too, whose predecessor is nothing at all. Dropping the oldest entry
    // therefore leaves the new oldest one starting from the null id, the same
    // way a freshly created ref's first entry does; keeping whatever it recorded
    // would leave the log claiming a predecessor that is no longer in it.
    let mut previous: Option<ObjectId> = None;
    for line in lines {
        let want = previous.unwrap_or_else(|| ObjectId::null(line.old.kind()));
        let start = out.len();
        if rewrite && want != line.old {
            let mut fixed = want.to_hex().to_string().into_bytes();
            fixed.extend_from_slice(&line.bytes[want.to_hex().to_string().len()..]);
            out.extend_from_slice(&fixed);
        } else {
            out.extend_from_slice(&line.bytes);
        }
        // `fprintf(cb->newlog, "%s %s %s %"PRItime" %+05d\t%s", …)`
        // (`expire_reflog_ent()`, refs/files-backend.c): git rebuilds the line from
        // the fields it parsed, and that format string carries the tab whether or
        // not the input had one. Everything before the tab re-renders to the same
        // bytes the input held, which is why they are copied rather than rebuilt —
        // but a line written without a message, as `symbolic-ref` writes one, has
        // no tab to copy, and expiring its log is where git puts one back.
        if !out[start..].contains(&b'\t') {
            out.push(b'\t');
        }
        out.push(b'\n');
        previous = Some(line.new);
    }
    std::fs::write(path, out)?;
    Ok(())
}

/// `cmd_reflog_delete`'s `struct option options[]` (builtin/reflog.c), in table order:
/// the first four of [`EXPIRE_OPTS`] and nothing else, all negatable.
const DELETE_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "dry-run",   neg: true, arg: super::Arg::None },
    super::LongOpt { name: "rewrite",   neg: true, arg: super::Arg::None },
    super::LongOpt { name: "updateref", neg: true, arg: super::Arg::None },
    super::LongOpt { name: "verbose",   neg: true, arg: super::Arg::None },
];

/// `git reflog delete [--rewrite] [--updateref] [--dry-run] <ref>@{<n>}…` — port of
/// `cmd_reflog_delete`.
///
/// Each selector names one entry, counted from the newest. The entry is dropped and the
/// rest of the file is left as it was: the neighbours keep the ids they recorded unless
/// `--rewrite` asks for the chain to be closed up, and the ref itself only moves under
/// `--updateref`. A selector past the end of the log is silently ignored, as git's
/// `mark_reflog_expiry` is.
fn delete_entries(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let mut rewrite = false;
    let mut updateref = false;
    let mut dry_run = false;
    let mut verbose = false;
    let mut selectors: Vec<&str> = Vec::new();
    let mut literal = false;
    for a in args {
        let s = a.as_str();
        if literal {
            selectors.push(s);
            continue;
        }
        if s == "--" || s == "--end-of-options" {
            literal = true;
            continue;
        }
        // `-n` is the only short entry in the table, so it is what
        // `parse_short_opt()` consumes before the `h` test.
        if super::asks_for_help(s, "n") {
            return Ok(super::show_usage(DELETE_USAGE));
        }
        if let Some(body) = s.strip_prefix("--") {
            let (opt, unset) = match super::resolve_long(DELETE_OPTS, body) {
                super::Resolved::One(opt, unset) => (opt, unset),
                super::Resolved::Ambiguous(first, second) => {
                    return Ok(super::ambiguous_option(s, &first, &second, DELETE_USAGE))
                }
                super::Resolved::Unknown => return Ok(super::unknown_option(s, DELETE_USAGE)),
            };
            // Every entry is a flag, so an attached value is `PARSE_OPT_ERROR` out
            // of `get_value()`: one line, no usage block.
            if body.contains('=') {
                let shown = match unset {
                    true => format!("no-{}", opt.name),
                    false => opt.name.to_string(),
                };
                eprintln!("error: option `{shown}' takes no value");
                return Ok(ExitCode::from(129));
            }
            match opt.name {
                "dry-run" => dry_run = !unset,
                "rewrite" => rewrite = !unset,
                "updateref" => updateref = !unset,
                "verbose" => verbose = !unset,
                _ => unreachable!("resolve_long only returns DELETE_OPTS entries"),
            }
            continue;
        }
        // A short cluster, `-n` being the table's only entry.
        if s.len() > 1 && s.starts_with('-') {
            for c in s[1..].chars() {
                if c != 'n' {
                    return Ok(super::unknown_option(&format!("-{c}"), DELETE_USAGE));
                }
                dry_run = true;
            }
            continue;
        }
        selectors.push(s);
    }
    if selectors.is_empty() {
        // `return error(_("no reflog specified to delete"))` — a bare `error()`,
        // so no usage block, and its -1 reaches exit(3) as 255.
        eprintln!("error: no reflog specified to delete");
        return Ok(ExitCode::from(255));
    }

    // `for (i = 0; i < argc; i++) status |= reflog_delete(argv[i], flags, verbose);`
    // (`builtin/reflog.c:328-329`): every operand is attempted, and one failure
    // only decides the exit status.
    let mut status = ExitCode::SUCCESS;
    for spec in selectors {
        if !delete_one(repo, spec, rewrite, updateref, dry_run, verbose)? {
            status = ExitCode::from(255);
        }
    }
    Ok(status)
}

/// `reflog_delete()` (`reflog.c:520-566`) for one `<ref>@{<selector>}` operand.
/// `false` is its `error()` return.
///
/// ```c
/// const char *spec = strstr(rev, "@{");
/// if (!spec)
///         return error(_("not a reflog: %s"), rev);
/// if (!repo_dwim_log(the_repository, rev, spec - rev, NULL, &ref)) {
///         status |= error(_("no reflog for '%s'"), rev);
///         goto cleanup;
/// }
/// recno = strtoul(spec + 2, &ep, 10);
/// if (*ep == '}') {
///         opts.recno = -recno;
///         refs_for_each_reflog_ent(refs, ref, count_reflog_ent, &opts);
/// } else {
///         opts.expire_total = approxidate(spec + 2);
///         refs_for_each_reflog_ent(refs, ref, count_reflog_ent, &opts);
///         opts.expire_total = 0;
/// }
/// status |= refs_reflog_expire(refs, ref, flags, …, should_prune_fn, …, &cb);
/// ```
///
/// Three things a re-derivation gets wrong. The lookup is `repo_dwim_log()`, so
/// an ambiguous `dup@{0}` finds `refs/heads/dup`'s log rather than failing. The
/// message names the operand *as typed*, selector included. And the selector may
/// be a date: `count_reflog_ent()` then counts the entries older than it and
/// `expire_total` is reset to 0, which turns the date into an ordinal before the
/// expiry walk ever runs.
fn delete_one(
    repo: &gix::Repository,
    spec: &str,
    rewrite: bool,
    updateref: bool,
    dry_run: bool,
    verbose: bool,
) -> Result<bool> {
    let Some(at) = spec.find("@{") else {
        eprintln!("error: not a reflog: {spec}");
        return Ok(false);
    };
    let Some(full) = dwim_log(repo, &spec[..at]) else {
        eprintln!("error: no reflog for '{spec}'");
        return Ok(false);
    };
    let path = log_file(repo, &full);
    let mut lines = read_raw_log(&path)?.unwrap_or_default();

    // `strtoul(spec + 2, &ep, 10)`: leading digits, and `*ep == '}'` is what says
    // the whole selector was the number.
    let tail = &spec[at + 2..];
    let digits = tail.len() - tail.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let mut countdown: i64 = if tail[digits..] == *"}" {
        // `opts.recno = -recno` then one `++` per entry.
        lines.len() as i64 - tail[..digits].parse::<i64>().unwrap_or(0)
    } else {
        // `if (!cb->expire_total || timestamp < cb->expire_total) cb->recno++;`
        let target = crate::date::approxidate(tail);
        lines.iter().filter(|l| l.time < target).count() as i64
    };

    // `refs_reflog_expire()` walks the log oldest entry first, and
    // `should_expire_reflog_ent()` reduces — with everything but `recno` unset —
    // to `if (cb->opts.recno && --(cb->opts.recno) == 0) return 1;`. So exactly one
    // entry is dropped, and a countdown that never reaches 0 drops none.
    let mut doomed: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        let expire = countdown != 0 && {
            countdown -= 1;
            countdown == 0
        };
        if expire {
            doomed = Some(i);
        }
        if verbose {
            // `printf("keep %s", message)` — the reflog message already carries its
            // own newline, so git adds none.
            let verb = if !expire {
                "keep"
            } else if dry_run {
                "would prune"
            } else {
                "prune"
            };
            let message = match line.bytes.iter().position(|b| *b == b'\t') {
                Some(tab) => &line.bytes[tab + 1..],
                None => &[][..],
            };
            let mut out = format!("{verb} ").into_bytes();
            out.extend_from_slice(message);
            out.push(b'\n');
            std::io::Write::write_all(&mut std::io::stdout(), &out)?;
        }
    }

    if dry_run {
        return Ok(true);
    }
    if let Some(i) = doomed {
        lines.remove(i);
        write_raw_log(&path, &lines, rewrite)?;
    }
    // `EXPIRE_REFLOGS_UPDATE_REF`: the files backend writes the ref straight to its
    // lockfile, so the update leaves no reflog entry of its own.
    if updateref {
        if let Some(newest) = lines.last() {
            if !is_symref(repo, &full) {
                update_ref_to(repo, &full, newest.new)?;
            }
        }
    }
    Ok(true)
}

/// ```c
/// /*
///  * It doesn't make sense to adjust a reference pointed
///  * to by a symbolic ref based on expiring entries in
///  * the symbolic reference's reflog. …
///  */
/// int update = 0;
///
/// if ((expire_flags & EXPIRE_REFLOGS_UPDATE_REF) &&
///     !is_null_oid(&cb.last_kept_oid)) {
///         int type;
///         const char *ref;
///
///         ref = refs_resolve_ref_unsafe(&refs->base, refname,
///                                       RESOLVE_REF_NO_RECURSE,
///                                       NULL, &type);
///         update = !!(ref && !(type & REF_ISSYMREF));
/// }
/// ```
///
/// (`files_reflog_expire()`, refs/files-backend.c:3205-3224.) `RESOLVE_REF_NO_RECURSE`
/// is what makes this a test of the ref *itself* rather than of what it points at, so
/// `git reflog delete --updateref HEAD@{0}` on an attached `HEAD` leaves both `HEAD`
/// and the branch alone — where dereferencing would move the branch and writing
/// `HEAD` directly would detach it.
fn is_symref(repo: &gix::Repository, full_name: &str) -> bool {
    std::fs::read(ref_file(repo, full_name))
        .map(|body| body.starts_with(b"ref:"))
        .unwrap_or(false)
}

/// Point `full_name` at `oid` without adding a reflog entry of its own, which is
/// what `--updateref` does after the log was rewritten.
///
/// The files backend does this inside the lock it already holds for the log:
///
/// ```c
/// if ((flags & EXPIRE_REFLOGS_UPDATE_REF) && !is_null_oid(&cb.last_kept_oid)) {
///         if (write_ref_to_lockfile(refs, &lock, &cb.last_kept_oid, 0, &err) ||
///             commit_ref(&lock)) { … }
/// }
/// ```
/// (`refs/files-backend.c`)
///
/// `write_ref_to_lockfile()`/`commit_ref()` sit *below* the transaction layer, so
/// no reflog entry is appended. A `gix` `RefEdit` cannot express that — `RefLog::Only`
/// writes the log and not the ref (the exact inverse of what is wanted, which is
/// what this used to do), and `RefLog::AndReference` appends an entry whenever a log
/// file already exists, which here it always does. So the loose ref is written
/// directly, lock file and all.
fn update_ref_to(repo: &gix::Repository, full_name: &str, oid: ObjectId) -> Result<()> {
    let path = ref_file(repo, full_name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock = path.with_extension("lock");
    let mut body = oid.to_hex().to_string().into_bytes();
    body.push(b'\n');
    std::fs::write(&lock, &body)?;
    std::fs::rename(&lock, &path)?;
    Ok(())
}

/// The loose file a ref lives in, by the same per-worktree rule as [`log_file`].
fn ref_file(repo: &gix::Repository, full_name: &str) -> PathBuf {
    let root = if full_name.starts_with("refs/") {
        repo.common_dir()
    } else {
        repo.git_dir()
    };
    root.join(full_name)
}

/// `cmd_reflog_expire`'s `struct option options[]` (builtin/reflog.c), in table order,
/// as [`super::resolve_long`] reads it.
///
/// The two timestamp entries are `OPT_CALLBACK_F(… PARSE_OPT_NONEG …)`, so they take a
/// value and have no `--no-` spelling; every other entry is an `OPT_BIT` or an
/// `OPT_BOOL`, which take none and negate. Driving the parse off the table is what
/// gives `--rew`, `--sing` and the rest their abbreviations, `--exp=now` its
/// `ambiguous option:` refusal, and `--all=x` the `takes no value` diagnostic —
/// none of which a hand-written `match` on whole spellings can produce.
const EXPIRE_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "dry-run",            neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "rewrite",            neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "updateref",          neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "verbose",            neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "expire",             neg: false, arg: super::Arg::Required },
    super::LongOpt { name: "expire-unreachable", neg: false, arg: super::Arg::Required },
    super::LongOpt { name: "stale-fix",          neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "all",                neg: true,  arg: super::Arg::None },
    super::LongOpt { name: "single-worktree",    neg: true,  arg: super::Arg::None },
];

/// `git reflog expire [--expire=<time>] [--expire-unreachable=<time>] [--all] …` — port
/// of `cmd_reflog_expire`.
///
/// An entry is dropped when it is older than the cutoff that applies to it: `--expire`
/// for one whose new id is still reachable from the ref, `--expire-unreachable` for one
/// whose is not. `now` expires everything, `never` nothing; without either option git's
/// `gc.reflogExpire` (90 days) and `gc.reflogExpireUnreachable` (30 days) defaults apply.
///
/// A named ref is looked up with `repo_dwim_log()`, the same function `drop` uses, and a
/// name that names no reflog is an `error()` that does not stop the loop:
///
/// ```c
/// if (!repo_dwim_log(the_repository, argv[i], strlen(argv[i]), NULL, &ref)) {
///         status |= error(_("reflog could not be found: '%s'"), argv[i]);
///         continue;
/// }
/// ```
///
/// `error()` returns `-1`, so the `status` this ORs into leaves `cmd_reflog_expire` as
/// `-1` and reaches the process as 255. Reading the log file and skipping it when it is
/// absent — which is what this did instead — reports that repository as expired.
fn expire_entries(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    const DAY: i64 = 24 * 60 * 60;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let mut all = false;
    let mut single_worktree = false;
    let mut dry_run = false;
    let mut verbose = false;
    let mut rewrite = false;
    let mut updateref = false;
    let mut expire: Option<i64> = None;
    let mut expire_unreachable: Option<i64> = None;
    let mut refs: Vec<String> = Vec::new();
    // ```c
    // if (!strcmp(date, "never") || !strcmp(date, "false"))
    //         *timestamp = 0;
    // else if (!strcmp(date, "all") || !strcmp(date, "now"))
    //         *timestamp = TIME_MAX;
    // else
    //         *timestamp = approxidate_careful(date, &errors);
    // ```
    //
    // (`parse_expiry_date()`, date.c.) `never` is *zero*, not a floor — which matters
    // because `reflog_expiry_prepare()` compares the two cutoffs against each other.
    let cutoff = |value: &str| -> Option<i64> {
        match value {
            "now" | "all" => Some(i64::MAX),
            "never" | "false" => Some(0),
            _ => {
                let (timestamp, error) = crate::date::approxidate_careful(value);
                (!error).then_some(timestamp)
            }
        }
    };
    let mut literal = false;
    let mut i = 0;
    while i < args.len() {
        let s = args[i].as_str();
        i += 1;
        if literal {
            refs.push(s.to_owned());
            continue;
        }
        if s == "--" || s == "--end-of-options" {
            literal = true;
            continue;
        }
        // `-n` is `expire`'s only short entry, so it is what `parse_short_opt()`
        // consumes before the `h` test that answers help.
        if super::asks_for_help(s, "n") {
            return Ok(super::show_usage(EXPIRE_USAGE));
        }
        if let Some(body) = s.strip_prefix("--") {
            // `parse_long_opt()` resolves the whole body — `=<value>` included —
            // as one name before any value is split off it.
            let inline = body.split_once('=').map(|(_, v)| v);
            let (opt, unset) = match super::resolve_long(EXPIRE_OPTS, body) {
                super::Resolved::One(opt, unset) => (opt, unset),
                super::Resolved::Ambiguous(first, second) => {
                    return Ok(super::ambiguous_option(s, &first, &second, EXPIRE_USAGE))
                }
                super::Resolved::Unknown => return Ok(super::unknown_option(s, EXPIRE_USAGE)),
            };
            // `optname()`: the table's own spelling, `no-`-prefixed for the unset
            // sense, however far the typed name was abbreviated.
            let shown = match unset {
                true => format!("no-{}", opt.name),
                false => opt.name.to_string(),
            };
            // `get_value()`: a value-taking entry takes the attached one or else
            // the next argument; a flag refuses an attached one outright. Both
            // rejections are `PARSE_OPT_ERROR` — their own line, no usage block.
            let value = if opt.arg == super::Arg::Required && !unset {
                match inline {
                    Some(v) => v.to_owned(),
                    None => match args.get(i) {
                        Some(v) => {
                            i += 1;
                            v.clone()
                        }
                        None => {
                            eprintln!("error: option `{shown}' requires a value");
                            return Ok(ExitCode::from(129));
                        }
                    },
                }
            } else {
                if inline.is_some() {
                    eprintln!("error: option `{shown}' takes no value");
                    return Ok(ExitCode::from(129));
                }
                String::new()
            };
            match opt.name {
                "dry-run" => dry_run = !unset,
                "rewrite" => rewrite = !unset,
                "updateref" => updateref = !unset,
                "verbose" => verbose = !unset,
                // `opts.stalefix` only pre-marks reachable objects so that a broken
                // commit is pruned; the entry is accepted and the walk is not run.
                "stale-fix" => {}
                "all" => all = !unset,
                "single-worktree" => single_worktree = !unset,
                "expire" | "expire-unreachable" => {
                    let Some(t) = cutoff(&value) else {
                        // `parse_opt_expiry_date_cb()` reports through
                        // `error(_("invalid timestamp '%s' given to '--%s'"), arg,
                        // opt->long_name)` (parse-options-cb.c), which `parse_options()`
                        // turns into exit 128 via the `die` its callers install.
                        crate::git_fatal!(
                            "invalid timestamp '{value}' given to '--{}'",
                            opt.name
                        );
                    };
                    if opt.name == "expire" {
                        expire = Some(t);
                    } else {
                        expire_unreachable = Some(t);
                    }
                }
                _ => unreachable!("resolve_long only returns EXPIRE_OPTS entries"),
            }
            continue;
        }
        // A short cluster. `-n` is the only entry, so anything else in it is the
        // `unknown switch` the caller names by its first offending character.
        if s.len() > 1 && s.starts_with('-') {
            for c in s[1..].chars() {
                if c != 'n' {
                    return Ok(super::unknown_option(&format!("-{c}"), EXPIRE_USAGE));
                }
                dry_run = true;
            }
            continue;
        }
        refs.push(s.to_owned());
    }

    // `repo_config(the_repository, reflog_expire_config, &opts)` (builtin/reflog.c:216).
    let config = ExpireConfig::read(repo, now - 90 * DAY, now - 30 * DAY);

    // `int status = 0`, which every `error()` below ORs `-1` into.
    let mut failed = false;
    let targets: Vec<String> = if all {
        // ```c
        // worktrees = get_worktrees();
        // for (p = worktrees; *p; p++) {
        //         if (single_worktree && !(*p)->is_current)
        //                 continue;
        //         collected.worktree = *p;
        //         refs_for_each_reflog(get_worktree_ref_store(*p), collect_reflog, &collected);
        // }
        // ```
        //
        // (builtin/reflog.c:253-260.) `--all` covers every worktree, not just this one —
        // a linked worktree contributes its per-worktree logs under
        // `worktrees/<id>/<ref>` (`collect_reflog()` drops the shared refs it would
        // otherwise report a second time).
        let mut names = Vec::new();
        for root in reflog_roots(repo) {
            collect_logs(&root, "", &mut names)?;
        }
        if !single_worktree {
            for (id, root) in linked_worktree_log_roots(repo) {
                let mut own = Vec::new();
                collect_logs(&root, "", &mut own)?;
                names.extend(
                    own.into_iter()
                        // The shared half of a linked worktree's store is the same
                        // `refs/…` this worktree already listed.
                        .filter(|name| !name.starts_with("refs/"))
                        .map(|name| format!("worktrees/{id}/{name}")),
                );
            }
        }
        names.sort();
        names.dedup();
        names
    } else if refs.is_empty() {
        // `cmd_reflog_expire` loops over `argc` refs and says nothing when there
        // are none: `git reflog expire` on its own is a successful no-op, not a
        // usage error.
        Vec::new()
    } else {
        // `repo_dwim_log()`, and the `error()` for a name it does not answer. The
        // loop continues past a miss, so `expire nosuchref refs/heads/main` still
        // expires the branch and still fails.
        let mut names = Vec::new();
        for name in &refs {
            match dwim_log(repo, name) {
                Some(full) => names.push(full),
                None => {
                    eprintln!("error: reflog could not be found: '{name}'");
                    failed = true;
                }
            }
        }
        names
    };

    for full in targets {
        // `reflog_expire_options_set_refname(&cb.opts, ref)` before each expiry: the
        // command line wins, then the first `gc.<pattern>.reflog*` whose pattern matches,
        // then `refs/stash`'s never-expire rule, then the `gc.reflog*` defaults.
        let (expire, expire_unreachable) = config.for_ref(&full, expire, expire_unreachable);
        let path = log_file(repo, &full);
        let Some(lines) = read_raw_log(&path)? else {
            continue;
        };
        // ```c
        // if (!cb->opts.expire_unreachable || is_head(refname)) {
        //         cb->unreachable_expire_kind = UE_HEAD;
        // } else {
        //         commit = lookup_commit_reference_gently(the_repository, oid, 1);
        //         …
        //         cb->unreachable_expire_kind = commit ? UE_NORMAL : UE_ALWAYS;
        // }
        //
        // if (cb->opts.expire_unreachable <= cb->opts.expire_total)
        //         cb->unreachable_expire_kind = UE_ALWAYS;
        //
        // switch (cb->unreachable_expire_kind) {
        // case UE_ALWAYS:  return;
        // case UE_HEAD:    refs_for_each_ref(…, push_tip_to_list, &cb->tips); …
        // case UE_NORMAL:  commit_list_insert(commit, &cb->mark_list);
        // }
        // ```
        //
        // (`reflog_expiry_prepare()`, reflog.c:446-483.) `HEAD`'s reachability set is
        // built from *every ref*, not from HEAD's own tip — which is what keeps a `HEAD`
        // entry naming a commit that some branch still holds. `UE_ALWAYS` skips the
        // reachability question entirely and expires on age alone.
        let kind = if expire_unreachable == 0 || is_head_log(&full) {
            Unreachable::Head
        } else if ref_tip_commit(repo, &full).is_some() {
            Unreachable::Normal
        } else {
            Unreachable::Always
        };
        let kind = match expire_unreachable <= expire {
            true => Unreachable::Always,
            false => kind,
        };
        let reachable = match kind {
            Unreachable::Always => None,
            Unreachable::Head => Some(reachable_from_all_refs(repo)?),
            Unreachable::Normal => Some(reachable_from_ref(repo, &full)?),
        };
        let mut kept: Vec<RawLine> = Vec::new();
        for line in lines {
            // `is_unreachable()` answers "keep" for a null id and for anything that is
            // not a commit, and it is asked about *both* ends of the entry.
            let unreachable = |id: &ObjectId| {
                reachable.as_ref().is_some_and(|set| {
                    !id.is_null() && repo.find_commit(*id).is_ok() && !set.contains(id)
                })
            };
            let expired = line.time < expire
                || (line.time < expire_unreachable
                    && match kind {
                        Unreachable::Always => true,
                        _ => unreachable(&line.old) || unreachable(&line.new),
                    });
            if verbose {
                // `should_expire_reflog_ent_verbose()` (reflog.c:404-424). `message`
                // carries its own newline.
                let what = match (expired, dry_run) {
                    (false, _) => "keep",
                    (true, true) => "would prune",
                    (true, false) => "prune",
                };
                let mut out = Vec::from(what.as_bytes());
                out.push(b' ');
                out.extend_from_slice(&raw_line_message(&line));
                let _ = std::io::Write::write_all(&mut std::io::stdout(), &out);
            }
            if !expired {
                kept.push(line);
            }
        }
        if dry_run {
            continue;
        }
        write_raw_log(&path, &kept, rewrite)?;
        if updateref {
            if let Some(newest) = kept.last() {
                if !is_symref(repo, &full) {
                    update_ref_to(repo, &full, newest.new)?;
                }
            }
        }
    }
    // `return status`: `-1` from any `error()` above, which the process truncates to 255.
    Ok(match failed {
        true => ExitCode::from(255),
        false => ExitCode::SUCCESS,
    })
}

// ---------------------------------------------------------------------------
// drop / write — whole reflogs, rather than entries within one
// ---------------------------------------------------------------------------

/// `cmd_reflog_drop`'s option table (builtin/reflog.c:358-363): two `OPT_BOOL`s,
/// so both negate and neither takes a value.
const DROP_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "all",             neg: true, arg: super::Arg::None },
    super::LongOpt { name: "single-worktree", neg: true, arg: super::Arg::None },
];

/// `git reflog drop [--all [--single-worktree]] [<ref>...]` — port of
/// `cmd_reflog_drop`, which removes whole reflogs rather than entries inside one.
///
/// A named ref goes through `repo_dwim_log()`, so the reflog has to belong to a ref
/// that resolves: a log file left behind by a ref that no longer exists is not found
/// by name. Each miss is an `error()` that does not stop the loop, and the `-1` it
/// ORs into the return reaches `exit(3)` as 255.
fn drop_reflogs(repo: &gix::Repository, args: &[String]) -> Result<ExitCode> {
    let mut all = false;
    let mut single_worktree = false;
    let mut refs: Vec<&str> = Vec::new();
    let mut opts_done = false;
    for a in args {
        let s = a.as_str();
        if opts_done {
            refs.push(s);
            continue;
        }
        if s == "--" || s == "--end-of-options" {
            opts_done = true;
            continue;
        }
        // This table has no short entry at all, so the first character behind a
        // single `-` is the one `parse_short_opt()` tests for help.
        if super::asks_for_help(s, "") {
            return Ok(super::show_usage(DROP_USAGE));
        }
        if let Some(body) = s.strip_prefix("--") {
            // Resolved on the whole body, `=<value>` included, as `parse_long_opt()`
            // does — the lookup is what keeps `--all=x` from reaching the flag.
            let (opt, unset) = match super::resolve_long(DROP_OPTS, body) {
                super::Resolved::One(opt, unset) => (opt, unset),
                super::Resolved::Ambiguous(first, second) => {
                    return Ok(super::ambiguous_option(s, &first, &second, DROP_USAGE))
                }
                super::Resolved::Unknown => return Ok(super::unknown_option(s, DROP_USAGE)),
            };
            if body.contains('=') {
                // `PARSE_OPT_ERROR` out of `get_value()`: one line and no block,
                // naming the table entry however far it was abbreviated.
                let shown = if unset { format!("no-{}", opt.name) } else { opt.name.to_string() };
                eprintln!("error: option `{shown}' takes no value");
                return Ok(ExitCode::from(129));
            }
            match opt.name {
                "all" => all = !unset,
                "single-worktree" => single_worktree = !unset,
                _ => unreachable!("resolve_long only returns DROP_OPTS entries"),
            }
            continue;
        }
        if s.len() > 1 && s.starts_with('-') {
            return Ok(super::unknown_option(s, DROP_USAGE));
        }
        refs.push(s);
    }

    if !refs.is_empty() && all {
        // `usage(_("references specified along with --all"))` — the bare `usage()`,
        // which prints the string it was handed rather than the option block.
        eprintln!("usage: references specified along with --all");
        return Ok(ExitCode::from(129));
    }

    if all {
        // git collects from every worktree's ref store, or from this one alone under
        // `--single-worktree`, and deletes what it collected from the *main* store.
        // [`reflog_roots`] returns this worktree's private root and the shared one, so
        // both settings produce the same set here and the flag changes nothing: what
        // is missed is another worktree's per-worktree logs, which `--single-worktree`
        // would have excluded anyway. Kept as the same walk `expire --all` uses.
        let _ = single_worktree;
        let mut names = Vec::new();
        for root in reflog_roots(repo) {
            collect_logs(&root, "", &mut names)?;
        }
        names.sort();
        names.dedup();
        for name in names {
            remove_reflog_file(&log_file(repo, &name))?;
        }
        return Ok(ExitCode::SUCCESS);
    }

    let mut ret = ExitCode::SUCCESS;
    for name in refs {
        let Some(full) = dwim_log(repo, name) else {
            eprintln!("error: reflog could not be found: '{name}'");
            ret = ExitCode::from(255);
            continue;
        };
        remove_reflog_file(&log_file(repo, &full))?;
    }
    Ok(ret)
}

/// `repo_dwim_log()` (refs.c:840-879): the first of git's rev-parse spellings of
/// `name` that both resolves as a reference and has a reflog. When the reference
/// resolves somewhere else and only the target carries a log, that target is the
/// answer instead — which is how a symref's log is found through its own name.
pub(crate) fn dwim_log(repo: &gix::Repository, name: &str) -> Option<String> {
    let substituted = substitute_branch_name(repo, name);
    let name = substituted.as_deref().unwrap_or(name);
    // `ref_rev_parse_rules` (refs.c), in order.
    const RULES: &[&str] = &[
        "",
        "refs/",
        "refs/tags/",
        "refs/heads/",
        "refs/remotes/",
    ];
    let candidates = RULES
        .iter()
        .map(|prefix| format!("{prefix}{name}"))
        .chain(std::iter::once(format!("refs/remotes/{name}/HEAD")));
    for path in candidates {
        // `refs_resolve_ref_unsafe(refs, path.buf, RESOLVE_REF_READING, …)`: the
        // spelling has to name a reference that exists, not merely a log file, and a
        // symref chain that dead-ends counts as not existing.
        let Some(resolved) = resolve_ref_reading(repo, &path) else {
            continue;
        };
        if log_file(repo, &path).is_file() {
            return Some(path);
        }
        // `else if (strcmp(ref, path.buf) && refs_reflog_exists(refs, ref))`.
        if resolved != path && log_file(repo, &resolved).is_file() {
            return Some(resolved);
        }
    }
    None
}

/// `substitute_branch_name()` (refs.c:826-841): the name `repo_dwim_log()` really
/// looks up, when `repo_interpret_branch_name()` rewrites the whole spec. `None`
/// leaves the spec as typed.
///
/// Two of the three rewrites are here: the bare `@` that `interpret_empty_at()` turns
/// into `HEAD`, and `@{-<n>}`, which `interpret_nth_prior_checkout()` reads off HEAD's
/// own log. The third, `@{upstream}`/`@{push}`, is not — it resolves through the
/// branch's remote configuration and carries its own family of `die()`s, so
/// `git reflog drop @{u}` still reports the spec as a reflog it could not find rather
/// than git's `no upstream configured for branch '<name>'`.
fn substitute_branch_name(repo: &gix::Repository, name: &str) -> Option<String> {
    if name == "@" {
        return Some("HEAD".to_owned());
    }
    // The rewrite only applies when it consumed the entire spec; `@{-1}~2` keeps the
    // remainder, which is not a reflog name anyway.
    let (nth, used) = super::check_ref_format::parse_nth_prior(name.as_bytes())?;
    if used != name.len() {
        return None;
    }
    let branch = super::check_ref_format::nth_branch_switch(repo, nth)?;
    String::from_utf8(branch).ok()
}

/// `refs_resolve_ref_unsafe(…, RESOLVE_REF_READING, …)` — see
/// [`crate::refname::resolve_ref_reading`], which the ref-name shortening rules
/// need for the same reason `repo_dwim_log()` does.
pub(crate) use crate::refname::resolve_ref_reading;

/// `refs_delete_reflog()` for the files backend: unlink the log and then take the
/// directories it left empty, up to and including `logs` itself.
fn remove_reflog_file(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    }
    let mut dir = path.parent();
    while let Some(d) = dir {
        if std::fs::remove_dir(d).is_err() {
            break;
        }
        if d.file_name().is_some_and(|n| n == "logs") {
            break;
        }
        dir = d.parent();
    }
    Ok(())
}

/// `git reflog write <ref> <old-oid> <new-oid> <message>` — port of
/// `cmd_reflog_write`, which appends one entry to a reflog and touches nothing else:
/// the reference itself is neither created nor moved, and the two ids are recorded
/// exactly as given rather than read off the ref.
fn write_reflog(repo: &mut gix::Repository, args: &[String]) -> Result<ExitCode> {
    // The table is `OPT_END()` alone, so every dashed token is unknown — except the
    // help test, which `parse_options_step()` makes before it consults the table.
    let mut operands: Vec<&str> = Vec::new();
    let mut opts_done = false;
    for a in args {
        let s = a.as_str();
        if opts_done {
            operands.push(s);
            continue;
        }
        if s == "--" || s == "--end-of-options" {
            opts_done = true;
            continue;
        }
        if super::asks_for_help(s, "") {
            return Ok(super::show_usage(WRITE_USAGE));
        }
        if s.len() > 1 && s.starts_with('-') {
            return Ok(super::unknown_option(s, WRITE_USAGE));
        }
        operands.push(s);
    }
    // `usage_with_options()`, which unlike `-h` writes to stderr.
    if operands.len() != 4 {
        eprint!("{WRITE_USAGE}");
        return Ok(ExitCode::from(129));
    }
    let (name, old_spec, new_spec, message) = (operands[0], operands[1], operands[2], operands[3]);

    if !is_root_ref(name) && !super::check_ref_format::check_refname_format(name.as_bytes(), 0) {
        crate::git_fatal!("invalid reference name: {name}");
    }

    let old = parse_write_oid(repo, old_spec, "old")?;
    let new = parse_write_oid(repo, new_spec, "new")?;

    // `git_committer_info(0)`: `<name> <<email>> <seconds> <tz>`, the same string the
    // reflog writer would have filled in on its own. The non-strict form, so a machine
    // with no `user.*` gets the synthesized identity rather than a refusal.
    crate::ensure_reflog_identity(repo);
    let committer = repo
        .committer()
        .transpose()?
        .ok_or_else(|| anyhow!("no committer identity available for the reflog entry"))?;
    let mut line = format!(
        "{old} {new} {} <{}> {}",
        committer.name.to_str_lossy(),
        committer.email.to_str_lossy(),
        committer.time,
    )
    .into_bytes();
    // `log_ref_write_fd()` adds the tab and the message only when the normalized
    // message has something in it.
    let message = normalize_reflog_message(message);
    if !message.is_empty() {
        line.push(b'\t');
        line.extend_from_slice(message.as_bytes());
    }
    line.push(b'\n');

    // `ref_transaction_commit()` locks the ref before it appends, so a name that
    // collides with the reference namespace is refused there rather than by the file
    // system. `refs_verify_refname_available()` reports the *other* ref by name.
    if let Some(other) = df_conflicting_ref(repo, name)? {
        crate::git_fatal!(
            "cannot commit reflog update: cannot lock ref '{name}': \
             '{other}' exists; cannot create '{name}'"
        );
    }
    let path = log_file(repo, name);
    // With no ref in the way it can still be the *logs* that collide: a directory
    // where this entry's file belongs holds some other ref's log.
    if path.is_dir() {
        crate::git_fatal!(
            "cannot commit reflog update: cannot update the ref '{name}': \
             there are still logs under '{}'",
            crate::setup::git_path_display(repo, &path)
        );
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    file.write_all(&line)?;
    Ok(ExitCode::SUCCESS)
}

/// `refs_verify_refname_available()`: the existing reference that stops `name` from
/// being a reference too, because one of them would have to be a directory the other
/// is a file in. git checks the prefixes of `name` first and then the names under it.
fn df_conflicting_ref(repo: &gix::Repository, name: &str) -> Result<Option<String>> {
    let mut names: Vec<String> = Vec::new();
    for reference in repo.references()?.all()?.filter_map(Result::ok) {
        names.push(reference.name().as_bstr().to_str_lossy().into_owned());
    }
    names.sort();
    let mut prefix_end = 0;
    while let Some(slash) = name[prefix_end..].find('/') {
        prefix_end += slash;
        let prefix = &name[..prefix_end];
        if names.iter().any(|n| n == prefix) {
            return Ok(Some(prefix.to_owned()));
        }
        prefix_end += 1;
    }
    let under = format!("{name}/");
    Ok(names.into_iter().find(|n| n.starts_with(&under)))
}

/// One of `reflog write`'s two object arguments: a full hex id, which must name an
/// object that is present unless it is the null id.
fn parse_write_oid(repo: &gix::Repository, spec: &str, which: &str) -> Result<ObjectId> {
    let Ok(id) = ObjectId::from_hex(spec.as_bytes()) else {
        crate::git_fatal!("invalid {which} object ID: '{spec}'");
    };
    if !id.is_null() && !repo.has_object(id) {
        crate::git_fatal!("{which} object '{spec}' does not exist");
    }
    Ok(id)
}

/// `is_root_ref()` (refs.c:915-939): an all-upper-case-`-`-`_` name that is not one
/// of the two pseudo-refs, and then either ends in `_HEAD` or is on the short list of
/// irregular root refs.
fn is_root_ref(name: &str) -> bool {
    let syntax_ok = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b == b'-' || b == b'_');
    if !syntax_ok || matches!(name, "FETCH_HEAD" | "MERGE_HEAD") {
        return false;
    }
    name.ends_with("_HEAD")
        || matches!(
            name,
            "HEAD"
                | "AUTO_MERGE"
                | "BISECT_EXPECTED_REV"
                | "NOTES_MERGE_PARTIAL"
                | "NOTES_MERGE_REF"
                | "MERGE_AUTOSTASH"
        )
}

/// `copy_reflog_msg()` (refs.c:1031-1045): every run of whitespace becomes one space,
/// a leading run is dropped outright, and the result is right-trimmed.
fn normalize_reflog_message(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut was_space = true;
    for c in msg.chars() {
        let is_space = c.is_ascii_whitespace();
        if was_space && is_space {
            continue;
        }
        was_space = is_space;
        out.push(if is_space { ' ' } else { c });
    }
    while out.ends_with(char::is_whitespace) {
        out.pop();
    }
    out
}

/// Every commit reachable from a ref's current tip, which is what decides whether an
/// entry counts as unreachable for `--expire-unreachable`.
/// `reflog_expiry_prepare()`'s three reachability regimes.
#[derive(Clone, Copy, PartialEq)]
enum Unreachable {
    /// `UE_ALWAYS`: nothing is consulted; age alone decides.
    Always,
    /// `UE_HEAD`: every ref is a tip.
    Head,
    /// `UE_NORMAL`: the ref's own tip is the only one.
    Normal,
}

/// `is_head()` (reflog.c:439-444): the ref name with any worktree prefix stripped
/// is exactly `HEAD`.
fn is_head_log(full_name: &str) -> bool {
    full_name == "HEAD"
        || full_name
            .rsplit_once('/')
            .is_some_and(|(head, tail)| tail == "HEAD" && head.starts_with("worktrees/"))
}

/// `lookup_commit_reference_gently(the_repository, oid, 1)` on a ref's target: the
/// commit it names, or `None` when it names none.
fn ref_tip_commit(repo: &gix::Repository, full_name: &str) -> Option<ObjectId> {
    repo.try_find_reference(full_name)
        .ok()
        .flatten()
        .and_then(|mut r| r.peel_to_id_in_place().ok())
        .filter(|id| repo.find_commit(id.detach()).is_ok())
        .map(|id| id.detach())
}

/// `push_tip_to_list()` over `refs_for_each_ref()`, closed over: the commits every
/// non-symbolic ref reaches, which is `UE_HEAD`'s notion of reachable.
fn reachable_from_all_refs(
    repo: &gix::Repository,
) -> Result<std::collections::HashSet<ObjectId>> {
    let mut tips: Vec<ObjectId> = Vec::new();
    if let Ok(platform) = repo.references() {
        if let Ok(iter) = platform.all() {
            for reference in iter.flatten() {
                let mut reference = reference;
                if reference.target().try_id().is_none() {
                    continue; // `if (ref->flags & REF_ISSYMREF) return 0;`
                }
                if let Ok(id) = reference.peel_to_id_in_place() {
                    if repo.find_commit(id.detach()).is_ok() {
                        tips.push(id.detach());
                    }
                }
            }
        }
    }
    let mut set = std::collections::HashSet::new();
    if let Ok(walk) = repo.rev_walk(tips).all() {
        for info in walk.flatten() {
            set.insert(info.id);
        }
    }
    Ok(set)
}

fn reachable_from_ref(
    repo: &gix::Repository,
    full_name: &str,
) -> Result<std::collections::HashSet<ObjectId>> {
    let mut set = std::collections::HashSet::new();
    let Some(tip) = repo
        .try_find_reference(full_name)
        .ok()
        .flatten()
        .and_then(|mut r| r.peel_to_id_in_place().ok())
        .map(|id| id.detach())
    else {
        return Ok(set);
    };
    let Ok(walk) = repo.rev_walk([tip]).all() else {
        return Ok(set);
    };
    for info in walk.flatten() {
        set.insert(info.id);
    }
    Ok(set)
}

#[cfg(test)]
mod drop_write_tests {
    use super::{is_root_ref, normalize_reflog_message};

    /// `copy_reflog_msg()`: a run of whitespace becomes one space, a leading run is
    /// dropped, and the result is right-trimmed — so the entry stays one line however
    /// the message was typed. Verified against stock git 2.55.0, where
    /// `git reflog write <ref> <old> <new> "  lots   of\n\nwhitespace\there   "`
    /// records `lots of whitespace here`.
    #[test]
    fn a_message_is_collapsed_to_single_spaces_and_trimmed() {
        assert_eq!(
            normalize_reflog_message("  lots   of\n\nwhitespace\there   "),
            "lots of whitespace here"
        );
        assert_eq!(normalize_reflog_message("plain"), "plain");
    }

    /// An empty result means no tab and no message at all in the written line, which
    /// is `log_ref_write_fd()`'s `if (msg && *msg)`. Stock writes the same line for
    /// `""` and for `"   "`.
    #[test]
    fn a_message_of_only_whitespace_becomes_nothing() {
        assert_eq!(normalize_reflog_message(""), "");
        assert_eq!(normalize_reflog_message("   "), "");
        assert_eq!(normalize_reflog_message("\n\t "), "");
    }

    /// `is_root_ref()`: upper-case-`-`-`_` syntax, not one of the two pseudo-refs, and
    /// then either `_HEAD`-suffixed or on the irregular list. This is what lets
    /// `reflog write` name a root ref at all — everything else has to pass
    /// `check_refname_format(ref, 0)`, which one-level names fail. Each of these was
    /// run against stock git 2.55.0: `HEAD`, `ORIG_HEAD`, `AUTO_MERGE` and `FOO_HEAD`
    /// are written, while `MERGE_HEAD`, `onelevel` and `FOO` are
    /// `fatal: invalid reference name`.
    #[test]
    fn only_git_s_root_refs_skip_the_refname_check() {
        for ok in ["HEAD", "ORIG_HEAD", "FOO_HEAD", "AUTO_MERGE", "MERGE_AUTOSTASH"] {
            assert!(is_root_ref(ok), "{ok} is a root ref");
        }
        // The two pseudo-refs are excluded even though they match the syntax.
        for no in ["MERGE_HEAD", "FETCH_HEAD", "FOO", "onelevel", "refs/heads/main", ""] {
            assert!(!is_root_ref(no), "{no} is not a root ref");
        }
    }
}
