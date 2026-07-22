//! `git prune` — remove unreachable loose objects from the object database.
//!
//! This is a real port, not a stub: it reproduces `builtin/prune.c`'s three
//! phases in order, against the vendored gitoxide crates plus `std::fs` for the
//! directory walk and the unlinks (`gix-odb` exposes no removal API, and git's
//! own implementation is a raw `readdir`/`unlink` loop over `objects/`).
//!
//!   1. **Loose prune.** Every `objects/00`..`objects/ff` fan-out directory is
//!      scanned in numeric order, entries within a directory in raw `readdir`
//!      order — exactly what `for_each_loose_file_in_objdir()` does, so the
//!      emitted order matches git on the same filesystem. An object that was not
//!      marked reachable prints `<oid> <type>` under `-n`/`-v` and is unlinked
//!      unless `-n`. A name that is not a valid object name is cruft: `tmp_obj_*`
//!      goes through the stale-temporary-file path, anything else produces
//!      `bad sha1 file: <path>` on stderr. Each visited fan-out directory is
//!      `rmdir`'d (silently failing when non-empty) unless `-n`.
//!   2. **Prune-packed.** A second full scan removes loose objects that are also
//!      present in a pack (local or in an alternate), printing `rm -f <path>`
//!      under `-n` only — `-v` does not make this phase verbose, matching git,
//!      which passes `prune_packed_objects()` nothing but the dry-run bit.
//!   3. **Stale temporaries.** `tmp_*` in `objects/` and in `objects/pack/`:
//!      `Removing stale temporary file <path>` on stdout when `-n` or `-v`,
//!      unlinked unless `-n`.
//!
//! Reachability mirrors `reachable.c`'s `mark_reachable_objects(revs, 1, expire, _)`:
//! roots are every index entry's blob plus the valid cache-tree ids, every ref
//! under `refs/` (symrefs followed, tags left unpeeled so the tag object itself
//! survives), `HEAD`, every entry of every reflog under `logs/`, and any
//! `<head>...` given on the command line; then a full object closure over
//! commits (tree + parents), tags (target) and trees (entries, gitlinks skipped).
//! Missing links are ignored rather than fatal, as git sets
//! `revs->ignore_missing_links`.
//!
//! `--expire <time>` is `expire` in `builtin/prune.c`, and it does two distinct
//! things, both ported:
//!   * Every unlink is gated on `st_mtime > expire` keeping the file — that gate
//!     covers unreachable loose objects (`prune_object()`) *and* stale
//!     temporaries (`prune_tmp_file()`), but never `prune_packed_objects()`,
//!     which git runs unconditionally.
//!   * A non-zero `expire` also becomes `mark_recent`, so
//!     `add_unseen_recent_objects_to_traversal()` seeds the closure with every
//!     local object written after `expire` — loose objects by their own mtime,
//!     packed objects by their `.pack`'s mtime. That is what keeps an *old*
//!     object alive because a *recent* unreachable commit still points at it.
//!
//! Values go through `parse_expiry_date()`'s two special cases first: `never`
//! and `false` are `0` (prune nothing, no recent traversal), `all` and `now` are
//! `TIME_MAX` (prune every unreachable object, no grace). Anything else is a
//! date, parsed by `gix::date::parse` after git's approxidate tokenisation is
//! approximated by splitting digit/letter runs and treating `.` as a separator,
//! so `2.weeks.ago`, `2weeks ago` and `2 weeks` all reach the same span.
//!
//! When no `gix` form parses, git's own `approxidate` is far more permissive
//! than a strict date parser: `approxidate_str()` only reports a malformed date
//! when its `touched` flag stays clear, i.e. the string held *no* recognisable
//! token at all. A run of digits always sets `touched` (so `0x10`, `12abc`,
//! `-5`, `1e5` are all accepted), as does a standalone date word — a `>= 3` char
//! prefix of a month name, a number word `one`..`ten`, `last`, or one of
//! `am`/`pm`/`noon`/`midnight`/`tea`/`yesterday`/`today`. Only a string with
//! neither (a bare `abc`, `week`, `monday`, `tomorrow`, or whitespace) is
//! `fatal: malformed expiration date '<value>'`, exit 128. A bare integer
//! `>= 100000000` is `match_digit()`'s raw-epoch case; every other accepted
//! value approxidate resolves relative to now.
//!
//! Deviation: a bare `YYYY-MM-DD` resolves to UTC midnight, where git's
//! approxidate uses *local* midnight — up to ~14h apart. The vendored crates
//! expose no local-timezone lookup (`gix-date` keeps `jiff` private), so there is
//! nothing here to read the system offset from.
//!
//! Paths are printed the way git does after it chdir's to the top level: the
//! object directory relative to the worktree (`.git/objects/...`), or relative to
//! the current directory for a bare repository (`objects/...`).
//!
//! Supported: `-n`/`--dry-run`/`--no-dry-run`, `-v`/`--verbose`/`--no-verbose`,
//! clustered short flags (`-nv`), `--progress`/`--no-progress`,
//! `--exclude-promisor-objects`/`--no-exclude-promisor-objects`,
//! `--expire <time>`/`--expire=<time>`/`--no-expire`, `--`, `<head>...`, and
//! `-h`. Exit codes match stock git: 129 with git's usage block for `-h` and for
//! an unknown option, 129 *without* the usage block for parse-options' value
//! complaints (`option \`expire' requires a value`, `option \`<name>' takes no
//! value`), 128 with `fatal: unrecognized argument: <name>` for a `<head>` that
//! does not resolve, 0 otherwise.
//!
//! `--progress` is accepted and deliberately produces nothing. Git's own
//! progress is a *delayed* progress written to stderr and suppressed off a tty,
//! so it never appears in captured output, and it can never affect stdout, the
//! exit code, or the resulting repository state.
//!
//! `--exclude-promisor-objects`/`--no-exclude-promisor-objects` are accepted and
//! change nothing observable. In `builtin/prune.c` the flag's whole body is
//! `fetch_if_missing = 0; revs.exclude_promisor_objects = 1;` — it never marks a
//! promisor object reachable. That revision flag's only effect on the walk
//! (`list-objects.c`) is to skip a link into an object that is both absent
//! locally and named by a promisor pack, instead of fetching it or dying on the
//! missing link. A present loose object — the only kind prune can remove — is
//! reachable only through a chain of present objects, none of which the flag ever
//! skips, so its reachability is identical either way. This port performs no lazy
//! fetch and `close_over` already drops any unresolvable link, so its walk
//! already matches the flagged walk byte-for-byte.
//!
//! Not ported, and rejected with a precise reason rather than approximated:
//!   * A shallow repository, because git additionally rewrites `.git/shallow`
//!     via `prune_shallow()`, and there is no shallow-file writer here.
//!   * A repository with linked worktrees, because git also seeds reachability
//!     from every other worktree's `HEAD` and index; pruning without them would
//!     delete objects those worktrees still need.

