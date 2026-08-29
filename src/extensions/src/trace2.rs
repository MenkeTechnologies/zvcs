//! Trace2 — git's structured telemetry stream, event (JSON) target only.
//!
//! Git ships three Trace2 targets: `normal` (human-readable lines), `perf`
//! (column-aligned performance rows) and `event` (one JSON object per line).
//! The first two print a C `file:line` column drawn from the emitting
//! `__FILE__`/`__LINE__` — `common-init.c:57`, `trace2/tr2_tgt_normal.c:128` —
//! whose *values* name git's own translation units. A Rust port cannot
//! reproduce those strings, so `trace2.normalTarget` / `trace2.perfTarget` and
//! their `*Brief` companions are deliberately not served here.
//!
//! The event target is different: it is JSON, every field is a named key, and
//! `trace2.eventBrief` (`GIT_TRACE2_EVENT_BRIEF`) omits `time`, `file` and
//! `line` outright — leaving a stream this port reproduces field for field.
//! That is what this module implements, ported from git 2.55.0's
//! `trace2/tr2_tgt_event.c`, `tr2_dst.c`, `tr2_sid.c`, `tr2_sysenv.c`,
//! `tr2_cfg.c`, `tr2_cmd_name.c`, `tr2_tbuf.c` and `json-writer.c`.
//!
//! Only records this binary can honestly produce are emitted: `version`,
//! `too_many_files`, `start`, `cmd_name`, `def_param`, `error`, `exec`,
//! `exec_result`, `exit` and `atexit`. Git's `region_*`, `data*`, `timer`,
//! `counter` and `th_*` records describe instrumentation this port does not
//! carry; inventing them would mean inventing timings, so they are absent
//! rather than faked. `trace2.eventNesting` exists solely to bound
//! `region_enter`/`region_leave` depth, so with no regions to bound it is not
//! implemented either — reading it would change nothing observable.
//!
//! Nothing here writes a byte unless a target is configured: [`start`] is the
//! only entry point that opens a destination, and every other function returns
//! immediately when it did not.

use std::os::unix::io::RawFd;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

/// The `evt` field of the `version` record: the event-format version this
/// stream conforms to. Bumped by git when records are removed or reinterpreted.
/// `tr2_tgt_event.c`'s `TR2_EVENT_VERSION`.
const EVENT_VERSION: &str = "4";

/// The `exe` field of the `version` record — the git version this port serves,
/// matching what `git version` prints.
const GIT_VERSION: &str = "2.55.0";

/// Sentinel file that parks a directory target once it holds too many traces.
/// `tr2_dst.c`'s `DISCARD_SENTINEL_NAME`.
const DISCARD_SENTINEL_NAME: &str = "git-trace2-discard";

/// How many `<sid>.<n>` names an auto-named (directory) target will try before
/// giving up. `tr2_dst.c`'s `MAX_AUTO_ATTEMPTS`.
const MAX_AUTO_ATTEMPTS: u32 = 10;

/// Env var carrying the parent process's SID into a child git, so a nested
/// invocation's events join the parent's session. `tr2_sid.c`.
const ENVVAR_PARENT_SID: &str = "GIT_TRACE2_PARENT_SID";

/// Env var carrying the parent's command hierarchy into a child git, so
/// `cmd_name`'s `hierarchy` field spells the full chain. `tr2_cmd_name.c`.
const ENVVAR_PARENT_NAME: &str = "GIT_TRACE2_PARENT_NAME";

// ---------------------------------------------------------------------------
// Settings (tr2_sysenv.c)
// ---------------------------------------------------------------------------

/// A Trace2 setting, each readable from either an environment variable or a
/// git config key. Only the settings this module actually acts on are listed;
/// the normal/perf target settings are not served (see the module docs), so
/// naming them here would claim support this file does not provide.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Var {
    /// Comma-delimited config key globs to echo as `def_param` records.
    CfgParam,
    /// Comma-delimited environment variable names to echo as `def_param`.
    EnvVars,
    /// Whether to report destination-open failures on stderr.
    DstDebug,
    /// Where the event stream goes.
    Event,
    /// Whether to omit `time`/`file`/`line` from most records.
    EventBrief,
    /// Directory-target file-count ceiling before traces are discarded.
    MaxFiles,
}

impl Var {
    /// The environment variable that sets this, and the config key it
    /// overrides. `tr2_sysenv_settings`; the config names are the lowercased
    /// form git's config parser normalizes to.
    const fn names(self) -> (&'static str, &'static str) {
        match self {
            Var::CfgParam => ("GIT_TRACE2_CONFIG_PARAMS", "trace2.configparams"),
            Var::EnvVars => ("GIT_TRACE2_ENV_VARS", "trace2.envvars"),
            Var::DstDebug => ("GIT_TRACE2_DST_DEBUG", "trace2.destinationdebug"),
            Var::Event => ("GIT_TRACE2_EVENT", "trace2.eventtarget"),
            Var::EventBrief => ("GIT_TRACE2_EVENT_BRIEF", "trace2.eventbrief"),
            Var::MaxFiles => ("GIT_TRACE2_MAX_FILES", "trace2.maxfiles"),
        }
    }

