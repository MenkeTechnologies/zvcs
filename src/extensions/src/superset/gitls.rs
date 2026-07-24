//! `git zls` — a git-aware directory listing, like `eza --git`.
//!
//! Each entry is prefixed with a two-column git status field `[staged][unstaged]`
//! (index-vs-HEAD, then worktree-vs-index), using eza's letters: `N` new, `M`
//! modified, `D` deleted, `R` renamed, `C` copied, `T` type-change, `U`
//! conflicted, `I` ignored, `-` unchanged. A directory folds the status of the
//! paths under it, so a subtree with any change is flagged. Outside a git repo the
//! column is omitted and it is a plain listing.
//!
//! Flags: `-a` include dotfiles, `-l` long (perms, size, relative mtime), `-t`
//! sort by mtime (newest first), `-r` reverse. The per-path status is the same
//! gix status walk `git status` uses ([`crate::porcelain::status`]).

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;
use gix::bstr::{BString, ByteSlice};

use crate::superset::lscolors::Theme;

/// Parsed `zls` options.
struct Opts {
    all: bool,
    long: bool,
    reverse: bool,
    by_mtime: bool,
    path: PathBuf,
}

pub fn zls(args: &[String]) -> Result<ExitCode> {
    let opts = parse(args)?;
    let colored = std::io::IsTerminal::is_terminal(&std::io::stdout())
        && std::env::var_os("NO_COLOR").is_none();
    // eza-compatible palette from LS_COLORS + EXA_COLORS/EZA_COLORS.
    let theme = crate::superset::lscolors::Theme::from_env(colored);

    // Entries to list: the directory's children, or the single named file.
    let meta = std::fs::symlink_metadata(&opts.path)
        .map_err(|e| anyhow::anyhow!("{}: {e}", opts.path.display()))?;
    let (base, names): (PathBuf, Vec<PathBuf>) = if meta.is_dir() {
        let mut v = Vec::new();
        for e in std::fs::read_dir(&opts.path).map_err(|e| anyhow::anyhow!("{}: {e}", opts.path.display()))? {
            let e = e?;
            let name = e.file_name();
            let bytes = name.as_encoded_bytes();
            if !opts.all && bytes.first() == Some(&b'.') {
                continue;
            }
            v.push(PathBuf::from(name));
        }
        (opts.path.clone(), v)
    } else {
        let base = opts.path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let name = opts.path.file_name().map(PathBuf::from).unwrap_or_else(|| opts.path.clone());
        (base, vec![name])
    };

    // Git status, scoped to the listing directory. `prefix` is the base dir
    // relative to the repo worktree (forward slashes), used to join entry names
    // into repo-relative paths for the status lookup.
    let (status, prefix) = load_status(&base);

    // Decorate, sort, render.
    let mut rows: Vec<Row> = names
        .iter()
        .map(|name| Row::build(&base, name, &status, &prefix))
        .collect();
    // Case-insensitive by name (like eza); mtime sort falls back to it on ties.
    rows.sort_by(|a, b| {
        if opts.by_mtime {
            b.mtime.cmp(&a.mtime).then_with(|| a.sort_key.cmp(&b.sort_key))
        } else {
            a.sort_key.cmp(&b.sort_key)
        }
    });
    if opts.reverse {
        rows.reverse();
    }

    let has_git = !prefix_is_none(&prefix);
    // Size the size/date columns to their widest value so nothing runs ragged —
    // relative dates ("5 hours ago" vs "3 years, 2 months ago") vary in width.
    let (size_w, date_w) = if opts.long {
        (
            rows.iter().map(|r| r.size_str.len()).max().unwrap_or(0),
            rows.iter().map(|r| r.date_str.len()).max().unwrap_or(0),
        )
    } else {
        (0, 0)
    };
    let out = std::io::stdout();
    let mut w = std::io::BufWriter::new(out.lock());
    use std::io::Write;
    for row in &rows {
        row.render(&mut w, opts.long, has_git, &theme, size_w, date_w)?;
    }
    w.flush().ok();
    Ok(ExitCode::SUCCESS)
}

