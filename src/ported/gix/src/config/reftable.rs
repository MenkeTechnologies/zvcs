//! The configuration of the `reftable` ref storage backend: what git 2.56 reads,
//! lazily and at most once per ref store, the first time the backend writes a
//! table, compacts a stack or decides whether to write a reflog
//! (`reftable_be_write_options()`, refs/reftable-backend.c:361-392).
//!
//! The options are produced by a callback installed into the store's
//! [`Backend`](gix_ref::reftable::Backend) when the repository is opened, so
//! they are computed — and an invalid value is refused — at git's moment, not
//! when the repository is opened. git refuses with `die()`; the callback
//! cannot fail, so it hands the message to the [die hook](set_die_hook).

use std::{ffi::OsString, path::PathBuf, sync::OnceLock};

use gix_ref::{reftable::WriteConfig, store::WriteReflog};

/// What the callback does with a value git would `die()` on. It never returns.
static DIE_HOOK: OnceLock<fn(&str) -> !> = OnceLock::new();

/// Install what is called with the message (without `fatal: `) when a value
/// [`write_config()`] refuses is met inside a ref store operation. Only the
/// first installation takes effect.
///
/// Without one, the message is printed as `fatal: <message>` to stderr after
/// removing every registered lock and temporary file, and the process exits
/// with 128 — what `die()` does, including its `atexit` cleanup of lock files.
pub fn set_die_hook(hook: fn(&str) -> !) {
    let _ = DIE_HOOK.set(hook);
}

fn die(message: &str) -> ! {
    if let Some(hook) = DIE_HOOK.get() {
        hook(message);
    }
    gix_tempfile::registry::cleanup_tempfiles();
    eprintln!("fatal: {message}");
    std::process::exit(128)
}

/// One configuration variable as a `repo_config()` callback receives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The key with section and variable name lower-cased, `<section>.<name>`
    /// or `<section>.<subsection>.<name>` (the subsection kept as written).
    pub key: String,
    /// The value, `None` for a variable written without `=` (git's `NULL`).
    pub value: Option<String>,
    /// The file the value comes from, `None` for the command line, which is how
    /// `die_bad_number()` decides whether to name a file.
    pub file: Option<PathBuf>,
}

/// Every variable of `config` in the order `repo_config()` hands them to a
/// callback: section by section, each variable of a section in file order.
///
/// gitoxide's environment-override layer (`GIT_NO_REPLACE_OBJECTS`,
/// `GIT_NAMESPACE`, …) is left out: git reads those variables with `getenv()`
/// and never passes them to a config callback (`do_git_config_sequence()`,
/// config.c:1570-1602).
pub fn entries_in_order(config: &gix_config::File) -> Vec<Entry> {
    use gix_config::Source;
    use std::collections::HashMap;

    let mut out = Vec::new();
    for section in config.sections() {
        let meta = section.meta();
        if meta.source == Source::EnvOverride {
            continue;
        }
        let file = match meta.source {
            Source::Cli | Source::Env | Source::Api => None,
            _ => meta.path.clone(),
        };
        let header = section.header();
        let name = header.name().to_string().to_ascii_lowercase();
        let prefix = match header.subsection_name() {
            Some(sub) => format!("{name}.{sub}"),
            None => name,
        };
        let body = section.body();
        // A cursor per variable name, so that a name repeated within one
        // section yields its values in order. Only the last occurrence can be
        // told apart from an empty value when written without `=`.
        let mut seen: HashMap<String, usize> = HashMap::new();
        for raw_name in body.value_names() {
            let var = raw_name.to_ascii_lowercase();
            let index = seen.entry(var.clone()).or_insert(0);
            let at = *index;
            *index += 1;
            let values = body.values(&var);
            let value = if at + 1 == values.len() && body.value_implicit(&var) == Some(None) {
                None
            } else {
                values.get(at).map(|v| v.to_string())
            };
            out.push(Entry {
                key: format!("{prefix}.{var}"),
                value,
                file: file.clone(),
            });
        }
    }
    out
}

