use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gix::bstr::{BStr, ByteSlice};
use gix::hash::ObjectId;
use gix::prelude::ObjectIdExt;
use gix::refs::FullName;

/// `git worktree` — inspect and lock the working trees attached to a repository.
///
/// Ported sub-commands (stdout, stderr and exit codes match stock git):
///   * `git worktree list`                      → aligned human-readable listing
///   * `git worktree list -v|--verbose`         → lock/prune reasons on indented lines
///   * `git worktree list --porcelain`          → machine-readable records
///   * `git worktree list --porcelain -z`       → same, NUL-terminated
///   * `git worktree list --expire <date>`      → narrow the `prunable` window
///   * `git worktree lock [--reason <s>] <wt>`  → create `worktrees/<id>/locked`
///   * `git worktree unlock <wt>`               → remove it
///
/// `git worktree` itself takes no options, so any dash-prefixed token ahead of
/// the subcommand is a usage error (exit 129) — `--foo` reports an unknown
/// option, `-x` an unknown switch, and `--` reports the missing subcommand.
///
/// The listing reproduces git's `get_worktrees()`: the main worktree first (its
/// path is `realpath(common_dir)` with a trailing `/.git` stripped), then the
/// linked worktrees read out of `<common-dir>/worktrees/*/gitdir`, sorted by
/// path. Abbreviated ids honour `core.abbrev` through gitoxide's disambiguating
/// `Id::shorten()`, and the two output columns are padded to the widest value,
/// exactly as `measure_widths()` does. `prunable` is annotated when the `gitdir`
/// file points at a `.git` entry that no longer exists, the worktree is not
/// locked, and its administrative `index` is no newer than the expiry threshold
/// — the only reason reachable from `list`, since git skips a worktree whose
/// `gitdir` file is missing or empty. `list` defaults that threshold to TIME_MAX,
/// so a missing checkout is prunable unless `--expire` narrows the window.
///
/// NOT ported, and reported as such rather than approximated: `add`, `move`,
/// `remove`, `prune` and `repair`. Those mutate the worktree administrative
/// files and, for `add`/`remove`, need a full checkout/removal of a working
/// tree; the vendored crates expose the pieces (`gix_worktree_state::checkout`)
/// but not git's worktree bookkeeping, so a faithful port is not possible here.
///
/// Paths are rendered as lossy UTF-8; git writes the raw bytes. Column widths
/// use `char` counts where git uses `utf8_strwidth()`, so a path containing
/// double-width characters can pad differently. Both are byte-identical for the
/// ASCII paths that occur in practice.
pub fn worktree(args: &[String]) -> Result<ExitCode> {
    // Dispatch hands us the tail *after* the verb, so the subcommand is at index
    // 0. Tolerate a leading `worktree` as well, matching the other multi-verb
    // porcelain modules, so either wiring convention works.
    let args: &[String] = match args.first() {
        Some(a) if a == "worktree" => &args[1..],
        _ => args,
    };

    let Some(sub) = args.first().map(String::as_str) else {
        return usage(Some("error: need a subcommand"), MAIN_USAGE);
    };

    // `git worktree` itself defines no options: parse_options() rejects every
    // dash-prefixed token before the subcommand. `--` ends option parsing without
    // ever producing one, so it reports the missing subcommand instead. A lone
    // `-` is not an option and falls through as a (bogus) subcommand name.
    match sub {
        // git's parse_options prints `-h` help on stdout and still exits 129.
        // `--help` is intercepted by the `git` wrapper and shows the man page,
        // which this binary has no equivalent for; the usage block is the
        // closest honest substitute.
        "-h" | "--help" => {
            print!("{MAIN_USAGE}");
            return Ok(ExitCode::from(129));
        }
        "--" => return usage(Some("error: need a subcommand"), MAIN_USAGE),
        _ => {}
    }
    if let Some(long) = sub.strip_prefix("--") {
        return usage(Some(&format!("error: unknown option `{long}'")), MAIN_USAGE);
    }
    if let Some(short) = sub.strip_prefix('-').filter(|s| !s.is_empty()) {
        // git names only the offending character, not the whole cluster.
        let c = short.chars().next().unwrap_or('-');
        return usage(Some(&format!("error: unknown switch `{c}'")), MAIN_USAGE);
    }

    match sub {
        "list" => list(&args[1..]),
        "lock" => lock(&args[1..]),
        "unlock" => unlock(&args[1..]),
        "add" => bail!("`worktree add` is not ported (no worktree bookkeeping in the vendored crates)"),
        "move" => bail!("`worktree move` is not ported (no worktree bookkeeping in the vendored crates)"),
        "remove" => {
            bail!("`worktree remove` is not ported (no worktree bookkeeping in the vendored crates)")
        }
        "prune" => bail!("`worktree prune` is not ported (no worktree bookkeeping in the vendored crates)"),
        "repair" => {
            bail!("`worktree repair` is not ported (no worktree bookkeeping in the vendored crates)")
        }
        other => usage(
            Some(&format!("error: unknown subcommand: `{other}'")),
            MAIN_USAGE,
        ),
    }
}

