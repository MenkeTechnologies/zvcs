use anyhow::Result;
use std::collections::BTreeMap;
use std::process::ExitCode;

use gix::bstr::BString;
use gix::hash::ObjectId;

/// The exact usage block stock `git status` prints on a usage error (exit 129).
const USAGE: &str = "usage: git status [<options>] [--] [<pathspec>...]

    -v, --[no-]verbose    be verbose
    -s, --[no-]short      show status concisely
    -b, --[no-]branch     show branch information
    --[no-]show-stash     show stash information
    --[no-]ahead-behind   compute full ahead/behind values
    --[no-]porcelain[=<version>]
                          machine-readable output
    --[no-]long           show status in long format (default)
    -z, --[no-]null       terminate entries with NUL
    -u, --[no-]untracked-files[=<mode>]
                          show untracked files, optional modes: all, normal, no. (Default: all)
    --[no-]ignored[=<mode>]
                          show ignored files, optional modes: traditional, matching, no. (Default: traditional)
    --[no-]ignore-submodules[=<when>]
                          ignore changes to submodules, optional when: all, dirty, untracked. (Default: all)
    --[no-]column[=<style>]
                          list untracked files in columns
    --no-renames          do not detect renames
    --renames             opposite of --no-renames
    -M, --find-renames[=<n>]
                          detect renames, optionally set similarity index
";

/// How untracked files are reported, mirroring git's `--untracked-files` modes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Untracked {
    /// `-uno` — no directory walk at all.
    No,
    /// `-unormal` (git's default) — collapse wholly-untracked directories.
    Normal,
    /// `-uall` — list every untracked file individually.
    All,
}

