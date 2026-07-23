use anyhow::{bail, Result};
use std::io::Write;
use std::process::ExitCode;

use gix::config::{File as ConfigFile, KeyRef, Source};

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Auto,
    Get,
    GetAll,
    List,
    Add,
    Unset,
    UnsetAll,
}

impl Mode {
    /// The long option that selects this mode, used verbatim in git's
    /// "cannot be used together" diagnostic.
    fn flag(self) -> &'static str {
        match self {
            Mode::Auto => "",
            Mode::Get => "--get",
            Mode::GetAll => "--get-all",
            Mode::List => "--list",
            Mode::Add => "--add",
            Mode::Unset => "--unset",
            Mode::UnsetAll => "--unset-all",
        }
    }
}

/// Exit 129 — git's usage-error code — after emitting `error: <msg>` on stderr.
///
/// `anyhow::bail!` would collapse to exit 1, so every usage diagnostic has to
/// report itself and return the code explicitly.
fn usage_error(msg: &str) -> Result<ExitCode> {
    eprintln!("error: {msg}");
    Ok(ExitCode::from(129))
}

/// Config that stock git would never surface.
///
/// `gix` synthesizes a config layer from the ambient environment
/// (`GIT_COMMITTER_NAME` → `gitoxide.committer.nameFallback`,
/// `GIT_TERMINAL_PROMPT` → `gitoxide.credentials.terminalPrompt`, …) and zvcs
/// injects its own defaults through the API scope. Neither exists as far as
/// `git config` is concerned, so both are hidden from reads and from `--list`.
/// `Source::Env` (`GIT_CONFIG_COUNT`) and `Source::Cli` (`git -c`) are real
/// git config and stay visible.
fn is_synthetic(source: Source) -> bool {
    matches!(source, Source::EnvOverride | Source::Api)
}