    /// The name git prints when it names this setting in a warning. Git always
    /// uses the environment spelling even when the value came from config —
    /// `tr2_sysenv_display_name`.
    const fn display_name(self) -> &'static str {
        self.names().0
    }
}

/// Every setting this module reads, in the order `tr2_sysenv_settings` lists
/// them. Used to scan a config section once and pick out the keys of interest.
const SETTINGS: &[Var] = &[
    Var::CfgParam,
    Var::EnvVars,
    Var::DstDebug,
    Var::Event,
    Var::EventBrief,
    Var::MaxFiles,
];

/// The system and global config files git reads before anything else.
///
/// Git resolves these with `read_very_early_config`: the system and global
/// files, following includes, and pointedly *not* the repository, the worktree
/// or the command line. That is why `git -c trace2.eventTarget=… ` does not
/// enable tracing and a repo-local `[trace2]` section is inert — both verified
/// against git 2.55.0.
///
/// `gix::config::File::from_globals` would be the obvious way to read that set,
/// but it also resolves `Source::GitInstallation`, which shells out to a real
/// `git config -lz --show-origin --name-only` to ask another git binary where
/// its installation config lives. Trace2 initialization runs on *every*
/// invocation, so that would add a subprocess to every command this shadow
/// serves — measured as one spawn per run outside a repository, where nothing
/// else warms the cache. The sources are therefore listed explicitly, omitting
/// the installation one: it describes the prefix of a *different* git binary,
/// which is not this shadow's own installation config in the first place.
/// `/etc/gitconfig`, `$XDG_CONFIG_HOME/git/config` and `~/.gitconfig` are read,
/// and `GIT_CONFIG_SYSTEM` / `GIT_CONFIG_GLOBAL` / `GIT_CONFIG_NOSYSTEM` are
/// honored, because each source resolves its own location.
fn early_config() -> gix::config::File {
    use gix::config::file::{Metadata, includes, init};
    use gix::config::source::Kind;

    // `gix_path::env::var`, which is not re-exported through `gix`: plain
    // `var_os`, except that `HOME` falls back to the platform home directory.
    let mut env_var = |name: &str| -> Option<std::ffi::OsString> {
        if name == "HOME" {
            home_dir().map(std::path::PathBuf::into_os_string)
        } else {
            std::env::var_os(name)
        }
    };

    let metas = [Kind::System, Kind::Global]
        .iter()
        .flat_map(|kind| kind.sources())
        .filter_map(|source| {
            let path = source
                .storage_location(&mut env_var)
                .filter(|p| p.is_file());
            Metadata {
                path,
                source: *source,
                level: 0,
                trust: gix::sec::Trust::Full,
            }
            .into()
        });

    let home = home_dir();
    let options = init::Options {
        includes: includes::Options::follow_without_conditional(home.as_deref()),
        ..Default::default()
    };
    gix::config::File::from_paths_metadata(metas, options)
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// `gix_path::env::home_dir`: `HOME` when set, else the platform's own answer.
fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .map(Into::into)
        .or_else(std::env::home_dir)
}

/// The Trace2 settings found in config, resolved once per process.
fn config_values() -> &'static Vec<(Var, String)> {
    static VALUES: OnceLock<Vec<(Var, String)>> = OnceLock::new();
    VALUES.get_or_init(|| {
        let mut found: Vec<(Var, String)> = Vec::new();
        let file = early_config();
        let Some(sections) = file.sections_by_name("trace2") else {
            return found;
        };
        // Walk every `[trace2]` section in file order and let a later entry
        // overwrite an earlier one, mirroring `tr2_sysenv_cb` being invoked
        // once per config line with last-one-wins assignment.
        for section in sections {
            for name in section.value_names() {
                let key = format!("trace2.{}", name.to_lowercase());
                let Some(var) = SETTINGS.iter().find(|v| v.names().1 == key) else {
                    continue;
                };
                let Some(value) = section.value(&name) else {
                    continue;
                };
                let value = value.to_string();
                match found.iter_mut().find(|(v, _)| v == var) {
                    Some(slot) => slot.1 = value,
                    None => found.push((*var, value)),
                }
            }
        }
        found
    })
}

/// The value of a Trace2 setting: the environment variable when it is set and
/// non-empty, else the config value, else `None`. `tr2_sysenv_get` — the
/// environment is consulted last and overwrites, so it wins.
fn sysenv_get(var: Var) -> Option<String> {
    let (env_name, _) = var.names();
    if let Ok(v) = std::env::var(env_name) {
        if !v.is_empty() {
            return Some(v);
        }
    }
    config_values()
        .iter()
        .find(|(v, _)| *v == var)
        .map(|(_, s)| s.clone())
}

/// Whether destination-open failures should be reported on stderr.
/// `tr2_dst_want_warning`: any value parsing as a positive integer enables it.
fn want_dst_warning() -> bool {
    sysenv_get(Var::DstDebug).is_some_and(|v| atoi(&v) > 0)
}