/// `git status` — working-tree status vs the index and `HEAD`.
///
/// Backed entirely by gitoxide's `Repository::status()` platform, which fans a
/// tree↔index diff (the staged changes) and an index↔worktree diff (the
/// unstaged changes plus the directory walk for untracked and ignored files)
/// into a single iterator. From those items we reconstruct git's own output.
///
/// Supported invocations (output byte-for-byte matches stock `git status`):
///   * `git status`                      — default long format.
///   * `git status -s|--short`           — short format.
///   * `git status --porcelain[=v1]`     — porcelain v1.
///   * `git status -b|--branch`          — the `## <branch>...<upstream> [ahead N, behind M]`
///                                          short-format header.
///   * `git status -u<mode>`             — all three `--untracked-files` modes.
///   * `git status --ignored[=<mode>]`   — the `!!` / `Ignored files:` listing.
///   * `git status --no-renames | --renames | -M | --find-renames[=<n>]`.
///   * unmerged (conflicted) paths, in both long and short form.
///
/// Faithfully unsupported cases `bail!` with a precise reason rather than
/// emitting wrong output: `--porcelain=v2`, `-z`, intent-to-add entries, and
/// pathspec-limited status.
pub fn status(args: &[String]) -> Result<ExitCode> {
    let mut short = false;
    let mut branch_header = false;
    // `None` until a flag names a mode, so `status.showUntrackedFiles` still wins
    // when the caller stays silent, exactly as git resolves it.
    let mut untracked_flag: Option<Untracked> = None;
    let mut show_ignored = false;
    // `None` keeps git's configured default (`status.renames`/`diff.renames`).
    let mut renames: Option<Option<gix::diff::Rewrites>> = None;

    for a in args {
        let s = a.as_str();
        match s {
            "-s" | "--short" => short = true,
            "--porcelain" | "--porcelain=v1" | "--porcelain=1" => short = true,
            "--long" => short = false,
            "--porcelain=v2" | "--porcelain=2" => {
                anyhow::bail!("porcelain v2 format is not supported")
            }
            "-z" | "--null" => anyhow::bail!("NUL-terminated output (-z) is not supported"),
            "-b" | "--branch" => branch_header = true,
            "--no-branch" => branch_header = false,
            "--ignored" | "--ignored=traditional" | "--ignored=matching" => show_ignored = true,
            "--ignored=no" | "--no-ignored" => show_ignored = false,
            "-u" | "--untracked-files" | "-uall" | "--untracked-files=all" => {
                untracked_flag = Some(Untracked::All);
            }
            "-uno" | "--untracked-files=no" | "--no-untracked-files" => {
                untracked_flag = Some(Untracked::No);
            }
            "-unormal" | "--untracked-files=normal" => {
                untracked_flag = Some(Untracked::Normal);
            }
            // Everything after `--` is a pathspec; the pathspec arm below rejects
            // any that follow, and a trailing `--` on its own is a no-op.
            "--" => {}
            "--no-renames" => renames = Some(None),
            "--renames" | "-M" | "--find-renames" => {
                renames = Some(Some(gix::diff::Rewrites::default()));
            }
            // git validates the `--porcelain=<version>` value as it parses, dying
            // immediately (exit 128) on anything but v1/v2 — a later valid
            // `--porcelain=v1` does not rescue an earlier bad version.
            _ if s.starts_with("--porcelain=") => {
                let version = &s["--porcelain=".len()..];
                eprintln!("fatal: unsupported porcelain version '{version}'");
                return Ok(ExitCode::from(128));
            }
            _ if s.starts_with("--untracked-files=") => {
                let mode = &s["--untracked-files=".len()..];
                match parse_untracked_mode(mode) {
                    Some(m) => untracked_flag = Some(m),
                    None => {
                        eprintln!("fatal: Invalid untracked files mode '{mode}'");
                        return Ok(ExitCode::from(128));
                    }
                }
            }
            _ if s.starts_with("--ignored=") => {
                let mode = &s["--ignored=".len()..];
                eprintln!("fatal: Invalid ignored mode '{mode}'");
                return Ok(ExitCode::from(128));
            }
            _ if s.starts_with("--find-renames=") || s.starts_with("-M") => {
                let raw = s
                    .strip_prefix("--find-renames=")
                    .unwrap_or_else(|| s.trim_start_matches("-M"));
                match parse_similarity(raw) {
                    Some(rewrites) => renames = Some(Some(rewrites)),
                    None => {
                        eprintln!("error: unknown option `{}'", s.trim_start_matches('-'));
                        eprint!("{USAGE}");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            _ if s.starts_with("--") => {
                eprintln!("error: unknown option `{}'", &s[2..]);
                eprint!("{USAGE}");
                return Ok(ExitCode::from(129));
            }
            // A cluster of short flags, e.g. `-sb`. `-u` and `-M` swallow the
            // remainder of the argument as their optional value, as git does.
            _ if s.starts_with('-') && s.len() > 1 => {
                let mut chars = s[1..].chars();
                while let Some(c) = chars.next() {
                    let rest = chars.as_str();
                    match c {
                        's' => short = true,
                        'b' => branch_header = true,
                        'v' => {}
                        'z' => anyhow::bail!("NUL-terminated output (-z) is not supported"),
                        'u' => {
                            // A bare `-u` (no attached value) is git's `all` default;
                            // an attached value parses exactly as `--untracked-files=`.
                            untracked_flag = Some(if rest.is_empty() {
                                Untracked::All
                            } else {
                                match parse_untracked_mode(rest) {
                                    Some(m) => m,
                                    None => {
                                        eprintln!("fatal: Invalid untracked files mode '{rest}'");
                                        return Ok(ExitCode::from(128));
                                    }
                                }
                            });
                            break;
                        }
                        'M' => {
                            match parse_similarity(rest) {
                                Some(rewrites) => renames = Some(Some(rewrites)),
                                None => {
                                    eprintln!("error: unknown option `{}'", &s[1..]);
                                    eprint!("{USAGE}");
                                    return Ok(ExitCode::from(129));
                                }
                            }
                            break;
                        }
                        other => {
                            eprintln!("error: unknown switch `{other}'");
                            eprint!("{USAGE}");
                            return Ok(ExitCode::from(129));
                        }
                    }
                }
            }
            _ => anyhow::bail!("pathspec-limited status ({a:?}) is not supported"),
        }
    }

    let repo = gix::discover(".")?;

    // Resolve the head into an owned description so the borrow ends before we
    // re-open references for the tracking computation.
    let head = repo.head()?;
    let unborn = head.is_unborn();
    let head_state = if unborn {
        HeadState::Unborn(referent_short(head.referent_name(), "main"))
    } else if head.is_detached() {
        let short_id = head
            .id()
            .map(|id| id.shorten_or_id().to_string())
            .unwrap_or_default();
        HeadState::Detached(short_id)
    } else {
        HeadState::Branch(referent_short(head.referent_name(), "HEAD"))
    };
    drop(head);

    // `MERGE_HEAD` is what makes git treat the run as "from merge": it both
    // enables the in-progress banner and suppresses the unstage hint.
    let merging = repo.git_dir().join("MERGE_HEAD").exists();

    let untracked = untracked_flag.unwrap_or_else(|| configured_untracked(&repo));

    // Collect the four change classes from the unified status iterator.
    let mut staged: Vec<(StageKind, BString, Option<BString>)> = Vec::new();
    let mut unstaged: Vec<(WorkKind, BString)> = Vec::new();
    let mut unmerged: Vec<(u8, BString)> = Vec::new();
    let mut untracked_paths: Vec<BString> = Vec::new();
    let mut ignored_paths: Vec<BString> = Vec::new();

    let mut platform = repo
        .status(gix::progress::Discard)?
        .untracked_files(match untracked {
            Untracked::No => gix::status::UntrackedFiles::None,
            Untracked::Normal => gix::status::UntrackedFiles::Collapsed,
            Untracked::All => gix::status::UntrackedFiles::Files,
        });
    if show_ignored {
        // git lists ignored entries at the same granularity as untracked ones.
        let mode = if untracked == Untracked::All {
            gix::dir::walk::EmissionMode::Matching
        } else {
            gix::dir::walk::EmissionMode::CollapseDirectory
        };
        platform = platform.dirwalk_options(|opts| opts.emit_ignored(Some(mode)));
    }
    if let Some(rewrites) = renames {
        platform = platform.tree_index_track_renames(match rewrites {
            Some(r) => gix::status::tree_index::TrackRenames::Given(r),
            None => gix::status::tree_index::TrackRenames::Disabled,
        });
    }

    let patterns: Vec<BString> = Vec::new();
    for item in platform.into_iter(patterns)? {
        match item? {
            gix::status::Item::TreeIndex(change) => {
                use gix::diff::index::ChangeRef;
                match change {
                    ChangeRef::Addition { location, .. } => {
                        staged.push((StageKind::New, location.into_owned(), None));
                    }
                    ChangeRef::Deletion { location, .. } => {
                        staged.push((StageKind::Deleted, location.into_owned(), None));
                    }
                    ChangeRef::Modification {
                        location,
                        previous_entry_mode,
                        entry_mode,
                        ..
                    } => {
                        let kind = if type_class(previous_entry_mode) != type_class(entry_mode) {
                            StageKind::TypeChange
                        } else {
                            StageKind::Modified
                        };
                        staged.push((kind, location.into_owned(), None));
                    }
                    ChangeRef::Rewrite {
                        source_location,
                        location,
                        copy,
                        ..
                    } => {
                        let kind = if copy {
                            StageKind::Copied
                        } else {
                            StageKind::Renamed
                        };
                        staged.push((kind, location.into_owned(), Some(source_location.into_owned())));
                    }
                }
            }
            gix::status::Item::IndexWorktree(iw) => {
                use gix::status::index_worktree::Item;
                use gix::status::plumbing::index_as_worktree::{Change, Conflict, EntryStatus};
                match iw {
                    Item::Modification { rela_path, status, .. } => match status {
                        // gitoxide already folds the up-to-three conflict stages
                        // of one path into a single summary, which maps 1:1 onto
                        // git's stagemask.
                        EntryStatus::Conflict { summary, .. } => {
                            let mask = match summary {
                                Conflict::BothDeleted => 1,
                                Conflict::AddedByUs => 2,
                                Conflict::DeletedByThem => 3,
                                Conflict::AddedByThem => 4,
                                Conflict::DeletedByUs => 5,
                                Conflict::BothAdded => 6,
                                Conflict::BothModified => 7,
                            };
                            unmerged.push((mask, rela_path));
                        }
                        EntryStatus::IntentToAdd => {
                            anyhow::bail!("intent-to-add entries (git add -N) are not supported")
                        }
                        EntryStatus::NeedsUpdate(_) => {}
                        EntryStatus::Change(change) => match change {
                            Change::Removed => unstaged.push((WorkKind::Deleted, rela_path)),
                            Change::Type { .. } => unstaged.push((WorkKind::TypeChange, rela_path)),
                            Change::Modification { .. } | Change::SubmoduleModification(_) => {
                                unstaged.push((WorkKind::Modified, rela_path))
                            }
                        },
                    },
                    Item::DirectoryContents { entry, .. } => match entry.status {
                        gix::dir::entry::Status::Untracked => {
                            untracked_paths.push(walk_path(&entry));
                        }
                        gix::dir::entry::Status::Ignored(_) => {
                            ignored_paths.push(walk_path(&entry));
                        }
                        _ => {}
                    },
                    // Rename tracking is disabled for the index↔worktree pass in the
                    // default status platform, so this never fires; ignore defensively.
                    Item::Rewrite { .. } => {}
                }
            }
        }
    }

    // git orders each section (and each short-format block) by path.
    staged.sort_by(|a, b| a.1.cmp(&b.1));
    unstaged.sort_by(|a, b| a.1.cmp(&b.1));
    unmerged.sort_by(|a, b| a.1.cmp(&b.1));
    untracked_paths.sort();
    ignored_paths.sort();

    let tracking = if unborn {
        None
    } else {
        tracking_info(&repo)?
    };

    if short {
        let mut out = String::new();
        if branch_header {
            out.push_str(&short_branch_header(&head_state, tracking.as_ref()));
        }
        out.push_str(&render_short(
            staged,
            unstaged,
            unmerged,
            &untracked_paths,
            &ignored_paths,
        ));
        print!("{out}");
    } else {
        print!(
            "{}",
            render_long(
                &head_state,
                &tracking_lines(tracking.as_ref()),
                unborn,
                merging,
                untracked,
                show_ignored,
                &staged,
                &unstaged,
                &unmerged,
                &untracked_paths,
                &ignored_paths,
            )
        );
    }

    Ok(ExitCode::SUCCESS)
}

/// Resolve `status.showUntrackedFiles`, which stands in for an absent
/// `--untracked-files` flag. Anything unrecognised falls back to git's default.
fn configured_untracked(repo: &gix::Repository) -> Untracked {
    let Some(value) = repo.config_snapshot().string("status.showUntrackedFiles") else {
        return Untracked::Normal;
    };
    match value.as_slice() {
        b"no" => Untracked::No,
        b"all" => Untracked::All,
        _ => Untracked::Normal,
    }
}

/// Resolve a `--untracked-files=<mode>` / `-u<mode>` value the way git does.
/// The three named modes match verbatim; any other value is run through git's
/// `git_parse_maybe_bool`, where a truthy value means `normal` and a falsy value
/// means `no`. `None` is git's "Invalid untracked files mode" (fatal, exit 128).
fn parse_untracked_mode(value: &str) -> Option<Untracked> {
    match value {
        "no" => Some(Untracked::No),
        "normal" => Some(Untracked::Normal),
        "all" => Some(Untracked::All),
        _ => match parse_maybe_bool(value) {
            Some(true) => Some(Untracked::Normal),
            Some(false) => Some(Untracked::No),
            None => None,
        },
    }
}

/// Port of git's `git_parse_maybe_bool`: recognise the textual booleans, then
/// fall back to an integer parse where any non-zero value is `true`. `None` is
/// git's parse failure.
fn parse_maybe_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "" | "false" | "no" | "off" => return Some(false),
        "true" | "yes" | "on" => return Some(true),
        _ => {}
    }
    parse_git_int(value).map(|n| n != 0)
}

/// Port of git's `git_parse_int` (`git_parse_signed` with an `INT_MAX` ceiling):
/// C `strtoimax(value, &end, 0)` — base auto-detected from the `0x`/`0` prefix —
/// followed by `get_unit_factor` (an optional single `k`/`m`/`g` suffix, 1024-
/// based) and the range check. `None` is git's EINVAL / ERANGE.
fn parse_git_int(value: &str) -> Option<i64> {
    let b = value.as_bytes();
    let mut i = 0;
    // strtoimax skips leading C whitespace.
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let mut negative = false;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        negative = b[i] == b'-';
        i += 1;
    }
    // base-0 prefix detection: `0x`/`0X` (with a hex digit) is hex, a lone
    // leading `0` is octal, everything else decimal.
    let base: u64 = if i < b.len() && b[i] == b'0' {
        if i + 2 < b.len()
            && (b[i + 1] == b'x' || b[i + 1] == b'X')
            && (b[i + 2] as char).is_ascii_hexdigit()
        {
            i += 2;
            16
        } else {
            8 // the leading `0` is itself the first octal digit
        }
    } else {
        10
    };
    let digits_start = i;
    let mut val: i64 = 0;
    while i < b.len() {
        let digit = match b[i] {
            b'0'..=b'9' => (b[i] - b'0') as u64,
            b'a'..=b'f' => (b[i] - b'a' + 10) as u64,
            b'A'..=b'F' => (b[i] - b'A' + 10) as u64,
            _ => break,
        };
        if digit >= base {
            break;
        }
        // Overflow here is git's ERANGE from strtoimax.
        val = val.checked_mul(base as i64)?.checked_add(digit as i64)?;
        i += 1;
    }
    if i == digits_start {
        return None; // no digits converted -> EINVAL
    }
    if negative {
        val = -val;
    }
    // get_unit_factor: the remainder must be exactly empty or one of k/m/g.
    let factor: i64 = match value[i..].to_ascii_lowercase().as_str() {
        "" => 1,
        "k" => 1024,
        "m" => 1024 * 1024,
        "g" => 1024 * 1024 * 1024,
        _ => return None, // EINVAL
    };
    // git_parse_int caps at INT_MAX before applying the factor.
    const MAX: i64 = i32::MAX as i64;
    if (val < 0 && -MAX / factor > val) || (val > 0 && MAX / factor < val) {
        return None; // ERANGE
    }
    Some(val * factor)
}