/// Split argv into flags (possibly bundled, e.g. `-la`) and an optional path.
fn parse(args: &[String]) -> Result<Opts> {
    let mut o = Opts { all: false, long: false, reverse: false, by_mtime: false, path: PathBuf::from(".") };
    let mut path: Option<PathBuf> = None;
    for arg in args {
        if let Some(flags) = arg.strip_prefix('-').filter(|_| arg.len() > 1 && arg != "--") {
            for c in flags.chars() {
                match c {
                    'a' => o.all = true,
                    'l' => o.long = true,
                    'r' => o.reverse = true,
                    't' => o.by_mtime = true,
                    // Unknown flags are ignored rather than fatal — this is a repl
                    // convenience, not a full ls; `-h` never reaches here (dispatch
                    // prints usage for it).
                    _ => {}
                }
            }
        } else if path.is_none() {
            path = Some(PathBuf::from(arg));
        }
    }
    if let Some(p) = path {
        o.path = p;
    }
    Ok(o)
}

/// A `None` prefix (`load_status` found no repo) is encoded as a single-element
/// sentinel so callers can tell "no git" from "repo root" (empty prefix).
fn prefix_is_none(prefix: &Option<String>) -> bool {
    prefix.is_none()
}

/// The git status map for the repo containing `dir`, plus `dir`'s path relative
/// to the worktree (forward slashes). `None` prefix when `dir` is not in a repo.
fn load_status(dir: &Path) -> (HashMap<BString, (u8, u8)>, Option<String>) {
    let Ok(repo) = gix::discover(dir) else {
        return (HashMap::new(), None);
    };
    let Some(workdir) = repo.workdir() else {
        return (HashMap::new(), None);
    };
    // Base dir relative to the worktree root, forward-slashed. Empty at the root.
    let prefix = dir
        .canonicalize()
        .ok()
        .and_then(|d| workdir.canonicalize().ok().and_then(|w| d.strip_prefix(&w).ok().map(Path::to_path_buf)))
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    (status_map(&repo, &prefix), Some(prefix))
}

/// repo-relative path → (staged, unstaged) eza-style status chars. Scoped to
/// `prefix` via a pathspec so a big repo does not walk more than the listing dir.
fn status_map(repo: &gix::Repository, prefix: &str) -> HashMap<BString, (u8, u8)> {
    let mut map: HashMap<BString, (u8, u8)> = HashMap::new();
    let Ok(platform) = repo.status(gix::progress::Discard) else {
        return map;
    };
    let platform = platform
        .untracked_files(gix::status::UntrackedFiles::Files)
        .dirwalk_options(|opts| opts.emit_ignored(Some(gix::dir::walk::EmissionMode::Matching)));
    let patterns: Vec<BString> = if prefix.is_empty() {
        Vec::new()
    } else {
        vec![BString::from(prefix)]
    };
    let Ok(iter) = platform.into_iter(patterns) else {
        return map;
    };
    for item in iter {
        let Ok(item) = item else { continue };
        match item {
            gix::status::Item::TreeIndex(change) => {
                use gix::diff::index::ChangeRef;
                let (loc, ch) = match change {
                    ChangeRef::Addition { location, .. } => (location.into_owned(), b'N'),
                    ChangeRef::Deletion { location, .. } => (location.into_owned(), b'D'),
                    ChangeRef::Modification { location, previous_entry_mode, entry_mode, .. } => {
                        let ch = if type_class(previous_entry_mode) != type_class(entry_mode) { b'T' } else { b'M' };
                        (location.into_owned(), ch)
                    }
                    ChangeRef::Rewrite { location, copy, .. } => {
                        (location.into_owned(), if copy { b'C' } else { b'R' })
                    }
                };
                map.entry(loc).or_insert((b'-', b'-')).0 = ch;
            }
            gix::status::Item::IndexWorktree(iw) => {
                use gix::status::index_worktree::Item;
                use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};
                match iw {
                    Item::Modification { rela_path, status, .. } => match status {
                        EntryStatus::Conflict { .. } => {
                            let e = map.entry(rela_path).or_insert((b'-', b'-'));
                            *e = (b'U', b'U');
                        }
                        EntryStatus::Change(Change::Removed) => {
                            map.entry(rela_path).or_insert((b'-', b'-')).1 = b'D';
                        }
                        EntryStatus::Change(Change::Type { .. }) => {
                            map.entry(rela_path).or_insert((b'-', b'-')).1 = b'T';
                        }
                        EntryStatus::Change(Change::Modification { .. })
                        | EntryStatus::Change(Change::SubmoduleModification(_)) => {
                            map.entry(rela_path).or_insert((b'-', b'-')).1 = b'M';
                        }
                        _ => {}
                    },
                    Item::DirectoryContents { entry, .. } => {
                        use gix::dir::entry::Status;
                        let ch = match entry.status {
                            Status::Untracked => Some(b'N'),
                            Status::Ignored(_) => Some(b'I'),
                            _ => None,
                        };
                        if let Some(c) = ch {
                            map.entry(entry.rela_path.clone()).or_insert((b'-', b'-')).1 = c;
                        }
                    }
                    Item::Rewrite { .. } => {}
                }
            }
        }
    }
    map
}