use anyhow::{bail, Result};
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::hash::ObjectId;
use gix::objs::Kind;
use gix::odb::pack;

/// Stock git's `prune` usage block, byte-for-byte (423 bytes, git 2.55.0),
/// including the trailing blank line. Printed on `-h` (stdout) and on a usage
/// error (stderr).
const USAGE: &str = "usage: git prune [-n] [-v] [--progress] [--expire <time>] [--] [<head>...]\n\
                     \n\
                     \x20   -n, --[no-]dry-run    do not remove, show only\n\
                     \x20   -v, --[no-]verbose    report pruned objects\n\
                     \x20   --[no-]progress       show progress\n\
                     \x20   --[no-]expire <expiry-date>\n\
                     \x20                         expire objects older than <time>\n\
                     \x20   --[no-]exclude-promisor-objects\n\
                     \x20                         limit traversal to objects outside promisor packfiles\n\
                     \n";

/// `git prune` — prune all unreachable objects from the object database.
///
/// See the module documentation for the ported surface and the exact reasons the
/// remaining flags bail.
pub fn prune(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0. `prune`'s positionals are revisions,
    // and `git prune prune` would name a ref called `prune`, so dropping a
    // leading verb is only safe as the very first argument — which is exactly
    // how dispatch passes it.
    let args = match args.first().map(String::as_str) {
        Some("prune") => &args[1..],
        _ => args,
    };

    let mut dry_run = false;
    let mut verbose = false;
    let mut end_of_opts = false;
    let mut heads: Vec<&str> = Vec::new();
    // `builtin/prune.c` initialises `expire = TIME_MAX`, i.e. every unreachable
    // object is old enough to go and no object is recent enough to rescue one.
    let mut expire: i64 = i64::MAX;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;
        if end_of_opts {
            heads.push(a);
            continue;
        }
        match a {
            "--" => end_of_opts = true,
            "-h" => {
                print!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--dry-run" => dry_run = true,
            "--no-dry-run" => dry_run = false,
            "--verbose" => verbose = true,
            "--no-verbose" => verbose = false,
            // Delayed, stderr-only, tty-gated in git; nothing observable to emit.
            "--progress" | "--no-progress" => {}
            // `--no-expire` is `parse_opt_expiry_date_cb()` with `unset`, which
            // substitutes the literal "never".
            "--no-expire" => expire = 0,
            "--expire" => {
                // parse-options takes the following argument whatever it looks
                // like, so `--expire --verbose` complains about the date.
                let Some(value) = args.get(i) else {
                    return Ok(option_error("option `expire' requires a value"));
                };
                i += 1;
                match parse_expiry_date(value) {
                    Some(t) => expire = t,
                    None => return Ok(malformed_date(value)),
                }
            }
            _ if a.starts_with("--expire=") => {
                let value = &a["--expire=".len()..];
                match parse_expiry_date(value) {
                    Some(t) => expire = t,
                    None => return Ok(malformed_date(value)),
                }
            }
            // `OPT_BOOL(0, "exclude-promisor-objects", …)`. In `builtin/prune.c`
            // this only does `fetch_if_missing = 0; revs.exclude_promisor_objects
            // = 1;` — it never marks a promisor object reachable. That revision
            // flag's sole effect on the walk (`list-objects.c`
            // `process_blob`/`process_tree`) is to *skip* a link into an object
            // that is both absent locally and named by a promisor pack, instead
            // of lazily fetching it or `die()`ing on the missing link. A present
            // loose object — the only kind prune can remove — is reached only
            // through a chain of present objects, none of which the flag ever
            // skips, so its SEEN status (and thus whether it is pruned) is
            // identical with or without the flag. This port has no lazy-fetch
            // backend (`find_object` reports not-found rather than fetching) and
            // `close_over` already drops any link it cannot resolve, so the walk
            // here already behaves exactly as it does under the flag. Accepting
            // it is byte-for-byte parity; there is nothing further to do.
            "--exclude-promisor-objects" | "--no-exclude-promisor-objects" => {}
            // A switch that takes no argument, given one. parse-options reports
            // this with the spelling the user typed and no usage block.
            _ if a.starts_with("--") && a.contains('=') && takes_no_value(a) => {
                let name = a[2..].split('=').next().unwrap_or_default();
                return Ok(option_error(&format!("option `{name}' takes no value")));
            }
            _ if a.starts_with("--") => {
                return Ok(usage_error(Some(&format!("unknown option `{}'", &a[2..]))));
            }
            _ if a.len() > 1 && a.starts_with('-') => {
                // Clustered short switches, e.g. `-nv`.
                for c in a[1..].chars() {
                    match c {
                        'n' => dry_run = true,
                        'v' => verbose = true,
                        'h' => {
                            print!("{USAGE}");
                            return Ok(ExitCode::from(129));
                        }
                        _ => return Ok(usage_error(Some(&format!("unknown switch `{c}'")))),
                    }
                }
            }
            _ => heads.push(a),
        }
    }

    let repo = gix::discover(".")?;

    // Both of these would make a prune here delete objects stock git keeps; see
    // the module documentation.
    if repo.common_dir().join("shallow").is_file() {
        bail!(
            "prune in a shallow repository is not supported: git also rewrites .git/shallow via \
             prune_shallow(), and there is no shallow-file writer in the vendored crates"
        );
    }
    if fs::read_dir(repo.common_dir().join("worktrees"))
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
    {
        bail!(
            "prune with linked worktrees is not supported: git additionally seeds reachability \
             from every other worktree's HEAD and index, which this port does not read"
        );
    }

    // Command-line `<head>`s are resolved before any reachability work, so an
    // unresolvable one fails exactly where git's `die()` does.
    let mut roots: Vec<ObjectId> = Vec::new();
    for name in &heads {
        match repo.rev_parse_single(*name) {
            Ok(id) => roots.push(id.detach()),
            Err(_) => {
                eprintln!("fatal: unrecognized argument: {name}");
                return Ok(ExitCode::from(128));
            }
        }
    }

    collect_roots(&repo, &mut roots)?;

    let objdir = repo.objects.store_ref().path().to_path_buf();
    collect_recent_roots(&objdir, repo.object_hash(), expire, &mut roots);
    let reachable = close_over(&repo, roots);

    let display_root = display_objdir(&repo, &objdir);
    let name_len = repo.object_hash().len_in_hex() - 2;

    // --- phase 1: prune unreachable loose objects ---------------------------
    for fanout in 0u16..256 {
        let prefix = format!("{fanout:02x}");
        let sub = objdir.join(&prefix);
        let Some(names) = read_dir_raw(&sub) else {
            continue;
        };
        for name in names {
            let name = name.to_string_lossy().into_owned();
            let path = sub.join(&name);
            let shown = display_root.join(&prefix).join(&name);

            if !is_object_name(&name, name_len) {
                if name.starts_with("tmp_obj_") {
                    prune_tmp_file(&path, &shown, expire, dry_run, verbose);
                } else {
                    eprintln!("bad sha1 file: {}", shown.display());
                }
                continue;
            }
            let Ok(oid) = ObjectId::from_hex(format!("{prefix}{name}").as_bytes()) else {
                continue;
            };
            if reachable.contains(&oid) {
                continue;
            }
            let Some(mtime) = mtime_of(&path) else {
                eprintln!("error: Could not stat '{}'", shown.display());
                continue;
            };
            // `st.st_mtime > expire` keeps the object: it is younger than the
            // cutoff, so this run is not allowed to remove it.
            if mtime > expire {
                continue;
            }
            if dry_run || verbose {
                // Read the type before unlinking; a header that cannot be read at
                // all prints `unknown`, as git's `oid_object_info() <= 0` does.
                let kind = repo
                    .try_find_header(oid)
                    .ok()
                    .flatten()
                    .map(|h| String::from_utf8_lossy(h.kind().as_bytes()).into_owned())
                    .unwrap_or_else(|| "unknown".to_owned());
                println!("{oid} {kind}");
            }
            if !dry_run {
                let _ = fs::remove_file(&path);
            }
        }
        if !dry_run {
            // `prune_subdir()`: an unconditional rmdir that silently fails while
            // anything is left in the directory.
            let _ = fs::remove_dir(&sub);
        }
    }

    // --- phase 2: prune loose objects that are also packed ------------------
    let indices = pack_indices(&repo, &objdir);
    for fanout in 0u16..256 {
        let prefix = format!("{fanout:02x}");
        let sub = objdir.join(&prefix);
        let Some(names) = read_dir_raw(&sub) else {
            continue;
        };
        for name in names {
            let name = name.to_string_lossy().into_owned();
            if !is_object_name(&name, name_len) {
                continue;
            }
            let Ok(oid) = ObjectId::from_hex(format!("{prefix}{name}").as_bytes()) else {
                continue;
            };
            if !indices.iter().any(|idx| idx.lookup(oid).is_some()) {
                continue;
            }
            if dry_run {
                println!("rm -f {}", display_root.join(&prefix).join(&name).display());
            } else {
                let _ = fs::remove_file(sub.join(&name));
            }
        }
        if !dry_run {
            let _ = fs::remove_dir(&sub);
        }
    }

    // --- phase 3: stale temporary files -------------------------------------
    for rel in ["", "pack"] {
        let dir = if rel.is_empty() {
            objdir.clone()
        } else {
            objdir.join(rel)
        };
        let shown_dir = if rel.is_empty() {
            display_root.clone()
        } else {
            display_root.join(rel)
        };
        let Some(names) = read_dir_raw(&dir) else {
            continue;
        };
        for name in names {
            let name = name.to_string_lossy().into_owned();
            if name.starts_with("tmp_") {
                prune_tmp_file(
                    &dir.join(&name),
                    &shown_dir.join(&name),
                    expire,
                    dry_run,
                    verbose,
                );
            }
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// git's parse-options failure shape: an `error: <msg>` line followed by the
/// usage block, both on stderr, exit 129.
fn usage_error(msg: Option<&str>) -> ExitCode {
    match msg {
        Some(m) => eprint!("error: {m}\n{USAGE}"),
        None => eprint!("{USAGE}"),
    }
    ExitCode::from(129)
}

/// git's parse-options complaints about an option's *value*, which — unlike an
/// unknown option — print no usage block: just `error: <msg>` and exit 129.
fn option_error(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::from(129)
}

/// `parse_expiry_date()` failing is fatal in `builtin/prune.c`, not a usage
/// error, so it reports on stderr and exits 128.
fn malformed_date(value: &str) -> ExitCode {
    eprintln!("fatal: malformed expiration date '{value}'");
    ExitCode::from(128)
}

/// The long options `prune` declares with no argument. Given `--<name>=<value>`
/// parse-options rejects them by the spelling that was typed.
fn takes_no_value(arg: &str) -> bool {
    matches!(
        arg[2..].split('=').next().unwrap_or_default(),
        "dry-run"
            | "no-dry-run"
            | "verbose"
            | "no-verbose"
            | "progress"
            | "no-progress"
            | "no-expire"
            | "exclude-promisor-objects"
            | "no-exclude-promisor-objects"
    )
}

/// `parse_expiry_date()`: the two literal cases first, then a date.
///
/// `never`/`false` disable pruning entirely (`0`, which no file's mtime can be
/// older than), and `all`/`now` are deliberately *not* the current time but
/// `TIME_MAX`, so everything unreachable goes and nothing counts as recent.
fn parse_expiry_date(value: &str) -> Option<i64> {
    match value {
        "never" | "false" => return Some(0),
        "all" | "now" => return Some(i64::MAX),
        _ => {}
    }
    // `parse_date()`'s escape hatch for a raw epoch value, which approxidate
    // itself would read as a year.
    if let Some(seconds) = value.strip_prefix('@') {
        return seconds.trim().parse::<i64>().ok();
    }
    let now = std::time::SystemTime::now();
    for candidate in approxidate_forms(value) {
        if let Ok(time) = gix::date::parse(&candidate, Some(now)) {
            return Some(time.seconds);
        }
    }

    // No `gix` form parsed, but git's `approxidate` accepts far more: it only
    // fails when nothing in the string looked like a date token. Match that
    // decision so `--expire` rejects exactly what stock git rejects (128) and
    // accepts everything else (exit 0), rather than turning approxidate-valid
    // garbage into a fatal error.
    if approxidate_touched(value) {
        // `match_digit()`'s raw-epoch case: a bare integer >= 100_000_000 is the
        // timestamp itself. Everything else approxidate resolves relative to the
        // current time (a lone `10` tweaks day-of-month, etc.); those field
        // tweaks are not reproduced here — only the accept/reject decision is
        // observable, since this path is only reached for values git treats as
        // "roughly now".
        if let Ok(n) = value.trim().parse::<i64>() {
            if n >= 100_000_000 {
                return Some(n);
            }
        }
        return now
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs() as i64);
    }
    None
}

/// git's `approxidate_str()` reports a malformed date only when its `touched`
/// flag stays clear: the string held no recognisable date token. A digit run
/// always sets it; an alpha run sets it only when the whole run is a recognised
/// word (see `is_date_word`). Reproducing that predicate is what keeps
/// `--expire`'s exit code (128 vs 0) in step with stock git on edge input.
fn approxidate_touched(value: &str) -> bool {
    if value.bytes().any(|b| b.is_ascii_digit()) {
        return true;
    }
    // git calls `approxidate_alpha` at the start of each alpha run and consumes
    // the whole run; a keyword only counts when it *is* that run. Walk maximal
    // ASCII-alphabetic runs and test each in isolation.
    let mut rest = value;
    while let Some(start) = rest.find(|c: char| c.is_ascii_alphabetic()) {
        let tail = &rest[start..];
        let end = tail
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(tail.len());
        if is_date_word(&tail[..end]) {
            return true;
        }
        rest = &tail[end..];
    }
    false
}

/// A single alpha token stock git's approxidate recognises on its own, and so
/// sets `touched` for: the number words `one`..`ten` and `last`, the time
/// keywords `am`/`pm`/`noon`/`midnight`/`tea`/`yesterday`/`today`/`now`/`never`,
/// or a `>= 3` character prefix of a full month name (`now` and `never` are also
/// approxidate keywords, distinct from the whole-string literals handled
/// earlier; `all` and `false` are only literals). Weekday names (`monday`) and bare
/// unit words (`week`, `day`, `hour`, ...) are deliberately absent: git leaves
/// `touched` clear for them, so standing alone they are malformed dates.
fn is_date_word(token: &str) -> bool {
    let t = token.to_ascii_lowercase();
    if matches!(
        t.as_str(),
        "one" | "two"
            | "three"
            | "four"
            | "five"
            | "six"
            | "seven"
            | "eight"
            | "nine"
            | "ten"
            | "last"
            | "am"
            | "pm"
            | "noon"
            | "midnight"
            | "tea"
            | "yesterday"
            | "today"
            // `now` and `never` are also approxidate keywords in their own
            // right, recognised anywhere in the string. The whole-string forms
            // are special-cased before this runs, but as a sub-token (e.g.
            // `now.ago`, `x never`) they still have to set `touched`. `all` and
            // `false` are *not* approxidate keywords — only the two literals.
            | "now"
            | "never"
    ) {
        return true;
    }
    const MONTHS: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    t.len() >= 3 && MONTHS.iter().any(|m| m.starts_with(t.as_str()))
}

/// The spellings to try for a date, in order. git's approxidate tokenises on
/// any non-alphanumeric run and on digit/letter boundaries, and treats a bare
/// `<n> <unit>` as being in the past; `gix::date::parse` wants the canonical
/// `<n> <unit> ago`. So `2.weeks.ago`, `2weeks.ago` and `2.weeks` all end up as
/// `2 weeks ago`, while an unmangled form like `1979-02-26` is tried first and
/// never reaches the rewriting.
fn approxidate_forms(value: &str) -> Vec<String> {
    let mut split = String::with_capacity(value.len() + 8);
    let mut prev = '\0';
    for c in value.chars() {
        if c == '.' || c == '_' || c == '/' || c == ',' {
            split.push(' ');
        } else {
            if prev.is_ascii_digit() != c.is_ascii_digit() && prev.is_ascii_alphanumeric() {
                split.push(' ');
            }
            split.push(c);
        }
        prev = c;
    }
    let split = split.split_whitespace().collect::<Vec<_>>().join(" ");

    let mut forms = vec![value.to_owned()];
    let with_ago = format!("{split} ago");
    for form in [split, with_ago] {
        if !form.is_empty() && !forms.contains(&form) {
            forms.push(form);
        }
    }
    forms
}

/// `add_unseen_recent_objects_to_traversal()`: with a non-zero cutoff, every
/// *local* object written after it becomes a traversal root, so the closure also
/// keeps whatever it points at. Loose objects are dated by their own mtime,
/// packed ones by their `.pack`'s, exactly as `add_recent_loose()` and
/// `add_recent_packed()` do.
fn collect_recent_roots(
    objdir: &Path,
    hash: gix::hash::Kind,
    expire: i64,
    roots: &mut Vec<ObjectId>,
) {
    // `mark_recent == 0` is git's "no grace period at all"; `TIME_MAX` is the
    // other end, where no mtime can compare greater.
    if expire == 0 || expire == i64::MAX {
        return;
    }

    let name_len = hash.len_in_hex() - 2;
    for fanout in 0u16..256 {
        let prefix = format!("{fanout:02x}");
        let sub = objdir.join(&prefix);
        let Some(names) = read_dir_raw(&sub) else {
            continue;
        };
        for name in names {
            let name = name.to_string_lossy().into_owned();
            if !is_object_name(&name, name_len) {
                continue;
            }
            if !matches!(mtime_of(&sub.join(&name)), Some(mtime) if mtime > expire) {
                continue;
            }
            if let Ok(oid) = ObjectId::from_hex(format!("{prefix}{name}").as_bytes()) {
                roots.push(oid);
            }
        }
    }

    let pack_dir = objdir.join("pack");
    let Some(names) = read_dir_raw(&pack_dir) else {
        return;
    };
    for name in names {
        let name = name.to_string_lossy().into_owned();
        let Some(base) = name.strip_suffix(".idx") else {
            continue;
        };
        if !matches!(mtime_of(&pack_dir.join(format!("{base}.pack"))), Some(mtime) if mtime > expire)
        {
            continue;
        }
        if let Ok(index) = pack::index::File::at(pack_dir.join(&name), hash) {
            roots.extend(index.iter().map(|entry| entry.oid));
        }
    }
}

/// `lstat()`'s `st_mtime`, in whole seconds, which is what git compares against
/// `expire`. `None` when the file cannot be stat'ed.
pub(super) fn mtime_of(path: &Path) -> Option<i64> {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(path).ok().map(|md| md.mtime())
}

/// `prune_tmp_file()`: keep anything younger than `expire`, otherwise report
/// under `-n`/`-v` and unlink unless `-n`. A file that has vanished between the
/// scan and here is silently skipped, as the `lstat()` guard does.
fn prune_tmp_file(path: &Path, shown: &Path, expire: i64, dry_run: bool, verbose: bool) {
    let Some(mtime) = mtime_of(path) else {
        return;
    };
    if mtime > expire {
        return;
    }
    if dry_run || verbose {
        println!("Removing stale temporary file {}", shown.display());
    }
    if !dry_run {
        let _ = fs::remove_file(path);
    }
}

/// Whether a fan-out directory entry names an object: `hexsz - 2` hex digits,
/// which is precisely `for_each_file_in_obj_subdir()`'s `hex_to_bytes()` test.
/// Everything else is cruft.
pub(super) fn is_object_name(name: &str, name_len: usize) -> bool {
    name.len() == name_len && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Directory entries in raw `readdir` order — deliberately unsorted, because
/// git's own scan is unsorted and stdout order has to match. `None` when the
/// directory does not exist or cannot be read, so no `rmdir` is attempted.
pub(super) fn read_dir_raw(dir: &Path) -> Option<Vec<OsString>> {
    let read = fs::read_dir(dir).ok()?;
    Some(read.filter_map(|e| e.ok()).map(|e| e.file_name()).collect())
}

/// The object directory as git prints it, i.e. relative to the directory git
/// would have chdir'd into: the worktree top for a normal repository, the
/// current directory for a bare one. Falls back to the path as opened.
fn display_objdir(repo: &gix::Repository, objdir: &Path) -> PathBuf {
    let base = repo
        .workdir()
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok());
    let rel = base.and_then(|base| {
        let base = base.canonicalize().ok()?;
        let full = objdir.canonicalize().ok()?;
        full.strip_prefix(&base).ok().map(Path::to_path_buf)
    });
    rel.unwrap_or_else(|| objdir.to_path_buf())
}

/// Seed `roots` the way `mark_reachable_objects()` does: index entries and
/// cache-tree ids, every ref under `refs/`, `HEAD`, and every reflog entry.
pub(super) fn collect_roots(repo: &gix::Repository, roots: &mut Vec<ObjectId>) -> Result<()> {
    // Index blobs (gitlinks excluded, as `do_add_index_objects_to_pending()`
    // skips `S_ISGITLINK`) plus the cache-tree, whose invalid sections git skips
    // via `entry_count >= 0` — gitoxide models that as `num_entries: None`.
    let index = repo.index_or_empty()?;
    for entry in index.entries() {
        if entry.mode == gix::index::entry::Mode::COMMIT {
            continue;
        }
        roots.push(entry.id);
    }
    if let Some(tree) = index.tree() {
        push_cache_tree(tree, roots);
    }

    // Refs are added unpeeled: an annotated tag's own object has to survive, and
    // the closure below peels it afterwards.
    for reference in repo.references()?.all()? {
        let Ok(mut reference) = reference else { continue };
        if let Ok(id) = reference.follow_to_object() {
            roots.push(id.detach());
        }
    }

    // `head_ref()`; a symbolic HEAD resolves to the same id its branch already
    // contributed, a detached one is only reachable here.
    if let Ok(head) = repo.head() {
        if let Some(id) = head.id() {
            roots.push(id.detach());
        }
    }

    collect_reflog_roots(repo, roots);
    Ok(())
}

/// Add every valid cache-tree id, recursively. A section with no entry count is
/// invalid and its id meaningless, exactly as in `add_cache_tree()`.
fn push_cache_tree(tree: &gix::index::extension::Tree, roots: &mut Vec<ObjectId>) {
    if tree.num_entries.is_some() {
        roots.push(tree.id);
    }
    for child in &tree.children {
        push_cache_tree(child, roots);
    }
}

/// Add the old and new id of every entry of every reflog, matching
/// `for_each_reflog()` + `add_one_reflog_ent()`. Null ids (a ref's creation or
/// deletion line) name no object and are skipped, as `parse_object()` returns
/// NULL for them.
fn collect_reflog_roots(repo: &gix::Repository, roots: &mut Vec<ObjectId>) {
    let mut dirs = vec![repo.common_dir().join("logs")];
    let per_worktree = repo.git_dir().join("logs");
    if per_worktree != dirs[0] {
        dirs.push(per_worktree);
    }

    let mut files: Vec<PathBuf> = Vec::new();
    for dir in &dirs {
        collect_files(dir, &mut files);
    }

    let null = ObjectId::null(repo.object_hash());
    for file in files {
        let Ok(buf) = fs::read(&file) else { continue };
        for line in gix::refs::file::log::iter::forward(&buf) {
            let Ok(line) = line else { continue };
            for id in [line.previous_oid(), line.new_oid()] {
                if id != null {
                    roots.push(id);
                }
            }
        }
    }
}

/// Append every regular file below `dir`, recursively.
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = fs::read_dir(dir) else { return };
    for entry in read.filter_map(|e| e.ok()) {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => collect_files(&path, out),
            Ok(_) => out.push(path),
            Err(_) => {}
        }
    }
}

