//! `[zvcs]` gitconfig — the switches that make the coordination layer autonomous.
//!
//! Configure once in `.gitconfig` (or `.git/config`) and the daemon does the
//! work on a timer; nothing is run by hand:
//!
//! ```gitconfig
//! [zvcs]
//!     autoreconcile = true   ; keep every CLEAN repo (this one + submodules) at origin/main
//!     autobump      = true   ; forward-only submodule gitlink bumps
//!     interval      = 30     ; seconds between autonomous passes (default 30)
//!     replvimode    = true   ; vi keybindings in the `git zrepl` console (default emacs)
//! ```
//!
//! `replvimode` is a `git zrepl` UI setting rather than daemon behavior, but it
//! lives in the same `[zvcs]` namespace and is read via [`config_bool`] so it
//! works whether or not `zrepl` is launched inside a repository.
//!
//! # Stock-git config primitives
//!
//! The second half of this module is not about `[zvcs]` at all: it holds the
//! readers for *git's own* configuration that more than one porcelain verb needs,
//! so the same key is parsed and diagnosed the same way everywhere. Today that is
//! [`parse_config_ulong`] / [`last_value_with_origin`] / [`config_ulong`] (git's
//! `git_config_ulong` and the `bad numeric config value` diagnostic it dies with)
//! and [`FsyncPolicy`] (`core.fsync`, `core.fsyncMethod`, `core.fsyncObjectFiles`).

use std::path::PathBuf;
use std::time::Duration;

/// Expand a leading `~` / `~/` to `$HOME` (git expands `~` for path-typed config
/// keys; `zvcs.crawlroots` is read as a raw string, so we do it here).
fn expand_tilde(tok: &str) -> PathBuf {
    if tok == "~" {
        if let Some(h) = std::env::var_os("HOME") {
            return PathBuf::from(h);
        }
    } else if let Some(rest) = tok.strip_prefix("~/") {
        if let Some(h) = std::env::var_os("HOME") {
            return PathBuf::from(h).join(rest);
        }
    }
    PathBuf::from(tok)
}

/// Resolved `[zvcs]` settings for a repository.
pub struct ZvcsConfig {
    /// Reconcile every clean repo (top-level + submodules) to origin/main on `interval`.
    pub autoreconcile: bool,
    /// Forward-only submodule gitlink bumps on `interval`.
    pub autobump: bool,
    /// Debounce window for coalescing watch-driven reaction bursts.
    pub interval: Duration,
    /// Roots for the repo crawler (`zvcs.crawlroots`, whitespace/comma separated).
    /// Empty means "use `$HOME`".
    pub crawlroots: Vec<PathBuf>,
    /// Crawl the configured roots for git repos in the background on daemon start
    /// (`zvcs.autocrawl`). Off by default — a whole-device scan is opt-in.
    pub autocrawl: bool,
    /// A `zvcs.hook` command to run on ref-change in any watched repo. When set,
    /// the daemon watches every indexed repo (not just the working submodules)
    /// and fires the hook per repo. `None` means no hooks.
    pub hook: Option<String>,
    /// Maintain each watched repo's cached status in the db on ref-change
    /// (`zvcs.autostatus`), so `git zstatus --all` is instant. Off by default.
    pub autostatus: bool,
    /// Watch every indexed repo and fire each repo's *own* `zvcs.hook` on
    /// ref-change (`zvcs.autohook`). This is the master switch that makes
    /// **per-repo (local) hooks** work without also setting a hook on the
    /// daemon's repo. Off by default.
    pub autohook: bool,
    /// On a commit to any indexed checkout, fast-forward every **local dup** of
    /// that repo (other checkouts with the same `origin` URL) to it, offline and
    /// in parallel — the automatic form of `git zsync`'s dup fan-out
    /// (`zvcs.autodups`). Off by default.
    pub autodups: bool,
    /// On ref-change in any watched repo, precompute the log caches for the
    /// commits that just arrived (`zvcs.precache`, on by default).
    ///
    /// Abbreviations and tree-diff tallies are pure functions of immutable
    /// objects, so they can be computed the moment a commit exists rather than
    /// the moment someone runs `log --stat`. The daemon is already awake and
    /// already knows the refs moved; doing it there is free from the user's
    /// point of view. git cannot do this at all — nothing of git is running
    /// between two commands.
    pub precache: bool,
}

