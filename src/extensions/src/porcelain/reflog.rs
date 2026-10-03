use anyhow::{anyhow, bail, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::hash::ObjectId;
use gix::refs::reftable::ExpireFlags;

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
    // token — the subcommand synopsis on stdout, exit 0. Once a subcommand has
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
    // `refs_for_each_reflog(get_main_ref_store(repo), show_reflog, NULL)`
    // (builtin/reflog.c:167-178): in a linked worktree its own reflogs merged
    // with the shared ones.
    let mut out = Vec::new();
    for name in crate::refstore::reflog_names(repo, false)? {
        out.extend_from_slice(&name);
        out.push(b'\n');
    }
    std::io::Write::write_all(&mut std::io::stdout(), &out)?;
    Ok(ExitCode::SUCCESS)
}

/// `git reflog exists <ref>` — `refs_reflog_exists()` on the main ref store.
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

    Ok(if crate::refstore::reflog_exists(repo, name) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

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
pub(super) struct ExpireConfig {
    /// `opts->default_expire_total`.
    default_total: i64,
    /// `opts->default_expire_unreachable`.
    default_unreachable: i64,
    /// `opts->entries`, in configuration order — the first matching pattern wins.
    entries: Vec<(String, Option<i64>, Option<i64>)>,
}

impl ExpireConfig {
    /// `REFLOG_EXPIRE_OPTIONS_INIT(now)` (reflog.h:25-28) — entries 30 days old
    /// expire, and unreachable ones after 90 — then `reflog_expire_config()`.
    pub(super) fn read(repo: &gix::Repository, now: i64) -> Self {
        const DAY: i64 = 24 * 60 * 60;
        let mut out = ExpireConfig {
            default_total: now - 30 * DAY,
            default_unreachable: now - 90 * DAY,
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
    pub(super) fn for_ref(&self, refname: &str, cli_total: Option<i64>, cli_unreach: Option<i64>) -> (i64, i64) {
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

/// `files_reflog_path()` (refs/files-backend.c:239-264): the file the files
/// backend keeps the reflog of `full_name` in. A per-worktree name (`HEAD`,
/// `refs/bisect/…`, …) is the current worktree's, `main-worktree/<ref>` the
/// main one's, `worktrees/<id>/<ref>` that worktree's, and anything else is
/// shared.
pub(crate) fn log_file(repo: &gix::Repository, full_name: &str) -> PathBuf {
    use gix::refs::reftable::{parse_worktree_ref, WorktreeType};
    let (kind, worktree, bare) = parse_worktree_ref(full_name.into());
    let bare = gix::path::from_bstr(bare);
    match kind {
        WorktreeType::Current => repo.git_dir().join("logs").join(full_name),
        WorktreeType::Shared | WorktreeType::Main => repo.common_dir().join("logs").join(bare),
        WorktreeType::Other => repo
            .common_dir()
            .join("worktrees")
            .join(gix::path::from_bstr(worktree.unwrap_or_default()))
            .join("logs")
            .join(bare),
    }
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
        if !reflog_delete(repo, spec, rewrite, updateref, dry_run, verbose)? {
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
pub(super) fn reflog_delete(
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
    // `strtoul(spec + 2, &ep, 10)`: leading digits, and `*ep == '}'` — the one
    // character after them, whatever follows the brace — says it was a number.
    // `count_reflog_ent()` then counts the entries `refs_for_each_reflog_ent()`
    // yields: all of them for `@{<n>}` (`opts.recno = -recno` then one `++`
    // each), or those older than the date.
    let tail = &spec[at + 2..];
    let digits = tail.len() - tail.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let older_than = (!tail[digits..].starts_with('}')).then(|| crate::date::approxidate(tail));
    let mut recno: i64 = match older_than {
        None => -tail[..digits].parse::<i64>().unwrap_or(0),
        Some(_) => 0,
    };
    let count = |time: i64| older_than.map_or(true, |target| time < target);

    // `refs_reflog_expire()` walks the log oldest entry first, and
    // `should_expire_reflog_ent()` reduces — with everything but `recno` unset —
    // to `if (cb->opts.recno && --(cb->opts.recno) == 0) return 1;`. So exactly one
    // entry is dropped, and a countdown that never reaches 0 drops none.
    let flags = ExpireFlags { dry_run, update_ref: updateref, rewrite };
    if crate::refstore::is_reftable(repo) {
        crate::refstore::for_each_reflog_entry(repo, &full, false, |entry| {
            if count(entry.timestamp as i64) {
                recno += 1;
            }
            std::ops::ControlFlow::Continue(())
        })?;
        let mut cb = ExpirePolicyCb::new(repo, 0, 0, verbose, dry_run);
        cb.recno = recno;
        return reftable_reflog_expire(repo, &full, flags, &mut cb);
    }

    let path = log_file(repo, &full);
    let mut lines = read_raw_log(&path)?.unwrap_or_default();
    recno += lines.iter().filter(|l| count(l.time)).count() as i64;
    let mut cb = ExpirePolicyCb::new(repo, 0, 0, verbose, dry_run);
    cb.recno = recno;
    cb.prepare_for(&full, ref_value_resolved(repo, &full));
    let mut doomed: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        if cb.should_expire(line.old, line.new, line.time, &raw_line_message(line)) {
            doomed = Some(i);
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

/// `files_ref_path()` (refs/files-backend.c:266-290): the loose file a ref
/// lives in, by the same per-worktree rule as [`log_file`].
fn ref_file(repo: &gix::Repository, full_name: &str) -> PathBuf {
    use gix::refs::reftable::{parse_worktree_ref, WorktreeType};
    let (kind, worktree, bare) = parse_worktree_ref(full_name.into());
    let bare = gix::path::from_bstr(bare);
    match kind {
        WorktreeType::Current => repo.git_dir().join(full_name),
        WorktreeType::Shared | WorktreeType::Main => repo.common_dir().join(bare),
        WorktreeType::Other => repo
            .common_dir()
            .join("worktrees")
            .join(gix::path::from_bstr(worktree.unwrap_or_default()))
            .join(bare),
    }
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

    let request = ExpireRequest {
        all,
        single_worktree,
        refs,
        expire,
        expire_unreachable,
        flags: ExpireFlags { dry_run, update_ref: updateref, rewrite },
        verbose,
    };
    // `return status`: `-1` from any `error()`, which the process truncates to 255.
    Ok(match expire_reflogs(repo, &request)? {
        true => ExitCode::SUCCESS,
        false => ExitCode::from(255),
    })
}

/// What `cmd_reflog_expire()` was asked to do once its options are parsed.
pub(super) struct ExpireRequest {
    /// `--all`.
    pub(super) all: bool,
    /// `--single-worktree`.
    pub(super) single_worktree: bool,
    /// The `<refs>` operands.
    pub(super) refs: Vec<String>,
    /// `--expire`, when given.
    pub(super) expire: Option<i64>,
    /// `--expire-unreachable`, when given.
    pub(super) expire_unreachable: Option<i64>,
    /// `--dry-run`, `--updateref`, `--rewrite`.
    pub(super) flags: ExpireFlags,
    /// `--verbose`.
    pub(super) verbose: bool,
}

impl ExpireRequest {
    /// `git reflog expire --all`, the request `gc` and `maintenance` run.
    pub(super) fn all() -> Self {
        ExpireRequest {
            all: true,
            single_worktree: false,
            refs: Vec::new(),
            expire: None,
            expire_unreachable: None,
            flags: ExpireFlags::default(),
            verbose: false,
        }
    }
}

/// The body of `cmd_reflog_expire()` (builtin/reflog.c:215-300) after option
/// parsing: `false` is the `-1` status of an `error()` along the way.
pub(super) fn expire_reflogs(repo: &gix::Repository, request: &ExpireRequest) -> Result<bool> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // `repo_config(the_repository, reflog_expire_config, &opts)` (builtin/reflog.c:216)
    // over `REFLOG_EXPIRE_OPTIONS_INIT(now)`.
    let config = ExpireConfig::read(repo, now);

    // `int status = 0`, which every `error()` below ORs `-1` into.
    let mut ok = true;
    let mut targets: Vec<String> = Vec::new();
    if request.all {
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
        // (builtin/reflog.c:253-260.) `--all` covers every worktree, in
        // `get_worktrees()` order.
        targets = collect_reflogs(repo, request.single_worktree)?;
    }
    // `repo_dwim_log()`, and the `error()` for a name it does not answer. The
    // loop continues past a miss, so `expire nosuchref refs/heads/main` still
    // expires the branch and still fails.
    for name in &request.refs {
        match dwim_log(repo, name) {
            Some(full) => targets.push(full),
            None => {
                eprintln!("error: reflog could not be found: '{name}'");
                ok = false;
            }
        }
    }

    for full in targets {
        // `reflog_expire_options_set_refname(&cb.opts, ref)` before each expiry: the
        // command line wins, then the first `gc.<pattern>.reflog*` whose pattern matches,
        // then `refs/stash`'s never-expire rule, then the `gc.reflog*` defaults.
        let (expire, expire_unreachable) = config.for_ref(&full, request.expire, request.expire_unreachable);
        let mut cb = ExpirePolicyCb::new(repo, expire, expire_unreachable, request.verbose, request.flags.dry_run);
        if crate::refstore::is_reftable(repo) {
            ok &= reftable_reflog_expire(repo, &full, request.flags, &mut cb)?;
            continue;
        }
        let path = log_file(repo, &full);
        let Some(lines) = read_raw_log(&path)? else {
            continue;
        };
        cb.prepare_for(&full, ref_value_resolved(repo, &full));
        let mut kept: Vec<RawLine> = Vec::new();
        for line in lines {
            if !cb.should_expire(line.old, line.new, line.time, &raw_line_message(&line)) {
                kept.push(line);
            }
        }
        if request.flags.dry_run {
            continue;
        }
        write_raw_log(&path, &kept, request.flags.rewrite)?;
        if request.flags.update_ref {
            if let Some(newest) = kept.last() {
                if !is_symref(repo, &full) {
                    update_ref_to(repo, &full, newest.new)?;
                }
            }
        }
    }
    Ok(ok)
}

/// `collect_reflog()` (builtin/reflog.c:91-109) over every worktree's ref store,
/// or only the current one's with `single_worktree`: the reflogs `--all` names,
/// each spelt by `strbuf_worktree_ref()`, a shared one only through the current
/// worktree so it is not collected once per worktree.
fn collect_reflogs(repo: &gix::Repository, single_worktree: bool) -> Result<Vec<String>> {
    use gix::refs::reftable::{parse_worktree_ref, WorktreeType};
    let mut names = Vec::new();
    for wt in super::prune::get_worktrees(repo) {
        if single_worktree && !wt.is_current {
            continue;
        }
        for name in wt.reflog_names(repo)? {
            if !wt.is_current && parse_worktree_ref(name.as_ref()).0 == WorktreeType::Shared {
                continue;
            }
            names.push(wt.ref_name(name.as_ref()).to_string());
        }
    }
    Ok(names)
}

/// `refs_reflog_expire()` on a reftable repository (`reftable_be_reflog_expire()`,
/// refs/reftable-backend.c:2575-2737), the policy deciding each entry. A failure is
/// git's `error()`, reported and returned as `false`.
fn reftable_reflog_expire(
    repo: &gix::Repository,
    full: &str,
    flags: ExpireFlags,
    cb: &mut ExpirePolicyCb<'_>,
) -> Result<bool> {
    let name = gix::refs::FullName::try_from(full)?;
    if (cb.expire_unreachable == 0 || is_head_log(full)) && cb.expire_unreachable > cb.expire_total {
        cb.head_tips = Some(all_ref_tip_commits(repo));
    }
    match repo.reftable_reflog_expire(name.as_ref(), flags, cb) {
        Ok(()) => Ok(true),
        Err(err) => {
            eprintln!("error: {err}");
            Ok(false)
        }
    }
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
        // `--single-worktree`, and deletes what it collected from the *main* store
        // (builtin/reflog.c:370-393).
        let mut ok = true;
        for name in collect_reflogs(repo, single_worktree)? {
            ok &= delete_reflog(repo, &name)?;
        }
        return Ok(if ok { ExitCode::SUCCESS } else { ExitCode::from(255) });
    }

    let mut ret = ExitCode::SUCCESS;
    for name in refs {
        let Some(full) = dwim_log(repo, name) else {
            eprintln!("error: reflog could not be found: '{name}'");
            ret = ExitCode::from(255);
            continue;
        };
        if !delete_reflog(repo, &full)? {
            ret = ExitCode::from(255);
        }
    }
    Ok(ret)
}

/// `refs_delete_reflog()` on the main ref store: the files backend unlinks the
/// log, the reftable backend writes deletions for its entries
/// (`reftable_be_delete_reflog()`, refs/reftable-backend.c:2480-2501). `false`
/// is a reported `error()`.
fn delete_reflog(repo: &gix::Repository, full: &str) -> Result<bool> {
    if !crate::refstore::is_reftable(repo) {
        remove_reflog_file(&log_file(repo, full))?;
        return Ok(true);
    }
    let name = gix::refs::FullName::try_from(full)?;
    match repo.reftable_delete_reflog(name.as_ref()) {
        Ok(()) => Ok(true),
        Err(err) => {
            eprintln!("error: {err}");
            Ok(false)
        }
    }
}

/// `repo_dwim_log()` (refs.c:839-878): the first of git's rev-parse spellings of
/// `name` that both resolves as a reference and has a reflog in the ref store,
/// or the reference it resolves to when only that one carries a log — the one
/// port in [`crate::objname::dwim_log`].
pub(crate) fn dwim_log(repo: &gix::Repository, name: &str) -> Option<String> {
    crate::objname::dwim_log(repo, name).1
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

    // `ref_transaction_update_reflog()` queues a `REF_LOG_ONLY` update that
    // records the two ids as given (`REF_LOG_USE_PROVIDED_OIDS`, refs.c:1464-1494);
    // the reftable transaction this port drives always logs the reference's
    // current value as the old one, so it cannot write this entry.
    if crate::refstore::is_reftable(repo) {
        bail!("reflog write is not supported in a reftable repository: the ref store cannot log provided object ids");
    }

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
pub(super) fn normalize_reflog_message(msg: &str) -> String {
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

/// `struct expire_reflog_policy_cb` (reflog.h) and the three callbacks
/// `refs_reflog_expire()` drives with it — `reflog_expiry_prepare()`,
/// `should_expire_reflog_ent[_verbose]()` and `reflog_expiry_cleanup()`
/// (reflog.c:337-505) — for one reflog. `reflog expire` and `reflog delete`
/// (and through it `stash drop`) decide every entry here, whichever backend
/// stores the log.
pub(super) struct ExpirePolicyCb<'r> {
    repo: &'r gix::Repository,
    /// `opts.expire_total`.
    expire_total: i64,
    /// `opts.expire_unreachable`.
    expire_unreachable: i64,
    /// `opts.recno`: `reflog delete`'s count-down to the entry it drops, 0 when unused.
    recno: i64,
    /// `unreachable_expire_kind`.
    kind: Unreachable,
    /// The commits `mark_reachable()` reaches from the tips; `is_unreachable()`
    /// digs to the root on a miss, so this is the full closure.
    reachable: HashSet<ObjectId>,
    /// `UE_HEAD`'s tips, read before the reftable store takes the lock on
    /// its stack: the callbacks run under that lock, so they cannot read
    /// references through the store themselves.
    head_tips: Option<Vec<ObjectId>>,
    /// `--verbose`: `should_expire_reflog_ent_verbose()`.
    verbose: bool,
    /// `cb->dry_run`, which only changes what `--verbose` says.
    dry_run: bool,
}

impl<'r> ExpirePolicyCb<'r> {
    pub(super) fn new(repo: &'r gix::Repository, expire_total: i64, expire_unreachable: i64, verbose: bool, dry_run: bool) -> Self {
        ExpirePolicyCb {
            repo,
            expire_total,
            expire_unreachable,
            recno: 0,
            kind: Unreachable::Always,
            reachable: HashSet::new(),
            head_tips: None,
            verbose,
            dry_run,
        }
    }

    /// `reflog_expiry_prepare()` (reflog.c:446-483) for the reflog of
    /// `refname`, whose reference holds `oid` (null when symbolic or missing).
    ///
    /// `HEAD`'s reachability set is built from *every ref*, not from HEAD's own
    /// tip — which is what keeps a `HEAD` entry naming a commit that some branch
    /// still holds. `UE_ALWAYS` skips the reachability question entirely and
    /// expires on age alone.
    fn prepare_for(&mut self, refname: &str, oid: ObjectId) {
        let tip = (!oid.is_null()).then(|| peel_to_commit(self.repo, oid)).flatten();
        self.kind = if self.expire_unreachable == 0 || is_head_log(refname) {
            Unreachable::Head
        } else if tip.is_some() {
            Unreachable::Normal
        } else {
            Unreachable::Always
        };
        if self.expire_unreachable <= self.expire_total {
            self.kind = Unreachable::Always;
        }
        self.reachable = match self.kind {
            Unreachable::Always => HashSet::new(),
            Unreachable::Head => {
                let tips = self.head_tips.take().unwrap_or_else(|| all_ref_tip_commits(self.repo));
                reachable_commits(self.repo, tips)
            }
            Unreachable::Normal => reachable_commits(self.repo, tip.into_iter().collect()),
        };
    }

    /// `should_expire_reflog_ent()` (reflog.c:370-402), printed as
    /// `should_expire_reflog_ent_verbose()` (:404-424) does under `--verbose`.
    /// `message` is the entry's message as git passes it, its newline included.
    fn should_expire(&mut self, old: ObjectId, new: ObjectId, timestamp: i64, message: &[u8]) -> bool {
        let expire = self.decide(old, new, timestamp);
        if self.verbose {
            let what = match (expire, self.dry_run) {
                (false, _) => "keep",
                (true, true) => "would prune",
                (true, false) => "prune",
            };
            let mut out = Vec::from(what.as_bytes());
            out.push(b' ');
            out.extend_from_slice(message);
            let _ = std::io::Write::write_all(&mut std::io::stdout(), &out);
        }
        expire
    }

    fn decide(&mut self, old: ObjectId, new: ObjectId, timestamp: i64) -> bool {
        if timestamp < self.expire_total {
            return true;
        }
        // `opts.stalefix` is accepted and not acted on; see `--stale-fix` above.
        if timestamp < self.expire_unreachable {
            match self.kind {
                Unreachable::Always => return true,
                Unreachable::Head | Unreachable::Normal => {
                    if self.is_unreachable(old) || self.is_unreachable(new) {
                        return true;
                    }
                }
            }
        }
        if self.recno != 0 {
            self.recno -= 1;
            if self.recno == 0 {
                return true;
            }
        }
        false
    }

    /// `is_unreachable()` (reflog.c:337-365): a null id and anything that does
    /// not peel to a commit are kept; a commit is unreachable when it is not in
    /// the closure of the tips.
    fn is_unreachable(&self, oid: ObjectId) -> bool {
        if oid.is_null() {
            return false;
        }
        peel_to_commit(self.repo, oid).is_some_and(|commit| !self.reachable.contains(&commit))
    }
}

/// The reftable backend calls back into the same policy.
impl gix::refs::reftable::ExpirePolicy for ExpirePolicyCb<'_> {
    fn prepare(&mut self, refname: &gix::refs::FullNameRef, oid: &gix::hash::oid) {
        self.prepare_for(&refname.as_bstr().to_str_lossy(), oid.to_owned());
    }

    fn should_prune(&mut self, entry: &gix::refs::log::Line) -> bool {
        // The record's message is handed over without its newline. git's
        // writer ends every message it stores with one, an empty message
        // included (`reftable_writer_add_log()`, reftable/writer.c:466-489), and
        // `should_expire_reflog_ent_verbose()` prints it as stored.
        let mut message = entry.message.to_vec();
        message.push(b'\n');
        self.should_expire(entry.previous_oid, entry.new_oid, entry.signature.time.seconds, &message)
    }

    fn cleanup(&mut self) {
        self.reachable.clear();
    }

    /// `peel_object()`: the object an annotated tag at `oid` finally names,
    /// stored with the reference `--updateref` writes.
    fn peel(&mut self, oid: &gix::hash::oid) -> Option<ObjectId> {
        let object = self.repo.find_object(oid).ok()?;
        if object.kind != gix::object::Kind::Tag {
            return None;
        }
        object.peel_tags_to_end().ok().map(|peeled| peeled.id)
    }
}

/// `is_head()` (reflog.c:439-444): the ref name with any worktree prefix stripped
/// is exactly `HEAD`.
fn is_head_log(full_name: &str) -> bool {
    gix::refs::reftable::parse_worktree_ref(full_name.into()).2 == "HEAD"
}

/// `lookup_commit_reference_gently(the_repository, oid, 1)`: the commit `oid`
/// names, through any chain of annotated tags, or `None` for anything else.
fn peel_to_commit(repo: &gix::Repository, oid: ObjectId) -> Option<ObjectId> {
    let object = repo.find_object(oid).ok()?;
    object.peel_tags_to_end().ok().filter(|o| o.kind == gix::object::Kind::Commit).map(|o| o.id)
}

/// The value `files_reflog_expire()` hands to the prepare callback:
/// `lock_ref_oid_basic()` resolves the reference through symbolic ones
/// (refs/files-backend.c:1296-1303), null when it does not resolve.
fn ref_value_resolved(repo: &gix::Repository, full_name: &str) -> ObjectId {
    repo.try_find_reference(full_name)
        .ok()
        .flatten()
        .and_then(|mut r| r.follow_to_object().ok())
        .map(|id| id.detach())
        .unwrap_or_else(|| ObjectId::null(repo.object_hash()))
}

/// `push_tip_to_list()` over `refs_for_each_ref()`: the commit of every
/// reference that is not symbolic, `UE_HEAD`'s tips.
fn all_ref_tip_commits(repo: &gix::Repository) -> Vec<ObjectId> {
    let mut tips = Vec::new();
    if let Ok(platform) = repo.references() {
        if let Ok(iter) = platform.all() {
            for reference in iter.flatten() {
                // `if (ref->flags & REF_ISSYMREF) return 0;`
                let Some(id) = reference.target().try_id().map(ToOwned::to_owned) else {
                    continue;
                };
                tips.extend(peel_to_commit(repo, id));
            }
        }
    }
    tips
}

/// The commits reachable from `tips` through their parents.
fn reachable_commits(repo: &gix::Repository, tips: Vec<ObjectId>) -> HashSet<ObjectId> {
    let mut set = HashSet::new();
    if let Ok(walk) = repo.rev_walk(tips).all() {
        for info in walk.flatten() {
            set.insert(info.id);
        }
    }
    set
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