/// `reftable_be_write_options()` (refs/reftable-backend.c:361-392) over the
/// configuration `entries`, with `umask` the process umask and `autocompaction`
/// the value of `GIT_TEST_REFTABLE_AUTOCOMPACTION`, if set. `Err` is the
/// message git dies with, without `fatal: `.
///
/// ```c
/// opts->opts.default_permissions = calc_shared_perm(refs->base.repo, 0666 & ~mask);
/// opts->opts.disable_auto_compact =
///         !git_env_bool("GIT_TEST_REFTABLE_AUTOCOMPACTION", 1);
/// opts->opts.lock_timeout_ms = 100;
/// opts->log_all_ref_updates = LOG_REFS_UNSET;
///
/// repo_config(refs->base.repo, reftable_be_config, refs);
///
/// if (!opts->opts.block_size)
///         opts->opts.block_size = 4096;
/// ```
pub fn write_config(entries: &[Entry], umask: u32, autocompaction: Option<&OsString>) -> Result<WriteConfig, String> {
    let mut config = WriteConfig::default();
    config.opts.block_size = 0;

    let shared = shared_repository(entries)?;
    config.opts.default_permissions = Some(calc_shared_perm(shared, 0o666 & !umask));
    config.opts.disable_auto_compact = match autocompaction {
        None => false,
        Some(v) => {
            let v = v.to_string_lossy();
            match parse_maybe_bool(Some(&v)) {
                Some(b) => !b,
                None => {
                    return Err(format!(
                        "bad boolean environment value '{v}' for 'GIT_TEST_REFTABLE_AUTOCOMPACTION'"
                    ));
                }
            }
        }
    };
    config.opts.lock_timeout_ms = 100;
    config.log_all_ref_updates = None;

    for entry in entries {
        reftable_be_config(entry, &mut config)?;
    }

    if config.opts.block_size == 0 {
        config.opts.block_size = gix_reftable_default_block_size();
    }
    Ok(config)
}

/// The library's default block size, which git mirrors in
/// `reftable_be_write_options()` as reflog messages are trimmed to half of it.
fn gix_reftable_default_block_size() -> u32 {
    WriteConfig::default().opts.block_size
}

/// `reftable_be_config()` (refs/reftable-backend.c:323-359) for one variable.
fn reftable_be_config(entry: &Entry, config: &mut WriteConfig) -> Result<(), String> {
    let value = entry.value.as_deref();
    let opts = &mut config.opts;
    match entry.key.as_str() {
        "reftable.blocksize" => {
            let block_size = config_ulong(entry)?;
            if block_size > 16_777_215 {
                return Err("reftable block size cannot exceed 16MB".into());
            }
            opts.block_size = block_size as u32;
        }
        "reftable.restartinterval" => {
            let restart_interval = config_ulong(entry)?;
            if restart_interval > u64::from(u16::MAX) {
                // git's message says "block size" here too.
                return Err(format!("reftable block size cannot exceed {}", u16::MAX));
            }
            opts.restart_interval = restart_interval as u16;
        }
        "reftable.indexobjects" => {
            opts.skip_index_objects = !config_bool(&entry.key, value)?;
        }
        "reftable.geometricfactor" => {
            let factor = config_ulong(entry)?;
            if factor > u64::from(u8::MAX) {
                return Err(format!("reftable geometric factor cannot exceed {}", u8::MAX));
            }
            opts.auto_compaction_factor = factor as u8;
        }
        "reftable.locktimeout" => {
            let lock_timeout = config_int64(entry)?;
            // `lock_timeout > LONG_MAX` cannot hold where `long` is 64 bits.
            if lock_timeout < 0 && lock_timeout != -1 {
                return Err("reftable lock timeout does not support negative values other than -1".into());
            }
            opts.lock_timeout_ms = lock_timeout;
        }
        "core.logallrefupdates" => {
            config.log_all_ref_updates = Some(parse_log_all_ref_updates(value)?);
        }
        _ => {}
    }
    Ok(())
}

/// `refs_parse_log_all_ref_updates_config()` (refs.c:1055-1062).
fn parse_log_all_ref_updates(value: Option<&str>) -> Result<WriteReflog, String> {
    if value.is_some_and(|v| v.eq_ignore_ascii_case("always")) {
        return Ok(WriteReflog::Always);
    }
    Ok(if config_bool("core.logallrefupdates", value)? {
        WriteReflog::Normal
    } else {
        WriteReflog::Disable
    })
}