impl Default for ZvcsConfig {
    /// Everything off — used when the daemon runs outside a repository but still
    /// has directory triggers to watch.
    fn default() -> Self {
        Self {
            autoreconcile: false,
            autobump: false,
            interval: Duration::from_secs(30),
            crawlroots: Vec::new(),
            autocrawl: false,
            hook: None,
            autostatus: false,
            autohook: false,
            autodups: false,
            // Nothing is watched in the default config, so there is nothing to
            // warm; the switch matters only once a watch set exists.
            precache: false,
        }
    }
}

impl ZvcsConfig {
    /// Read `[zvcs]` from the repository's merged config. Absent keys default to
    /// off; `interval` defaults to 30s and ignores non-positive values.
    pub fn load(repo: &gix::Repository) -> Self {
        let snap = repo.config_snapshot();
        let interval = snap
            .integer("zvcs.interval")
            .filter(|s| *s > 0)
            .unwrap_or(30) as u64;
        let crawlroots = snap
            .string("zvcs.crawlroots")
            .map(|s| {
                s.to_string()
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .filter(|t| !t.is_empty())
                    .map(expand_tilde)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            autoreconcile: snap.boolean("zvcs.autoreconcile").unwrap_or(false),
            autobump: snap.boolean("zvcs.autobump").unwrap_or(false),
            interval: Duration::from_secs(interval),
            crawlroots,
            autocrawl: snap.boolean("zvcs.autocrawl").unwrap_or(false),
            hook: snap
                .string("zvcs.hook")
                .map(|s| s.to_string())
                .filter(|s| !s.trim().is_empty()),
            autostatus: snap.boolean("zvcs.autostatus").unwrap_or(false),
            autohook: snap.boolean("zvcs.autohook").unwrap_or(false),
            autodups: snap.boolean("zvcs.autodups").unwrap_or(false),
            precache: snap.boolean("zvcs.precache").unwrap_or(true),
        }
    }

    /// Whether any autonomous (working-tree) behavior is enabled.
    pub fn any_autonomous(&self) -> bool {
        self.autoreconcile || self.autobump
    }

    /// Whether the daemon should run the watch loop at all — autonomy, hooks,
    /// status maintenance, or dup fan-out.
    pub fn should_watch(&self) -> bool {
        self.any_autonomous() || self.hooks_enabled() || self.autostatus || self.autodups
    }

    /// Whether the watcher should cover every indexed repo (not just working
    /// submodules): needed for machine-wide hooks, status, or dup fan-out.
    pub fn watch_all_repos(&self) -> bool {
        self.hooks_enabled() || self.autostatus || self.autodups
    }

    /// Whether hooks should fire: a hook set here, or the `autohook` master
    /// switch (which fires each repo's own local hook).
    pub fn hooks_enabled(&self) -> bool {
        self.hook.is_some() || self.autohook
    }
}

/// The global+system config `git config` reads outside a repository:
/// git-installation, system, and per-user (`~/.gitconfig`) files, with
/// `GIT_CONFIG_*` environment overrides layered on top (highest precedence).
/// Empty (never an error) when no such files exist.
pub fn global_config() -> gix::config::File {
    let mut file = gix::config::File::from_globals().unwrap_or_default();
    if let Ok(env) = gix::config::File::from_environment_overrides() {
        // `append` only errors on a malformed section header it just parsed from
        // the environment; ignore that and keep the good global read rather than
        // discarding it wholesale.
        let _ = file.append(env);
    }
    file
}

/// Read a boolean `[zvcs]` key from the current directory's config whether or
/// not it is inside a repository: the repo's merged snapshot when present, else
/// the [`global_config`] cascade. Used by verbs (e.g. `git zrepl`) that must
/// honor settings even when run outside a repo. `None` if the key is unset.
pub fn config_bool(key: &str) -> Option<bool> {
    match gix::discover(".") {
        Ok(repo) => repo.config_snapshot().boolean(key),
        Err(_) => global_config().boolean(key).ok().flatten(),
    }
}

// ---------------------------------------------------------------------------
// Stock-git config primitives
// ---------------------------------------------------------------------------

/// git's `git_parse_ulong`, the parser behind `git_config_ulong` and hence behind
/// every byte-sized config value (`gc.maxCruftSize`, `pack.packSizeLimit`,
/// `core.bigFileThreshold`, …). Returns the value, or the reason string git's
/// `die_bad_number` prints after it: `"invalid unit"` for a value it cannot read,
/// `"out of range"` for one that overflows an `unsigned long`.
///
/// The grammar is C `strtoumax` with base 0 (`0x400` is hex, `010` is octal,
/// everything else decimal) followed by `get_unit_factor`: an optional single
/// `k`/`m`/`g` magnitude suffix, either case, with nothing after it. A leading `+`
/// is accepted and leading ASCII whitespace is skipped; a leading `-`, an empty
/// value, or any trailing junk (a stray character, a second suffix) is an invalid
/// unit. Verified one value at a time against git 2.55.0's `gc.maxcruftsize` and
/// `pack.packsizelimit` diagnostics — `1k`/`0x400`/`010`/`0k` parse, `-1`/`1.5`/
/// `1x`/``/`5 ` are invalid units, and a 24-digit value is out of range.
pub fn parse_config_ulong(raw: &str) -> Result<u64, &'static str> {
    // git guards `*value == '-'` because `strtoumax` would otherwise negate and
    // wrap a negative into a huge unsigned. Trimming first folds ` -1` in with
    // `-1`, matching git's rejection of both.
    let (negative, magnitude) = split_sign(raw);
    if negative {
        return Err(INVALID_UNIT);
    }
    parse_magnitude(magnitude)
}