/// `git config` — get/set/list configuration values, backed by gitoxide.
///
/// Reads resolve through the fully-merged config snapshot (system + global +
/// local, last-one-wins), matching stock `git config`'s default scope. Outside a
/// repository a read still works, falling back to the global+system+env cascade
/// exactly as stock `git config` does (so `config --list` / `config user.name`
/// never require a repo). Writes target the repository-local file only
/// (`<common_dir>/config`) and so still need a repo — attempting one without one
/// fails with `not in a git directory`; the other scopes (`--global`,
/// `--system`, `--worktree`, `--file`) are rejected with a precise error rather
/// than silently mistargeted.
///
/// Supported forms:
///   * `git config <name>` / `--get <name>`   → last value, exit 1 if absent
///   * `git config --get-all <name>`          → every value, one per line
///   * `git config -l` / `--list`             → all `key=value`, merged scopes
///   * `git config <name> <value>`            → set (overwrite last), local
///   * `git config <name> <value> <pattern>`  → rewrite values matching the ERE,
///                                              or append when none match, local
///   * `git config --add <name> <value>`      → append a multivar entry, local
///   * `git config --unset <name>`            → drop the value, exit 5 if absent
///   * `git config --unset-all <name>`        → drop every value of the key
///   * `--name-only`                          → with `--list`, keys without values
///
/// Usage errors (conflicting action flags, a misplaced `--name-only`, a wrong
/// argument count) report `error: …` on stderr and exit 129, as git's
/// parse-options layer does; they never travel as `anyhow` errors, which would
/// collapse to exit 1.
pub fn config(args: &[String]) -> Result<ExitCode> {
    let mut mode = Mode::Auto;
    let mut name_only = false;
    let mut positional: Vec<&str> = Vec::new();
    // git config parses options with `PARSE_OPT_STOP_AT_NON_OPTION`: option
    // scanning ends at the FIRST argument that is not an option, and that token
    // plus every one after it are operands — even the ones that look like
    // `--flags`. Two independent terminators reach this state:
    //   * a bare `--`, which is consumed (never a positional itself), or
    //   * the first non-option token (anything not starting with `-`, plus a
    //     lone `-`), which is itself the first operand.
    // Consequences that must match stock git:
    //   * `git config user.name value --get` is a 3-operand value-pattern set —
    //     the trailing `--get` is the pattern, not an action flag.
    //   * `git config key --local a b` is 4 operands (`--local` is data here),
    //     so it is rejected as "no action specified", not a scoped write.
    //   * `git config --get -- --list` still reads the key literally `--list`.
    let mut end_of_options = false;

    for a in args {
        if end_of_options {
            positional.push(a.as_str());
            continue;
        }
        if a.as_str() == "--" {
            end_of_options = true;
            continue;
        }
        // First non-option token: it ends option parsing AND is the first
        // operand. A lone `-` is a non-option (git treats it as data), so it
        // stops here too.
        if a.as_str() == "-" || !a.starts_with('-') {
            end_of_options = true;
            positional.push(a.as_str());
            continue;
        }

        let action = match a.as_str() {
            "-l" | "--list" => Some(Mode::List),
            "--get" => Some(Mode::Get),
            "--get-all" => Some(Mode::GetAll),
            "--add" => Some(Mode::Add),
            "--unset" => Some(Mode::Unset),
            "--unset-all" => Some(Mode::UnsetAll),
            _ => None,
        };

        // Action flags are git's `OPT_CMDMODE`: they all write one slot, and a
        // second *different* one is rejected the moment it is parsed — before
        // any post-parse validation gets a chance to complain.
        if let Some(new) = action {
            if mode != Mode::Auto && mode != new {
                return usage_error(&format!(
                    "options '{}' and '{}' cannot be used together",
                    new.flag(),
                    mode.flag()
                ));
            }
            mode = new;
            continue;
        }

        match a.as_str() {
            "--name-only" => name_only = true,
            // Default (repository-local) scope is the only writable target.
            "--local" => {}
            "--global" | "--system" | "--worktree" => {
                bail!("only the default (local) scope is supported, got {a}")
            }
            "--file" => bail!("--file is not supported, only the repository-local config"),
            other if other.starts_with('-') => bail!("unknown option {other}"),
            other => positional.push(other),
        }
    }

    // Post-parse validation, in git's own order and — like git — ahead of any
    // repository lookup, so a usage error reports the same way outside a repo.
    //
    // An entirely actionless invocation is reported first. Without an action
    // flag the form is `<name> [value [value-pattern]]`, and git recognizes no
    // action at all outside that 1..=3 window, the zero-argument case included.
    if mode == Mode::Auto && !(1..=3).contains(&positional.len()) {
        return usage_error("no action specified");
    }
    if name_only && mode != Mode::List {
        return usage_error("--name-only is only applicable to --list or --get-regexp");
    }
    match mode {
        Mode::List if !positional.is_empty() => {
            return usage_error("wrong number of arguments, should be 0");
        }
        Mode::Get | Mode::GetAll if !(1..=2).contains(&positional.len()) => {
            return usage_error("wrong number of arguments, should be from 1 to 2");
        }
        _ => {}
    }

    // A repository is optional: reads resolve fine outside one (git reads global
    // and system config with no repo present), while writes target the local
    // scope and still require a repo. Discovery failure is therefore not fatal
    // here — only an attempted write without a repo is.
    let repo = gix::discover(".").ok();

    // The config to READ from: the repo's fully-merged snapshot when inside one,
    // else the global+system+env cascade that `git config` falls back to.
    let snapshot = repo.as_ref().map(gix::Repository::config_snapshot);
    let global;
    let file: &gix::config::File = match snapshot.as_ref() {
        Some(s) => s.plumbing(),
        None => {
            global = global_config()?;
            &global
        }
    };

    // Writes need the local scope; outside a repo they fail the way git does.
    let for_write = || {
        repo.as_ref()
            .ok_or_else(|| anyhow::anyhow!("not in a git directory"))
    };

    match mode {
        Mode::List => list(file, name_only),
        // `--get <name> <value-pattern>` filters returned values by an ERE — a
        // read-side feature distinct from the value-pattern *set* form below and
        // not yet implemented; the two-argument read is refused rather than faked.
        Mode::Get | Mode::GetAll if positional.len() == 2 => {
            bail!("value-pattern filtering is not supported")
        }
        Mode::Get => get(file, positional[0], false),
        Mode::GetAll => get(file, positional[0], true),
        // No action flag: one positional reads, two set the value.
        Mode::Auto if positional.len() == 1 => get(file, positional[0], false),
        Mode::Auto if positional.len() == 2 => {
            write_local(for_write()?, positional[0], positional[1], WriteOp::Set)
        }
        // `<name> <value> <value-pattern>` rewrites the values whose text matches
        // the POSIX ERE, or adds a new value when none match.
        Mode::Auto => set_with_value_pattern(for_write()?, positional[0], positional[1], positional[2]),
        Mode::Add => {
            let (name, value) = name_and_value(&positional)?;
            write_local(for_write()?, name, value, WriteOp::Add)
        }
        Mode::Unset => {
            let name = one_name(&positional)?;
            write_local(for_write()?, name, "", WriteOp::Unset)
        }
        Mode::UnsetAll => {
            let name = one_name(&positional)?;
            write_local(for_write()?, name, "", WriteOp::UnsetAll)
        }
    }
}

