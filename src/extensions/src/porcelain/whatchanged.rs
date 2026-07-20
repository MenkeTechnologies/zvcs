//! `git whatchanged` — commit history with the raw diff each commit introduces.
//!
//! Stock git documents the command as exactly `git log --raw --no-merges`, and as of
//! git 2.47 it is deprecated: it refuses to run at all unless `--i-still-use-this` is
//! passed. Both halves are ported here.
//!
//! ### Covered (byte-identical stdout and exit code against stock git 2.55.0)
//!
//! * No `--i-still-use-this`: the 702-byte deprecation notice on stderr, empty stdout,
//!   exit 128. This is the whole behaviour of the stock command on modern git, so it
//!   is the path most callers actually hit.
//! * `--i-still-use-this`: the `medium` commit header (`commit`/`Author:`/`Date:` plus
//!   the four-space-indented message) followed by a blank line and the recursive
//!   `--raw` change list, newest commit first by commit date, commits separated by a
//!   blank line.
//! * Merge commits are skipped entirely (`--no-merges`), and — unlike `git log --raw`,
//!   which sets `always_show_header` — a commit whose diff is empty prints nothing and
//!   does not consume a `--max-count` slot. Both match `cmd_whatchanged`.
//! * A root commit is diffed against the empty tree, so it lists its whole tree as
//!   additions.
//! * `-n <n>` / `-n<n>` / `-<n>` / `--max-count[=]<n>`, `--no-renames`, and a single
//!   `<rev>` (default `HEAD`).
//! * Object ids are abbreviated the way git's `diff_aligned_abbrev` does: `core.abbrev`
//!   when set, otherwise an auto width floored at 7, extended per id until unambiguous;
//!   an absent side renders as that many `0`s.
//!
//! ### Honest limitations (bailed on with a precise message, never silently ignored)
//!
//! * **Rename detection.** git's `diff.renames` defaults to on, so a commit that both
//!   adds and deletes files gets `R<score>` lines in a queue order produced by
//!   `diffcore_rename`. The vendored `gix-diff` rewrite tracker computes similarity
//!   from a line-based blob diff (`rewrites/tracker.rs`, `(old_len - removed_bytes) /
//!   max(old_len, new_len)`) rather than git's 64-byte spanhash `src_copied`, does not
//!   expose an integer score on `ChangeRef::Rewrite` at all, and emits changes in
//!   tree-walk order rather than git's rename-queue order. Neither the `R<score>`
//!   digits nor the line ordering could be reproduced, so when rename detection is
//!   active *and* a commit's diff contains both an addition and a deletion, this bails
//!   instead of printing plausible-looking wrong lines. `--no-renames` (or
//!   `diff.renames=false`) makes every commit reproducible.
//! * `-p`/`--patch`, `--stat` and friends, `--pretty`/`--format`, `--graph`, pathspec
//!   filtering, date/author filters, multiple revisions, `-M`/`-C`, and `-h`.
//! * The auto abbreviation width is derived from gix's *packed* object count; git also
//!   estimates loose objects, so the two can differ by a hex digit in a repository with
//!   many loose objects and no pack.
//! * `i18n.commitEncoding` / the commit `encoding` header is not applied; the message
//!   bytes are passed through as stored.

use anyhow::{anyhow, bail, Result};
use std::cmp::Ordering;
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BStr, BString, ByteSlice};
use gix::hash::ObjectId;
use gix::objs::tree::EntryMode;
use gix::prelude::ObjectIdExt;
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;

/// Stock git's deprecation notice, byte-for-byte (702 bytes). Written to stderr, with
/// nothing on stdout, when `--i-still-use-this` is absent; exit code 128.
const DEPRECATION: &str = concat!(
    "'git whatchanged' is nominated for removal.\n",
    "\n",
    "hint: You can replace 'git whatchanged <opts>' with:\n",
    "hint:\tgit log <opts> --raw --no-merges\n",
    "hint: Or make an alias:\n",
    "hint:\tgit config set --global alias.whatchanged 'log --raw --no-merges'\n",
    "\n",
    "If you still use this command, here's what you can do:\n",
    "\n",
    "- read https://git-scm.com/docs/BreakingChanges.html\n",
    "- check if anyone has discussed this on the mailing\n",
    "  list and if they came up with something that can\n",
    "  help you: https://lore.kernel.org/git/?q=git%20whatchanged\n",
    "- send an email to <git@vger.kernel.org> to let us\n",
    "  know that you still use this command and were unable\n",
    "  to determine a suitable replacement\n",
    "\n",
    "fatal: refusing to run without --i-still-use-this\n",
);