/// git's `git_parse_int`, the parser behind `git_config_int` and hence behind the
/// plain-integer config values (`core.maxTreeDepth`, `gc.auto`, …). Identical to
/// [`parse_config_ulong`] except that `strtoimax` accepts a leading `-`, so a
/// negative value is a *number* rather than an invalid unit — which is why
/// `core.maxTreeDepth=-1` rejects every tree instead of dying.
pub fn parse_config_int(raw: &str) -> Result<i64, &'static str> {
    let (negative, magnitude) = split_sign(raw);
    let value = parse_magnitude(magnitude)?;
    let signed = i64::try_from(value).map_err(|_| OUT_OF_RANGE)?;
    Ok(if negative { -signed } else { signed })
}

/// config.c's `iskeychar()`: the bytes a section or variable name may contain.
/// git's `isalnum` is its own ASCII-only table (`ctype.c`), not the locale's, so
/// a non-ASCII byte is never a key character no matter what `LC_CTYPE` says.
fn is_key_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-'
}

/// Port of config.c's `git_config_parse_key()`: sanity-check `key` and return it
/// canonicalized — section and variable name lower-cased, the extended basename
/// (the `[section "Sub Section"]` part between the first and last dot) left
/// exactly as typed.
///
/// The error strings are git's own `error()` text, printed by the caller as
/// `error: <text>` before it dies with `unable to parse command-line config`:
///
/// * `key does not contain a section: <key>` — no dot, or a leading one
///   (`last_dot == NULL || last_dot == key`).
/// * `key does not contain variable name: <key>` — a trailing dot (`!last_dot[1]`).
/// * `invalid key: <key>` — a byte outside [`is_key_char`] in the section or the
///   variable name, or a variable name whose first byte is not a letter.
/// * `invalid key (newline): <key>` — a newline inside the extended basename,
///   which is the one byte git rejects there.
pub fn parse_config_key(key: &str) -> Result<String, String> {
    let bytes = key.as_bytes();
    // `strrchr(key, '.')`, then git's `last_dot == NULL || last_dot == key`.
    let Some(baselen) = bytes.iter().rposition(|&c| c == b'.').filter(|&i| i != 0) else {
        return Err(format!("key does not contain a section: {key}"));
    };
    // `!last_dot[1]`: the dot is the last byte, so there is no variable name.
    if baselen + 1 >= bytes.len() {
        return Err(format!("key does not contain variable name: {key}"));
    }

    let mut canonical = Vec::with_capacity(bytes.len());
    let mut dot = false;
    for (i, &raw) in bytes.iter().enumerate() {
        let mut c = raw;
        if c == b'.' {
            dot = true;
        }
        // Everything before the first dot is the section and everything after
        // the last is the variable name; both are validated and lower-cased.
        // What lies between is the extended basename, left untouched.
        if !dot || i > baselen {
            if !is_key_char(c) || (i == baselen + 1 && !c.is_ascii_alphabetic()) {
                return Err(format!("invalid key: {key}"));
            }
            c = c.to_ascii_lowercase();
        } else if c == b'\n' {
            return Err(format!("invalid key (newline): {key}"));
        }
        canonical.push(c);
    }
    Ok(String::from_utf8_lossy(&canonical).into_owned())
}