/// Parse the `<n>` of `-M<n>` / `--find-renames=<n>` into a similarity fraction.
/// git accepts a bare percentage (`-M50`) or a fraction (`-M0.5`).
fn parse_similarity(raw: &str) -> Option<gix::diff::Rewrites> {
    let (body, had_percent) = match raw.strip_suffix('%') {
        Some(body) => (body, true),
        None => (raw, false),
    };
    if body.is_empty() {
        return Some(gix::diff::Rewrites::default());
    }
    let value: f32 = body.parse().ok()?;
    // git reads a bare integer as a percentage (`-M50`) and a decimal as a
    // fraction (`-M0.5`); an explicit `%` always means a percentage.
    let percentage = if had_percent || !body.contains('.') {
        value / 100.0
    } else {
        value
    };
    if !(0.0..=1.0).contains(&percentage) {
        return None;
    }
    Some(gix::diff::Rewrites {
        percentage: Some(percentage),
        ..Default::default()
    })
}

/// The repo-relative path a dirwalk entry should be displayed as: git suffixes a
/// `/` on directories (and nested repositories) it reports as a single entry.
fn walk_path(entry: &gix::dir::Entry) -> BString {
    let mut path = entry.rela_path.clone();
    if matches!(
        entry.disk_kind,
        Some(gix::dir::entry::Kind::Directory) | Some(gix::dir::entry::Kind::Repository)
    ) {
        path.push(b'/');
    }
    path
}