/// The `S_IFMT` mask git uses to tell a *type* change (`T`) from a plain modification
/// (`M`); `100644` and `100755` share a type, `120000` and `160000` do not.
const IFMT: u16 = 0o170000;

/// git's `MINIMUM_ABBREV`, the floor `core.abbrev` is clamped to.
const MINIMUM_ABBREV: usize = 4;

/// git's `FALLBACK_DEFAULT_ABBREV`, the floor of the auto-computed width.
const FALLBACK_ABBREV: usize = 7;

/// One side of a change: absent (`None`) means the path was added or deleted.
#[derive(Clone, Copy)]
struct Side {
    mode: EntryMode,
    id: ObjectId,
}

/// A single blob-level change, in the shape the `--raw` line needs.
struct Change {
    old: Option<Side>,
    new: Option<Side>,
    path: BString,
}

/// A tree entry, materialised so the borrow on the tree's buffer ends before we recurse.
struct Entry {
    mode: EntryMode,
    name: BString,
    id: ObjectId,
}

/// `git whatchanged` — see the module documentation for the covered surface.
pub fn whatchanged(args: &[String]) -> Result<ExitCode> {
    // Dispatch passes the subcommand itself at index 0.
    let args = match args.first().map(String::as_str) {
        Some("whatchanged") => &args[1..],
        _ => args,
    };

    // git runs the deprecation check inside `cmd_whatchanged`, i.e. after repository
    // setup, so a missing repository is still reported first.
    let repo = gix::discover(".")?;

    let mut opted_in = false;
    let mut no_renames = false;
    let mut max_count: Option<usize> = None;
    let mut rev: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--i-still-use-this" => opted_in = true,
            "--no-renames" => no_renames = true,
            "-n" | "--max-count" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| anyhow!("option `{a}` requires a value"))?;
                max_count = Some(parse_count(a, v)?);
            }
            "--" => {
                if i + 1 < args.len() {
                    bail!("pathspec filtering is not ported");
                }
            }
            s if s.starts_with("--max-count=") => {
                max_count = Some(parse_count("--max-count", &s["--max-count=".len()..])?);
            }
            s if s.starts_with('-') => {
                let body = &s[1..];
                if let Some(num) = body.strip_prefix('n') {
                    // `-nN` shorthand.
                    max_count = Some(parse_count("-n", num)?);
                } else if !body.is_empty() && body.bytes().all(|c| c.is_ascii_digit()) {
                    // `-N` shorthand.
                    max_count = Some(parse_count(s, body)?);
                } else {
                    bail!(
                        "unsupported flag {s:?} (ported: --i-still-use-this, --no-renames, \
                         -n/--max-count/-nN/-N)"
                    );
                }
            }
            s => {
                if rev.is_some() {
                    bail!("multiple revisions are not ported");
                }
                rev = Some(s.to_string());
            }
        }
        i += 1;
    }

    if !opted_in {
        eprint!("{DEPRECATION}");
        return Ok(ExitCode::from(128));
    }

    // Resolve the starting tip. A bare `HEAD` may be unborn, which git reports as a
    // fatal error rather than as empty output.
    let tip = match &rev {
        Some(spec) => repo.rev_parse_single(spec.as_str())?.detach(),
        None => match repo.head()?.try_peel_to_id()? {
            Some(id) => id.detach(),
            None => bail!("your current branch does not have any commits yet"),
        },
    };

    let renames = !no_renames && renames_enabled(&repo);
    let abbrev = base_abbrev(&repo)?;

    // Newest-first by commit date, the default `git log` ordering.
    let walk = repo
        .rev_walk([tip])
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
        .all()?;

    let limit = max_count.unwrap_or(usize::MAX);
    let mut out: Vec<u8> = Vec::new();
    let mut shown = 0usize;

    for info in walk {
        if shown >= limit {
            break;
        }
        let commit = info?.object()?;

        // `--no-merges`: a merge is dropped from the output, but the walk still
        // traverses through it, and it never consumes a `--max-count` slot.
        let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
        if parents.len() > 1 {
            continue;
        }

        let new_tree = commit.tree_id()?.detach();
        let old_tree = match parents.first() {
            Some(p) => Some(repo.find_object(*p)?.peel_to_tree()?.id),
            None => None, // root commit: diff against the empty tree
        };

        let mut changes: Vec<Change> = Vec::new();
        walk_trees(&repo, old_tree, Some(new_tree), BStr::new(""), &mut changes)?;

        // `cmd_whatchanged` leaves `always_show_header` off, so a commit that produced
        // no diff prints nothing at all and git restores the `--max-count` it spent.
        if changes.is_empty() {
            continue;
        }

        // git would run `diffcore_rename` over this pair set; see the module docs for
        // why that cannot be reproduced byte-identically from the vendored crates.
        if renames
            && changes.iter().any(|c| c.old.is_none())
            && changes.iter().any(|c| c.new.is_none())
        {
            bail!(
                "commit {} both adds and deletes paths, so git's rename detection would \
                 emit R<score> lines; the vendored gix-diff exposes no diffcore-rename \
                 score or queue order (re-run with --no-renames, or set diff.renames=false)",
                commit.id()
            );
        }

        if shown > 0 {
            out.push(b'\n');
        }
        render_commit(&repo, &commit, &changes, abbrev, &mut out)?;
        shown += 1;
    }

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&out)?;
    stdout.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Parse a positive commit count, with a git-shaped error.