/// The check config.c's `config_parse_pair()` runs over a `-c`/`--config-env`
/// key before [`parse_config_key`]: an empty key is its own diagnostic.
///
/// Returns `Ok(())` for a key git would accept, or the `error()` text it prints
/// for one it would not.
pub fn check_config_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("empty config key".to_string());
    }
    parse_config_key(key).map(|_| ())
}

/// The reason string git's `die_bad_number` prints for a value it cannot read.
const INVALID_UNIT: &str = "invalid unit";
/// The reason string git's `die_bad_number` prints for a value that overflows.
const OUT_OF_RANGE: &str = "out of range";

/// Strip leading ASCII whitespace and an optional sign, as `strtoimax` does.
fn split_sign(raw: &str) -> (bool, &str) {
    let rest = raw.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    match rest.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, rest.strip_prefix('+').unwrap_or(rest)),
    }
}

/// `strtoumax` with base 0 followed by `get_unit_factor`: the unsigned magnitude
/// shared by git's int and ulong config parsers.
fn parse_magnitude(rest: &str) -> Result<u64, &'static str> {
    let (radix, digits) = if let Some(r) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        (16u32, r)
    } else if rest.len() > 1 && rest.starts_with('0') {
        // Base 0 reads a leading zero as octal, and the zero is part of the
        // number: `0k` is 0 with a `k` suffix, not an empty number, so the `0` is
        // kept rather than stripped.
        (8, rest)
    } else {
        (10, rest)
    };

    let split = digits.find(|c: char| !c.is_digit(radix)).unwrap_or(digits.len());
    let (number, tail) = digits.split_at(split);
    if number.is_empty() {
        return Err(INVALID_UNIT);
    }
    let value = u64::from_str_radix(number, radix).map_err(|_| OUT_OF_RANGE)?;

    // `get_unit_factor`: an empty tail scales by one, one k/m/g byte scales and
    // must end the string, anything else is not a unit.
    let factor: u64 = match tail.as_bytes() {
        [] => 1,
        [b'k' | b'K'] => 1024,
        [b'm' | b'M'] => 1024 * 1024,
        [b'g' | b'G'] => 1024 * 1024 * 1024,
        _ => return Err(INVALID_UNIT),
    };
    value.checked_mul(factor).ok_or(OUT_OF_RANGE)
}

/// The last value configured for the dotted `<section>.<key>` anywhere in the
/// merged config, paired with git's origin clause for the diagnostic it prints
/// when the value is unreadable.
///
/// The merged config is walked in order and the last match kept, reproducing git's
/// last-value-wins, and the winning value's source is carried so a rejection can
/// name it exactly as `git_config_ulong` does: a file-backed value adds
/// ` in file <path>` (git renders the repository config as `.git/config`, so the
/// leading `./` gitoxide reports is trimmed), while a value from `-c`/environment
/// adds nothing — matching git 2.55.0's output for both sources.
///
/// Only the sub-section-less form is understood, which is all any of the keys read
/// through here have; a `key` containing no `.` yields `None`.
pub fn last_value_with_origin(repo: &gix::Repository, key: &str) -> Option<(String, String)> {
    use gix::bstr::ByteSlice as _;

    let (section, name) = key.split_once('.')?;
    let config = repo.config_snapshot().plumbing().clone();
    let mut found: Option<(String, Option<PathBuf>)> = None;
    for sec in config.sections() {
        let header = sec.header();
        if header.subsection_name().is_some() || !header.name().to_string().eq_ignore_ascii_case(section) {
            continue;
        }
        let path = sec.meta().path.clone();
        for value in sec.body().values(name) {
            found = Some((value.to_str_lossy().into_owned(), path.clone()));
        }
    }
    let (raw, path) = found?;
    let origin = match path {
        Some(p) => {
            let shown = p.to_string_lossy();
            format!(" in file {}", shown.strip_prefix("./").unwrap_or(&shown))
        }
        None => String::new(),
    };
    Some((raw, origin))
}