/// C `atoi`: leading whitespace, optional sign, then as many digits as parse.
/// Anything else yields 0. Git reads `GIT_TRACE2_MAX_FILES` and
/// `GIT_TRACE2_DST_DEBUG` with it, so `"3x"` is 3 and `"x"` is 0.
fn atoi(s: &str) -> i64 {
    let s = s.trim_start();
    let (neg, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    let n: i64 = digits[..end].parse().unwrap_or(0);
    if neg { -n } else { n }
}

/// Git's `git_parse_maybe_bool` for the `*Brief` settings: `Some(true)` /
/// `Some(false)` for a recognized spelling, `None` for anything else (which
/// leaves the current setting untouched rather than forcing it false).
fn parse_maybe_bool(s: &str) -> Option<bool> {
    let t = s.trim();
    if t.eq_ignore_ascii_case("true")
        || t.eq_ignore_ascii_case("yes")
        || t.eq_ignore_ascii_case("on")
    {
        return Some(true);
    }
    if t.eq_ignore_ascii_case("false")
        || t.eq_ignore_ascii_case("no")
        || t.eq_ignore_ascii_case("off")
        || t.is_empty()
    {
        return Some(false);
    }
    match t.parse::<i64>() {
        Ok(0) => Some(false),
        Ok(_) => Some(true),
        Err(_) => None,
    }
}

// ---------------------------------------------------------------------------
// Session id (tr2_sid.c)
// ---------------------------------------------------------------------------

/// Build this process's session id, inheriting the parent's when one was
/// exported. `tr2_sid_compute`: `[<parent>/]<utc datetime>-<host>-<pid>`,
/// where `<host>` is `H` plus the first 8 hex digits of the SHA-1 of the
/// hostname (`Localhost` when the hostname is unavailable) and `<pid>` is `P`
/// plus the low 32 bits of the process id in 8 hex digits.
fn compute_sid() -> String {
    let mut sid = String::new();
    if let Ok(parent) = std::env::var(ENVVAR_PARENT_SID) {
        if !parent.is_empty() {
            sid.push_str(&parent);
            sid.push('/');
        }
    }

    let (tm, usec) = now_utc();
    sid.push_str(&format!(
        "{:4}{:02}{:02}T{:02}{:02}{:02}.{:06}Z",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        usec
    ));

    sid.push('-');
    match hostname() {
        Some(host) => {
            let mut hasher = gix::hash::hasher(gix::hash::Kind::Sha1);
            hasher.update(host.as_bytes());
            match hasher.try_finalize() {
                Ok(id) => {
                    sid.push('H');
                    sid.push_str(&id.to_hex().to_string()[..8]);
                }
                Err(_) => sid.push_str("Localhost"),
            }
        }
        None => sid.push_str("Localhost"),
    }

    sid.push_str(&format!("-P{:08x}", std::process::id()));
    sid
}

/// The machine's hostname, or `None` when `gethostname` fails or reports an
/// empty name — the case git falls back to the literal `Localhost` for.
fn hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    std::str::from_utf8(&buf[..end])
        .ok()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Time (tr2_tbuf.c)
// ---------------------------------------------------------------------------

/// The current UTC time broken into fields, plus the microseconds within the
/// second. `gettimeofday` + `gmtime_r`, exactly as `tr2_tbuf_utc_datetime*` do.
fn now_utc() -> (libc::tm, i64) {
    let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
    unsafe { libc::gettimeofday(&mut tv, std::ptr::null_mut()) };
    let secs = tv.tv_sec as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::gmtime_r(&secs, &mut tm) };
    (tm, tv.tv_usec as i64)
}

/// The `time` field's spelling: `tr2_tbuf_utc_datetime_extended`'s
/// `"%4d-%02d-%02dT%02d:%02d:%02d.%06ldZ"`.
fn utc_datetime_extended() -> String {
    let (tm, usec) = now_utc();
    format!(
        "{:4}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        usec
    )
}

// ---------------------------------------------------------------------------
// JSON (json-writer.c)
// ---------------------------------------------------------------------------

/// A single-line JSON object, built in the order fields are appended.
///
/// Git's `json-writer.c` emits compact JSON with no reordering, so building
/// the text directly reproduces it byte for byte and keeps field order — which
/// consumers of the event stream do rely on — under explicit control.
struct Jw {
    buf: String,
}

impl Jw {
    /// Open an object.
    fn new() -> Self {
        Jw {
            buf: String::from("{"),
        }
    }

    /// Separate this field from the previous one, if any.
    fn comma(&mut self) {
        if !self.buf.ends_with('{') && !self.buf.ends_with('[') {
            self.buf.push(',');
        }
    }

    /// Append `"<key>":"<value>"`.
    fn str(&mut self, key: &str, value: &str) {
        self.comma();
        quote(&mut self.buf, key);
        self.buf.push(':');
        quote(&mut self.buf, value);
    }

    /// Append `"<key>":<value>` for an integer — `jw_object_intmax`.
    fn int(&mut self, key: &str, value: i64) {
        self.comma();
        quote(&mut self.buf, key);
        self.buf.push_str(&format!(":{value}"));
    }

    /// Append `"<key>":<value>` for a duration in seconds. `jw_object_double`
    /// with precision 6 formats as `%.6f`, keeping trailing zeros.
    fn secs(&mut self, key: &str, value: f64) {
        self.comma();
        quote(&mut self.buf, key);
        self.buf.push_str(&format!(":{value:.6}"));
    }