enum HeadState {
    Branch(String),
    Detached(String),
    Unborn(String),
}

/// Upstream relationship of the current branch, as git's `stat_tracking_info`
/// computes it.
struct Tracking {
    upstream: String,
    /// The configured upstream ref no longer exists.
    gone: bool,
    ahead: usize,
    behind: usize,
}

#[derive(Clone, Copy)]
enum StageKind {
    New,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChange,
}

#[derive(Clone, Copy)]
enum WorkKind {
    Modified,
    Deleted,
    TypeChange,
}

/// Shorten a `HEAD` referent name (`refs/heads/main` → `main`), or fall back.
fn referent_short(name: Option<&gix::refs::FullNameRef>, fallback: &str) -> String {
    use gix::bstr::ByteSlice;
    name.map(|n| n.shorten().to_str_lossy().into_owned())
        .unwrap_or_else(|| fallback.to_owned())
}

/// Map an index-entry mode to a coarse type class, ignoring the executable bit
/// (git treats a permission-only change as `modified`, not `typechange`).
/// 0 = regular blob, 1 = symlink, 2 = gitlink/commit, 3 = tree.
fn type_class(mode: gix::index::entry::Mode) -> u8 {
    match mode.to_tree_entry_mode() {
        Some(m) if m.is_link() => 1,
        Some(m) if m.is_commit() => 2,
        Some(m) if m.is_tree() => 3,
        _ => 0,
    }
}