/// git's `git_config_ulong` for the dotted `key`: `Ok(None)` when unset,
/// `Ok(Some(v))` for a value git can read, and `Err(<message>)` carrying the exact
/// `fatal:` line git dies with otherwise — `bad numeric config value '<raw>' for
/// '<key>'<origin>: <reason>`, with the key lowercased the way git's config reader
/// has already normalised it by the time `die_bad_number` sees it.
///
/// Callers print the message and exit 128, which is what `die()` does.
pub fn config_ulong(repo: &gix::Repository, key: &str) -> Result<Option<u64>, String> {
    let Some((raw, origin)) = last_value_with_origin(repo, key) else {
        return Ok(None);
    };
    match parse_config_ulong(&raw) {
        Ok(v) => Ok(Some(v)),
        Err(reason) => Err(bad_number(&raw, key, &origin, reason)),
    }
}

/// git's `git_config_int` for the dotted `key`, the signed counterpart of
/// [`config_ulong`] with the same `Err` contract.
pub fn config_int(repo: &gix::Repository, key: &str) -> Result<Option<i64>, String> {
    let Some((raw, origin)) = last_value_with_origin(repo, key) else {
        return Ok(None);
    };
    match parse_config_int(&raw) {
        Ok(v) => Ok(Some(v)),
        Err(reason) => Err(bad_number(&raw, key, &origin, reason)),
    }
}

/// git's `die_bad_number` message, minus the `fatal: ` prefix the caller adds.
/// The key is lowercased because git's config reader has already normalised it by
/// the time the number is parsed.
fn bad_number(raw: &str, key: &str, origin: &str, reason: &str) -> String {
    format!(
        "bad numeric config value '{raw}' for '{}'{origin}: {reason}",
        key.to_lowercase()
    )
}

/// The index-write options `repo`'s configuration asks for.
///
/// Two keys land here, both verified against git 2.55.0 by looking at the bytes
/// of the index it produced:
///
/// * `index.recordEndOfIndexEntries` — whether to append the `EOIE` extension.
///   git's documented default is "true if `index.threads` has been explicitly
///   enabled, false otherwise", and "enabled" means a threads value that is not
///   `1`/`false`: `index.threads` of `0`, `true`, `2` and `4` each produce an
///   `EOIE`, while `1`, `false` and an unset value do not.
/// * `index.skipHash` — whether to zero the index's trailing checksum instead of
///   computing it.
///
/// Note that `EOIE` is only ever appended when some *other* extension was
/// written, since it exists to record where the extensions begin; an index
/// carrying neither a tree-cache nor the sparse marker has none either way.
pub fn index_write_options(repo: &gix::Repository) -> gix::index::write::Options {
    use gix::bstr::ByteSlice as _;

    let snap = repo.config_snapshot();
    // `git_config_get_index_threads`: `true` means "one per core" (0), `false`
    // means "one thread" (1), and anything else is the literal count.
    let threads_enabled = snap
        .string("index.threads")
        .and_then(|v| v.to_str().ok().map(str::to_owned))
        .map(|v| match v.as_str() {
            "true" | "yes" | "on" => true,
            "false" | "no" | "off" | "" => false,
            other => other.trim().parse::<i64>().map(|n| n != 1).unwrap_or(false),
        })
        .unwrap_or(false);
    let end_of_index_entry = snap
        .boolean("index.recordEndOfIndexEntries")
        .unwrap_or(threads_enabled);

    gix::index::write::Options {
        extensions: gix::index::write::Extensions::Given {
            // The tree-cache is written whenever the caller left one in place;
            // only `EOIE` is under config control here.
            tree_cache: true,
            end_of_index_entry,
        },
        skip_hash: snap.boolean("index.skipHash").unwrap_or(false),
    }
}

/// One repository component that `core.fsync` can harden, as named in
/// `git-config(1)`. The aggregates (`objects`, `derived-metadata`, `committed`,
/// `added`, `all`) expand into sets of these.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum FsyncComponent {
    /// `loose-object` — objects added to the repo in loose-object form.
    LooseObject,
    /// `pack` — objects added to the repo in packfile form.
    Pack,
    /// `pack-metadata` — packfile bitmaps and indexes (and the `.rev`/`.mtimes`
    /// sidecars written beside them).
    PackMetadata,
    /// `commit-graph` — the commit-graph file.
    CommitGraph,
    /// `index` — the index, when it is modified.
    Index,
    /// `reference` — references modified in the repo.
    Reference,
}