/// Faithful copy of `porcelain::status::type_class` — the object class of an
/// index entry mode, for detecting a type-change (file↔symlink↔gitlink).
fn type_class(mode: gix::index::entry::Mode) -> u8 {
    match mode.to_tree_entry_mode() {
        Some(m) if m.is_link() => 1,
        Some(m) if m.is_commit() => 2,
        Some(m) if m.is_tree() => 3,
        _ => 0,
    }
}

/// A listing entry ready to render. The size and date are pre-rendered so the
/// caller can size their columns to the widest value (no ragged alignment).
struct Row {
    name: String,
    sort_key: String,
    kind: Kind,
    git: (u8, u8),
    perms: u32,
    size_str: String,
    date_str: String,
    mtime: i64,
}

#[derive(Clone, Copy)]
enum Kind {
    Dir,
    Symlink,
    Exec,
    File,
}

impl Row {
    fn build(base: &Path, name: &Path, status: &HashMap<BString, (u8, u8)>, prefix: &Option<String>) -> Row {
        let full = base.join(name);
        let meta = std::fs::symlink_metadata(&full).ok();
        let ft = meta.as_ref().map(|m| m.file_type());
        let perms = meta.as_ref().map(|m| m.permissions().mode()).unwrap_or(0);
        let kind = match ft {
            Some(t) if t.is_symlink() => Kind::Symlink,
            Some(t) if t.is_dir() => Kind::Dir,
            _ if perms & 0o111 != 0 => Kind::Exec,
            _ => Kind::File,
        };
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let mtime = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // Git status: exact path for a file, folded over the subtree for a dir.
        let git = prefix
            .as_ref()
            .map(|p| {
                let name_str = name.to_string_lossy();
                let rel = if p.is_empty() { name_str.to_string() } else { format!("{p}/{name_str}") };
                fold_status(status, &rel, matches!(kind, Kind::Dir))
            })
            .unwrap_or((b'-', b'-'));

        let name = name.to_string_lossy().into_owned();
        let sort_key = name.to_lowercase();
        Row {
            name,
            sort_key,
            kind,
            git,
            perms,
            size_str: human_size(size),
            date_str: rel_mtime(mtime),
            mtime,
        }
    }

    /// eza column order: metadata, then the git field, then the name — the git
    /// status sits immediately before the name it describes. `size_w`/`date_w`
    /// are the column widths the caller computed across all rows, so the size and
    /// (variable-length) relative date align instead of running ragged.
    fn render<W: std::io::Write>(
        &self,
        w: &mut W,
        long: bool,
        has_git: bool,
        theme: &Theme,
        size_w: usize,
        date_w: usize,
    ) -> std::io::Result<()> {
        if long {
            // Pad to width on the PLAIN text, then color — coloring first would
            // put ANSI bytes inside the field and break alignment.
            let size = size_field(&self.size_str, size_w, theme);
            let date_pad = " ".repeat(date_w.saturating_sub(self.date_str.len()));
            let date = format!("{date_pad}{}", theme.paint(theme.sgr("da"), &self.date_str));
            write!(w, "{} {size} {date}  ", self.perm_string(theme))?;
        }
        if has_git {
            write!(w, "{} ", self.git_field(theme))?;
        }
        writeln!(w, "{}", self.colored_name(theme))
    }