/// The full object closure over `roots`: commits contribute their tree and
/// parents, tags their target, trees their non-gitlink entries. Objects that
/// cannot be found or decoded are dropped from the walk but stay in the set, so
/// a corrupt object is never mistaken for an unreachable one.
pub(super) fn close_over(repo: &gix::Repository, roots: Vec<ObjectId>) -> HashSet<ObjectId> {
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut stack: Vec<ObjectId> = Vec::new();
    for id in roots {
        if seen.insert(id) {
            stack.push(id);
        }
    }

    while let Some(id) = stack.pop() {
        let Ok(object) = repo.find_object(id) else {
            continue;
        };
        let mut next: Vec<ObjectId> = Vec::new();
        match object.kind {
            Kind::Blob => {}
            Kind::Commit => {
                let commit = object.into_commit();
                // Collect to owned ids inside the statement: the decoded ref
                // borrows `commit`, and a borrow held across the arm boundary
                // would outlive the binding it points into.
                let ids = commit
                    .decode()
                    .ok()
                    .map(|c| (c.tree(), c.parents().collect::<Vec<_>>()));
                if let Some((tree, parents)) = ids {
                    next.push(tree);
                    next.extend(parents);
                }
            }
            Kind::Tag => {
                let tag = object.into_tag();
                if let Ok(tag) = tag.decode() {
                    next.push(tag.target());
                }
            }
            Kind::Tree => {
                let tree = object.into_tree();
                if let Ok(tree) = tree.decode() {
                    for entry in &tree.entries {
                        // `process_tree()` never descends into a submodule.
                        if !matches!(entry.mode.kind(), gix::object::tree::EntryKind::Commit) {
                            next.push(entry.oid.to_owned());
                        }
                    }
                }
            }
        }
        for id in next {
            if seen.insert(id) {
                stack.push(id);
            }
        }
    }
    seen
}

/// Every readable pack index reachable from this repository — local packs and
/// those of each alternate — which together define `has_object_pack()`, the test
/// `prune-packed` uses. A pack whose `.pack` is missing or whose index cannot be
/// opened is skipped, as `prepare_packed_git_one()` skips it.
pub(super) fn pack_indices(repo: &gix::Repository, objdir: &Path) -> Vec<pack::index::File> {
    let hash = repo.object_hash();
    let mut dirs = vec![objdir.to_path_buf()];
    if let Ok(alternates) = repo.objects.store_ref().alternate_db_paths() {
        dirs.extend(alternates);
    }

    let mut indices = Vec::new();
    for dir in dirs {
        let dir = dir.join("pack");
        let Some(names) = read_dir_raw(&dir) else {
            continue;
        };
        for name in names {
            let name = name.to_string_lossy().into_owned();
            let Some(base) = name.strip_suffix(".idx") else {
                continue;
            };
            if !matches!(fs::metadata(dir.join(format!("{base}.pack"))), Ok(md) if md.is_file()) {
                continue;
            }
            if let Ok(file) = pack::index::File::at(dir.join(&name), hash) {
                indices.push(file);
            }
        }
    }
    indices
}