impl FsyncComponent {
    /// The single bit this component occupies in a [`FsyncPolicy`]'s set.
    const fn bit(self) -> u8 {
        match self {
            Self::LooseObject => 1 << 0,
            Self::Pack => 1 << 1,
            Self::PackMetadata => 1 << 2,
            Self::CommitGraph => 1 << 3,
            Self::Index => 1 << 4,
            Self::Reference => 1 << 5,
        }
    }
}

/// `objects` — `loose-object,pack`.
const FSYNC_OBJECTS: u8 = FsyncComponent::LooseObject.bit() | FsyncComponent::Pack.bit();
/// `derived-metadata` — `pack-metadata,commit-graph`.
const FSYNC_DERIVED_METADATA: u8 = FsyncComponent::PackMetadata.bit() | FsyncComponent::CommitGraph.bit();
/// `committed` — currently equivalent to `objects`.
const FSYNC_COMMITTED: u8 = FSYNC_OBJECTS;
/// `added` — `committed,index`.
const FSYNC_ADDED: u8 = FSYNC_COMMITTED | FsyncComponent::Index.bit();
/// `all` — every individual component.
const FSYNC_ALL: u8 = FSYNC_OBJECTS | FSYNC_DERIVED_METADATA | FsyncComponent::Index.bit() | FsyncComponent::Reference.bit();
/// The platform default, documented as `committed,-loose-object`.
const FSYNC_DEFAULT: u8 = FSYNC_COMMITTED & !FsyncComponent::LooseObject.bit();

/// How `core.fsyncMethod` says a hardened file should be flushed.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum FsyncMethod {
    /// `fsync` — the `fsync()` system call or the platform equivalent. On macOS
    /// the platform equivalent of a *durable* flush is `fcntl(F_FULLFSYNC)`,
    /// which is what git issues there and what [`FsyncPolicy::harden`] issues too.
    Fsync,
    /// `writeout-only` — a pagecache writeback request without waiting for the
    /// disk cache to flush (`fcntl(F_BARRIERFSYNC)` on macOS, `fdatasync()`
    /// elsewhere). git documents this as the default on macOS.
    WriteoutOnly,
    /// `batch` — writeout-only flushes staged behind a single full fsync at the
    /// end. git documents this as applying to loose object files only, with
    /// everything else hardened "as if `fsync` was specified"; nothing here writes
    /// loose objects in a batch, so it behaves as [`FsyncMethod::Fsync`].
    Batch,
}

/// The resolved `core.fsync` / `core.fsyncMethod` hardening policy: which
/// components get flushed, and how.
///
/// Reproduced from `git-config(1)`'s `core.fsync` entry (git 2.55.0), including
/// its two diagnostics, both verified byte-for-byte:
///
/// ```text
/// $ git -c core.fsync=bogus write-tree
/// warning: ignoring unknown core.fsync component 'bogus'
/// $ git -c core.fsyncMethod=bogus write-tree
/// warning: ignoring unknown core.fsyncMethod value 'bogus'
/// ```
///
/// Both are warnings: an unreadable component or method is dropped and the run
/// continues. `core.fsyncObjectFiles` is handled by [`FsyncPolicy::load`] too,
/// since git reads it in the same place.
#[derive(Copy, Clone, Debug)]
pub struct FsyncPolicy {
    /// The set of [`FsyncComponent`] bits to harden.
    components: u8,
    /// How to harden them.
    method: FsyncMethod,
}

impl Default for FsyncPolicy {
    /// The platform default: `committed,-loose-object` hardened with the method
    /// git defaults to on this platform (`writeout-only` on macOS, `fsync`
    /// elsewhere).
    fn default() -> Self {
        Self {
            components: FSYNC_DEFAULT,
            method: if cfg!(target_os = "macos") {
                FsyncMethod::WriteoutOnly
            } else {
                FsyncMethod::Fsync
            },
        }
    }
}