/// `repo_settings_get_shared_repository()` (repo-settings.c:196-208): the
/// last `core.sharedRepository` through `git_config_perm()`, `PERM_UMASK` (0)
/// if unset.
fn shared_repository(entries: &[Entry]) -> Result<i32, String> {
    let var = "core.sharedrepository";
    match entries.iter().rev().find(|e| e.key == var) {
        Some(entry) => config_perm(var, entry.value.as_deref()),
        None => Ok(PERM_UMASK),
    }
}

const PERM_UMASK: i32 = 0;
const OLD_PERM_GROUP: i64 = 1;
const OLD_PERM_EVERYBODY: i64 = 2;
const PERM_GROUP: i32 = 0o660;
const PERM_EVERYBODY: i32 = 0o664;

/// `git_config_perm()` (setup.c:2132-2181).
fn config_perm(var: &str, value: Option<&str>) -> Result<i32, String> {
    let Some(value) = value else {
        return Ok(PERM_GROUP);
    };
    match value {
        "umask" => return Ok(PERM_UMASK),
        "group" => return Ok(PERM_GROUP),
        "all" | "world" | "everybody" => return Ok(PERM_EVERYBODY),
        _ => {}
    }

    // `i = strtol(value, &endptr, 8)`; not an octal number: maybe true/false?
    let parsed = strto(value.as_bytes(), Some(8));
    if parsed.end != value.len() {
        return Ok(if config_bool(var, Some(value))? {
            PERM_GROUP
        } else {
            PERM_UMASK
        });
    }
    // `strtol()` clamps to `long`, which `int i` then truncates.
    let long = if parsed.overflow || parsed.magnitude > i64::MAX as u64 {
        if parsed.negative { i64::MIN } else { i64::MAX }
    } else if parsed.negative {
        -(parsed.magnitude as i64)
    } else {
        parsed.magnitude as i64
    };
    match long {
        0 => return Ok(PERM_UMASK),
        OLD_PERM_GROUP => return Ok(PERM_GROUP),
        OLD_PERM_EVERYBODY => return Ok(PERM_EVERYBODY),
        _ => {}
    }
    let i = long as i32;
    if (i & 0o600) != 0o600 {
        return Err(format!(
            "problem with core.sharedRepository filemode value (0{:03o}).\n\
             The owner of files must always have read and write permissions.",
            i
        ));
    }
    Ok(-(i & 0o666))
}

/// `calc_shared_perm()` (path.c:739-760) for the `shared_repo` setting.
fn calc_shared_perm(shared_repo: i32, mode: u32) -> u32 {
    let mut tweak = shared_repo.unsigned_abs();
    if mode & 0o200 == 0 {
        tweak &= !0o222;
    }
    if mode & 0o100 != 0 {
        tweak |= (tweak & 0o444) >> 2;
    }
    if shared_repo < 0 {
        (mode & !0o777) | tweak
    } else {
        mode | tweak
    }
}

/// `git_config_bool()` (config.c:1304-1310).
fn config_bool(var: &str, value: Option<&str>) -> Result<bool, String> {
    parse_maybe_bool(value).ok_or_else(|| format!("bad boolean config value '{}' for '{var}'", value.unwrap_or("")))
}

/// `git_parse_maybe_bool()` (parse.c:186-193); `None` is git's `-1`.
fn parse_maybe_bool(value: Option<&str>) -> Option<bool> {
    let Some(value) = value else {
        return Some(true);
    };
    if value.is_empty() {
        return Some(false);
    }
    for word in ["true", "yes", "on"] {
        if value.eq_ignore_ascii_case(word) {
            return Some(true);
        }
    }
    for word in ["false", "no", "off"] {
        if value.eq_ignore_ascii_case(word) {
            return Some(false);
        }
    }
    parse_signed(value, i64::from(i32::MAX)).ok().map(|v| v != 0)
}