const MAIN_USAGE: &str = "\
usage: git worktree add [-f] [--detach] [--checkout] [--lock [--reason <string>]]
                        [--orphan] [(-b | -B) <new-branch>] <path> [<commit-ish>]
   or: git worktree list [-v | --porcelain [-z]]
   or: git worktree lock [--reason <string>] <worktree>
   or: git worktree move <worktree> <new-path>
   or: git worktree prune [-n] [-v] [--expire <expire>]
   or: git worktree remove [-f] <worktree>
   or: git worktree repair [<path>...]
   or: git worktree unlock <worktree>

";

const LIST_USAGE: &str = "\
usage: git worktree list [-v | --porcelain [-z]]

    --[no-]porcelain      machine-readable output
    -v, --[no-]verbose    show extended annotations and reasons, if available
    --[no-]expire <expiry-date>
                          add 'prunable' annotation to missing worktrees older than <time>
    -z                    terminate records with a NUL character

";

const LOCK_USAGE: &str = "\
usage: git worktree lock [--reason <string>] <worktree>

    --[no-]reason <string>
                          reason for locking

";

const UNLOCK_USAGE: &str = "\
usage: git worktree unlock <worktree>

";

/// Print an optional `error:` line plus a usage block on stderr and exit 129,
/// which is what git's `usage_with_options()` does.
fn usage(err: Option<&str>, text: &str) -> Result<ExitCode> {
    if let Some(e) = err {
        eprintln!("{e}");
    }
    eprint!("{text}");
    Ok(ExitCode::from(129))
}

/// Print a `fatal:` line on stderr and exit 128, matching git's `die()`.
fn die(msg: &str) -> Result<ExitCode> {
    eprintln!("fatal: {msg}");
    Ok(ExitCode::from(128))
}

// ---------------------------------------------------------------------------
// The worktree model, mirroring `struct worktree` in git's worktree.c.
// ---------------------------------------------------------------------------

/// What `HEAD` resolves to inside one worktree.
enum HeadInfo {
    /// `HEAD` is a symref; the id is null for an unborn branch.
    Branch { oid: ObjectId, name: FullName },
    /// `HEAD` holds an object id directly.
    Detached(ObjectId),
    /// `HEAD` could not be resolved; git renders this as `(error)`.
    Unknown,
}

struct Wt {
    /// Displayed path of the checkout (the main worktree, or the linked one).
    path: PathBuf,
    /// The directory name under `worktrees/`; `None` for the main worktree.
    id: Option<String>,
    is_bare: bool,
    head: HeadInfo,
    /// `Some(reason)` when `worktrees/<id>/locked` exists; the reason may be empty.
    locked: Option<String>,
    /// `Some(reason)` when git would report the worktree as prunable.
    prunable: Option<String>,
}

impl Wt {
    fn is_linked(&self) -> bool {
        self.id.is_some()
    }

    /// The object id shown for this worktree; null when bare or unborn.
    fn oid(&self) -> ObjectId {
        match &self.head {
            HeadInfo::Branch { oid, .. } => *oid,
            HeadInfo::Detached(oid) => *oid,
            HeadInfo::Unknown => ObjectId::null(gix::hash::Kind::Sha1),
        }
    }
}

/// Trim trailing ASCII whitespace, as git's `strbuf_rtrim()` does.
fn rtrim(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &bytes[..end]
}

fn path_to_string(p: &Path) -> String {
    gix::path::into_bstr(p).to_str_lossy().into_owned()
}