    /// Append `"<key>":[…]` for an array of strings — `jw_array_argv`.
    fn array(&mut self, key: &str, items: impl IntoIterator<Item = impl AsRef<str>>) {
        self.comma();
        quote(&mut self.buf, key);
        self.buf.push_str(":[");
        let mut first = true;
        for item in items {
            if !first {
                self.buf.push(',');
            }
            first = false;
            quote(&mut self.buf, item.as_ref());
        }
        self.buf.push(']');
    }

    /// Close the object and yield the line (without its newline).
    fn finish(mut self) -> String {
        self.buf.push('}');
        self.buf
    }
}

/// Git's `append_quoted_string`: quote, escaping `"` and `\`, the five
/// short-form control characters, and any other byte below 0x20 as `\u00xx`.
/// Everything at 0x20 or above — including non-ASCII UTF-8 — is passed through
/// unchanged, so writing chars here is byte-identical to git writing bytes.
fn quote(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{c}' => out.push_str("\\f"),
            '\u{8}' => out.push_str("\\b"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

// ---------------------------------------------------------------------------
// Destination (tr2_dst.c)
// ---------------------------------------------------------------------------

/// An opened event destination.
struct Dst {
    /// The descriptor every record is written to.
    fd: RawFd,
    /// Set when the target directory was already full and this stream is going
    /// to the discard sentinel — the `version` record is followed by a
    /// `too_many_files` record to say so.
    too_many_files: bool,
}

/// Resolve `GIT_TRACE2_EVENT` / `trace2.eventTarget` to a descriptor.
///
/// Faithful port of `tr2_dst_get_trace_fd`, in git's order: the disabled
/// spellings, the "write to stderr" spellings, a bare single digit as a
/// descriptor number, an absolute path (a directory gets an auto-named file
/// inside it, anything else is appended to), then a `af_unix:` socket. A value
/// matching none of those is always warned about, regardless of
/// `trace2.destinationDebug`.
fn open_dst() -> Option<Dst> {
    let value = sysenv_get(Var::Event)?;

    if value.is_empty() || value == "0" || value.eq_ignore_ascii_case("false") {
        return None;
    }
    if value == "1" || value.eq_ignore_ascii_case("true") {
        return Some(Dst {
            fd: libc::STDERR_FILENO,
            too_many_files: false,
        });
    }
    if value.len() == 1 && value.as_bytes()[0].is_ascii_digit() {
        return Some(Dst {
            fd: atoi(&value) as RawFd,
            too_many_files: false,
        });
    }
    if value.starts_with('/') {
        let path = std::path::Path::new(&value);
        return if path.is_dir() {
            open_auto_path(path)
        } else {
            open_path(path)
        };
    }
    if value.starts_with("af_unix:") {
        return open_unix_socket(&value);
    }

    // Git warns about a malformed value unconditionally — it is the only way a
    // user learns that a typo'd target silently disabled tracing.
    eprintln!(
        "warning: trace2: unknown value for '{}': '{value}'",
        Var::Event.display_name()
    );
    None
}

/// Open a plain file target: append, creating it if absent. `tr2_dst_try_path`.
fn open_path(path: &std::path::Path) -> Option<Dst> {
    match std::fs::OpenOptions::new()
        .write(true)
        .append(true)
        .create(true)
        .open(path)
    {
        Ok(file) => Some(Dst {
            fd: into_raw_fd(file),
            too_many_files: false,
        }),
        Err(e) => {
            if want_dst_warning() {
                eprintln!(
                    "warning: trace2: could not open '{}' for '{}' tracing: {}",
                    path.display(),
                    Var::Event.display_name(),
                    errno_text(&e)
                );
            }
            None
        }
    }
}

/// Open an auto-named file inside a directory target: `<dir>/<sid>`, retrying
/// as `<sid>.1` … `<sid>.9` on collision. `tr2_dst_try_auto_path`.
///
/// The name uses only the last component of the SID, so a nested git writes
/// beside its parent rather than into a path built from the whole chain.
fn open_auto_path(dir: &std::path::Path) -> Option<Dst> {
    let sid = sid();
    let leaf = sid.rsplit('/').next().unwrap_or(sid);
    let base = dir.join(leaf);

    match too_many_files(dir) {
        // The directory is already parked: stay quiet unless asked to debug.
        TooMany::Sentinel => {
            if want_dst_warning() {
                eprintln!(
                    "warning: trace2: not opening {} trace file due to too many files in target directory {}",
                    Var::Event.display_name(),
                    dir.display()
                );
            }
            None
        }
        // We are the process that tripped the limit, and we hold the freshly
        // created sentinel — git writes this session into it so the file that
        // parks the directory also records why.
        TooMany::JustCreated(file) => Some(Dst {
            fd: into_raw_fd(file),
            too_many_files: true,
        }),
        TooMany::No => {
            for attempt in 0..MAX_AUTO_ATTEMPTS {
                let path = if attempt == 0 {
                    base.clone()
                } else {
                    std::path::PathBuf::from(format!("{}.{attempt}", base.display()))
                };
                if let Ok(file) = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                {
                    return Some(Dst {
                        fd: into_raw_fd(file),
                        too_many_files: false,
                    });
                }
            }
            if want_dst_warning() {
                eprintln!(
                    "warning: trace2: could not open '{}' for '{}' tracing: {}",
                    base.display(),
                    Var::Event.display_name(),
                    errno_text(&std::io::Error::last_os_error())
                );
            }
            None
        }
    }
}

/// Outcome of the directory file-count check.
enum TooMany {
    /// Under the ceiling (or the check is disabled) — write a trace normally.
    No,
    /// A sentinel was already present: another process parked this directory.
    Sentinel,
    /// This process tripped the limit and created the sentinel, which it now
    /// owns and writes into.
    JustCreated(std::fs::File),
}

/// Guard a directory target against unbounded growth. `tr2_dst_too_many_files`:
/// disabled unless `trace2.maxFiles` is a positive count, then a sentinel file
/// short-circuits, then the directory's entries are counted up to the ceiling.
///
/// The count includes `.` and `..`, because git counts raw `readdir` results
/// and both are returned there. `std::fs::read_dir` filters them out, so they
/// are added back — without them a ceiling of 3 would admit two more files
/// than git allows.
fn too_many_files(dir: &std::path::Path) -> TooMany {
    let max = match sysenv_get(Var::MaxFiles) {
        Some(v) if !v.is_empty() && atoi(&v) >= 0 => atoi(&v),
        _ => 0,
    };
    if max == 0 {
        return TooMany::No;
    }

    let sentinel = dir.join(DISCARD_SENTINEL_NAME);
    if sentinel.symlink_metadata().is_ok() {
        return TooMany::Sentinel;
    }

    let mut count: i64 = 0;
    let dot_entries = 2; // `.` and `..`, which `readdir` yields and `read_dir` does not.
    let mut entries = std::fs::read_dir(dir).ok().into_iter().flatten();
    while count < max {
        if count < dot_entries {
            count += 1;
            continue;
        }
        if entries.next().is_none() {
            break;
        }
        count += 1;
    }

    if count >= max {
        // `create_new` is git's `O_CREAT | O_EXCL`: whoever wins the race owns
        // the sentinel, and the losers see it on their next check.
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&sentinel)
        {
            Ok(file) => TooMany::JustCreated(file),
            Err(_) => TooMany::Sentinel,
        }
    } else {
        TooMany::No
    }
}

/// Connect an `af_unix:[stream:|dgram:]<absolute path>` target.
/// `tr2_dst_try_unix_domain_socket`.
///
/// Trace2 writes whole messages, so either socket type carries them. With no
/// explicit type git tries `SOCK_STREAM` and falls back to `SOCK_DGRAM`, but
/// only when the stream attempt failed with `EPROTOTYPE` — any other error is
/// reported rather than retried against the wrong type.
fn open_unix_socket(value: &str) -> Option<Dst> {
    use std::os::unix::net::{UnixDatagram, UnixStream};

    let (path, try_stream, try_dgram) = if let Some(p) = value.strip_prefix("af_unix:stream:") {
        (p, true, false)
    } else if let Some(p) = value.strip_prefix("af_unix:dgram:") {
        (p, false, true)
    } else {
        (value.strip_prefix("af_unix:").unwrap_or(""), true, true)
    };

    if path.is_empty() {
        if want_dst_warning() {
            eprintln!(
                "warning: trace2: invalid AF_UNIX value '{value}' for '{}' tracing",
                Var::Event.display_name()
            );
        }
        return None;
    }
    if !path.starts_with('/') || path.len() >= sun_path_capacity() {
        if want_dst_warning() {
            eprintln!(
                "warning: trace2: invalid AF_UNIX path '{path}' for '{}' tracing",
                Var::Event.display_name()
            );
        }
        return None;
    }

    let mut last = std::io::Error::from_raw_os_error(libc::ENOENT);
    if try_stream {
        match UnixStream::connect(path) {
            Ok(sock) => {
                return Some(Dst {
                    fd: into_raw_fd(sock),
                    too_many_files: false,
                });
            }
            Err(e) => {
                let wrong_type = e.raw_os_error() == Some(libc::EPROTOTYPE);
                last = e;
                if !wrong_type && try_dgram {
                    // Git only falls through to DGRAM when the socket exists but
                    // is of the other type; a refused or missing socket is final.
                    return uds_failed(path, &last);
                }
            }
        }
    }
    if try_dgram {
        match UnixDatagram::unbound().and_then(|s| s.connect(path).map(|()| s)) {
            Ok(sock) => {
                return Some(Dst {
                    fd: into_raw_fd(sock),
                    too_many_files: false,
                });
            }
            Err(e) => last = e,
        }
    }
    uds_failed(path, &last)
}

/// Report a socket target that could not be connected, then disable tracing.
fn uds_failed(path: &str, err: &std::io::Error) -> Option<Dst> {
    if want_dst_warning() {
        eprintln!(
            "warning: trace2: could not connect to socket '{path}' for '{}' tracing: {}",
            Var::Event.display_name(),
            errno_text(err)
        );
    }
    None
}

/// How many bytes `sockaddr_un::sun_path` holds — 104 on macOS, 108 on Linux.
/// Git rejects a longer path outright rather than letting `strlcpy` truncate it.
fn sun_path_capacity() -> usize {
    let sa: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    sa.sun_path.len()
}

/// The bare `strerror` text git's `warning(…: %s, strerror(errno))` prints.
/// Rust appends its own `(os error N)`, which git does not, so it is trimmed.
fn errno_text(err: &std::io::Error) -> String {
    let text = err.to_string();
    match text.find(" (os error ") {
        Some(cut) => text[..cut].to_string(),
        None => text,
    }
}

/// Take ownership of an object's descriptor and leak it deliberately: the
/// destination stays open for the life of the process, and git likewise never
/// closes it before `atexit`.
fn into_raw_fd<T: std::os::unix::io::IntoRawFd>(owner: T) -> RawFd {
    owner.into_raw_fd()
}

// ---------------------------------------------------------------------------
// Process state
// ---------------------------------------------------------------------------

/// Everything the event target needs after initialization.
struct Event {
    dst: Dst,
    /// `trace2.eventBrief` — omit `time`, `file` and `line` from most records.
    brief: bool,
    /// This process's session id, shared by every record.
    sid: String,
    /// When the process clock started, for the `t_abs` elapsed field.
    t0: std::time::Instant,
}

/// The event target, or `None` when no destination was configured. Set exactly
/// once, by [`start`].
static EVENT: OnceLock<Option<Event>> = OnceLock::new();

/// This process's SID, computed on first use. Git computes it lazily too, and
/// only ever when a target is active — with tracing off nothing is computed and
/// `GIT_TRACE2_PARENT_SID` is never exported, verified against git 2.55.0.
fn sid() -> &'static str {
    static SID: OnceLock<String> = OnceLock::new();
    SID.get_or_init(compute_sid)
}