/// Why `git_parse_signed()`/`git_parse_unsigned()` failed: the `errno` that
/// `die_bad_number()` turns into its reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumError {
    /// `ERANGE`
    OutOfRange,
    /// `EINVAL`
    Invalid,
}

/// `git_config_ulong()` (config.c:1256-1262) for `entry`.
fn config_ulong(entry: &Entry) -> Result<u64, String> {
    let value = entry.value.as_deref().unwrap_or("");
    parse_unsigned(value, u64::MAX).map_err(|err| bad_number(entry, err))
}

/// `git_config_int64()` (config.c:1247-1253) for `entry`.
fn config_int64(entry: &Entry) -> Result<i64, String> {
    let value = entry.value.as_deref().unwrap_or("");
    parse_signed(value, i64::MAX).map_err(|err| bad_number(entry, err))
}

/// `die_bad_number()` (config.c:1191-1227): files are named as git has them,
/// a value from the command line has no file.
fn bad_number(entry: &Entry, err: NumError) -> String {
    let value = entry.value.as_deref().unwrap_or("");
    let reason = match err {
        NumError::OutOfRange => "out of range",
        NumError::Invalid => "invalid unit",
    };
    match &entry.file {
        None => format!("bad numeric config value '{value}' for '{}': {reason}", entry.key),
        Some(path) => {
            let shown = path.to_string_lossy();
            let shown = shown.strip_prefix("./").unwrap_or(&shown);
            format!(
                "bad numeric config value '{value}' for '{}' in file {shown}: {reason}",
                entry.key
            )
        }
    }
}

/// `get_unit_factor()` (parse.c:5-16).
fn unit_factor(end: &str) -> Option<u64> {
    match end {
        "" => Some(1),
        _ if end.eq_ignore_ascii_case("k") => Some(1024),
        _ if end.eq_ignore_ascii_case("m") => Some(1024 * 1024),
        _ if end.eq_ignore_ascii_case("g") => Some(1024 * 1024 * 1024),
        _ => None,
    }
}

/// `git_parse_signed()` (parse.c:18-51) with `max` the largest value of the
/// target type.
fn parse_signed(value: &str, max: i64) -> Result<i64, NumError> {
    if value.is_empty() {
        return Err(NumError::Invalid);
    }
    let parsed = strto(value.as_bytes(), None);
    // `strtoimax()` sets `ERANGE` beyond `intmax_t`.
    let limit = if parsed.negative { 1u64 << 63 } else { i64::MAX as u64 };
    if parsed.overflow || parsed.magnitude > limit {
        return Err(NumError::OutOfRange);
    }
    if parsed.end == 0 {
        return Err(NumError::Invalid);
    }
    let factor = unit_factor(&value[parsed.end..]).ok_or(NumError::Invalid)? as i64;
    let val: i64 = if parsed.negative {
        (parsed.magnitude as i64).wrapping_neg()
    } else {
        parsed.magnitude as i64
    };
    if (val < 0 && (-max - 1) / factor > val) || (val > 0 && max / factor < val) {
        return Err(NumError::OutOfRange);
    }
    Ok(val * factor)
}

/// `git_parse_unsigned()` (parse.c:53-88) with `max` the largest value of the
/// target type.
fn parse_unsigned(value: &str, max: u64) -> Result<u64, NumError> {
    if value.is_empty() {
        return Err(NumError::Invalid);
    }
    // Negative values would be accepted by `strtoumax()`.
    if value.contains('-') {
        return Err(NumError::Invalid);
    }
    let parsed = strto(value.as_bytes(), None);
    if parsed.overflow {
        return Err(NumError::OutOfRange);
    }
    if parsed.end == 0 {
        return Err(NumError::Invalid);
    }
    let factor = unit_factor(&value[parsed.end..]).ok_or(NumError::Invalid)?;
    match factor.checked_mul(parsed.magnitude) {
        Some(v) if v <= max => Ok(v),
        _ => Err(NumError::OutOfRange),
    }
}