    /// The colored, `ls -l`-style permission string: the type char in the entry's
    /// kind color, each rwx bit in its permission color, and `-` fillers in the
    /// punctuation color.
    fn perm_string(&self, theme: &Theme) -> String {
        let (type_ch, type_key) = match self.perms & 0o170000 {
            0o040000 => ('d', "di"),
            0o120000 => ('l', "ln"),
            0o140000 => ('s', "so"),
            0o060000 => ('b', "bd"),
            0o020000 => ('c', "cd"),
            0o010000 => ('p', "pi"),
            _ => ('-', "xx"),
        };
        let mut s = theme.paint(theme.sgr(type_key), &type_ch.to_string());
        // (shift, [read_key, write_key, exec_key]) per owner/group/other triad.
        for (shift, keys) in [(6, ["ur", "uw", "ux"]), (3, ["gr", "gw", "gx"]), (0, ["tr", "tw", "tx"])] {
            let bits = (self.perms >> shift) & 0o7;
            for (i, (set, ch)) in [(bits & 0o4, 'r'), (bits & 0o2, 'w'), (bits & 0o1, 'x')].iter().enumerate() {
                if *set != 0 {
                    s.push_str(&theme.paint(theme.sgr(keys[i]), &ch.to_string()));
                } else {
                    s.push_str(&theme.paint(theme.sgr("xx"), "-"));
                }
            }
        }
        s
    }

    /// The two-column git field, each letter in its eza git color.
    fn git_field(&self, theme: &Theme) -> String {
        format!("{}{}", git_char(self.git.0, theme), git_char(self.git.1, theme))
    }

    /// The entry name colored by extension (`*.ext`) if matched, else by kind,
    /// with a trailing `/` on directories.
    fn colored_name(&self, theme: &Theme) -> String {
        let suffix = if matches!(self.kind, Kind::Dir) { "/" } else { "" };
        let text = format!("{}{suffix}", self.name);
        let sgr = theme.ext_sgr(&self.name).unwrap_or_else(|| {
            theme.sgr(match self.kind {
                Kind::Dir => "di",
                Kind::Symlink => "ln",
                Kind::Exec => "ex",
                Kind::File => "fi",
            })
        });
        theme.paint(sgr, &text)
    }
}

/// The size column: right-aligned to `width` on the plain text, then the number
/// painted in the size color and the unit in the unit color (eza splits them).
fn size_field(size_str: &str, width: usize, theme: &Theme) -> String {
    let unit_at = size_str
        .rfind(|c: char| !c.is_ascii_alphabetic())
        .map(|i| i + 1)
        .unwrap_or(0);
    let (num, unit) = size_str.split_at(unit_at);
    let lead = " ".repeat(width.saturating_sub(size_str.len()));
    format!("{lead}{}{}", theme.paint(theme.sgr("sn"), num), theme.paint(theme.sgr("sb"), unit))
}

/// One git status letter, painted in its eza color (`ga`/`gm`/`gd`/…), with `-`
/// and ignored `I` in the punctuation color.
fn git_char(ch: u8, theme: &Theme) -> String {
    let c = (ch as char).to_string();
    let key = match ch {
        b'N' => "ga", // new
        b'M' => "gm", // modified
        b'D' => "gd", // deleted
        b'R' | b'C' => "gv", // renamed / copied
        b'T' => "gt", // type-change
        b'U' => "gc", // conflicted
        b'I' => "gi", // ignored
        _ => "xx",    // '-' unchanged
    };
    theme.paint(theme.sgr(key), &c)
}

/// Fold the status of `rel` (a file's exact path, or every path under a dir).
fn fold_status(status: &HashMap<BString, (u8, u8)>, rel: &str, is_dir: bool) -> (u8, u8) {
    if let Some(&xy) = status.get(rel.as_bytes().as_bstr()) {
        return xy;
    }
    if !is_dir {
        return (b'-', b'-');
    }
    // Directory: reduce the status of every contained path. A uniform non-`-`
    // column keeps its letter; a mix collapses to `M` ("something changed").
    let dir_prefix = format!("{rel}/");
    let (mut x, mut y) = (b'-', b'-');
    for (path, &(px, py)) in status {
        if path.as_bstr().to_str_lossy().starts_with(&dir_prefix) {
            x = combine(x, px);
            y = combine(y, py);
        }
    }
    (x, y)
}

/// Combine two status letters for a folded directory column.
fn combine(acc: u8, ch: u8) -> u8 {
    match (acc, ch) {
        (a, b'-') => a,
        (b'-', c) => c,
        (a, c) if a == c => a,
        _ => b'M',
    }
}

/// Human-readable byte size (1024-based, one decimal above K).
pub(crate) fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    if bytes < 1024 {
        return format!("{bytes}B");
    }
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1}{}", UNITS[i])
}

/// Relative mtime, e.g. `3 days ago`, via the shared date formatter.
fn rel_mtime(secs: i64) -> String {
    crate::date::show_date_relative(secs, crate::date::now_seconds())
}