/// This process's SID for callers outside tracing. `trace2_session_id()` is what
/// `upload-pack` advertises under `transfer.advertiseSID`, and it returns the
/// same id whether or not a trace target is active.
pub(crate) fn session_id() -> &'static str {
    sid()
}

/// The live event target, or `None` when tracing is off or not yet started.
fn event() -> Option<&'static Event> {
    EVENT.get()?.as_ref()
}

/// Seconds since the process clock started, for `t_abs` / `t_rel`.
fn elapsed(ev: &Event) -> f64 {
    ev.t0.elapsed().as_secs_f64()
}

/// Write one record to the destination.
///
/// A single unbuffered `write` per line, like git: the target may be an
/// `O_APPEND` file or a socket shared with other processes, where the kernel's
/// atomic append is what keeps interleaved records whole. A short write is not
/// retried — a partial retry would land after another writer's record and
/// corrupt the stream — and a failed write is simply dropped, since telemetry
/// must never disturb the command that produced it. Rust ignores `SIGPIPE`
/// process-wide, matching git's `sigchain_push(SIGPIPE, SIG_IGN)` here.
fn write_line(ev: &Event, line: &str) {
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');
    unsafe {
        libc::write(ev.dst.fd, buf.as_ptr().cast(), buf.len());
    }
}

