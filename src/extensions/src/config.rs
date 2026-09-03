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
//! so the same key is parsed and diagnosed the same way everywhere:
//!
//! * the value parsers — [`parse_config_ulong`] / [`parse_config_int`] (git's
//!   `git_parse_ulong`/`git_parse_int`) and [`parse_config_key`] /
//!   [`check_config_key`] (`git_config_parse_key`);
//! * the *lookup* readers, which keep the last value the way
//!   `repo_config_get_*` does: [`last_value_with_origin`],
//!   [`last_value_implicit`], [`config_ulong`], [`config_int`];
//! * the *callback* reader, [`walk_config`], which hands back every configured
//!   value in parse order with the origin each of git's two source-naming
//!   diagnostics needs ([`ValueOrigin`]). That is what the ports of
//!   `git_default_config` and the callbacks stacked on it are built from —
//!   [`crate::default_config`], [`crate::diff_config`], [`crate::status_config`],
//!   [`crate::log_config`], [`crate::cmd_config`];
//! * and [`FsyncPolicy`] (`core.fsync`, `core.fsyncMethod`,
//!   `core.fsyncObjectFiles`), which is read at the write site rather than at
//!   parse time — see [`crate::default_config`] for why.

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
    match crate::setup::discover() {
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

/// The last value configured for the dotted `<section>.<key>`, distinguishing a
/// key gitoxide reports as having **no** value from one whose value is empty.
///
/// `None` for a key that is not set anywhere, `Some(None)` for the valueless form,
/// `Some(Some(v))` for a value. [`last_value_with_origin`] flattens the first two
/// together, which is right for the numeric readers (they treat both as
/// unreadable) and wrong for a reader that has to tell git's `NULL` value from
/// git's `""` — `git_config_bool(var, NULL)` is *true* (`parse.c:168-169`) while
/// `git_config_bool(var, "")` is false, and `git_config_string(var, NULL)` is
/// `config_error_nonbool` while `""` is an empty string.
///
/// # How the distinction survives the parser
///
/// gitoxide's parser emits an empty `Event::Value` for a key with no `=`
/// (`gix-config/src/parse/from_bytes/mod.rs:294-303`), so the raw event stream
/// looks the same for both spellings — but it is the event's *position* that
/// carries the difference, and `key_and_value_range_by_in`
/// (`gix-config/src/file/section/body.rs:182-217`) reads it: a `Value` sitting
/// directly after the name event is the valueless form and reports no value at
/// all. That is what `value_implicit()` returns and what this function forwards.
/// The plainer `values()` accessor does not — it renders the same occurrence as
/// an empty string — so a reader that needs the distinction has to come through
/// here.
///
/// Only the **last** occurrence of a name within one section is classified this
/// way; earlier repeats of the same name are only reachable through `values()`.
pub fn last_value_implicit(repo: &gix::Repository, key: &str) -> Option<Option<String>> {
    use gix::bstr::ByteSlice as _;

    let (section, name) = key.split_once('.')?;
    let config = repo.config_snapshot().plumbing().clone();
    let mut found: Option<Option<String>> = None;
    for sec in config.sections() {
        let header = sec.header();
        if header.subsection_name().is_some() || !header.name().to_string().eq_ignore_ascii_case(section) {
            continue;
        }
        if let Some(v) = sec.body().value_implicit(name) {
            found = Some(v.map(|raw| raw.to_str_lossy().into_owned()));
        }
    }
    found
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
    config_int_named(repo, key, &key.to_lowercase())
}

/// [`config_int`] for the callers whose diagnostic does **not** lower-case the key.
///
/// `die_bad_number()` prints whatever string its caller passed as the variable
/// name, and that string is a literal in the C source rather than the normalised
/// key the parser produced. Most of them are written lowercase, so lower-casing is
/// the right default — but not all: `parallel-checkout.c:60,65` spell
/// `checkout.workers` and `checkout.thresholdForParallelism` in camelCase, and git
/// 2.55.0 reports them that way verbatim:
///
/// ```text
/// fatal: bad numeric config value 'bogus' for 'checkout.thresholdForParallelism': invalid unit
/// ```
///
/// `key` is what is looked up (case-insensitively, as always); `reported_as` is
/// what the message says.
pub fn config_int_named(
    repo: &gix::Repository,
    key: &str,
    reported_as: &str,
) -> Result<Option<i64>, String> {
    let Some((raw, origin)) = last_value_with_origin(repo, key) else {
        return Ok(None);
    };
    match parse_config_int(&raw) {
        Ok(v) => Ok(Some(v)),
        Err(reason) => Err(format!(
            "bad numeric config value '{raw}' for '{reported_as}'{origin}: {reason}"
        )),
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
///   computing it. Its *default* is not a constant: `feature.manyFiles=true`
///   flips it on (`repo-settings.c:59-63`, then `:79`, which passes the cascaded
///   value in as this key's fallback), so the trailer of an index written under
///   `feature.manyFiles` is twenty zero bytes with no `index.skipHash` set
///   anywhere. That cascade is resolved by [`crate::repo_settings::RepoSettings`],
///   which also validates the two `feature.*` booleans the way git does.
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
        // `do_write_index()` only chooses a version for a state that has none;
        // an index read off disk and rewritten keeps its own. The commands that
        // build a state from scratch say so with [`index_write_options_fresh`].
        version: None,
        extensions: gix::index::write::Extensions::Given {
            // The tree-cache is written whenever the caller left one in place;
            // only `EOIE` is under config control here.
            tree_cache: true,
            end_of_index_entry,
        },
        // `index.skipHash` over the `feature.manyFiles` cascade. A repository whose
        // settings block cannot be resolved has already been refused by the
        // dispatcher's gate for every verb that writes an index, so an `Err` here
        // can only come from a verb outside that list; fall back to git's
        // unconfigured default rather than fail an index write over it.
        skip_hash: crate::repo_settings::RepoSettings::load(repo)
            .map(|s| s.index_skip_hash)
            .unwrap_or(false),
    }
}

/// [`index_write_options`] for an index state git would have built from scratch,
/// so the version has to be chosen rather than carried over.
///
/// `unpack_trees()` hands `do_write_index()` a result state whose `version` is
/// zero whenever the command never read the index it replaces — plain
/// `read-tree <tree-ish>` and `read-tree --empty` are exactly that, while
/// `--reset` / `-m` / `--prefix` read the index first (builtin/read-tree.c:236)
/// and so carry its version through. Only the zero case reaches
/// [`index_format_default`].
pub fn index_write_options_fresh(repo: &gix::Repository) -> gix::index::write::Options {
    gix::index::write::Options {
        version: Some(index_format_default(repo)),
        ..index_write_options(repo)
    }
}

/// `INDEX_FORMAT_LB` / `INDEX_FORMAT_UB` / `INDEX_FORMAT_DEFAULT`
/// (`read-cache.h:9-11`): the supported range, and the version git falls back to
/// when a request lands outside it.
const INDEX_FORMAT_LB: i64 = 2;
const INDEX_FORMAT_UB: i64 = 4;
const INDEX_FORMAT_DEFAULT: i64 = 3;

/// `get_index_format_default()` (read-cache.c:2830-2861): the version git writes
/// for a state that carries none of its own.
///
/// ```c
/// char *envversion = getenv("GIT_INDEX_VERSION");
/// unsigned int version = INDEX_FORMAT_DEFAULT;
///
/// if (!envversion) {
///         prepare_repo_settings(r);
///         if (r->settings.index_version >= 0)
///                 version = r->settings.index_version;
///         if (version < INDEX_FORMAT_LB || INDEX_FORMAT_UB < version) {
///                 warning(_("index.version set, but the value is invalid.\n"
///                           "Using version %i"), INDEX_FORMAT_DEFAULT);
///                 return INDEX_FORMAT_DEFAULT;
///         }
///         return version;
/// }
///
/// version = strtoul(envversion, &endp, 10);
/// if (*endp || version < INDEX_FORMAT_LB || INDEX_FORMAT_UB < version) {
///         warning(_("GIT_INDEX_VERSION set, but the value is invalid.\n"
///                   "Using version %i"), INDEX_FORMAT_DEFAULT);
///         return INDEX_FORMAT_DEFAULT;
/// }
/// return version;
/// ```
///
/// Two things follow from the shape of that function and are reproduced here:
/// the environment variable is consulted *instead of* the configuration rather
/// than before it — a `GIT_INDEX_VERSION` that is set decides the answer even
/// when it is nonsense — and version 3 is what an invalid request lands on,
/// which the writer then demotes to 2 unless an entry needs the extended flags.
///
/// `r->settings.index_version` is `-1` until `feature.manyFiles` sets it to 4
/// (`repo-settings.c:59`) and `index.version` overrides whatever it holds.
pub fn index_format_default(repo: &gix::Repository) -> gix::index::Version {
    let version = match std::env::var("GIT_INDEX_VERSION") {
        Ok(raw) => match raw.parse::<i64>() {
            Ok(n) if (INDEX_FORMAT_LB..=INDEX_FORMAT_UB).contains(&n) => n,
            _ => {
                eprintln!(
                    "warning: GIT_INDEX_VERSION set, but the value is invalid.\nUsing version {INDEX_FORMAT_DEFAULT}"
                );
                INDEX_FORMAT_DEFAULT
            }
        },
        Err(_) => {
            // The `feature.manyFiles` cascade, then the key that overrides it.
            // A `feature.*` value git would have died on has already stopped the
            // command in the dispatcher's settings gate, so an unreadable one
            // here can only mean the key is absent.
            let cascaded = crate::repo_settings::RepoSettings::load(repo)
                .map(|s| s.many_files)
                .unwrap_or(false)
                .then_some(4);
            let configured = config_int_named(repo, "index.version", "index.version")
                .ok()
                .flatten()
                .or(cascaded);
            match configured {
                None => INDEX_FORMAT_DEFAULT,
                Some(n) if (INDEX_FORMAT_LB..=INDEX_FORMAT_UB).contains(&n) => n,
                Some(_) => {
                    eprintln!(
                        "warning: index.version set, but the value is invalid.\nUsing version {INDEX_FORMAT_DEFAULT}"
                    );
                    INDEX_FORMAT_DEFAULT
                }
            }
        }
    };
    index_version(version)
}

/// One of git's three index versions as `gix` spells it. Anything outside the
/// supported range has already been mapped onto [`INDEX_FORMAT_DEFAULT`].
pub fn index_version(version: i64) -> gix::index::Version {
    match version {
        2 => gix::index::Version::V2,
        4 => gix::index::Version::V4,
        _ => gix::index::Version::V3,
    }
}

/// `add_patterns_from_file()` (dir.c:1013), the `core.excludesFile` half of
/// `setup_standard_excludes()`:
///
/// ```c
/// void add_patterns_from_file(struct dir_struct *dir, const char *fname)
/// {
///         if (add_patterns(fname, "", 0, &dir->exclude_list_group[EXC_FILE].pl[0],
///                          NULL, 0, NULL) < 0)
///                 die(_("cannot use %s as an exclude file"), fname);
/// }
/// ```
///
/// `add_patterns()` answers `-1` for a path it cannot read, and a *missing* file
/// is not one of those: `warn_on_fopen_errors()` stays silent for `ENOENT` and
/// git carries on with no exclude file at all. A **directory** is one — the
/// `open()` succeeds and the read does not — which is why
/// `git -c core.excludesFile=<dir> status` dies before it prints a line, with no
/// `--ignored` needed to provoke it.
///
/// Returns the `fatal:` line to print, or `None` when the exclude file is usable
/// (or absent). The path is named as configured, which is what git's `fname`
/// holds after `expand_user_path()` leaves a relative path alone.
pub fn excludes_file_fatal(repo: &gix::Repository) -> Option<String> {
    use gix::bstr::ByteSlice;
    let snapshot = repo.config_snapshot();
    let raw = snapshot.string("core.excludesFile")?;
    let shown = raw.to_str_lossy().into_owned();
    if shown.is_empty() {
        return None;
    }
    let path = gix::path::from_bstr(gix::bstr::BStr::new(&raw)).into_owned();
    let path = match path.strip_prefix("~") {
        Ok(rest) => match std::env::var_os("HOME") {
            Some(home) => std::path::PathBuf::from(home).join(rest),
            None => path,
        },
        Err(_) => path,
    };
    match std::fs::read(&path) {
        Ok(_) => None,
        // `ENOENT` is git's silent case: no exclude file, no diagnostic.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => Some(format!("cannot use {shown} as an exclude file")),
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

// ---------------------------------------------------------------------------
// The config callback walk: every occurrence, in parse order, with its origin
// ---------------------------------------------------------------------------

/// Where one configured value came from, in the terms `git_die_config_linenr()`
/// (config.c:2552-2559) reasons about.
///
/// ```c
/// NORETURN
/// void git_die_config_linenr(const char *key, const char *filename, int linenr)
/// {
///         if (!filename)
///                 die(_("unable to parse '%s' from command-line config"), key);
///         else
///                 die(_("bad config variable '%s' in file '%s' at line %d"),
///                     key, filename, linenr);
/// }
/// ```
///
/// The branch is on `kvi->filename` being `NULL`, which is exactly the
/// `CONFIG_ORIGIN_CMDLINE` case: `-c`, `--config-env`, `GIT_CONFIG_PARAMETERS`
/// and the `GIT_CONFIG_KEY_<n>`/`GIT_CONFIG_VALUE_<n>` pairs all arrive through
/// `git_config_from_parameters()` with no file behind them, and all three spell
/// themselves "command-line config" in the message no matter which one was used.
/// Verified against git 2.55.0 for `-c core.checkstat=bogus` and for the
/// `GIT_CONFIG_KEY_0`/`GIT_CONFIG_VALUE_0` pair — byte-identical output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueOrigin {
    /// `kvi->filename == NULL`.
    CommandLine,
    /// `CONFIG_ORIGIN_FILE`, with the line the variable sits on.
    File {
        /// The path as git names it — `.git/config` for the repository config.
        path: String,
        /// 1-based line number, which is what `cs->linenr` counts.
        line: usize,
    },
}

impl ValueOrigin {
    /// `git_die_config_linenr(key, …)`, minus the `fatal: ` prefix `die()` adds.
    ///
    /// This is the *second* line of the two-line refusal git prints when a config
    /// callback returns negative rather than dying itself: the callback's own
    /// `error()` text comes first, then `configset_iter()` (config.c:1654-1673)
    /// turns the negative return into this.
    pub fn die_linenr(&self, key: &str) -> String {
        match self {
            ValueOrigin::CommandLine => {
                format!("unable to parse '{key}' from command-line config")
            }
            ValueOrigin::File { path, line } => {
                format!("bad config variable '{key}' in file '{path}' at line {line}")
            }
        }
    }

    /// The origin clause `die_bad_number()` (config.c:1188-1223) appends instead:
    /// ` in file <path>`, with no line number and no quotes, or nothing at all for
    /// a command-line value. Kept in step with [`last_value_with_origin`], which
    /// builds the same clause for the last-value-wins readers.
    pub fn bad_number_clause(&self) -> String {
        match self {
            ValueOrigin::CommandLine => String::new(),
            ValueOrigin::File { path, .. } => format!(" in file {path}"),
        }
    }
}

/// One `<key> <value>` pair as a config callback receives it.
#[derive(Clone, Debug)]
pub struct ConfigValue {
    /// The full dotted key, normalised the way git's parser normalises it before
    /// the callback sees it: section and variable name lower-cased, a
    /// `[section "Sub Section"]` subsection left exactly as written.
    pub key: String,
    /// `None` is git's `NULL` value — the `[core]\n\tbare\n` spelling with no `=`
    /// at all, which `git_config_bool` reads as *true* and `git_config_string`
    /// refuses with `config_error_nonbool`. `Some("")` is the distinct
    /// `bare =` spelling.
    pub value: Option<String>,
    /// Where it came from, for the two diagnostics that name their source.
    pub origin: ValueOrigin,
}

/// Every configured `<key> <value>` pair, in the order a config callback would be
/// handed them — git's `configset_iter()` (config.c:1654-1673).
///
/// # Why order, and why every occurrence
///
/// A callback-shaped reader is not a lookup. `repo_config(r, fn, data)` walks the
/// configset's insertion list and calls `fn` once per *value*, so a key spelled
/// twice is validated twice and the **first** bad spelling is the one that kills
/// the command — even when a later line would have overridden it. That is the
/// opposite of the targeted `repo_config_get_*` readers ([`config_ulong`],
/// [`config_int`], [`crate::repo_settings::config_bool_strict`]), which keep the
/// last value and never see the earlier ones. Both behaviours are real and both
/// were measured against git 2.55.0:
///
/// ```text
/// $ git -c core.createObject=bogus -c core.createObject=rename status -s
/// fatal: invalid mode for object creation: bogus
/// $ git -c core.packedGitLimit=bogus -c core.packedGitLimit=1m status -s
/// ?? b.bundle
/// ```
///
/// The insertion list is built as the sources are parsed, in
/// `do_git_config_sequence()` order (config.c:1570-1602): system, XDG, user,
/// repository, worktree, then everything from `-c`/environment. gitoxide's
/// snapshot presents its sections in that same order, so walking the sections and
/// their values in order reproduces it.
///
/// # The valueless form
///
/// gitoxide's parser emits an empty `Event::Value` for a name written without `=`,
/// so the two spellings look alike in `values()`; only `value_implicit()`
/// distinguishes them, and only for the **last** occurrence of a name in a section
/// (`key_and_value_range_by_in`, gix-config/src/file/section/body.rs:182-217,
/// scans backwards). So the last occurrence is classified through
/// `value_implicit()` and the earlier repeats are taken from `values()` — exact
/// for a section that spells a name once, which is every real config file.
///
/// # Line numbers
///
/// The snapshot carries each section's source path but not the line any value sits
/// on, and `bad config variable '<key>' in file '<path>' at line <n>` needs the
/// line. So each distinct file is re-parsed once through
/// [`gix::config::parse::Events`] to build [`FileLines`], and the walk asks it for
/// the line of the next occurrence of a given `(section, subsection, name)`.
/// Every configured value of `key`, in configuration order, with this port's
/// second delivery of a `-c key=value` discounted.
///
/// The plain `snapshot.values(key)` reader sees a valued command-line override
/// *twice* — see [`crate::setup::double_delivered`] — which is invisible to a
/// last-one-wins reader and wrong for every multi-valued key: `git -c
/// format.to=a format-patch` must write one `To:` header, not two. This is the
/// multi-value reader those keys need.
pub fn multi_values(repo: &gix::Repository, key: &str) -> Vec<String> {
    let wanted = normalize_key(key);
    walk_config(repo)
        .into_iter()
        .filter(|v| normalize_key(&v.key) == wanted)
        .filter_map(|v| v.value)
        .collect()
}

pub fn walk_config(repo: &gix::Repository) -> Vec<ConfigValue> {
    use gix::bstr::ByteSlice as _;
    use std::collections::HashMap;

    let config = repo.config_snapshot().plumbing().clone();
    let mut lines: HashMap<PathBuf, FileLines> = HashMap::new();
    let mut echoes = CliEcho::new();
    let mut out = Vec::new();

    for sec in config.sections() {
        let header = sec.header();
        let section = header.name().to_string().to_ascii_lowercase();
        let subsection = header
            .subsection_name()
            .map(|s| s.to_str_lossy().into_owned());
        let meta = sec.meta();
        let path = origin_path(meta);
        let body = sec.body();

        // Positional cursors, one per distinct value name in this section, so a
        // name repeated inside one section reads its values in order.
        let mut seen: HashMap<String, usize> = HashMap::new();
        for raw_name in body.value_names() {
            let name = raw_name.to_ascii_lowercase();
            let at = seen.entry(name.clone()).or_insert(0);
            let index = *at;
            *at += 1;

            let all = body.values(&name);
            let is_last = index + 1 == all.len();
            let value = if is_last && body.value_implicit(&name) == Some(None) {
                None
            } else {
                all.get(index).map(|v| v.to_str_lossy().into_owned())
            };

            let key = match &subsection {
                Some(sub) => format!("{section}.{sub}.{name}"),
                None => format!("{section}.{name}"),
            };
            // One `-c key=value` arrives on two sources; git's callback runs
            // once per configured value, so the second copy is not a second
            // occurrence. See [`CliEcho`].
            if echoes.is_echo(meta.source, &key, value.as_deref()) {
                continue;
            }
            let origin = match &path {
                None => ValueOrigin::CommandLine,
                Some(p) => {
                    let index = lines
                        .entry(p.clone())
                        .or_insert_with(|| FileLines::read(p));
                    match index.next_line(&section, subsection.as_deref(), &name) {
                        Some(line) => ValueOrigin::File {
                            path: shown_path(p),
                            line,
                        },
                        // A file whose text could not be re-parsed leaves the
                        // origin unknown; naming a line we did not find would be
                        // worse than naming none, and the command-line shape is
                        // the one git prints when it has no file to name.
                        None => ValueOrigin::CommandLine,
                    }
                }
            };
            out.push(ConfigValue { key, value, origin });
        }
    }
    out
}

/// The second copy of a `-c key=value`, discounted once per override.
///
/// `crate::setup::double_delivered` explains why there are two: a valued
/// command-line override is handed to `gix` on `Source::Cli` *and* written into
/// the `GIT_CONFIG_KEY_<n>` / `_VALUE_<n>` triple, so the merged snapshot holds
/// two sections carrying one setting. Resolution is unharmed — `Source::Cli`
/// outranks `Source::Env` — but a walk that counts occurrences sees double, and
/// git counts one: `git_config()` calls the callback once per configured value
/// and `git config --list` prints one line per configured value.
///
/// **The environment copy is the one that survives**, and the command-line copy
/// is the echo — the opposite of what the origin label would suggest. `gix`
/// parses a `cli_overrides` entry with its config-file parser, which drops
/// unquoted trailing blanks: `-c 'test.v=12 '` reaches the snapshot as `12` on
/// `Source::Cli`, and as `12 ` — byte for byte what argv held — through the
/// environment. git keeps the blank and refuses the value
/// (`bad numeric config value '12 ' for 'test.v': invalid unit`), so discounting
/// the environment copy silently made the port *accept* it. The cost is the
/// origin `--show-origin` prints for such a value, `environment:` where stock
/// says `command line:`. A wrong label is visible to whoever reads it; a
/// silently different value is not.
///
/// Both copies sort after every file, so keeping the environment one leaves the
/// last-one-wins order over file configuration exactly where it was.
///
/// Each override is discounted **once**, matched on the key and on the value
/// with surrounding blanks removed — the trailing blank is precisely what the
/// command-line channel dropped, so an exact comparison would never match.
pub(crate) struct CliEcho(std::collections::HashMap<(String, String), usize>);

impl CliEcho {
    pub(crate) fn new() -> Self {
        let mut pending: std::collections::HashMap<(String, String), usize> = Default::default();
        for (key, value) in crate::setup::double_delivered() {
            *pending.entry((normalize_key(key), value.trim().to_owned())).or_default() += 1;
        }
        CliEcho(pending)
    }

    /// Whether this occurrence is the echo rather than a setting of its own.
    /// Consumes the pending discount, so a repeated `-c` of one key still yields
    /// one occurrence per `-c`.
    pub(crate) fn is_echo(
        &mut self,
        source: gix::config::Source,
        key: &str,
        value: Option<&str>,
    ) -> bool {
        if self.0.is_empty() || source != gix::config::Source::Cli {
            return false;
        }
        // A bare `-c key` never reached the environment channel, so it has no
        // second copy and must keep its occurrence.
        let Some(value) = value else { return false };
        let entry = (normalize_key(key), value.trim().to_owned());
        match self.0.get_mut(&entry) {
            Some(count) if *count > 0 => {
                *count -= 1;
                true
            }
            _ => false,
        }
    }
}

/// A config key in the spelling the snapshot walk produces: section and value
/// name lower-cased, subsection left alone (`git_config_parse_key`).
fn normalize_key(key: &str) -> String {
    let Some((section, rest)) = key.split_once('.') else {
        return key.to_ascii_lowercase();
    };
    match rest.rsplit_once('.') {
        Some((subsection, name)) => format!(
            "{}.{subsection}.{}",
            section.to_ascii_lowercase(),
            name.to_ascii_lowercase()
        ),
        None => format!("{}.{}", section.to_ascii_lowercase(), rest.to_ascii_lowercase()),
    }
}

/// The file behind a section, or `None` for the `-c`/environment sources git
/// reports as command-line config.
///
/// `Source::Cli` and `Source::Env` are the two gitoxide assigns to values that
/// reach the snapshot without a file, which is exactly `CONFIG_ORIGIN_CMDLINE`.
/// A section that has neither a recognised source nor a path is treated the same
/// way, since there is no file name to print.
fn origin_path(meta: &gix::config::file::Metadata) -> Option<PathBuf> {
    use gix::config::Source;

    match meta.source {
        Source::Cli | Source::Env => None,
        _ => meta.path.clone(),
    }
}

/// The path as git prints it: gitoxide reports the repository config as
/// `./.git/config`, git as `.git/config`.
fn shown_path(path: &std::path::Path) -> String {
    let shown = path.to_string_lossy();
    shown.strip_prefix("./").unwrap_or(&shown).to_string()
}

/// The line each variable in one config file sits on.
///
/// Built by re-parsing the file with gitoxide's event parser and counting the
/// newlines that precede each `SectionValueName` event. Counting `Event::Newline`
/// runs rather than measuring rendered spans is what makes this exact: the parser
/// emits the line ending *inside* a `\`-continued value as its own `Newline`
/// event too (gix-config's own doc example renders `file=a\<LF>    c` as
/// `["file", "=", "a\\", "\n", "    c"]`), so a continuation advances the count
/// just as `cs->linenr` advances in `get_value()`.
struct FileLines {
    /// `(section, subsection, name, line)` for every variable, in file order.
    entries: Vec<(String, Option<String>, String, usize)>,
    /// How far the walk has consumed, so repeats of one name read successive lines.
    cursor: usize,
}

impl FileLines {
    fn read(path: &std::path::Path) -> Self {
        let mut entries = Vec::new();
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(events) = gix::config::parse::Events::from_bytes(&bytes, None) {
                collect_lines(&events, &mut entries);
            }
        }
        FileLines { entries, cursor: 0 }
    }

    /// The line of the next not-yet-consumed occurrence of this variable.
    ///
    /// The scan runs forward from the cursor and then, if that finds nothing,
    /// from the start — so a file pulled into the snapshot twice (the same
    /// `include.path` reached by two routes) reports its first line again rather
    /// than nothing at all.
    fn next_line(&mut self, section: &str, subsection: Option<&str>, name: &str) -> Option<usize> {
        let matches = |e: &(String, Option<String>, String, usize)| {
            e.0 == section && e.1.as_deref() == subsection && e.2 == name
        };
        let at = self.entries[self.cursor..]
            .iter()
            .position(matches)
            .map(|i| i + self.cursor)
            .or_else(|| self.entries.iter().position(matches))?;
        self.cursor = at + 1;
        Some(self.entries[at].3)
    }
}

/// Walk one parsed file's events, tracking the current section header and the
/// running line count, and record where each variable name appears.
fn collect_lines(
    events: &gix::config::parse::Events,
    out: &mut Vec<(String, Option<String>, String, usize)>,
) {
    use gix::bstr::ByteSlice as _;
    use gix::config::parse::EventRef;

    let mut line = 1usize;
    let mut section = String::new();
    let mut subsection: Option<String> = None;
    for event in events.iter() {
        match event {
            EventRef::Newline(nl) => line += nl.iter().filter(|&&b| b == b'\n').count(),
            EventRef::SectionHeader {
                name,
                subsection_name,
                ..
            } => {
                section = name.to_str_lossy().to_ascii_lowercase();
                subsection = subsection_name.map(|s| s.to_str_lossy().into_owned());
            }
            EventRef::SectionValueName(name) => out.push((
                section.clone(),
                subsection.clone(),
                name.to_str_lossy().to_ascii_lowercase(),
                line,
            )),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// The config *file* reader's own refusals
// ---------------------------------------------------------------------------
//
// Everything above this line is about the *values* a config file holds. This
// section is about the file itself failing to parse, and about the repository
// format the file declares — two diagnostics that only a file can reach, and
// that `-c` therefore cannot be used to test.
//
// git's parser dies where it stands. `git_parse_source()` (config.c:1141-1170)
// builds the message from the source it was reading,
//
// ```c
// case CONFIG_ORIGIN_FILE:
//         error_msg = xstrfmt(_("bad config line %d in file %s"),
//                               cs->linenr, cs->name);
//         break;
// ```
//
// and then (config.c:1171-1185) picks what to do with it:
//
// ```c
// switch (opts && opts->error_action ?
//         opts->error_action :
//         cs->default_error_action) {
// case CONFIG_ERROR_DIE:
//         die("%s", error_msg);
// ```
//
// `do_config_from_file()` (config.c:1394) sets `top.default_error_action =
// CONFIG_ERROR_DIE`, and `do_git_config_sequence()` passes a literal `NULL` for
// `opts` to every one of its five `git_config_from_file_with_options()` calls, so
// every on-disk config file in the sequence dies. That is why the refusal is
// `fatal:` at exit 128 rather than an ordinary error — and why it comes out of
// the *reader*, before any command has run.

/// The two moments git reads configuration from a file, which decide *when* a
/// malformed one is fatal and therefore which files a given check may name.
///
/// * [`ConfigScopes::EarlyGlobal`] is `read_very_early_config()`, reached from
///   `tr2_sysenv_load()` inside `trace2_initialize()` — which `init_git()`
///   (common-init.c:77) runs *before* `cmd_main()`. It reads system, XDG and
///   user configuration and nothing else, so a malformed one of those kills
///   every invocation, `git --version` and `git --exec-path` included, before a
///   single argument has been looked at. Measured against git 2.55.0: with `[]`
///   in `~/.gitconfig`, all of `--version`, `--exec-path`, `--html-path`,
///   `--man-path`, `--help`, `version`, `help`, `stripspace` and an unknown verb
///   exit 128 with `fatal: bad config line 1 in file <path>`.
/// * [`ConfigScopes::Repository`] is the repository half of
///   `do_git_config_sequence()` — `$GIT_COMMON_DIR/config` and, when
///   `extensions.worktreeConfig` is on, `$GIT_DIR/config.worktree`. git reaches
///   it from `run_builtin()` (git.c:479-491): `setup_git_directory()` and then
///   `check_pager_config()`, both before the builtin's own `fn` is called. So it
///   applies to every `RUN_SETUP`/`RUN_SETUP_GENTLY` entry of the `commands[]`
///   table and to none of the others. Measured with `[]` in `.git/config`:
///   `status -h`, `commit -h`, `merge-index`, `hash-object -`, `column`,
///   `patch-id`, `shortlog`, `http-backend` and an unknown verb all exit 128,
///   while `version`, `help`, `stripspace`, `credential-cache exit` and
///   `url-parse` — none of which take repository setup — exit 0.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConfigScopes {
    /// System, XDG and `~/.gitconfig`: what `read_very_early_config()` covers.
    EarlyGlobal,
    /// `$GIT_COMMON_DIR/config` and `$GIT_DIR/config.worktree`.
    Repository,
}

/// `fatal: bad config line <n> in file <path>` for the first file in `scopes`
/// that git's parser would refuse, or `None` when they all parse.
///
/// The returned string is the message without the `fatal: ` prefix, so it can be
/// handed to [`crate::fatal::die`] or printed by a gate that already knows it is
/// leaving with 128.
pub fn bad_config_line(scopes: ConfigScopes, naming: GitDirNaming) -> Option<String> {
    let (path, line) = first_unparsable_config_file(scopes, naming)?;
    Some(format!("bad config line {line} in file {path}"))
}

/// How the message spells `$GIT_DIR`, which is not a constant: git prints what
/// setup left in `$GIT_DIR`, and not every command gets there the same way.
///
/// `setup_git_directory()` chdirs to the top of the work tree and keeps the
/// relative `.git` the discovery walk found, so every `RUN_SETUP` verb names
/// `.git/config` from anywhere inside the work tree. `cmd_init_db()` does not use
/// that setup at all — it resolves the directory itself and calls
/// `set_git_dir(real_path(...))` — so `git init` in a repository whose config is
/// malformed names the *absolute* path. Both measured against git 2.55.0 in the
/// same repository:
///
/// ```text
/// $ git status
/// fatal: bad config line 8 in file .git/config
/// $ git init
/// fatal: bad config line 8 in file /tmp/rr/.git/config
/// ```
///
/// Ignored by [`ConfigScopes::EarlyGlobal`], whose files are named by absolute
/// path in every case.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GitDirNaming {
    /// `.git/config` — what `setup_git_directory()` leaves behind.
    AsDiscovered,
    /// The resolved path, as `cmd_init_db()`'s `real_path()` produces.
    Absolute,
}

/// The first config file in `scopes` that will not parse, as
/// `(path as git would print it, 1-based line)`.
///
/// The walk has to be its own thing rather than a question asked of a loaded
/// snapshot: by the time gitoxide reports the failure the file is gone from the
/// error — `gix_config::file::init::Error::Parse` carries only
/// `gix_config::parse::Error`, which knows the line but not the path. So the
/// candidates are re-derived in `do_git_config_sequence()` order (config.c:
/// system, XDG, user, repository, worktree) and each is parsed on its own; the
/// first refusal is the one git would have died on, because git aborts the whole
/// sequence at that same point.
///
/// `include.path` is followed depth-first after its including file parses, which
/// is where git resolves it — an included file that will not parse is named by
/// its own path, not the includer's. Conditional `includeIf` sections are not
/// followed; naming no file at all is better than naming the wrong one, and the
/// caller falls back to its own voice when this returns `None`.
pub fn first_unparsable_config_file(
    scopes: ConfigScopes,
    naming: GitDirNaming,
) -> Option<(String, usize)> {
    let mut seen: Vec<PathBuf> = Vec::new();
    for candidate in config_file_sequence(scopes, naming) {
        if let Some(found) = check_config_file(&candidate, &mut seen) {
            return Some(found);
        }
    }
    None
}

/// One file of the config sequence: where it lives, and how git names it.
struct ConfigCandidate {
    /// The path to open.
    path: PathBuf,
    /// The path as git's message spells it — the string git passed to
    /// `git_config_from_file()`, which is absolute for the global scopes and
    /// `$GIT_DIR`-relative for a discovered repository.
    shown: String,
}

/// Parse `candidate`, and on success follow its unconditional includes. Returns
/// the first `(shown path, line)` that will not parse.
fn check_config_file(
    candidate: &ConfigCandidate,
    seen: &mut Vec<PathBuf>,
) -> Option<(String, usize)> {
    // git's own `include.c` guards against an include cycle with a depth limit;
    // a visited set is the same guarantee and cheaper to state.
    if seen.contains(&candidate.path) {
        return None;
    }
    seen.push(candidate.path.clone());
    let Ok(bytes) = std::fs::read(&candidate.path) else {
        // `git_config_from_file_with_options()` opens with `fopen_or_warn()` and
        // simply returns -1 when that fails, so an unreadable file is not this
        // diagnostic.
        return None;
    };
    if let Some(line) = first_bad_config_line(&bytes) {
        return Some((candidate.shown.clone(), line));
    }
    // Only a file that parsed has includes to follow.
    let Ok(events) = gix::config::parse::Events::from_bytes(&bytes, None) else {
        return None;
    };
    for included in included_paths(&events, &candidate.path) {
        let shown = included.to_string_lossy().into_owned();
        let next = ConfigCandidate {
            path: included,
            shown,
        };
        if let Some(found) = check_config_file(&next, seen) {
            return Some(found);
        }
    }
    None
}

/// The 1-based line `git_parse_source()` would refuse in `bytes`, or `None` when
/// git's parser would accept the whole thing.
///
/// Two refusals, because gitoxide's parser is the stricter one in some places
/// and the looser one in others: whatever its own parser rejects outright, plus
/// [`unterminated_valueless_key`] for the form it accepts and git does not.
pub fn first_bad_config_line(bytes: &[u8]) -> Option<usize> {
    match gix::config::parse::Events::from_bytes(bytes, None) {
        Err(err) => Some(err.line_number()),
        Ok(events) => unterminated_valueless_key(&events),
    }
}

/// The line of the first valueless key that is not the last thing on its line,
/// which git's `get_value()` refuses and gitoxide's parser accepts.
///
/// git reads the name, skips spaces and tabs, and then allows exactly two
/// continuations — end of line, or `=` and a value (config.c, `get_value()`):
///
/// ```c
/// while (c == ' ' || c == '\t')
///         c = get_next_char(cs);
///
/// value = NULL;
/// if (c != '\n') {
///         if (c != '=')
///                 return -1;
///         value = parse_value(cs);
///         if (!value)
///                 return -1;
/// }
/// ```
///
/// `return -1` is what `git_parse_source()` turns into `bad config line %d in
/// file %s`. gitoxide's parser instead ends the implicit-boolean form wherever it
/// stands and carries on, so `garbage line` becomes the two valueless names
/// `garbage` and `line` rather than a refusal, and `b ; c` becomes a valueless
/// `b` followed by a comment. Measured against git 2.55.0, with the file as the
/// global config and `git config --list` reading it:
///
/// ```text
/// [a]\nb\n          rc=0, lists `a.b`      [a]\nb ; c\n   fatal: bad config line 2
/// [a]\nb   \n       rc=0, lists `a.b`      [a]\nb # c\n   fatal: bad config line 2
/// [a]\nb            rc=0, lists `a.b`      [a]\nb;c\n     fatal: bad config line 2
/// [a]\nb\t\n        rc=0, lists `a.b`      [a]\nb c\n     fatal: bad config line 2
/// ```
///
/// This is a check over the event stream gitoxide already produced rather than a
/// second parser: a valueless key is a `Value` event sitting directly after its
/// name (with only `Whitespace` between), and git's rule is that the next event
/// must be the line ending or the end of the file.
fn unterminated_valueless_key(events: &gix::config::parse::Events) -> Option<usize> {
    use gix::config::parse::EventRef;

    let mut line = 1usize;
    // Whether the last significant event was a name, and then whether the value
    // that followed it was the implicit (valueless) one.
    let mut after_name = false;
    let mut after_implicit_value = false;
    for event in events.iter() {
        match event {
            EventRef::Whitespace(_) => continue,
            EventRef::Newline(nl) => {
                line += nl.iter().filter(|&&b| b == b'\n').count();
                after_name = false;
                after_implicit_value = false;
            }
            EventRef::SectionValueName(_) => {
                if after_implicit_value {
                    return Some(line);
                }
                after_name = true;
                after_implicit_value = false;
            }
            EventRef::Value(_) if after_name => {
                after_name = false;
                after_implicit_value = true;
            }
            other => {
                if after_implicit_value && matches!(other, EventRef::Comment { .. }) {
                    return Some(line);
                }
                after_name = false;
                after_implicit_value = false;
            }
        }
    }
    None
}

/// The `include.path` targets of one parsed file, resolved the way
/// `expand_include_path()` (config.c) resolves them: `~` against `$HOME`, and a
/// relative path against the *including file's* directory.
///
/// `includeIf.<condition>.path` is deliberately left out — evaluating the
/// condition here would be a second implementation of `include_condition_is_true()`
/// and getting it wrong would name a file git never opened.
fn included_paths(events: &gix::config::parse::Events, from: &std::path::Path) -> Vec<PathBuf> {
    use gix::bstr::ByteSlice as _;
    use gix::config::parse::EventRef;

    let mut out = Vec::new();
    let mut in_include = false;
    let mut at_path = false;
    for event in events.iter() {
        match event {
            EventRef::SectionHeader {
                name,
                subsection_name,
                ..
            } => {
                in_include = subsection_name.is_none()
                    && name.to_str_lossy().eq_ignore_ascii_case("include");
                at_path = false;
            }
            EventRef::SectionValueName(name) => {
                at_path = in_include && name.to_str_lossy().eq_ignore_ascii_case("path");
            }
            EventRef::Value(raw) if at_path => {
                at_path = false;
                let text = raw.to_str_lossy().into_owned();
                if text.is_empty() {
                    continue;
                }
                let expanded = expand_tilde(&text);
                out.push(if expanded.is_absolute() {
                    expanded
                } else {
                    match from.parent() {
                        Some(dir) => dir.join(expanded),
                        None => expanded,
                    }
                });
            }
            _ => {}
        }
    }
    out
}

/// The files `do_git_config_sequence()` would open for `scopes`, in its order.
///
/// The two *global* scopes are delegated to gitoxide's
/// `Source::storage_location()`, which already implements git's environment rules
/// for them — `GIT_CONFIG_GLOBAL` replacing a scope outright, `XDG_CONFIG_HOME`
/// falling back to `$HOME/.config` — and reads nothing but the environment to do
/// it. Duplicates are possible (`GIT_CONFIG_GLOBAL` answers for both the XDG and
/// the user scope) and are dropped by the visited set in [`check_config_file`].
///
/// The *system* scope is not, and `Source::GitInstallation` is absent entirely:
/// both can start a `git` subprocess, and this list is built on every invocation.
/// See [`system_config_path`] for the whole of that reasoning — it is the reason
/// this function must not grow a `storage_location()` call for either scope.
fn config_file_sequence(scopes: ConfigScopes, naming: GitDirNaming) -> Vec<ConfigCandidate> {
    use gix::config::Source;

    let mut out = Vec::new();
    if scopes == ConfigScopes::EarlyGlobal {
        let mut env = |name: &str| std::env::var_os(name);
        if let Some(path) = system_config_path(&mut env) {
            let shown = path.to_string_lossy().into_owned();
            out.push(ConfigCandidate { path, shown });
        }
        // The two global scopes, in `do_git_config_sequence()` order — XDG
        // before user. Neither reads anything but the environment.
        for source in [Source::Git, Source::User] {
            if let Some(path) = source.storage_location(&mut env) {
                let shown = path.to_string_lossy().into_owned();
                out.push(ConfigCandidate { path, shown });
            }
        }
        return out;
    }

    let Some(dirs) = repository_directories() else {
        return out;
    };
    // `opts.commondir` is what the repository config is read from, so a linked
    // worktree reads the main repository's `config` and only `config.worktree`
    // is its own (config.c:1590-1600).
    out.push(dirs.candidate(&dirs.common_dir, "config", naming));
    if worktree_config_enabled(&dirs.common_dir.join("config")) {
        out.push(dirs.candidate(&dirs.git_dir, "config.worktree", naming));
    }
    out
}

/// git's `git_system_config()` (config.c:1499-1506) — the *only* system-scope
/// file `do_git_config_sequence()` reads.
///
/// ```c
/// char *git_system_config(void)
/// {
///         char *system_config = xstrdup_or_null(getenv("GIT_CONFIG_SYSTEM"));
///         if (!system_config)
///                 system_config = system_path(ETC_GITCONFIG);
///         normalize_path_copy(system_config, system_config);
///         return system_config;
/// }
/// ```
///
/// # Why this is hand-rolled instead of `Source::storage_location()`
///
/// **It must not be able to spawn a process, and `storage_location()` can.**
///
/// The gate this feeds runs on *every* invocation, before any argument is looked
/// at. `Source::GitInstallation.storage_location()` calls
/// `gix_path::env::installation_config()`, which locates the file by running
/// `git config -lz --show-origin --name-only` (gix-path/src/env/git/mod.rs:181)
/// — its own documentation says "both may spawn git once". zvcs's shipped
/// installation model is a `git` on `PATH` that *is* zvcs
/// (`~/.zvcs/bin/git` shadows git), so that child runs the gate, which spawns
/// another child, which runs the gate: an unbounded process fan-out that never
/// terminates. It was reproduced as `git init -q --bare` hanging with a `git`
/// shim first on `PATH`, and disappearing under `GIT_CONFIG_NOSYSTEM=1` —
/// exactly the variable `installation_config()` honours.
/// `Source::System.storage_location()` has the same hazard on Windows, where it
/// falls back to `system_prefix()` → `core_dir()` → `git --exec-path`.
///
/// Of the ways to break that loop, this is the one the C source settles:
///
/// * **Memoising the probe does not work.** The recursion is *across* processes.
///   Each child is a fresh process with a fresh `LazyLock`, so a per-process
///   cache still leaves one spawn per generation and the fan-out is unchanged.
/// * **Resolving the installation config without a subprocess is not possible**
///   off Windows. The value is *defined* as "ask the `git` in `PATH` where its
///   configuration is", and for zvcs that `git` is zvcs.
/// * **`installation_config_unsuppressed()` answers a different question** ("where
///   is git installed", for `etc/gitattributes` and friends) and spawns just the
///   same, so it is no use here either.
/// * **Dropping the scope is correct on its own terms.** `do_git_config_sequence()`
///   (config.c:1547-1613) reads `git_system_config()`, then `xdg_config`, then
///   `user_config`, then the repository pair, then the command line — and nothing
///   else. There is no installation-config scope anywhere in git. It is a
///   gitoxide concept for behaving like the git that is installed *alongside* it,
///   which is not a relationship zvcs has with itself. Naming such a file in
///   `bad config line <n> in file <path>` would be inventing a diagnostic git
///   cannot produce.
///
/// So the scope is gone from the sequence and the system path is derived the way
/// `git_system_config()` derives it, from the environment and a compiled-in
/// default. Nothing on this path starts a process.
///
/// `GIT_CONFIG_NOSYSTEM` is honoured because `git_config_system()`
/// (config.c:1542-1545) — `return !git_env_bool("GIT_CONFIG_NOSYSTEM", 0);` —
/// gates the whole system read on it in `do_git_config_sequence()` (config.c:1574).
///
/// The truthiness test is `git_parse_maybe_bool()`'s, through
/// [`crate::optint::maybe_bool`]: the words, the empty string for false, and any
/// base-0 integer as its truthiness — so `0x10` and `1k` switch the scope off and
/// `0x0` leaves it on, as they do in git. It used to be gitoxide's
/// `config::Boolean::try_from`, which accepts a narrower grammar and *silently
/// keeps the scope enabled* for anything it cannot read; git dies instead.
///
/// The refusal itself is not raised here. `git_config_system()` is reached from
/// `read_very_early_config()`, which `init_git()` runs before `cmd_main()` sees
/// the command line, so a malformed value refuses the invocation before any verb
/// exists to report it against — including `git --version`. `run_command()`
/// (lib.rs) makes that call at the top of the process for exactly that reason,
/// and by the time this function runs the value is already known to parse. The
/// call is kept here as well because [`crate::alias`] and the config-listing
/// paths reach this without going through the dispatcher.
fn system_config_path(env: &mut dyn FnMut(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let nosystem = env("GIT_CONFIG_NOSYSTEM")
        .map(|raw| raw.to_string_lossy().into_owned())
        .is_some_and(|raw| crate::setup::git_env_bool_value("GIT_CONFIG_NOSYSTEM", &raw));
    if nosystem {
        return None;
    }
    if let Some(path) = env("GIT_CONFIG_SYSTEM") {
        return Some(PathBuf::from(path));
    }
    system_path_etc_gitconfig()
}

/// `system_path(ETC_GITCONFIG)` for this build.
///
/// On a Unix build `ETC_GITCONFIG` is absolute and `system_path()` returns it
/// unchanged, which is the same answer gitoxide reaches through
/// `system_prefix()` — that function is a constant `/` off Windows
/// (gix-path/src/env/mod.rs:230-240), so there is no difference to preserve and
/// no reason to call it.
///
/// On Windows the prefix is discovered by running `git --exec-path`, which is the
/// spawn this whole function exists to avoid. The scope is skipped there rather
/// than probed: the gate then names no file, the command falls back to the port's
/// own diagnostic, and nothing recurses. Naming nothing is a degradation; a
/// process fan-out is a hang.
fn system_path_etc_gitconfig() -> Option<PathBuf> {
    if cfg!(windows) {
        None
    } else {
        Some(PathBuf::from("/etc/gitconfig"))
    }
}

/// The `$GIT_DIR` and `$GIT_COMMON_DIR` of the repository the current directory
/// is in, plus what setup would have made `$GIT_DIR` *look* like.
struct RepositoryDirs {
    git_dir: PathBuf,
    common_dir: PathBuf,
    /// `Some(top)` for a repository with a work tree, used to decide whether
    /// git's message would say the bare `.git`.
    work_tree: Option<PathBuf>,
    /// `$GIT_DIR` named the repository outright, which is git's
    /// `GIT_DIR_EXPLICIT`: `setup_explicit_git_dir()` installs the variable's
    /// own spelling and never chdirs, so the message repeats it verbatim —
    /// `GIT_DIR=.git` gives `.git/config` and an absolute one gives an absolute
    /// path. Both measured against git 2.55.0.
    explicit: bool,
}

impl RepositoryDirs {
    /// One candidate under `dir`, named the way git's message names it.
    ///
    /// `setup_git_directory()` chdirs to the top of the work tree and leaves
    /// `$GIT_DIR` as the relative `.git` it discovered, which is why git prints
    /// `.git/config` from anywhere inside the work tree and an absolute path for
    /// an explicit `$GIT_DIR` or a `--separate-git-dir` repository. Both were
    /// measured against git 2.55.0.
    fn candidate(&self, dir: &std::path::Path, file: &str, naming: GitDirNaming) -> ConfigCandidate {
        let shown_dir = match &self.work_tree {
            _ if naming == GitDirNaming::Absolute => crate::setup::realpath(dir),
            _ if self.explicit => dir.to_path_buf(),
            Some(top)
                if crate::setup::realpath(dir) == crate::setup::realpath(&top.join(".git")) =>
            {
                std::path::Path::new(".git").to_path_buf()
            }
            _ => dir.to_path_buf(),
        };
        let shown = shown_dir.join(file).to_string_lossy().into_owned();
        ConfigCandidate {
            path: dir.join(file),
            shown,
        }
    }
}

/// Locate the repository without opening it — opening is what fails when the
/// config will not parse, so the location has to come from the discovery walk
/// alone.
fn repository_directories() -> Option<RepositoryDirs> {
    // `setup_git_directory_gently_1()` reads `$GIT_DIR` before it walks anywhere
    // and returns `GIT_DIR_EXPLICIT` when it is set, so the variable short-circuits
    // discovery entirely.
    let (git_dir, work_tree, explicit) = match std::env::var_os("GIT_DIR") {
        Some(dir) => {
            let git_dir = PathBuf::from(dir);
            if !git_dir.is_dir() {
                return None;
            }
            let work_tree = std::env::var_os("GIT_WORK_TREE").map(PathBuf::from);
            (git_dir, work_tree, true)
        }
        None => {
            let (path, _trust) = gix::discover::upwards(std::path::Path::new(".")).ok()?;
            let (git_dir, work_tree) = path.into_repository_and_work_tree_directories();
            (git_dir, work_tree, false)
        }
    };
    // `get_common_dir()`: `$GIT_COMMON_DIR`, else a `commondir` file inside
    // `$GIT_DIR` redirecting the shared half of the repository elsewhere, which is
    // how a linked worktree is wired up. The file's content is a path, relative to
    // `$GIT_DIR` unless absolute; git's message shows it resolved, so a
    // `worktrees/<name>/../..` round trip is normalised away rather than printed.
    let common_dir = match std::env::var_os("GIT_COMMON_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => match std::fs::read_to_string(git_dir.join("commondir")) {
            Ok(text) => {
                let trimmed = text.trim_end_matches(['\n', '\r']);
                let candidate = PathBuf::from(trimmed);
                if trimmed.is_empty() {
                    git_dir.clone()
                } else if candidate.is_absolute() {
                    candidate
                } else {
                    normalize_lexically(&git_dir.join(candidate))
                }
            }
            Err(_) => git_dir.clone(),
        },
    };
    Some(RepositoryDirs {
        git_dir,
        common_dir,
        work_tree,
        explicit,
    })
}

/// Collapse `.` and `x/..` in a path without touching the filesystem, the way
/// `strbuf_normalize_path()` does — a linked worktree's `commondir` says `../..`
/// and git prints the collapsed result, not the round trip.
fn normalize_lexically(path: &std::path::Path) -> PathBuf {
    use std::path::Component;

    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Only a real directory name may be popped; `../..` at the front
                // of a relative path has to stay.
                if out.components().next_back().is_some_and(|c| matches!(c, Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Whether `extensions.worktreeConfig` is on, read straight out of the
/// repository config file.
///
/// It has to be read from the file rather than from a snapshot for the same
/// reason the rest of this section exists: the snapshot is what failed. The read
/// is deliberately forgiving — a file that will not parse never gets here,
/// because the caller checks it first.
fn worktree_config_enabled(config_path: &std::path::Path) -> bool {
    let Ok(bytes) = std::fs::read(config_path) else {
        return false;
    };
    let Ok(file) = gix::config::File::from_bytes_no_includes(
        &bytes,
        gix::config::file::Metadata::from(gix::config::Source::Local),
        Default::default(),
    ) else {
        return false;
    };
    matches!(file.boolean("extensions.worktreeConfig"), Ok(Some(true)))
}

/// How git's setup would have reported a repository this port could not open
/// because of its *configuration*, or `None` when the failure is not one of the
/// three that reach a user in git's own words.
///
/// The three are all raised by `read_and_verify_repository_format()`
/// (setup.c:751-816) reading `$GIT_COMMON_DIR/config` and handing the result to
/// `verify_repository_format()` (setup.c:888-925):
///
/// ```c
/// if (GIT_REPO_VERSION_READ < format->version) {
///         strbuf_addf(err, _("Expected git repo version <= %d, found %d"),
///                     GIT_REPO_VERSION_READ, format->version);
///         return -1;
/// }
/// ...
/// if (format->version == 0 && format->v1_only_extensions.nr) {
///         strbuf_addstr(err,
///                       Q_("repo version is 0, but v1-only extension found:",
///                          "repo version is 0, but v1-only extensions found:",
///                          format->v1_only_extensions.nr));
///
///         for (i = 0; i < format->v1_only_extensions.nr; i++)
///                 strbuf_addf(err, "\n\t%s",
///                             format->v1_only_extensions.items[i].string);
///         return -1;
/// }
/// ```
///
/// Each offender is appended as `"\n\t%s"`, so the message is two lines, and
/// `read_repository_format()` (setup.c:866-876) reads **only that one file** —
/// which is why `-c extensions.objectFormat=sha256` is accepted by both git and
/// this port while the same key in `.git/config` is refused by both. Measured
/// against git 2.55.0.
pub fn repository_format_message(err: &gix::config::Error) -> Option<String> {
    use gix::config::Error as E;
    match err {
        // `handle_extension()` (setup.c:653-716) classifies `objectformat` as
        // v1-only, and `check_repo_format()` (setup.c:718-749) appends the
        // extension's *suffix* — already lower-cased by the config parser — to
        // `v1_only_extensions`.
        E::ObjectFormatRequiresV1 => {
            Some("repo version is 0, but v1-only extension found:\n\tobjectformat".to_owned())
        }
        // `GIT_REPO_VERSION_READ` is 1.
        E::UnsupportedRepositoryFormatVersion { version } => {
            Some(format!("Expected git repo version <= 1, found {version}"))
        }
        _ => None,
    }
}

/// `handle_extension_v0()` (setup.c:614-634): the four extensions git honours at
/// *any* repository version, "respected even in v0-format repositories for
/// historical compatibility". They are consumed by that function and never reach
/// either offender list, so they are invisible to
/// [`verify_repository_format`] whatever the version says.
const EXTENSIONS_V0: &[&str] = &["noop", "preciousobjects", "partialclone", "worktreeconfig"];

/// `handle_extension()` (setup.c:655-716): every extension git knows about that
/// requires repository format version 1. `check_repo_format()` (setup.c:738-741)
/// appends each of these to `v1_only_extensions`, which
/// [`verify_repository_format`] complains about only when the version is 0.
///
/// The C is a chain of `strcmp(ext, …)` and this is its exact membership, in
/// source order. Anything absent from **both** lists lands in
/// `unknown_extensions`, which is the other half of the same function.
const EXTENSIONS_V1: &[&str] = &[
    "noop-v1",
    "objectformat",
    "compatobjectformat",
    "refstorage",
    "relativeworktrees",
    "submodulepathconfig",
];

/// `GIT_REPO_VERSION_READ` (repository.h): the highest `core.repositoryformatversion`
/// this build will read.
const GIT_REPO_VERSION_READ: i64 = 1;

/// What `read_repository_format()` (setup.c:857-877) collected from one config
/// file, in the shape `verify_repository_format()` consumes.
///
/// `version` is `-1` for a file that names no `core.repositoryformatversion` at
/// all, which is `REPOSITORY_FORMAT_INIT`'s value and the reason
/// `check_repository_format_gently()` (setup.c:766-769) treats a missing config
/// as a silent success:
///
/// ```c
/// /*
///  * For historical use of check_and_apply_repository_format() in git-init,
///  * we treat a missing config as a silent "ok", even when nongit_ok
///  * is unset.
///  */
/// if (candidate->version < 0)
///         return 0;
/// ```
#[derive(Default)]
struct RepositoryFormat {
    version: i64,
    v1_only_extensions: Vec<String>,
    unknown_extensions: Vec<String>,
}

/// `read_repository_format()` (setup.c:857-877) over one file: the whole of
/// `check_repo_format()`'s classification, and nothing else.
///
/// ```c
/// if (strcmp(var, "core.repositoryformatversion") == 0)
///         data->version = git_config_int(var, value, ctx->kvi);
/// else if (skip_prefix(var, "extensions.", &ext)) {
///         switch (handle_extension_v0(var, value, ext, data)) { … }
///         switch (handle_extension(var, value, ext, data)) {
///         case EXTENSION_OK:
///                 string_list_append(&data->v1_only_extensions, ext);
///                 return 0;
///         case EXTENSION_UNKNOWN:
///                 string_list_append(&data->unknown_extensions, ext);
///                 return 0;
///         }
/// }
/// ```
///
/// Three details are load-bearing and each is measurable:
///
/// * **One file, no includes.** `read_repository_format()` calls
///   `git_config_from_file()` directly, so an `[include]` in `.git/config` does
///   not contribute and neither does any other scope — which is why
///   `-c extensions.objectFormat=sha256` is accepted by git while the same key
///   in the file is refused.
/// * **The name is the parser's, already lower-cased.** `extensions.objectFormat`
///   reaches the callback as `extensions.objectformat`, and that lower-cased
///   suffix is what the message prints.
/// * **The version is the last one seen.** The callback runs per occurrence and
///   assigns, so a file naming the key twice keeps the second value.
///
/// The `EXTENSION_ERROR` arms — an `extensions.objectformat` naming a hash git
/// does not have, a repeated `compatobjectformat` — are deliberately not
/// reproduced: they are a different diagnostic (`error: invalid value for …`)
/// raised from inside the config reader, and guessing at it would invent a
/// refusal rather than port one. Such a value is classified here as the known
/// extension it names, so this can only ever report *less* than git, never more.
fn read_repository_format(path: &std::path::Path) -> RepositoryFormat {
    let mut format = RepositoryFormat {
        version: -1,
        ..Default::default()
    };
    let Ok(bytes) = std::fs::read(path) else {
        return format;
    };
    let Ok(file) = gix::config::File::from_bytes_no_includes(
        &bytes,
        gix::config::file::Metadata::from(gix::config::Source::Local),
        Default::default(),
    ) else {
        return format;
    };
    // `File::sections()` walks in file order, and `BodyRef::value_names()` yields
    // one entry per *occurrence*, so a key written twice is seen twice — which is
    // what `string_list_append()` per callback invocation amounts to.
    for section in file.sections() {
        let header = section.header();
        let name = String::from_utf8_lossy(header.name()).to_ascii_lowercase();
        let subsection = header
            .subsection_name()
            .map(|s| String::from_utf8_lossy(s).into_owned());
        let body = section.body();
        for key in body.value_names() {
            let key_lower = key.to_ascii_lowercase();
            // `git_config_parse_key()` lower-cases the section and the variable
            // and leaves a subsection alone, so the full key is rebuilt the same
            // way before `skip_prefix(var, "extensions.", &ext)` looks at it.
            let full = match &subsection {
                Some(sub) => format!("{name}.{sub}.{key_lower}"),
                None => format!("{name}.{key_lower}"),
            };
            if full == "core.repositoryformatversion" {
                // `git_config_int()`, last occurrence wins — `value()` already
                // answers with the last one. A value git's parser would refuse is
                // left alone here rather than guessed at; see the
                // `EXTENSION_ERROR` note above.
                if let Some(value) = body.value(&key) {
                    let text = String::from_utf8_lossy(value.as_slice()).into_owned();
                    if let Ok(v) = crate::optint::config_int(&text) {
                        format.version = v;
                    }
                }
                continue;
            }
            let Some(ext) = full.strip_prefix("extensions.") else {
                continue;
            };
            if EXTENSIONS_V0.contains(&ext) {
                continue;
            }
            if EXTENSIONS_V1.contains(&ext) {
                format.v1_only_extensions.push(ext.to_string());
            } else {
                format.unknown_extensions.push(ext.to_string());
            }
        }
    }
    format
}

/// `verify_repository_format()` (setup.c:881-917), rendered as the `err` strbuf
/// it fills — the exact text `check_repository_format_gently()` then passes to
/// `die()`.
///
/// ```c
/// if (GIT_REPO_VERSION_READ < format->version) {
///         strbuf_addf(err, _("Expected git repo version <= %d, found %d"),
///                     GIT_REPO_VERSION_READ, format->version);
///         return -1;
/// }
///
/// if (format->version >= 1 && format->unknown_extensions.nr) {
///         strbuf_addstr(err, Q_("unknown repository extension found:",
///                               "unknown repository extensions found:",
///                               format->unknown_extensions.nr));
///         for (i = 0; i < format->unknown_extensions.nr; i++)
///                 strbuf_addf(err, "\n\t%s", format->unknown_extensions.items[i].string);
///         return -1;
/// }
///
/// if (format->version == 0 && format->v1_only_extensions.nr) { … }
/// ```
///
/// The three arms are ordered and mutually exclusive, and the two extension arms
/// are each other's inverse: an *unknown* extension is only ever a problem at
/// version 1 or above, and a *known v1-only* one only at version 0. So
/// `extensions.bogus` in a v0 repository is silently ignored by git — the port
/// must ignore it too — while the same key at version 1 is fatal.
///
/// `Q_()` is the plural selector, so a second offender changes `extension` to
/// `extensions` in the first line. Each offender then follows as its own
/// tab-indented line.
fn verify_repository_format(format: &RepositoryFormat) -> Option<String> {
    if GIT_REPO_VERSION_READ < format.version {
        return Some(format!(
            "Expected git repo version <= {GIT_REPO_VERSION_READ}, found {}",
            format.version
        ));
    }
    if format.version >= 1 && !format.unknown_extensions.is_empty() {
        return Some(offender_list(
            "unknown repository extension found:",
            "unknown repository extensions found:",
            &format.unknown_extensions,
        ));
    }
    if format.version == 0 && !format.v1_only_extensions.is_empty() {
        return Some(offender_list(
            "repo version is 0, but v1-only extension found:",
            "repo version is 0, but v1-only extensions found:",
            &format.v1_only_extensions,
        ));
    }
    None
}

/// `Q_(singular, plural, n)` followed by one `"\n\t%s"` per offender.
fn offender_list(singular: &str, plural: &str, offenders: &[String]) -> String {
    let mut out = String::from(if offenders.len() == 1 { singular } else { plural });
    for name in offenders {
        out.push_str("\n\t");
        out.push_str(name);
    }
    out
}

/// `read_and_verify_repository_format()` (setup.c:753-777) for the repository the
/// current directory is in: the message git would `die()` with, or `None` when
/// the format is one this build reads.
///
/// The file is `$GIT_COMMON_DIR/config`, which is what
/// `check_repository_format_gently()` builds with `get_common_dir()` — a linked
/// worktree shares the main repository's format, so the format of
/// `worktrees/<name>/config` is never consulted.
///
/// `None` also covers "there is no repository here": with nothing found there is
/// nothing to verify, and the caller's command carries on to fail (or not) on its
/// own terms.
pub fn repository_format_refusal() -> Option<String> {
    let dirs = repository_directories()?;
    let format = read_repository_format(&dirs.common_dir.join("config"));
    // `if (candidate->version < 0) return 0;` — the historical silent "ok" for a
    // repository whose config names no version at all.
    if format.version < 0 {
        return None;
    }
    verify_repository_format(&format)
}

/// The `fatal:` line git would have printed for a repository this port failed to
/// open, searched for anywhere in an `anyhow` chain.
///
/// Two failures reach here and neither is this port's to describe in its own
/// voice: a configuration file that will not parse (which git reports from the
/// reader, [`bad_config_line`]) and a repository format the build cannot honour
/// ([`repository_format_message`]). Everything else keeps the `zvcs: <verb>: …`
/// prefix that marks it as this binary speaking for itself.
pub fn setup_fatal(err: &anyhow::Error) -> Option<String> {
    err.chain().find_map(|cause| {
        if let Some(open) = cause.downcast_ref::<gix::open::Error>() {
            return open_error_message(open);
        }
        if let Some(discover) = cause.downcast_ref::<gix::discover::Error>() {
            return match discover {
                gix::discover::Error::Open(open) => open_error_message(open),
                gix::discover::Error::Discover(_) => None,
            };
        }
        if is_gitmodules_failure(cause) {
            return gitmodules_message();
        }
        cause.downcast_ref::<gix::config::Error>().and_then(config_error_message)
    })
}

/// `bad config line <n> in file <path>` for the worktree's `.gitmodules`.
///
/// `config_from_gitmodules()` (submodule-config.c:784-814) reads the on-disk file
/// through `git_config_from_file_with_options(fn, config_source->file, data,
/// scope, NULL)` — `NULL` options, so the per-source `CONFIG_ERROR_DIE` applies
/// and a malformed `.gitmodules` is as fatal as a malformed `.git/config`. The
/// only tolerance in that file is `repo_read_gitmodules()`'s unmerged-index guard
/// (submodule-config.c:840); the `CONFIG_ERROR_SILENT` that does swallow a parse
/// failure lives in `fsck_blob()` (fsck.c:1212), which reports
/// `gitmodulesParse` itself instead. So the file is tolerated only by *not being
/// read* — see [`gix::status::index_worktree::BuiltinSubmoduleStatus`].
///
/// The path is absolute because `repo_worktree_path(repo, GITMODULES_FILE)`
/// builds it from the worktree root. Measured against git 2.55.0.
/// Whether this link in the chain is one of the errors that can only come from
/// reading `.gitmodules`.
///
/// The types below are the *outermost* wrappers, and they have to be, because
/// everything under them is `#[error(transparent)]` — and `transparent` forwards
/// `source()` as well as `Display`, so each inner layer is skipped entirely and
/// the `gix_config::parse::Error` at the bottom never appears in the chain at
/// all. `gix::submodule::modules::Error` is what `Repository::submodules()`
/// returns, and `gix_status::index_as_worktree::Error::SubmoduleStatus` is the
/// (non-transparent) wrapper the index-versus-worktree walk puts around it.
fn is_gitmodules_failure(cause: &(dyn std::error::Error + 'static)) -> bool {
    if cause.downcast_ref::<gix::submodule::modules::Error>().is_some()
        || cause.downcast_ref::<gix::submodule::open_modules_file::Error>().is_some()
    {
        return true;
    }
    // The index-versus-worktree walk boxes the submodule error and every layer
    // above it is transparent, so this is the one nameable type left in the chain
    // that says the failure came from reading `.gitmodules`.
    matches!(
        cause.downcast_ref::<gix::status::index_worktree::submodule_status::Error>(),
        Some(gix::status::index_worktree::submodule_status::Error::Modules(_))
    )
}

/// `bad config line <n> in file <path>` for the worktree's `.gitmodules`, or
/// `None` when that file parses after all.
///
/// The file is re-read rather than described from the error, for the same reason
/// as [`first_unparsable_config_file`]: the surviving error carries a line but
/// not a path, and re-parsing gives both from the one file git would have named.
fn gitmodules_message() -> Option<String> {
    let dirs = repository_directories()?;
    let path = crate::setup::realpath(&dirs.work_tree?).join(".gitmodules");
    let bytes = std::fs::read(&path).ok()?;
    let err = gix::config::parse::Events::from_bytes(&bytes, None).err()?;
    Some(format!(
        "bad config line {} in file {}",
        err.line_number(),
        path.display()
    ))
}

/// [`setup_fatal`] for one `gix::open::Error`.
fn open_error_message(err: &gix::open::Error) -> Option<String> {
    match err {
        gix::open::Error::Config(config) => config_error_message(config),
        _ => None,
    }
}

/// [`setup_fatal`] for one `gix::config::Error`.
///
/// A parse failure arrives here with no file attached, so the file is found by
/// re-walking the sequence. Both scopes are searched, global first, because that
/// is the order git reads them in and therefore the order it would have died in.
fn config_error_message(err: &gix::config::Error) -> Option<String> {
    use gix::config::Error as E;
    match err {
        E::Init(_) | E::ResolveIncludes(_) | E::Span(_) | E::ConfigValue(_) => {
            bad_config_line(ConfigScopes::EarlyGlobal, GitDirNaming::AsDiscovered)
                .or_else(|| bad_config_line(ConfigScopes::Repository, GitDirNaming::AsDiscovered))
        }
        other => repository_format_message(other),
    }
}

#[cfg(test)]
mod watch_gate_tests {
    use super::ZvcsConfig;
    use crate::superset::zconfig::{setting_names, value_hints};

    /// A scratch repository whose `[zvcs]` section is exactly `body`.
    fn repo_with(tag: &str, body: &str) -> (std::path::PathBuf, gix::Repository) {
        let dir = std::env::temp_dir().join(format!("zvcs-watchgate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let repo = gix::init(&dir).expect("init scratch repo");
        if !body.is_empty() {
            let cfg = repo.git_dir().join("config");
            let mut text = std::fs::read_to_string(&cfg).unwrap_or_default();
            text.push_str("\n[zvcs]\n");
            text.push_str(body);
            std::fs::write(&cfg, text).unwrap();
        }
        let repo = gix::open(&dir).expect("reopen with the written config");
        (dir, repo)
    }

    #[test]
    fn a_repository_with_no_zvcs_section_does_not_make_the_daemon_watch() {
        // Every autonomy switch must default off. A new one defaulting on would
        // make the daemon work in a repository that never asked for it — and
        // `git zconfig all off` could not take it back, because "off" is written
        // from the settings table and a key that is not in the table is not
        // written at all.
        let (dir, repo) = repo_with("default", "");
        let cfg = ZvcsConfig::load(&repo);
        assert!(!cfg.should_watch(), "an unconfigured repository must not start the watch loop");
        assert!(!cfg.any_autonomous(), "an unconfigured repository must have no autonomy");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_settings_tables_off_state_stops_the_watch_loop() {
        // The off state is derived from `git zconfig`'s own table rather than
        // typed here: booleans go false, counts go 0, which is exactly what
        // `git zconfig all off` writes. If a future input to `should_watch` is
        // not in that table, this is where it shows up — the daemon would keep
        // watching after the user turned everything off.
        let mut body = String::new();
        for name in setting_names() {
            if name == "all" {
                continue;
            }
            let off = if value_hints(name).contains(&"on") { "false" } else { "0" };
            body.push_str(&format!("\t{name} = {off}\n"));
        }
        let (dir, repo) = repo_with("alloff", &body);
        let cfg = ZvcsConfig::load(&repo);
        assert!(
            !cfg.should_watch(),
            "`zconfig all off` left the daemon watching; a watch input is missing from the settings table"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_switch_alone_is_enough_to_start_the_watch_loop() {
        // The converse, so the case above cannot pass because nothing can ever
        // start the loop. Each of these is one of `should_watch`'s inputs.
        for key in ["autoreconcile", "autobump", "autostatus", "autodups", "autohook"] {
            let (dir, repo) = repo_with(key, &format!("\t{key} = true\n"));
            let cfg = ZvcsConfig::load(&repo);
            assert!(cfg.should_watch(), "`zvcs.{key} = true` must start the watch loop");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