fn parse_count(flag: &str, value: &str) -> Result<usize> {
    value
        .parse::<usize>()
        .map_err(|_| anyhow!("invalid number {value:?} for {flag}"))
}

/// Whether git would run rename detection: `diff.renames` defaults to on, and only an
/// explicit false value turns it off (`copies`/`copy` turn it *up*, not off).
fn renames_enabled(repo: &gix::Repository) -> bool {
    match repo.config_snapshot().string("diff.renames") {
        None => true,
        Some(v) => !matches!(
            v.to_str_lossy().to_ascii_lowercase().as_str(),
            "false" | "no" | "off" | "0"
        ),
    }
}

/// The base abbreviation width for `--raw` object ids.
///
/// `core.abbrev` when set (clamped to `MINIMUM_ABBREV..=hexsz`, with `no`/`off`/`false`
/// meaning the full id), otherwise git's auto width: half the bit-length of the object
/// count, rounded up, floored at `FALLBACK_ABBREV`. Individual ids may be rendered
/// longer than this when they need it to stay unambiguous; the all-zero id of an absent
/// side is always rendered at exactly this width, as `diff_aligned_abbrev` does.
fn base_abbrev(repo: &gix::Repository) -> Result<usize> {
    let hexsz = repo.object_hash().len_in_hex();
    if let Some(v) = repo.config_snapshot().string("core.abbrev") {
        match v.to_str_lossy().as_ref() {
            "auto" => {}
            "no" | "off" | "false" => return Ok(hexsz),
            other => {
                let n: usize = other
                    .parse()
                    .map_err(|_| anyhow!("Invalid value for 'core.abbrev' = '{other}'"))?;
                return Ok(n.clamp(MINIMUM_ABBREV, hexsz));
            }
        }
    }
    let count = repo.objects.packed_object_count()?;
    let bits = u64::BITS - count.leading_zeros();
    Ok((bits.div_ceil(2) as usize).max(FALLBACK_ABBREV))
}