/// Read `HEAD` of `repo` the way git's `add_head_info()` does: resolve it in the
/// worktree's own ref store, keeping the symref target unpeeled.
fn head_info(repo: &gix::Repository) -> HeadInfo {
    let Ok(head) = repo.head() else {
        return HeadInfo::Unknown;
    };
    let null = ObjectId::null(repo.object_hash());
    if head.is_detached() {
        return HeadInfo::Detached(head.id().map_or(null, |id| id.detach()));
    }
    match head.referent_name() {
        Some(name) => HeadInfo::Branch {
            oid: head.id().map_or(null, |id| id.detach()),
            name: name.to_owned(),
        },
        None => HeadInfo::Unknown,
    }
}

/// Enumerate the main worktree followed by every linked worktree, sorted by
/// path — git's `get_worktrees()` plus its trailing `QSORT(list + 1, ...)`.
fn collect(repo: &gix::Repository, expire: u64) -> Result<Vec<Wt>> {
    let common = gix::path::realpath(repo.common_dir())?;

    // The main worktree's path is the common dir with a trailing `/.git` cut off,
    // which leaves a bare repository's path untouched.
    let main_path = if common.file_name().and_then(|n| n.to_str()) == Some(".git") {
        common.parent().unwrap_or(&common).to_path_buf()
    } else {
        common.clone()
    };
    let is_bare = repo.is_bare();
    let mut out = vec![Wt {
        path: main_path,
        id: None,
        is_bare,
        head: if is_bare {
            HeadInfo::Unknown
        } else {
            head_info(repo)
        },
        locked: None,
        prunable: None,
    }];

    let mut linked = Vec::new();
    let dir = match std::fs::read_dir(common.join("worktrees")) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(out);
        }
        Err(e) => return Err(e.into()),
    };
    for entry in dir {
        let entry = entry?;
        let admin = entry.path();
        let Some(id) = admin.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            continue;
        };
        // git skips a worktree whose `gitdir` file cannot be read or is empty.
        let Ok(raw) = std::fs::read(admin.join("gitdir")) else {
            continue;
        };
        let trimmed = rtrim(&raw);
        if trimmed.is_empty() {
            continue;
        }
        // The file names the worktree's `.git` entry; `should_prune_worktree()`
        // tests exactly this string for existence, before any normalisation.
        let dot_git = PathBuf::from(String::from_utf8_lossy(trimmed).into_owned());
        let missing = !dot_git.exists();

        // The checkout path drops that `/.git` suffix; a relative recording is
        // resolved against the administrative directory and then realpath'd.
        let mut path = if dot_git.file_name().and_then(|n| n.to_str()) == Some(".git") {
            dot_git.parent().unwrap_or(&dot_git).to_path_buf()
        } else {
            dot_git.clone()
        };
        if path.is_relative() {
            path = gix::path::realpath(admin.join(&path)).unwrap_or(path);
        }

        let locked_file = admin.join("locked");
        let locked = locked_file.is_file().then(|| {
            std::fs::read(&locked_file)
                .map(|b| String::from_utf8_lossy(rtrim(&b)).into_owned())
                .unwrap_or_default()
        });
        // A locked worktree is never reported prunable. Otherwise git only
        // annotates a missing checkout once it has gone stale: the administrative
        // `index` must be no newer than the expiry threshold. An unreadable
        // `index` counts as stale, matching the `stat()`-failure branch.
        let stale = || {
            let mtime = std::fs::metadata(admin.join("index"))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            mtime.is_none_or(|m| m <= expire)
        };
        let prunable = (locked.is_none() && missing && stale())
            .then(|| "gitdir file points to non-existent location".to_owned());

        let head = match repo.worktree_proxy_by_id(BStr::new(id.as_str())) {
            Some(proxy) => match proxy.into_repo_with_possibly_inaccessible_worktree() {
                Ok(wt_repo) => head_info(&wt_repo),
                Err(_) => HeadInfo::Unknown,
            },
            None => HeadInfo::Unknown,
        };

        linked.push(Wt {
            path,
            id: Some(id),
            is_bare: false,
            head,
            locked,
            prunable,
        });
    }

    linked.sort_by(|a, b| a.path.cmp(&b.path));
    out.extend(linked);
    Ok(out)
}

// ---------------------------------------------------------------------------
// `git worktree list`
// ---------------------------------------------------------------------------