/// The merged global+system config `git config` reads when run outside a
/// repository: git-installation, system, and per-user (`~/.gitconfig`) files,
/// with `GIT_CONFIG_*` environment overrides layered on top (highest
/// precedence), mirroring the snapshot a repo would expose minus its local file.
fn global_config() -> Result<gix::config::File> {
    let mut file = gix::config::File::from_globals()?;
    if let Ok(env) = gix::config::File::from_environment_overrides() {
        // `append` only errors on a malformed section header it just parsed from
        // the environment; treat that as "no valid overrides" rather than failing
        // an otherwise-good global read.
        let _ = file.append(env);
    }
    Ok(file)
}

fn one_name<'a>(positional: &[&'a str]) -> Result<&'a str> {
    match positional {
        [name] => Ok(*name),
        [] => bail!("no config key given"),
        _ => bail!("too many arguments, expected a single `<name>`"),
    }
}

fn name_and_value<'a>(positional: &[&'a str]) -> Result<(&'a str, &'a str)> {
    match positional {
        [name, value] => Ok((*name, *value)),
        _ => bail!("expected `<name> <value>`"),
    }
}

/// Parse `section[.subsection].name`, erroring the way stock git does when the
/// key has no section component.
fn parse_key(name: &str) -> Result<KeyRef<'_>> {
    KeyRef::parse_unvalidated(name.into())
        .ok_or_else(|| anyhow::anyhow!("key does not contain a section: {name}"))
}

/// `git config <name>` / `--get` / `--get-all` — read from the merged snapshot.
///
/// Exit code 1 (no output) when the key is absent, matching stock git.
fn get(file: &gix::config::File, name: &str, all: bool) -> Result<ExitCode> {
    let key = parse_key(name)?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let visible = |meta: &gix::config::file::Metadata| !is_synthetic(meta.source);

    if all {
        match file.raw_values_filter_by(key.section_name, key.subsection_name, key.value_name, visible) {
            Ok(values) => {
                for v in values {
                    out.write_all(&v)?;
                    out.write_all(b"\n")?;
                }
                Ok(ExitCode::SUCCESS)
            }
            Err(_) => Ok(ExitCode::from(1)),
        }
    } else {
        match file.raw_value_filter_by(key.section_name, key.subsection_name, key.value_name, visible) {
            Ok(v) => {
                out.write_all(&v)?;
                out.write_all(b"\n")?;
                Ok(ExitCode::SUCCESS)
            }
            Err(_) => Ok(ExitCode::from(1)),
        }
    }
}