/// Render one commit: the `medium` header, its message, a blank line, then the raw
/// change lines. No `Merge:` line is ever needed because merges are filtered out.
fn render_commit(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    changes: &[Change],
    abbrev: usize,
    out: &mut Vec<u8>,
) -> Result<()> {
    let author = commit.author()?;
    let time = author.time()?;

    out.extend_from_slice(format!("commit {}\n", commit.id()).as_bytes());
    out.extend_from_slice(b"Author: ");
    out.extend_from_slice(&author.name[..]);
    out.extend_from_slice(b" <");
    out.extend_from_slice(&author.email[..]);
    out.extend_from_slice(b">\n");
    out.extend_from_slice(
        format!("Date:   {}\n\n", format_git_date(time.seconds, time.offset)).as_bytes(),
    );

    // git's `pp_remainder`: leading blank lines are dropped, every remaining line gets a
    // four-space indent, and an all-whitespace line keeps only that indent.
    let raw = commit.message_raw()?;
    let bytes: &[u8] = &raw[..];
    let mut lines: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop(); // the newline terminating the last line, not an extra blank one
    }
    let mut started = false;
    for line in lines {
        let blank = line.iter().all(u8::is_ascii_whitespace);
        if blank && !started {
            continue;
        }
        started = true;
        out.extend_from_slice(b"    ");
        if !blank {
            out.extend_from_slice(line);
        }
        out.push(b'\n');
    }
    out.push(b'\n');

    for c in changes {
        render_raw(repo, c, abbrev, out)?;
    }
    Ok(())
}

/// `:<omode> <nmode> <ooid> <noid> <status>\t<path>` — git's raw diff line.
fn render_raw(repo: &gix::Repository, c: &Change, abbrev: usize, out: &mut Vec<u8>) -> Result<()> {
    let zeros = "0".repeat(abbrev);
    let (omode, ooid) = match c.old {
        Some(s) => (s.mode.value(), short(repo, s.id, abbrev)?),
        None => (0, zeros.clone()),
    };
    let (nmode, noid) = match c.new {
        Some(s) => (s.mode.value(), short(repo, s.id, abbrev)?),
        None => (0, zeros),
    };
    out.extend_from_slice(format!(":{omode:06o} {nmode:06o} {ooid} {noid} ").as_bytes());
    out.push(status(c));
    out.push(b'\t');
    out.extend_from_slice(&c.path);
    out.push(b'\n');
    Ok(())
}

/// git's `diff_abbrev_oid`: the id shortened to the configured width, extended until it
/// is unambiguous. `gix::Id::shorten` derives the same width from `core.abbrev` (or the
/// same auto formula) and performs the same disambiguation.
fn short(repo: &gix::Repository, id: ObjectId, abbrev: usize) -> Result<String> {
    if abbrev >= repo.object_hash().len_in_hex() {
        return Ok(id.to_hex().to_string());
    }
    Ok(id.attach(repo).shorten()?.to_string())
}

/// The status letter git prints for a change.
fn status(c: &Change) -> u8 {
    match (c.old, c.new) {
        (None, _) => b'A',
        (_, None) => b'D',
        (Some(o), Some(n)) => {
            if o.mode.value() & IFMT != n.mode.value() & IFMT {
                b'T'
            } else {
                b'M'
            }
        }
    }
}

/// Read the entries of `id` in stored (git-sorted) order; `None` is the empty tree.
fn read_entries(repo: &gix::Repository, id: Option<ObjectId>) -> Result<Vec<Entry>> {
    let Some(id) = id else { return Ok(Vec::new()) };
    let tree = repo.find_tree(id)?;
    Ok(tree
        .decode()?
        .entries
        .iter()
        .map(|e| Entry {
            mode: e.mode,
            name: BString::from(e.filename.to_vec()),
            id: e.oid.to_owned(),
        })
        .collect())
}

/// git's `tree-entry-comparison`: names compare byte-wise with an implicit `/` appended
/// to tree entries, so a blob and a tree of the same name never compare `Equal`.
fn entry_cmp(a: &Entry, b: &Entry) -> Ordering {
    let common = a.name.len().min(b.name.len());
    match a.name[..common].cmp(&b.name[..common]) {
        Ordering::Equal => {
            let ac = a
                .name
                .get(common)
                .copied()
                .or(a.mode.is_tree().then_some(b'/'));
            let bc = b
                .name
                .get(common)
                .copied()
                .or(b.mode.is_tree().then_some(b'/'));
            ac.cmp(&bc)
        }
        other => other,
    }
}