/// Begin a record with the fields every event carries: `event`, `sid`,
/// `thread`, and — unless brief mode suppressed them — `time`, `file` and
/// `line`. `event_fmt_prepare`.
///
/// Brief mode keeps `time` on `version` and `atexit` alone, so a stream still
/// has a wall-clock anchor at each end. The `file`/`line` pair names the source
/// position that emitted the record; here those are this crate's Rust
/// positions rather than git's C ones, which is the one field pair whose values
/// cannot match stock git.
fn prepare(ev: &Event, name: &str, file: &str, line: u32) -> Jw {
    let mut jw = Jw::new();
    jw.str("event", name);
    jw.str("sid", &ev.sid);
    jw.str("thread", "main");
    if !ev.brief || name == "version" || name == "atexit" {
        jw.str("time", &utc_datetime_extended());
    }
    if !ev.brief {
        jw.str("file", file);
        jw.int("line", line as i64);
    }
    jw
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// Open the event target and emit the `version` and `start` records.
///
/// Call this once, first thing, with the full argument vector. When no target
/// is configured it opens nothing, writes nothing, and leaves the environment
/// untouched — the only cost is reading the global config once.
pub fn start(argv: &[String]) {
    let initialized = EVENT.get_or_init(|| {
        let dst = open_dst()?;
        let brief = sysenv_get(Var::EventBrief)
            .and_then(|v| parse_maybe_bool(&v))
            .unwrap_or(false);
        Some(Event {
            dst,
            brief,
            sid: sid().to_string(),
            t0: std::time::Instant::now(),
        })
    });
    let Some(ev) = initialized.as_ref() else {
        return;
    };

    // Children inherit the session so their records join this one. Git exports
    // this only once tracing is active, so an untraced git leaves no trace of
    // itself in its children's environment.
    unsafe { std::env::set_var(ENVVAR_PARENT_SID, &ev.sid) };

    let mut jw = prepare(ev, "version", file!(), line!());
    jw.str("evt", EVENT_VERSION);
    jw.str("exe", GIT_VERSION);
    write_line(ev, &jw.finish());

    // The directory target was full, so this session is being written into the
    // sentinel that parked it. Say so straight after `version`, as git does.
    if ev.dst.too_many_files {
        let jw = prepare(ev, "too_many_files", file!(), line!());
        write_line(ev, &jw.finish());
    }

    let mut jw = prepare(ev, "start", file!(), line!());
    jw.secs("t_abs", elapsed(ev));
    jw.array("argv", argv);
    write_line(ev, &jw.finish());
}

/// Emit `cmd_name` for the resolved subcommand, then the `def_param` records
/// requested by `trace2.configParams` and `trace2.envVars`.
///
/// Call this once the verb is final — after alias expansion and autocorrection
/// — matching where git calls `trace2_cmd_name` in `run_builtin`.
pub fn cmd_name(name: &str) {
    let Some(ev) = event() else {
        return;
    };

    // `tr2_cmd_name_append_hierarchy`: prepend the parent's chain, then export
    // the extended chain for our own children.
    let hierarchy = match std::env::var(ENVVAR_PARENT_NAME) {
        Ok(parent) if !parent.is_empty() => format!("{parent}/{name}"),
        _ => name.to_string(),
    };
    unsafe { std::env::set_var(ENVVAR_PARENT_NAME, &hierarchy) };

    let mut jw = prepare(ev, "cmd_name", file!(), line!());
    jw.str("name", name);
    jw.str("hierarchy", &hierarchy);
    write_line(ev, &jw.finish());

    list_config(ev);
    list_env_vars(ev);
}

/// Emit a `def_param` for every config entry matching `trace2.configParams`.
///
/// `tr2_cfg_list_config_fl`: the setting is a comma-delimited list of globs,
/// matched against the full dotted key with git's `wildmatch` in case-folding
/// mode. Unlike the Trace2 settings themselves, this walks the *whole* config
/// cascade — repository and command-line values included, which is why a
/// `-c foo.bar=baz` override shows up here with scope `command`.
fn list_config(ev: &Event) {
    let Some(patterns) = split_list(Var::CfgParam) else {
        return;
    };
    // The repository's merged snapshot when we are in one, else the global
    // cascade, so `def_param` reports something outside a repo exactly as
    // git's `read_early_config` does.
    match crate::setup::discover() {
        Ok(repo) => emit_matching(ev, &repo.config_snapshot(), &patterns),
        Err(_) => emit_matching(ev, &crate::config::global_config(), &patterns),
    }
}

/// Walk a config file in declaration order and emit a `def_param` for every
/// entry whose key matches one of the globs.
fn emit_matching(ev: &Event, file: &gix::config::File, patterns: &[String]) {
    use gix::bstr::ByteSlice;

    for section in file.sections() {
        let scope = scope_name(section.meta().source);
        let header = section.header();
        // A subsection joins the key as `<section>.<subsection>.<name>`. Git
        // normalizes the section and value names to lowercase but leaves the
        // subsection verbatim, so `[remote "MixedCase"]` reports as
        // `remote.MixedCase.url` — verified against git 2.55.0.
        let section_name = header.name().to_str_lossy().to_lowercase();
        let prefix = match header.subsection_name() {
            Some(sub) => format!("{section_name}.{}", sub.to_str_lossy()),
            None => section_name,
        };
        for name in section.value_names() {
            let key = format!("{prefix}.{}", name.to_lowercase());
            if !patterns.iter().any(|p| glob_matches(p, &key)) {
                continue;
            }
            for value in section.values(&name) {
                let mut jw = prepare(ev, "def_param", file!(), line!());
                jw.str("scope", scope);
                jw.str("param", &key);
                jw.str("value", &value.to_str_lossy());
                write_line(ev, &jw.finish());
            }
        }
    }
}

/// Emit a `def_param` for each set variable named by `trace2.envVars`.
/// `tr2_list_env_vars_fl`: unset and empty variables are skipped, and the
/// records carry scope `command` since a variable has no config file to come
/// from.
fn list_env_vars(ev: &Event) {
    let Some(names) = split_list(Var::EnvVars) else {
        return;
    };
    for name in names {
        let Ok(value) = std::env::var(&name) else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        let mut jw = prepare(ev, "def_param", file!(), line!());
        jw.str("scope", "command");
        jw.str("param", &name);
        jw.str("value", &value);
        write_line(ev, &jw.finish());
    }
}

/// Split a comma-delimited setting into trimmed, non-empty entries.
/// `string_list_split_f` with `STRING_LIST_SPLIT_TRIM`. `None` when the setting
/// is unset or yields nothing, the case git short-circuits on.
fn split_list(var: Var) -> Option<Vec<String>> {
    let raw = sysenv_get(var)?;
    if raw.is_empty() {
        return None;
    }
    let items: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    (!items.is_empty()).then_some(items)
}

/// Git's `wildmatch(pattern, key, WM_CASEFOLD)`: case-insensitive, and without
/// `WM_PATHNAME` so `*` spans the dots in a dotted key.
fn glob_matches(pattern: &str, key: &str) -> bool {
    use gix::bstr::ByteSlice;
    gix::glob::wildmatch(
        pattern.as_bytes().as_bstr(),
        key.as_bytes().as_bstr(),
        gix::glob::wildmatch::Mode::IGNORE_CASE,
    )
}

/// The `scope` field's spelling for a config source — git's
/// `config_scope_name`. The installation and system files are both `system`;
/// the XDG and `~/.gitconfig` files are both `global`; everything injected by
/// the caller (`-c`, `GIT_CONFIG_*`, programmatic) is `command`.
fn scope_name(source: gix::config::Source) -> &'static str {
    use gix::config::Source;
    match source {
        Source::GitInstallation | Source::System => "system",
        Source::Git | Source::User => "global",
        Source::Local => "local",
        Source::Worktree => "worktree",
        Source::Env | Source::Cli | Source::Api | Source::EnvOverride => "command",
    }
}