/// What `strtoimax()`/`strtoumax()`/`strtol()` consumed of a string.
struct Strto {
    negative: bool,
    /// The absolute value, saturated when `overflow` is set.
    magnitude: u64,
    /// The digits did not fit 64 bits.
    overflow: bool,
    /// The byte index just past the number, `0` if there was none (`end == value`).
    end: usize,
}

/// The C library's `strto*()` grammar: leading white space, an optional
/// sign, then digits in `radix`, or with `None` the base-0 rule (`0x` prefix:
/// hexadecimal, leading `0`: octal, else decimal).
fn strto(s: &[u8], radix: Option<u32>) -> Strto {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r') {
        i += 1;
    }
    let mut negative = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        negative = s[i] == b'-';
        i += 1;
    }
    let hex_prefix =
        |at: usize| s.get(at) == Some(&b'0') && matches!(s.get(at + 1), Some(b'x' | b'X')) && s.get(at + 2).is_some_and(u8::is_ascii_hexdigit);
    let (radix, start) = match radix {
        Some(16) if hex_prefix(i) => (16, i + 2),
        Some(radix) => (radix, i),
        None if hex_prefix(i) => (16, i + 2),
        None if s.get(i) == Some(&b'0') => (8, i),
        None => (10, i),
    };
    let mut magnitude: u64 = 0;
    let mut overflow = false;
    let mut j = start;
    while let Some(digit) = s.get(j).and_then(|&b| char::from(b).to_digit(radix)) {
        match magnitude.checked_mul(u64::from(radix)).and_then(|m| m.checked_add(u64::from(digit))) {
            Some(m) => magnitude = m,
            None => {
                overflow = true;
                magnitude = u64::MAX;
            }
        }
        j += 1;
    }
    Strto {
        negative,
        magnitude,
        overflow,
        end: if j == start { 0 } else { j },
    }
}