/// `git config -l` — emit every `key=value` from the merged snapshot, in file
/// order. Section and value names are lower-cased (git-normalized); subsection
/// case is preserved. With `name_only`, the `=value` half is dropped, one line
/// per value occurrence.
///
/// Entries are emitted in the order they appear in their file, multivars
/// included: an `a=1 / b=2 / a=3` section lists as `a=1`, `b=2`, `a=3`, not with
/// the two `a`s collapsed together.
fn list(file: &gix::config::File, name_only: bool) -> Result<ExitCode> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for section in file.sections() {
        if is_synthetic(section.meta().source) {
            continue;
        }
        let header = section.header();
        let section_name = header.name().to_string().to_lowercase();
        let subsection = header.subsection_name().map(ToString::to_string);

        // `value_names()` walks the body in order, repeating a multivar's name
        // once per occurrence; the nth occurrence pairs with `values(name)[n]`.
        let mut occurrence: Vec<(String, usize)> = Vec::new();
        for raw_name in section.value_names() {
            let lname = raw_name.to_lowercase();
            let nth = occurrence.iter().filter(|(n, _)| *n == lname).count();
            occurrence.push((lname, nth));
        }

        for (value_name, nth) in &occurrence {
            let Some(value) = section.values(value_name).into_iter().nth(*nth) else {
                continue;
            };
            match &subsection {
                Some(sub) => write!(out, "{section_name}.{sub}.{value_name}")?,
                None => write!(out, "{section_name}.{value_name}")?,
            }
            if !name_only {
                out.write_all(b"=")?;
                out.write_all(&value)?;
            }
            out.write_all(b"\n")?;
        }
    }

    Ok(ExitCode::SUCCESS)
}

enum WriteOp {
    Set,
    Add,
    Unset,
    UnsetAll,
}

/// Mutate the repository-local config file (`<common_dir>/config`) and persist
/// it atomically. Serialized through the repo coordinator so a concurrent
/// zvcs writer can't interleave a partial rewrite.
fn write_local(repo: &gix::Repository, name: &str, value: &str, op: WriteOp) -> Result<ExitCode> {
    let key = parse_key(name)?;
    let section_lc = key.section_name.to_lowercase();
    let value_lc = key.value_name.to_lowercase();

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let path = repo.common_dir().join("config");
    let mut file = ConfigFile::from_path_no_includes(path.clone(), Source::Local)?;

    match op {
        WriteOp::Set => {
            file.set_raw_value_by(&section_lc, key.subsection_name, &value_lc, value)?;
        }
        WriteOp::Add => {
            file.section_mut_or_create_new(&section_lc, key.subsection_name)?
                .push(&value_lc, value)?;
        }
        WriteOp::Unset | WriteOp::UnsetAll => {
            let mut section = match file.section_mut(key.section_name, key.subsection_name) {
                Ok(s) => s,
                // Unsetting an absent key is exit 5 in stock git.
                Err(_) => return Ok(ExitCode::from(5)),
            };
            let count = section.values(key.value_name).len();
            if count == 0 {
                return Ok(ExitCode::from(5));
            }
            if matches!(op, WriteOp::Unset) && count > 1 {
                bail!("key contains multiple values: {name}");
            }
            if matches!(op, WriteOp::UnsetAll) {
                while section.remove(key.value_name).is_some() {}
            } else {
                section.remove(key.value_name);
            }
        }
    }

    persist(&path, &file)?;
    Ok(ExitCode::SUCCESS)
}