/// Emit an `error` record for a command failure.
///
/// Git also carries a `fmt` field holding the unexpanded printf format string,
/// so post-processors can group errors by kind. This port's errors arrive
/// already formatted, with no format string to report, so only `msg` is
/// emitted rather than inventing one.
pub fn error(msg: &str) {
    let Some(ev) = event() else {
        return;
    };
    let mut jw = prepare(ev, "error", file!(), line!());
    jw.str("msg", msg);
    write_line(ev, &jw.finish());
}

/// Emit `exec` immediately before replacing this process with another program,
/// and return the id that [`exec_result`] reports against if the exec fails.
///
/// A successful `exec` never returns, so this record is the last one the
/// session writes — which is exactly why git emits it beforehand.
pub fn exec(exe: &str, argv: &[String]) -> u32 {
    static NEXT_ID: AtomicU32 = AtomicU32::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let Some(ev) = event() else {
        return id;
    };
    let mut jw = prepare(ev, "exec", file!(), line!());
    jw.int("exec_id", id as i64);
    jw.str("exe", exe);
    jw.array("argv", argv);
    write_line(ev, &jw.finish());
    id
}

/// Emit `exec_result` when an [`exec`] returned, meaning it failed.
pub fn exec_result(id: u32, code: i32) {
    let Some(ev) = event() else {
        return;
    };
    let mut jw = prepare(ev, "exec_result", file!(), line!());
    jw.int("exec_id", id as i64);
    jw.int("code", code as i64);
    write_line(ev, &jw.finish());
}