/// Depth-first merge-walk of two trees rooted at `prefix`, collecting blob-level
/// changes. `--raw` in the log family is always recursive and never reports the tree
/// entries themselves, so this always descends and only ever pushes non-tree entries.
fn walk_trees(
    repo: &gix::Repository,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    prefix: &BStr,
    out: &mut Vec<Change>,
) -> Result<()> {
    let lhs = read_entries(repo, old)?;
    let rhs = read_entries(repo, new)?;
    let (mut i, mut j) = (0usize, 0usize);

    while i < lhs.len() || j < rhs.len() {
        let order = match (lhs.get(i), rhs.get(j)) {
            (Some(a), Some(b)) => entry_cmp(a, b),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => unreachable!("loop condition guarantees one side has an entry"),
        };
        match order {
            Ordering::Equal => {
                let (a, b) = (&lhs[i], &rhs[j]);
                i += 1;
                j += 1;
                if a.mode == b.mode && a.id == b.id {
                    continue;
                }
                let path = join(prefix, a.name.as_bstr());
                // `Equal` implies both sides are trees or neither is.
                if a.mode.is_tree() {
                    walk_trees(repo, Some(a.id), Some(b.id), path.as_bstr(), out)?;
                } else {
                    out.push(Change {
                        old: Some(side(a)),
                        new: Some(side(b)),
                        path,
                    });
                }
            }
            Ordering::Less => {
                let a = &lhs[i];
                i += 1;
                let path = join(prefix, a.name.as_bstr());
                if a.mode.is_tree() {
                    walk_trees(repo, Some(a.id), None, path.as_bstr(), out)?;
                } else {
                    out.push(Change {
                        old: Some(side(a)),
                        new: None,
                        path,
                    });
                }
            }
            Ordering::Greater => {
                let b = &rhs[j];
                j += 1;
                let path = join(prefix, b.name.as_bstr());
                if b.mode.is_tree() {
                    walk_trees(repo, None, Some(b.id), path.as_bstr(), out)?;
                } else {
                    out.push(Change {
                        old: None,
                        new: Some(side(b)),
                        path,
                    });
                }
            }
        }
    }
    Ok(())
}

fn side(e: &Entry) -> Side {
    Side {
        mode: e.mode,
        id: e.id,
    }
}

fn join(prefix: &BStr, name: &BStr) -> BString {
    let mut p = BString::from(prefix.to_vec());
    if !p.is_empty() {
        p.push(b'/');
    }
    p.extend_from_slice(name);
    p
}

/// Format a commit time exactly like git's default `DATE_NORMAL`:
/// `Www Mmm <sp-padded-day> HH:MM:SS YYYY +ZZZZ`, in the commit's own offset. Done by
/// hand because gix's exported `DEFAULT` format uses an unpadded day (`%-d`) where git
/// space-pads it (`%e`), and the crate exposes no custom format string.
fn format_git_date(seconds: i64, offset: i32) -> String {
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    // Shift into the commit's local wall-clock time, then split into whole days since
    // the Unix epoch and seconds within the day. `div_euclid`/`rem_euclid` keep the
    // split correct for pre-1970 (negative) timestamps.
    let local = seconds + offset as i64;
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (hour, min, sec) = (secs / 3600, (secs % 3600) / 60, secs % 60);

    // 1970-01-01 (day 0) was a Thursday, index 4 with Sunday = 0.
    let weekday = ((days.rem_euclid(7)) + 4).rem_euclid(7) as usize;
    let (year, month, day) = civil_from_days(days);

    let (sign, off) = if offset < 0 { ('-', -offset) } else { ('+', offset) };
    let (off_h, off_m) = (off / 3600, (off % 3600) / 60);

    format!(
        "{} {} {:>2} {:02}:{:02}:{:02} {} {}{:02}{:02}",
        WEEKDAYS[weekday],
        MONTHS[(month - 1) as usize],
        day,
        hour,
        min,
        sec,
        year,
        sign,
        off_h,
        off_m,
    )
}

/// Convert a day count since the Unix epoch into a civil `(year, month, day)`, month and
/// day 1-based. Howard Hinnant's `civil_from_days`, exact over the representable range.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if month <= 2 { year + 1 } else { year }, month as u32, day)
}