fn list(args: &[String]) -> Result<ExitCode> {
    let mut porcelain = false;
    let mut verbose = false;
    let mut nul = false;
    // `list` seeds the expiry at TIME_MAX, so every worktree whose `.git` entry
    // is gone counts as prunable unless `--expire` narrows the window.
    let mut expire = u64::MAX;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-h" | "--help" => {
                print!("{LIST_USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--porcelain" => porcelain = true,
            "--no-porcelain" => porcelain = false,
            "-v" | "--verbose" => verbose = true,
            "--no-verbose" => verbose = false,
            "-z" => nul = true,
            // A bare `--no-expire` resets the threshold to 0, which suppresses
            // the annotation entirely — the same value `--expire=never` yields.
            "--no-expire" => expire = 0,
            "--expire" => {
                let Some(v) = args.get(i + 1) else {
                    return usage(Some("error: option `expire' requires a value"), LIST_USAGE);
                };
                let Some(parsed) = parse_expiry(v) else {
                    return die(&format!("malformed expiration date '{v}'"));
                };
                expire = parsed;
                i += 1;
            }
            _ if a.starts_with("--expire=") => {
                let v = &a["--expire=".len()..];
                let Some(parsed) = parse_expiry(v) else {
                    return die(&format!("malformed expiration date '{v}'"));
                };
                expire = parsed;
            }
            // `--` ends option parsing; `list` takes no positionals, so anything
            // after it is a usage error and a trailing `--` is simply ignored.
            "--" => {
                if i + 1 < args.len() {
                    return usage(None, LIST_USAGE);
                }
                break;
            }
            _ if a.starts_with("--") => {
                return usage(
                    Some(&format!("error: unknown option `{}'", &a[2..])),
                    LIST_USAGE,
                );
            }
            _ if a.starts_with('-') && a.len() > 1 => {
                let c = a[1..].chars().next().unwrap_or('-');
                return usage(Some(&format!("error: unknown switch `{c}'")), LIST_USAGE);
            }
            // `list` takes no positionals; git prints the bare usage block.
            _ => return usage(None, LIST_USAGE),
        }
        i += 1;
    }

    // git checks these in this order, before touching the repository.
    if !porcelain && nul {
        return die("the option '-z' requires '--porcelain'");
    }
    if verbose && porcelain {
        return die("options '--verbose' and '--porcelain' cannot be used together");
    }

    let repo = gix::discover(".")?;
    let worktrees = collect(&repo, expire)?;

    let out = if porcelain {
        render_porcelain(&worktrees, nul)
    } else {
        render_plain(&repo, &worktrees, verbose)
    };
    print!("{out}");
    Ok(ExitCode::SUCCESS)
}

fn render_porcelain(worktrees: &[Wt], nul: bool) -> String {
    let t = if nul { '\0' } else { '\n' };
    let mut out = String::new();
    for wt in worktrees {
        out.push_str(&format!("worktree {}{t}", path_to_string(&wt.path)));
        if wt.is_bare {
            out.push_str(&format!("bare{t}"));
        } else {
            out.push_str(&format!("HEAD {}{t}", wt.oid().to_hex()));
            match &wt.head {
                HeadInfo::Detached(_) => out.push_str(&format!("detached{t}")),
                HeadInfo::Branch { name, .. } => {
                    out.push_str(&format!("branch {}{t}", name.as_bstr().to_str_lossy()));
                }
                HeadInfo::Unknown => {}
            }
        }
        if wt.is_linked() {
            if let Some(reason) = &wt.locked {
                if reason.is_empty() {
                    out.push_str(&format!("locked{t}"));
                } else if nul {
                    // Under -z git emits the reason verbatim.
                    out.push_str(&format!("locked {reason}{t}"));
                } else {
                    out.push_str(&format!("locked {}{t}", quote_c_style(reason)));
                }
            }
            if let Some(reason) = &wt.prunable {
                out.push_str(&format!("prunable {reason}{t}"));
            }
        }
        out.push(t);
    }
    out
}