/// `git config <name> <value> <value-pattern>` — the value-pattern set form.
///
/// Among the existing values of `<name>` in the repository-local file, the
/// POSIX ERE `<value-pattern>` selects which are rewritten to `<value>`
/// (a leading `!` inverts the match, matching against the value text as bytes,
/// unanchored — git's `regexec`). The outcomes mirror stock git exactly:
///
///   * no value matches   → append `<value>` as a new line (exit 0)
///   * exactly one matches → rewrite that value in place (exit 0)
///   * more than one       → without `--replace-all` git refuses: it prints
///                           `warning: <key> has multiple values` on stderr,
///                           leaves the file untouched, and exits 5
///   * invalid ERE         → `error: invalid pattern: <pattern>`, exit 6
fn set_with_value_pattern(
    repo: &gix::Repository,
    name: &str,
    value: &str,
    value_pattern: &str,
) -> Result<ExitCode> {
    let key = parse_key(name)?;
    let section_lc = key.section_name.to_lowercase();
    let value_lc = key.value_name.to_lowercase();

    // A leading `!` inverts the match; the remainder is the ERE. Compile it the
    // way git does before touching the file, so a bad pattern is exit 6 whether
    // or not any value would have matched.
    let (invert, pat) = match value_pattern.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, value_pattern),
    };
    let re = match regex::bytes::Regex::new(pat) {
        Ok(re) => re,
        Err(_) => {
            eprintln!("error: invalid pattern: {pat}");
            return Ok(ExitCode::from(6));
        }
    };

    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());

    let path = repo.common_dir().join("config");
    let mut file = ConfigFile::from_path_no_includes(path.clone(), Source::Local)?;

    // Existing values of the key in this file, in order of occurrence. An absent
    // key yields an empty list, which routes to the append branch below.
    let existing = file
        .raw_values_by(&section_lc, key.subsection_name, &value_lc)
        .unwrap_or_default();

    let mut matching: Vec<usize> = Vec::new();
    for (i, v) in existing.iter().enumerate() {
        if re.is_match(v.as_ref()) != invert {
            matching.push(i);
        }
    }

    match matching.as_slice() {
        // No value matches: append a new one (git's add-on-no-match).
        [] => {
            file.section_mut_or_create_new(&section_lc, key.subsection_name)?
                .push(&value_lc, value)?;
        }
        // Exactly one match: rewrite that value in place. The index is shared
        // with `raw_values_by` above — both walk values in occurrence order.
        [idx] => {
            file.raw_values_mut_by(&section_lc, key.subsection_name, &value_lc)?
                .set_string_at(*idx, value)?;
        }
        // Multiple matches without `--replace-all`: git warns and exits 5,
        // leaving the file untouched.
        _ => {
            let key_disp = match key.subsection_name {
                Some(sub) => format!("{section_lc}.{sub}.{value_lc}"),
                None => format!("{section_lc}.{value_lc}"),
            };
            eprintln!("warning: {key_disp} has multiple values");
            return Ok(ExitCode::from(5));
        }
    }

    persist(&path, &file)?;
    Ok(ExitCode::SUCCESS)
}

/// Write `file` to `path` atomically: serialize to a sibling temp file, then
/// rename over the target so a crash never leaves a half-written config.
/// Set `branch.<branch>.remote` and `branch.<branch>.merge` in the local config,
/// as `git push --set-upstream` / `git branch --set-upstream-to` do. Reuses the
/// same lock + atomic-write path as `git config`.
pub(crate) fn set_branch_upstream(
    repo: &gix::Repository,
    branch: &str,
    remote: &str,
    merge_ref: &str,
) -> Result<()> {
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    let path = repo.common_dir().join("config");
    let mut file = ConfigFile::from_path_no_includes(path.clone(), Source::Local)?;
    let sub = gix::bstr::BStr::new(branch);
    file.set_raw_value_by("branch", Some(sub), "remote", remote)?;
    file.set_raw_value_by("branch", Some(sub), "merge", merge_ref)?;
    persist(&path, &file)?;
    Ok(())
}

fn persist(path: &std::path::Path, file: &ConfigFile) -> Result<()> {
    let bytes = file.to_bstring();
    let tmp = path.with_extension("zvcs-tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}
