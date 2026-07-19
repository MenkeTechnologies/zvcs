use anyhow::{bail, Result};
use std::io::Write;
use std::process::ExitCode;

use gix::config::{File as ConfigFile, KeyRef, Source};

#[derive(PartialEq)]
enum Mode {
    Auto,
    Get,
    GetAll,
    List,
    Add,
    Unset,
    UnsetAll,
}

/// `git config` — get/set/list configuration values, backed by gitoxide.
///
/// Reads resolve through the fully-merged config snapshot (system + global +
/// local, last-one-wins), matching stock `git config`'s default scope. Writes
/// target the repository-local file only (`<common_dir>/config`); the other
/// scopes (`--global`, `--system`, `--worktree`, `--file`) are rejected with a
/// precise error rather than silently mistargeted.
///
/// Supported forms:
///   * `git config <name>` / `--get <name>`   → last value, exit 1 if absent
///   * `git config --get-all <name>`          → every value, one per line
///   * `git config -l` / `--list`             → all `key=value`, merged scopes
///   * `git config <name> <value>`            → set (overwrite last), local
///   * `git config --add <name> <value>`      → append a multivar entry, local
///   * `git config --unset <name>`            → drop the value, exit 5 if absent
///   * `git config --unset-all <name>`        → drop every value of the key
pub fn config(args: &[String]) -> Result<ExitCode> {
    let mut mode = Mode::Auto;
    let mut positional: Vec<&str> = Vec::new();

    for a in args {
        match a.as_str() {
            "-l" | "--list" => set_mode(&mut mode, Mode::List)?,
            "--get" => set_mode(&mut mode, Mode::Get)?,
            "--get-all" => set_mode(&mut mode, Mode::GetAll)?,
            "--add" => set_mode(&mut mode, Mode::Add)?,
            "--unset" => set_mode(&mut mode, Mode::Unset)?,
            "--unset-all" => set_mode(&mut mode, Mode::UnsetAll)?,
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

    let repo = gix::discover(".")?;

    match mode {
        Mode::List => list(&repo),
        Mode::Get | Mode::Auto if positional.len() == 1 => get(&repo, positional[0], false),
        Mode::Get => {
            let name = one_name(&positional)?;
            get(&repo, name, false)
        }
        Mode::GetAll => {
            let name = one_name(&positional)?;
            get(&repo, name, true)
        }
        // No action flag with two positionals is a plain set.
        Mode::Auto if positional.len() == 2 => write_local(&repo, positional[0], positional[1], WriteOp::Set),
        Mode::Auto => bail!("wrong number of arguments, expected `<name>` or `<name> <value>`"),
        Mode::Add => {
            let (name, value) = name_and_value(&positional)?;
            write_local(&repo, name, value, WriteOp::Add)
        }
        Mode::Unset => {
            let name = one_name(&positional)?;
            write_local(&repo, name, "", WriteOp::Unset)
        }
        Mode::UnsetAll => {
            let name = one_name(&positional)?;
            write_local(&repo, name, "", WriteOp::UnsetAll)
        }
    }
}

/// Install an action mode, rejecting a second action flag rather than silently
/// letting the last one win. Only the initial `Auto` may be replaced.
fn set_mode(slot: &mut Mode, new: Mode) -> Result<()> {
    if *slot != Mode::Auto {
        bail!("only one action at a time");
    }
    *slot = new;
    Ok(())
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
fn get(repo: &gix::Repository, name: &str, all: bool) -> Result<ExitCode> {
    let key = parse_key(name)?;
    let snapshot = repo.config_snapshot();
    let file = snapshot.plumbing();

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    if all {
        match file.raw_values_by(key.section_name, key.subsection_name, key.value_name) {
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
        match file.raw_value_by(key.section_name, key.subsection_name, key.value_name) {
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
/// case is preserved.
fn list(repo: &gix::Repository) -> Result<ExitCode> {
    let snapshot = repo.config_snapshot();
    let file = snapshot.plumbing();

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for section in file.sections() {
        let header = section.header();
        let section_name = header.name().to_string().to_lowercase();
        let subsection = header.subsection_name().map(ToString::to_string);

        // Preserve first-seen order but visit each value name once; a multivar's
        // values are then emitted together via `values(name)`.
        let mut seen: Vec<String> = Vec::new();
        for raw_name in section.value_names() {
            let lname = raw_name.to_lowercase();
            if !seen.iter().any(|s| s == &lname) {
                seen.push(lname);
            }
        }

        for value_name in &seen {
            for value in section.values(value_name) {
                match &subsection {
                    Some(sub) => write!(out, "{section_name}.{sub}.{value_name}=")?,
                    None => write!(out, "{section_name}.{value_name}=")?,
                }
                out.write_all(&value)?;
                out.write_all(b"\n")?;
            }
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

/// Write `file` to `path` atomically: serialize to a sibling temp file, then
/// rename over the target so a crash never leaves a half-written config.
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