/// The process umask, read the way git reads it (`umask(0)` then restoring it).
fn current_umask() -> u32 {
    #[cfg(unix)]
    {
        use rustix::fs::Mode;
        let mask = rustix::process::umask(Mode::empty());
        rustix::process::umask(mask);
        mask.bits() as u32
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Install into `backend` the callback producing its [`WriteConfig`] from
/// `config`, dying through the [die hook](set_die_hook) on a value git dies on.
pub(crate) fn install(backend: &gix_ref::reftable::Backend, config: crate::Config) {
    backend.set_write_config_fn(std::sync::Arc::new(move || {
        let entries = entries_in_order(&config);
        let autocompaction = std::env::var_os("GIT_TEST_REFTABLE_AUTOCOMPACTION");
        match write_config(&entries, current_umask(), autocompaction.as_ref()) {
            Ok(config) => config,
            Err(message) => die(&message),
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(key: &str, value: &str) -> Entry {
        Entry {
            key: key.into(),
            value: Some(value.into()),
            file: None,
        }
    }

    fn run(entries: &[Entry]) -> Result<WriteConfig, String> {
        write_config(entries, 0o022, None)
    }

    // Every expected message below is what stock git 2.56.0 prints after
    // `fatal: ` for `git -c <key>=<value> update-ref refs/heads/x HEAD` in a
    // reftable repository.
    #[test]
    fn range_checks_and_messages_match_git() {
        let cases = [
            ("reftable.blocksize", "20000000", "reftable block size cannot exceed 16MB"),
            (
                "reftable.blocksize",
                "abc",
                "bad numeric config value 'abc' for 'reftable.blocksize': invalid unit",
            ),
            (
                "reftable.blocksize",
                "-1",
                "bad numeric config value '-1' for 'reftable.blocksize': invalid unit",
            ),
            (
                "reftable.blocksize",
                "",
                "bad numeric config value '' for 'reftable.blocksize': invalid unit",
            ),
            (
                "reftable.blocksize",
                "99999999999999999999",
                "bad numeric config value '99999999999999999999' for 'reftable.blocksize': out of range",
            ),
            ("reftable.restartinterval", "65536", "reftable block size cannot exceed 65535"),
            ("reftable.geometricfactor", "256", "reftable geometric factor cannot exceed 255"),
            (
                "reftable.locktimeout",
                "-2",
                "reftable lock timeout does not support negative values other than -1",
            ),
            (
                "reftable.locktimeout",
                "9223372036854775808",
                "bad numeric config value '9223372036854775808' for 'reftable.locktimeout': out of range",
            ),
            (
                "reftable.indexobjects",
                "bogus",
                "bad boolean config value 'bogus' for 'reftable.indexobjects'",
            ),
            (
                "core.logallrefupdates",
                "bogus",
                "bad boolean config value 'bogus' for 'core.logallrefupdates'",
            ),
        ];
        for (key, value, expected) in cases {
            assert_eq!(run(&[cli(key, value)]).unwrap_err(), expected, "{key}={value}");
        }
    }

    #[test]
    fn accepted_values_reach_the_options() {
        let config = run(&[
            cli("reftable.blocksize", "1k"),
            cli("reftable.restartinterval", "65535"),
            cli("reftable.geometricfactor", "0xff"),
            cli("reftable.locktimeout", "-1"),
            cli("core.logallrefupdates", "ALWAYS"),
        ])
        .unwrap();
        assert_eq!(config.opts.block_size, 1024);
        assert_eq!(config.opts.restart_interval, 65535);
        assert_eq!(config.opts.auto_compaction_factor, 255);
        assert_eq!(config.opts.lock_timeout_ms, -1);
        assert_eq!(config.log_all_ref_updates, Some(WriteReflog::Always));

        // `reftable.indexObjects` without a value is `true`, a block size of 0 the default.
        let config = run(&[
            Entry {
                key: "reftable.indexobjects".into(),
                value: None,
                file: None,
            },
            cli("reftable.blocksize", "0"),
            cli("core.logallrefupdates", "false"),
        ])
        .unwrap();
        assert!(!config.opts.skip_index_objects);
        assert_eq!(config.opts.block_size, 4096);
        assert_eq!(config.log_all_ref_updates, Some(WriteReflog::Disable));
    }

    #[test]
    fn defaults_without_configuration() {
        let config = run(&[]).unwrap();
        assert_eq!(config.opts.block_size, 4096);
        assert_eq!(config.opts.lock_timeout_ms, 100);
        assert_eq!(config.opts.default_permissions, Some(0o644));
        assert!(!config.opts.disable_auto_compact);
        assert_eq!(config.log_all_ref_updates, None);
    }

    #[test]
    fn the_first_bad_value_in_order_is_refused_even_if_overridden_later() {
        let err = run(&[cli("reftable.blocksize", "abc"), cli("reftable.blocksize", "8192")]).unwrap_err();
        assert!(err.contains("'abc'"), "{err}");
    }

    #[test]
    fn a_file_is_named_without_its_leading_dot_slash() {
        let entry = Entry {
            key: "reftable.blocksize".into(),
            value: Some("abc".into()),
            file: Some("./.git/config".into()),
        };
        assert_eq!(
            run(&[entry]).unwrap_err(),
            "bad numeric config value 'abc' for 'reftable.blocksize' in file .git/config: invalid unit"
        );
    }

    #[test]
    fn shared_repository_and_autocompaction() {
        let group = run(&[cli("core.sharedrepository", "group")]).unwrap();
        assert_eq!(group.opts.default_permissions, Some(0o664));
        let mode = run(&[cli("core.sharedrepository", "0640")]).unwrap();
        assert_eq!(mode.opts.default_permissions, Some(0o640));
        assert_eq!(
            run(&[cli("core.sharedrepository", "0444")]).unwrap_err(),
            "problem with core.sharedRepository filemode value (0444).\n\
             The owner of files must always have read and write permissions."
        );
        assert_eq!(
            run(&[cli("core.sharedrepository", "bogus")]).unwrap_err(),
            "bad boolean config value 'bogus' for 'core.sharedrepository'"
        );

        let off = OsString::from("false");
        assert!(write_config(&[], 0o022, Some(&off)).unwrap().opts.disable_auto_compact);
        let bad = OsString::from("bogus");
        assert_eq!(
            write_config(&[], 0o022, Some(&bad)).unwrap_err(),
            "bad boolean environment value 'bogus' for 'GIT_TEST_REFTABLE_AUTOCOMPACTION'"
        );
    }
}