impl FsyncPolicy {
    /// Read `core.fsync`, `core.fsyncMethod` and `core.fsyncObjectFiles` from
    /// `repo`, warning on stderr exactly as git does for each unreadable piece.
    ///
    /// `Err(<message>)` carries the one `fatal:` line git dies with here — a
    /// `core.fsyncObjectFiles` value that is not a boolean — for the caller to
    /// print before exiting 128.
    pub fn load(repo: &gix::Repository) -> Result<Self, String> {
        let mut policy = Self::default();
        let snap = repo.config_snapshot();

        // Deprecated, but still read: git warns whenever it is set at all, and
        // then dies if the value is not a boolean.
        if let Some((raw, origin)) = last_value_with_origin(repo, "core.fsyncObjectFiles") {
            eprintln!("warning: core.fsyncObjectFiles is deprecated; use core.fsync instead");
            match snap.boolean("core.fsyncObjectFiles") {
                Some(true) => policy.components |= FsyncComponent::LooseObject.bit(),
                Some(false) => policy.components &= !FsyncComponent::LooseObject.bit(),
                None => {
                    return Err(format!(
                        "bad boolean config value '{raw}' for 'core.fsyncobjectfiles'{origin}"
                    ))
                }
            }
        }

        // `core.fsync` layers onto the platform default rather than replacing it:
        // the set starts at the default, `-<name>` removes, a bare `<name>` adds,
        // and `none` resets to empty. An empty value is the platform default.
        if let Some((raw, _)) = last_value_with_origin(repo, "core.fsync") {
            policy.components = parse_fsync_components(&raw, policy.components);
        }

        if let Some((raw, _)) = last_value_with_origin(repo, "core.fsyncMethod") {
            match raw.as_str() {
                "fsync" => policy.method = FsyncMethod::Fsync,
                "writeout-only" => policy.method = FsyncMethod::WriteoutOnly,
                "batch" => policy.method = FsyncMethod::Batch,
                other => eprintln!("warning: ignoring unknown core.fsyncMethod value '{other}'"),
            }
        }

        Ok(policy)
    }

    /// Whether `component` is in the hardened set.
    pub fn hardens(&self, component: FsyncComponent) -> bool {
        self.components & component.bit() != 0
    }

    /// Flush `file` if `component` is hardened, using the configured method.
    ///
    /// Errors are deliberately swallowed the way git's `fsync_or_die` cannot be:
    /// a filesystem that rejects the flush (a `tmpfs`, a network mount) must not
    /// turn a successful write into a failure, so the durability guarantee is
    /// best-effort and the write itself still stands.
    pub fn harden(&self, component: FsyncComponent, file: &std::fs::File) {
        if !self.hardens(component) {
            return;
        }
        match self.method {
            // `batch` applies to loose objects only; everything else is hardened
            // "as if fsync was specified", and nothing here batches.
            FsyncMethod::Fsync | FsyncMethod::Batch => full_fsync(file),
            FsyncMethod::WriteoutOnly => writeout_only(file),
        }
    }

    /// [`FsyncPolicy::harden`] for a file named by path — the shape every writer
    /// that closes its handle before this point (a pack, an index) needs.
    pub fn harden_path(&self, component: FsyncComponent, path: &std::path::Path) {
        if !self.hardens(component) {
            return;
        }
        if let Ok(file) = std::fs::File::open(path) {
            self.harden(component, &file);
        }
    }
}

/// git's `git_parse_fsync_components`: a comma/whitespace separated component
/// list layered onto `start`. `none` clears the set outright, `-<name>` removes a
/// component, a bare `<name>` adds one, and an unknown name warns and is skipped.
fn parse_fsync_components(raw: &str, start: u8) -> u8 {
    let mut bits = start;
    for token in raw.split([',', ' ', '\t', '\n', '\r']).filter(|t| !t.is_empty()) {
        if token == "none" {
            return 0;
        }
        let (negated, name) = match token.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, token),
        };
        let mask = match name {
            "loose-object" => FsyncComponent::LooseObject.bit(),
            "pack" => FsyncComponent::Pack.bit(),
            "pack-metadata" => FsyncComponent::PackMetadata.bit(),
            "commit-graph" => FsyncComponent::CommitGraph.bit(),
            "index" => FsyncComponent::Index.bit(),
            "reference" => FsyncComponent::Reference.bit(),
            "objects" => FSYNC_OBJECTS,
            "derived-metadata" => FSYNC_DERIVED_METADATA,
            "committed" => FSYNC_COMMITTED,
            "added" => FSYNC_ADDED,
            "all" => FSYNC_ALL,
            other => {
                eprintln!("warning: ignoring unknown core.fsync component '{other}'");
                continue;
            }
        };
        if negated {
            bits &= !mask;
        } else {
            bits |= mask;
        }
    }
    bits
}