fn render_plain(repo: &gix::Repository, worktrees: &[Wt], verbose: bool) -> String {
    // `measure_widths()`: the path column is the widest path, the id column the
    // longest abbreviation across every worktree.
    let paths: Vec<String> = worktrees.iter().map(|w| path_to_string(&w.path)).collect();
    let shas: Vec<String> = worktrees
        .iter()
        .map(|w| abbrev_hex(repo, w.oid()))
        .collect();
    let path_max = paths.iter().map(|p| p.chars().count()).max().unwrap_or(0);
    let sha_max = shas.iter().map(String::len).max().unwrap_or(0);

    let mut out = String::new();
    for ((wt, path), sha) in worktrees.iter().zip(&paths).zip(&shas) {
        out.push_str(path);
        out.push_str(&" ".repeat(path_max.saturating_sub(path.chars().count()) + 1));

        if wt.is_bare {
            out.push_str("(bare)");
        } else {
            out.push_str(sha);
            out.push_str(&" ".repeat(sha_max.saturating_sub(sha.len()) + 1));
            match &wt.head {
                HeadInfo::Detached(_) => out.push_str("(detached HEAD)"),
                HeadInfo::Branch { name, .. } => {
                    out.push_str(&format!("[{}]", name.as_ref().shorten().to_str_lossy()));
                }
                HeadInfo::Unknown => out.push_str("(error)"),
            }
        }

        if wt.is_linked() {
            if verbose {
                match wt.locked.as_deref() {
                    Some(r) if !r.is_empty() => out.push_str(&format!("\n\tlocked: {r}")),
                    Some(_) => out.push_str(" locked"),
                    None => {}
                }
                if let Some(r) = &wt.prunable {
                    out.push_str(&format!("\n\tprunable: {r}"));
                }
            } else {
                if wt.locked.is_some() {
                    out.push_str(" locked");
                }
                if wt.prunable.is_some() {
                    out.push_str(" prunable");
                }
            }
        }
        out.push('\n');
    }
    out
}

/// git's `parse_expiry_date()`: two keyword pairs bracket the range before
/// approxidate sees the string. `never`/`false` expire nothing (threshold 0),
/// `all`/`now` expire everything (threshold TIME_MAX) — the latter reads
/// backwards but is deliberate, since the caller wants everything already in the
/// past. `None` means the date was malformed.
fn parse_expiry(text: &str) -> Option<u64> {
    match text {
        "never" | "false" => return Some(0),
        "all" | "now" => return Some(u64::MAX),
        _ => {}
    }
    let now = Some(std::time::SystemTime::now());
    // approxidate treats `.` as a word separator, so `1.day.ago` is relative.
    let parsed = gix::date::parse(text, now)
        .or_else(|_| gix::date::parse(text.replace('.', " ").trim(), now))
        .ok()?;
    Some(parsed.seconds.max(0) as u64)
}

/// git's `find_unique_abbrev()`: the shortest unambiguous prefix at least
/// `core.abbrev` long. A null id has no object to disambiguate against, so git
/// simply emits that many zeroes.
fn abbrev_hex(repo: &gix::Repository, oid: ObjectId) -> String {
    if oid.is_null() {
        "0".repeat(hex_len(repo))
    } else {
        oid.attach(repo).shorten_or_id().to_string()
    }
}

/// The configured `core.abbrev`, falling back to git's automatic length which
/// scales with the number of packed objects.
fn hex_len(repo: &gix::Repository) -> usize {
    let hexsz = repo.object_hash().len_in_hex();
    let auto = || {
        let count = repo.objects.packed_object_count().unwrap_or(0);
        let bits = 64 - count.leading_zeros();
        (bits.div_ceil(2).max(7) as usize).min(hexsz)
    };
    match repo.config_snapshot().string("core.abbrev") {
        None => auto(),
        Some(value) => match &*value.to_str_lossy() {
            "auto" => auto(),
            // `core.abbrev=no|off|false` disables abbreviation entirely.
            "no" | "off" | "false" => hexsz,
            other => other
                .parse::<usize>()
                .map_or_else(|_| auto(), |n| n.clamp(4, hexsz)),
        },
    }
}