/// Emit the closing `exit` and `atexit` records. Call once, with the code the
/// process is about to return.
///
/// Git emits both: `exit` when the command returns and `atexit` when the
/// process actually unwinds. This port has one shutdown point, so the two are
/// written together and their `t_abs` values differ only by the write itself.
pub fn exit(code: i32) {
    let Some(ev) = event() else {
        return;
    };
    let mut jw = prepare(ev, "exit", file!(), line!());
    jw.secs("t_abs", elapsed(ev));
    jw.int("code", code as i64);
    write_line(ev, &jw.finish());

    let mut jw = prepare(ev, "atexit", file!(), line!());
    jw.secs("t_abs", elapsed(ev));
    jw.int("code", code as i64);
    write_line(ev, &jw.finish());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escaping_matches_gits_append_quoted_string() {
        // Git escapes the five short forms, quote and backslash, spells any
        // other sub-0x20 byte as \u00xx, and passes everything else — including
        // DEL and non-ASCII — through untouched.
        let mut out = String::new();
        quote(&mut out, "a\"b\\c\nd\te\r\u{c}\u{8}\u{1}\u{7f}é");
        assert_eq!(out, "\"a\\\"b\\\\c\\nd\\te\\r\\f\\b\\u0001\u{7f}é\"");
    }

    #[test]
    fn atoi_stops_at_the_first_non_digit() {
        // `GIT_TRACE2_MAX_FILES=3x` is 3 to git, and a non-numeric value is 0,
        // which disables the directory check rather than erroring.
        assert_eq!(atoi("3x"), 3);
        assert_eq!(atoi("x"), 0);
        assert_eq!(atoi("-4"), -4);
        assert_eq!(atoi("  12  "), 12);
        assert_eq!(atoi(""), 0);
    }

    #[test]
    fn maybe_bool_leaves_brief_alone_for_unparseable_values() {
        // git_parse_maybe_bool returns -1 for junk, and the caller then keeps
        // the previous setting instead of forcing it off.
        assert_eq!(parse_maybe_bool("true"), Some(true));
        assert_eq!(parse_maybe_bool("0"), Some(false));
        assert_eq!(parse_maybe_bool("2"), Some(true));
        assert_eq!(parse_maybe_bool("banana"), None);
    }

    #[test]
    fn config_globs_fold_case_and_span_dots() {
        // WM_CASEFOLD without WM_PATHNAME: `*` crosses the dot separators that
        // a pathname-mode match would stop at.
        assert!(glob_matches("core.*", "core.bare"));
        assert!(glob_matches("CORE.*", "core.bare"));
        assert!(glob_matches("*.name", "user.name"));
        assert!(glob_matches("remote.*.url", "remote.origin.url"));
        assert!(!glob_matches("core.*", "user.name"));
    }

    #[test]
    fn t_abs_keeps_six_decimals_including_trailing_zeros() {
        // jw_object_double(…, 6, v) is a plain %.6f, so 0.5 is "0.500000" and
        // never the shortest round-trip form Rust would print by default.
        let mut jw = Jw::new();
        jw.secs("t_abs", 0.5);
        assert_eq!(jw.finish(), "{\"t_abs\":0.500000}");
    }

    #[test]
    fn records_are_compact_json_in_append_order() {
        let mut jw = Jw::new();
        jw.str("event", "start");
        jw.int("line", 58);
        jw.array("argv", ["git", "status"]);
        assert_eq!(
            jw.finish(),
            "{\"event\":\"start\",\"line\":58,\"argv\":[\"git\",\"status\"]}"
        );
    }
}