/// C-style path quoting matching git's default `core.quotePath=true`: a path is
/// wrapped in double quotes and escaped when it contains control bytes, a quote,
/// a backslash, or any byte >= 0x80; otherwise it is emitted verbatim.
fn quote_path(path: impl AsRef<[u8]>) -> String {
    let bytes = path.as_ref();
    let needs = bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        // All bytes are printable ASCII here, so this is lossless.
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut out = String::from("\"");
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            0x0d => out.push_str("\\r"),
            b if b < 0x20 || b == 0x7f || b >= 0x80 => {
                out.push_str(&format!("\\{b:03o}"));
            }
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}

/// Resolve the upstream of the current branch and how far it has diverged.
/// Returns `None` when no upstream is configured, matching git's "no tracking
/// information at all" case.
fn tracking_info(repo: &gix::Repository) -> Result<Option<Tracking>> {
    use gix::bstr::ByteSlice;

    let Some(branch_ref) = repo.head_ref()? else {
        return Ok(None);
    };
    let Some(Ok(upstream_name)) = branch_ref.remote_tracking_ref_name(gix::remote::Direction::Fetch)
    else {
        return Ok(None);
    };
    let upstream = upstream_name.shorten().to_str_lossy().into_owned();
    let upstream_full = upstream_name.as_bstr().to_str_lossy().into_owned();

    let upstream_ref = match repo.try_find_reference(upstream_full.as_str())? {
        Some(r) => r,
        None => {
            return Ok(Some(Tracking {
                upstream,
                gone: true,
                ahead: 0,
                behind: 0,
            }));
        }
    };

    let upstream_id = upstream_ref.into_fully_peeled_id()?.detach();
    let local_id = repo.head_id()?.detach();

    Ok(Some(Tracking {
        upstream,
        gone: false,
        ahead: count_commits(repo, local_id, upstream_id)?,
        behind: count_commits(repo, upstream_id, local_id)?,
    }))
}