/// A durable flush: `fcntl(F_FULLFSYNC)` on macOS, where plain `fsync()` only
/// reaches the drive's write cache, and `fsync()` everywhere else.
fn full_fsync(file: &std::fs::File) {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::io::AsRawFd;
        // SAFETY: `fcntl` with `F_FULLFSYNC` takes no argument beyond the
        // descriptor, and the descriptor is owned by the borrowed `File`.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } == 0 {
            return;
        }
        // Not every filesystem implements it; git falls back to `fsync()`.
    }
    let _ = file.sync_all();
}

/// A writeout-only flush: `fcntl(F_BARRIERFSYNC)` on macOS, which orders the
/// write against later ones without waiting for the disk cache, and `fdatasync()`
/// elsewhere (`File::sync_data`).
fn writeout_only(file: &std::fs::File) {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::io::AsRawFd;
        /// `F_BARRIERFSYNC` from `<sys/fcntl.h>`; the `libc` crate does not
        /// export it, and its value is part of the macOS kernel ABI.
        const F_BARRIERFSYNC: libc::c_int = 85;
        // SAFETY: as in `full_fsync` — a no-argument `fcntl` on a borrowed fd.
        if unsafe { libc::fcntl(file.as_raw_fd(), F_BARRIERFSYNC) } == 0 {
            return;
        }
    }
    let _ = file.sync_data();
}

#[cfg(test)]
mod stock_config_tests {
    use super::*;

    /// git's `git_parse_ulong` grammar, one value at a time against the
    /// diagnostics git 2.55.0 prints for `gc.maxcruftsize`.
    #[test]
    fn config_ulong_grammar_matches_git() {
        assert_eq!(parse_config_ulong("1k"), Ok(1024));
        assert_eq!(parse_config_ulong("0x400"), Ok(1024));
        assert_eq!(parse_config_ulong("010"), Ok(8));
        assert_eq!(parse_config_ulong("0k"), Ok(0));
        assert_eq!(parse_config_ulong("2m"), Ok(2 * 1024 * 1024));
        assert_eq!(parse_config_ulong("-1"), Err("invalid unit"));
        assert_eq!(parse_config_ulong("1.5"), Err("invalid unit"));
        assert_eq!(parse_config_ulong("1x"), Err("invalid unit"));
        assert_eq!(parse_config_ulong(""), Err("invalid unit"));
        assert_eq!(parse_config_ulong("5 "), Err("invalid unit"));
        assert_eq!(parse_config_ulong(&"9".repeat(24)), Err("out of range"));
    }

    /// The signed parser differs from the unsigned one in exactly one place: a
    /// leading `-` is a sign, not junk. `core.maxTreeDepth=-1` depends on it —
    /// git accepts the value and then rejects every tree with it.
    #[test]
    fn config_int_accepts_the_negatives_ulong_rejects() {
        assert_eq!(parse_config_int("-1"), Ok(-1));
        assert_eq!(parse_config_int("-1k"), Ok(-1024));
        assert_eq!(parse_config_int("2048"), Ok(2048));
        assert_eq!(parse_config_int("abc"), Err("invalid unit"));
        assert_eq!(parse_config_int(&"9".repeat(24)), Err("out of range"));
    }

    /// `core.fsync` layers onto the platform default: `-<name>` removes,
    /// `<name>` adds, an aggregate expands, and `none` clears everything.
    #[test]
    fn fsync_components_layer_onto_the_platform_default() {
        let index = FsyncComponent::Index.bit();
        let loose = FsyncComponent::LooseObject.bit();

        // The documented platform default is `committed,-loose-object`, which
        // leaves `pack` alone — the index is NOT hardened unless asked for.
        assert_eq!(FSYNC_DEFAULT, FsyncComponent::Pack.bit());

        assert_eq!(parse_fsync_components("index", FSYNC_DEFAULT), FSYNC_DEFAULT | index);
        assert_eq!(parse_fsync_components("-pack", FSYNC_DEFAULT), 0);
        assert_eq!(parse_fsync_components("none", FSYNC_ALL), 0);
        assert_eq!(parse_fsync_components("all", 0), FSYNC_ALL);
        assert_eq!(parse_fsync_components("added", 0), FSYNC_COMMITTED | index);
        // `all,-loose-object` is `all` minus one bit, order-sensitively.
        assert_eq!(parse_fsync_components("all,-loose-object", 0), FSYNC_ALL & !loose);
        // An unknown component is skipped, not fatal, and the rest still applies.
        assert_eq!(parse_fsync_components("bogus,index", 0), index);
    }
}