/// git's `quote_c_style()` with the same trigger set as `core.quotePath=true`:
/// quote only when a control byte, quote, backslash or high byte is present.
fn quote_c_style(text: &str) -> String {
    let bytes = text.as_bytes();
    let needs = bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        return text.to_owned();
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
            b if b < 0x20 || b == 0x7f || b >= 0x80 => out.push_str(&format!("\\{b:03o}")),
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// `git worktree lock` / `git worktree unlock`
// ---------------------------------------------------------------------------

fn lock(args: &[String]) -> Result<ExitCode> {
    let mut reason: Option<String> = None;
    let mut target: Option<&str> = None;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-h" | "--help" => {
                print!("{LOCK_USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--reason" => {
                let Some(v) = args.get(i + 1) else {
                    return usage(None, LOCK_USAGE);
                };
                reason = Some(v.clone());
                i += 1;
            }
            "--no-reason" => reason = None,
            _ if a.starts_with("--reason=") => reason = Some(a["--reason=".len()..].to_owned()),
            _ if a.starts_with('-') && a != "-" => return usage(None, LOCK_USAGE),
            _ if target.is_none() => target = Some(a),
            _ => return usage(None, LOCK_USAGE),
        }
        i += 1;
    }

    let Some(arg) = target else {
        return usage(None, LOCK_USAGE);
    };

    let repo = gix::discover(".")?;
    let worktrees = collect(&repo, u64::MAX)?;
    let Some(wt) = find_worktree(&worktrees, arg) else {
        return die(&format!("'{arg}' is not a working tree"));
    };
    let Some(id) = &wt.id else {
        return die("The main working tree cannot be locked or unlocked");
    };
    if let Some(old) = &wt.locked {
        return if old.is_empty() {
            die(&format!("'{arg}' is already locked"))
        } else {
            die(&format!("'{arg}' is already locked, reason: {old}"))
        };
    }

    // git's `write_file()` completes a non-empty payload with a newline and
    // writes nothing at all for an empty reason.
    let body = match reason.as_deref() {
        Some(r) if !r.is_empty() => format!("{r}\n"),
        _ => String::new(),
    };
    std::fs::write(
        repo.common_dir().join("worktrees").join(id).join("locked"),
        body,
    )?;
    Ok(ExitCode::SUCCESS)
}

fn unlock(args: &[String]) -> Result<ExitCode> {
    let mut target: Option<&str> = None;
    for a in args {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{UNLOCK_USAGE}");
                return Ok(ExitCode::from(129));
            }
            s if s.starts_with('-') && s != "-" => return usage(None, UNLOCK_USAGE),
            s if target.is_none() => target = Some(s),
            _ => return usage(None, UNLOCK_USAGE),
        }
    }
    let Some(arg) = target else {
        return usage(None, UNLOCK_USAGE);
    };

    let repo = gix::discover(".")?;
    let worktrees = collect(&repo, u64::MAX)?;
    let Some(wt) = find_worktree(&worktrees, arg) else {
        return die(&format!("'{arg}' is not a working tree"));
    };
    let Some(id) = &wt.id else {
        return die("The main working tree cannot be locked or unlocked");
    };
    if wt.locked.is_none() {
        return die(&format!("'{arg}' is not locked"));
    }
    std::fs::remove_file(repo.common_dir().join("worktrees").join(id).join("locked"))?;
    Ok(ExitCode::SUCCESS)
}

/// git's `find_worktree()`: try a unique path-suffix match first, then compare
/// the realpath of the argument against the realpath of each worktree.
fn find_worktree<'a>(worktrees: &'a [Wt], arg: &str) -> Option<&'a Wt> {
    if let Some(found) = find_by_suffix(worktrees, arg) {
        return Some(found);
    }
    let want = gix::path::realpath(arg).ok()?;
    worktrees
        .iter()
        .find(|wt| gix::path::realpath(&wt.path).is_ok_and(|p| p == want))
}

/// A suffix match only counts when it starts on a directory boundary, and only
/// when exactly one worktree matches.
fn find_by_suffix<'a>(worktrees: &'a [Wt], suffix: &str) -> Option<&'a Wt> {
    if suffix.is_empty() {
        return None;
    }
    let mut found = None;
    let mut hits = 0usize;
    for wt in worktrees {
        let path = path_to_string(&wt.path);
        let Some(start) = path.len().checked_sub(suffix.len()) else {
            continue;
        };
        if !path.is_char_boundary(start) {
            continue;
        }
        let boundary = start == 0 || path.as_bytes()[start - 1] == b'/';
        if boundary && path[start..] == *suffix {
            found = Some(wt);
            hits += 1;
            if hits > 1 {
                return None;
            }
        }
    }
    (hits == 1).then_some(found).flatten()
}