/// Build the tracking header line(s) for the long format, matching git's
/// `format_tracking_info` output including advice hints. Empty when there is no
/// upstream configured.
fn tracking_lines(tracking: Option<&Tracking>) -> String {
    let Some(t) = tracking else {
        return String::new();
    };
    let upstream = &t.upstream;
    if t.gone {
        return format!(
            "Your branch is based on '{upstream}', but the upstream is gone.\n  (use \"git branch --unset-upstream\" to fixup)\n"
        );
    }
    let (ahead, behind) = (t.ahead, t.behind);
    if ahead == 0 && behind == 0 {
        format!("Your branch is up to date with '{upstream}'.\n")
    } else if behind == 0 {
        let noun = if ahead == 1 { "commit" } else { "commits" };
        format!(
            "Your branch is ahead of '{upstream}' by {ahead} {noun}.\n  (use \"git push\" to publish your local commits)\n"
        )
    } else if ahead == 0 {
        let noun = if behind == 1 { "commit" } else { "commits" };
        format!(
            "Your branch is behind '{upstream}' by {behind} {noun}, and can be fast-forwarded.\n  (use \"git pull\" to update your local branch)\n"
        )
    } else {
        format!(
            "Your branch and '{upstream}' have diverged,\nand have {ahead} and {behind} different commits each, respectively.\n  (use \"git pull\" if you want to integrate the remote branch with yours)\n"
        )
    }
}

/// The `## …` line of `git status -sb`, per git's `wt_shortstatus_print_tracking`.
fn short_branch_header(head_state: &HeadState, tracking: Option<&Tracking>) -> String {
    let mut out = String::from("## ");
    match head_state {
        HeadState::Detached(_) => {
            out.push_str("HEAD (no branch)\n");
            return out;
        }
        HeadState::Unborn(name) => {
            // An unborn branch has no commits to compare, so git stops at the name.
            out.push_str(&format!("No commits yet on {name}\n"));
            return out;
        }
        HeadState::Branch(name) => out.push_str(name),
    }

    let Some(t) = tracking else {
        out.push('\n');
        return out;
    };
    out.push_str("...");
    out.push_str(&t.upstream);
    if t.gone {
        out.push_str(" [gone]");
    } else if t.ahead > 0 && t.behind > 0 {
        out.push_str(&format!(" [ahead {}, behind {}]", t.ahead, t.behind));
    } else if t.ahead > 0 {
        out.push_str(&format!(" [ahead {}]", t.ahead));
    } else if t.behind > 0 {
        out.push_str(&format!(" [behind {}]", t.behind));
    }
    out.push('\n');
    out
}

/// Count commits reachable from `tip` but not from `hidden` — i.e. the ahead/
/// behind count, exactly as git derives it from the merge base.
fn count_commits(repo: &gix::Repository, tip: ObjectId, hidden: ObjectId) -> Result<usize> {
    let walk = repo
        .rev_walk(Some(tip))
        .with_hidden(Some(hidden))
        .all()?;
    Ok(walk.take_while(Result::is_ok).count())
}

#[allow(clippy::too_many_arguments)]
fn render_long(
    head_state: &HeadState,
    tracking: &str,
    unborn: bool,
    merging: bool,
    untracked_mode: Untracked,
    show_ignored: bool,
    staged: &[(StageKind, BString, Option<BString>)],
    unstaged: &[(WorkKind, BString)],
    unmerged: &[(u8, BString)],
    untracked: &[BString],
    ignored: &[BString],
) -> String {
    let mut out = String::new();

    match head_state {
        HeadState::Branch(name) => out.push_str(&format!("On branch {name}\n")),
        HeadState::Detached(short) => out.push_str(&format!("HEAD detached at {short}\n")),
        HeadState::Unborn(name) => out.push_str(&format!("On branch {name}\n")),
    }

    // git prints a blank line after the tracking block and after each
    // in-progress-operation block; a plain branch/detached header runs straight
    // into the first section.
    out.push_str(tracking);
    if !tracking.is_empty() {
        out.push('\n');
    }

    if merging {
        if unmerged.is_empty() {
            out.push_str("All conflicts fixed but you are still merging.\n");
            out.push_str("  (use \"git commit\" to conclude merge)\n");
        } else {
            out.push_str("You have unmerged paths.\n");
            out.push_str("  (fix conflicts and run \"git commit\")\n");
            out.push_str("  (use \"git merge --abort\" to abort the merge)\n");
        }
        out.push('\n');
    }

    if unborn {
        out.push_str("\nNo commits yet\n\n");
    }

    if !staged.is_empty() {
        out.push_str("Changes to be committed:\n");
        // Mid-merge git offers no unstage hint, as `git restore --staged` is not
        // the right advice while `MERGE_HEAD` is around.
        if !merging {
            if unborn {
                out.push_str("  (use \"git rm --cached <file>...\" to unstage)\n");
            } else {
                out.push_str("  (use \"git restore --staged <file>...\" to unstage)\n");
            }
        }
        for (kind, path, orig) in staged {
            let label = stage_label(*kind);
            match orig {
                Some(o) => out.push_str(&format!(
                    "\t{label:<12}{} -> {}\n",
                    quote_path(o),
                    quote_path(path)
                )),
                None => out.push_str(&format!("\t{label:<12}{}\n", quote_path(path))),
            }
        }
        out.push('\n');
    }

    if !unmerged.is_empty() {
        out.push_str("Unmerged paths:\n");
        out.push_str(unmerged_hint(unmerged));
        for (mask, path) in unmerged {
            let label = unmerged_label(*mask);
            out.push_str(&format!("\t{label:<17}{}\n", quote_path(path)));
        }
        out.push('\n');
    }

    if !unstaged.is_empty() {
        let any_deleted = unstaged.iter().any(|(k, _)| matches!(k, WorkKind::Deleted));
        let add_hint = if any_deleted { "git add/rm" } else { "git add" };
        out.push_str("Changes not staged for commit:\n");
        out.push_str(&format!(
            "  (use \"{add_hint} <file>...\" to update what will be committed)\n"
        ));
        out.push_str("  (use \"git restore <file>...\" to discard changes in working directory)\n");
        for (kind, path) in unstaged {
            let label = work_label(*kind);
            out.push_str(&format!("\t{label:<12}{}\n", quote_path(path)));
        }
        out.push('\n');
    }

    let committable = !staged.is_empty();

    if untracked_mode == Untracked::No {
        // git only mentions the suppressed listing when the run is committable —
        // otherwise the trailing summary already carries the `-u` hint.
        if committable {
            out.push_str("Untracked files not listed (use -u option to show untracked files)\n");
        }
    } else {
        if !untracked.is_empty() {
            out.push_str("Untracked files:\n");
            out.push_str("  (use \"git add <file>...\" to include in what will be committed)\n");
            for path in untracked {
                out.push_str(&format!("\t{}\n", quote_path(path)));
            }
            out.push('\n');
        }
        if show_ignored && !ignored.is_empty() {
            out.push_str("Ignored files:\n");
            out.push_str("  (use \"git add -f <file>...\" to include in what will be committed)\n");
            for path in ignored {
                out.push_str(&format!("\t{}\n", quote_path(path)));
            }
            out.push('\n');
        }
    }

    // Trailing summary — omitted entirely when there is anything staged
    // (git's "committable" state), matching stock output.
    if !committable {
        let workdir_dirty = !unstaged.is_empty() || !unmerged.is_empty();
        let summary = if workdir_dirty {
            "no changes added to commit (use \"git add\" and/or \"git commit -a\")"
        } else if !untracked.is_empty() {
            "nothing added to commit but untracked files present (use \"git add\" to track)"
        } else if unborn {
            "nothing to commit (create/copy files and use \"git add\" to track)"
        } else if untracked_mode == Untracked::No {
            "nothing to commit (use -u to show untracked files)"
        } else {
            "nothing to commit, working tree clean"
        };
        out.push_str(summary);
        out.push('\n');
    }

    out
}

fn render_short(
    staged: Vec<(StageKind, BString, Option<BString>)>,
    unstaged: Vec<(WorkKind, BString)>,
    unmerged: Vec<(u8, BString)>,
    untracked: &[BString],
    ignored: &[BString],
) -> String {
    struct Short {
        x: u8,
        y: u8,
        orig: Option<BString>,
    }

    // Merge the change streams per path: X is the staged (index) column, Y the
    // worktree column; a file can carry both (e.g. "MM"). Untracked and ignored
    // entries are *not* merged in — git prints them as separate trailing blocks
    // rather than interleaving them by path.
    let mut map: BTreeMap<BString, Short> = BTreeMap::new();
    for (kind, path, orig) in staged {
        let e = map.entry(path).or_insert(Short {
            x: b' ',
            y: b' ',
            orig: None,
        });
        e.x = stage_char(kind);
        if orig.is_some() {
            e.orig = orig;
        }
    }
    for (kind, path) in unstaged {
        let e = map.entry(path).or_insert(Short {
            x: b' ',
            y: b' ',
            orig: None,
        });
        e.y = work_char(kind);
    }
    for (mask, path) in unmerged {
        let (x, y) = unmerged_chars(mask);
        map.insert(path, Short { x, y, orig: None });
    }

    let mut out = String::new();
    for (path, e) in &map {
        let (x, y) = (e.x as char, e.y as char);
        match &e.orig {
            Some(o) => {
                out.push_str(&format!("{x}{y} {} -> {}\n", quote_path(o), quote_path(path)))
            }
            None => out.push_str(&format!("{x}{y} {}\n", quote_path(path))),
        }
    }
    for path in untracked {
        out.push_str(&format!("?? {}\n", quote_path(path)));
    }
    for path in ignored {
        out.push_str(&format!("!! {}\n", quote_path(path)));
    }
    out
}

/// git picks the resolution hint from which conflict flavours are present:
/// pure both-deleted conflicts want `git rm`, mixed delete/modify ones want
/// either, and everything else wants `git add`.
fn unmerged_hint(unmerged: &[(u8, BString)]) -> &'static str {
    let mut both_deleted = false;
    let mut del_mod_conflict = false;
    let mut not_deleted = false;
    for (mask, _) in unmerged {
        match mask {
            1 => both_deleted = true,
            3 | 5 => del_mod_conflict = true,
            _ => not_deleted = true,
        }
    }
    if !both_deleted {
        if del_mod_conflict {
            "  (use \"git add/rm <file>...\" as appropriate to mark resolution)\n"
        } else {
            "  (use \"git add <file>...\" to mark resolution)\n"
        }
    } else if !del_mod_conflict && !not_deleted {
        "  (use \"git rm <file>...\" to mark resolution)\n"
    } else {
        "  (use \"git add/rm <file>...\" as appropriate to mark resolution)\n"
    }
}

/// Long-format label for a conflict stagemask (bit 0 = base, 1 = ours, 2 = theirs).
fn unmerged_label(mask: u8) -> &'static str {
    match mask {
        1 => "both deleted:",
        2 => "added by us:",
        3 => "deleted by them:",
        4 => "added by them:",
        5 => "deleted by us:",
        6 => "both added:",
        _ => "both modified:",
    }
}

/// Short-format two-letter code for a conflict stagemask.
fn unmerged_chars(mask: u8) -> (u8, u8) {
    match mask {
        1 => (b'D', b'D'),
        2 => (b'A', b'U'),
        3 => (b'U', b'D'),
        4 => (b'U', b'A'),
        5 => (b'D', b'U'),
        6 => (b'A', b'A'),
        _ => (b'U', b'U'),
    }
}

fn stage_label(kind: StageKind) -> &'static str {
    match kind {
        StageKind::New => "new file:",
        StageKind::Modified => "modified:",
        StageKind::Deleted => "deleted:",
        StageKind::Renamed => "renamed:",
        StageKind::Copied => "copied:",
        StageKind::TypeChange => "typechange:",
    }
}

fn work_label(kind: WorkKind) -> &'static str {
    match kind {
        WorkKind::Modified => "modified:",
        WorkKind::Deleted => "deleted:",
        WorkKind::TypeChange => "typechange:",
    }
}

fn stage_char(kind: StageKind) -> u8 {
    match kind {
        StageKind::New => b'A',
        StageKind::Modified => b'M',
        StageKind::Deleted => b'D',
        StageKind::Renamed => b'R',
        StageKind::Copied => b'C',
        StageKind::TypeChange => b'T',
    }
}

fn work_char(kind: WorkKind) -> u8 {
    match kind {
        WorkKind::Modified => b'M',
        WorkKind::Deleted => b'D',
        WorkKind::TypeChange => b'T',
    }
}
