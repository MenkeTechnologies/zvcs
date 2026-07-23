//! `git for-each-repo` — run a command in every repository listed in a config
//! variable.
//!
//! A faithful port of `builtin/for-each-repo.c`. The whole documented surface is
//! covered; there is nothing in this command that gitoxide cannot serve, because
//! the work is config lookup plus a subprocess per repository.
//!
//! Ported flags — the complete option table:
//!   * `--config=<key>` / `--config <key>` — the multi-valued config variable
//!     holding the repository paths. `--no-config` clears it again.
//!   * `--keep-going` / `--no-keep-going` — keep iterating after a failing
//!     repository; the overall exit code is then 1, never the child's code.
//!   * `--` — stop option parsing.
//!   * `-h` — the usage block on stdout, exit 129.
//!
//! Option parsing reproduces `parse_options` with `PARSE_OPT_STOP_AT_NON_OPTION`:
//! the first non-option argument ends option parsing, so everything from there on
//! (including things that look like flags) is handed to the child untouched.
//! Unique-prefix abbreviations (`--conf=x`, `--keep`) are accepted as git accepts
//! them, and the diagnostics match byte-for-byte:
//!   * `error: unknown option \`bogus'` / `error: unknown switch \`x'`
//!   * `error: option \`config' requires a value`
//!   * `error: option \`keep-going' takes no value`
//!   all followed by the usage block on stderr, exit 129.
//!
//! Config handling mirrors `repo_config_get_string_multi`:
//!   * The key is validated by a direct port of `git_config_parse_key`, so
//!     `--config=foo` is `key does not contain a section: foo`, `--config=a.b.`
//!     is `key does not contain variable name: a.b.` and `--config=a.b c` is
//!     `invalid key: a.b c` — each followed by
//!     `fatal: got bad config --config=<key>`, a blank line, the usage block, and
//!     exit 129.
//!   * A key that exists but has an entry without `=` (an implicit boolean) is
//!     `error: missing value for '<key>'` plus the same `got bad config` block.
//!   * A key with no values at all is not an error: exit 0, nothing run.
//!   * Values are read from the fully merged snapshot in source order (system,
//!     global, local), which is the order git iterates them in. Outside a
//!     repository only the global set plus `GIT_CONFIG_*` overrides is used,
//!     matching git's behaviour there.
//!   * Each value goes through git's `interpolate_path`, so `~/`, `~user/` and
//!     `%(prefix)/` expand.
//!
//! Subprocess behaviour matches `run_command_on_repo`: the child's exit code is
//! returned as-is and iteration stops at the first failure, unless `--keep-going`
//! is given, in which case every repository is visited and the result is 1. A
//! path that cannot be entered reproduces git's
//! `fatal: cannot change to '<path>': <reason>` with exit 128. stdin, stdout and
//! stderr are inherited.
//!
//! Two deliberate deviations, both forced by this being the git shadow binary and
//! not git:
//!   * git spawns `git -C <path> <args>` (`child.git_cmd = 1`). zvcs has no
//!     global `-C`, so the child is `current_exe()` with its working directory set
//!     to `<path>` — the same semantics, and it keeps the promise that zvcs never
//!     forks upstream git (see `credential_cache.rs`, `upload_archive.rs`).
//!   * Because the child is zvcs, an empty `<arguments>` list produces zvcs's own
//!     "no subcommand given" error rather than git's usage dump. Both exit 1.

use anyhow::Result;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use gix::bstr::BString;

const USAGE: &str = concat!(
    "usage: git for-each-repo --config=<config> [--] <arguments>\n",
    "\n",
    "    --[no-]config <config>\n",
    "                          config key storing a list of repository paths\n",
    "    --[no-]keep-going     keep going even if command fails in a repository\n",
    "\n",
);

/// The long options `cmd_for_each_repo` declares, in table order.
const OPTIONS: [&str; 2] = ["config", "keep-going"];

/// Outcome of parsing the option prefix of the argument vector.
enum Parsed {
    /// Options consumed; `rest` is the child's argument vector.
    Ok {
        config_key: Option<String>,
        keep_going: bool,
        rest: Vec<String>,
    },
    /// Parsing already reported everything; just return this code.
    Exit(ExitCode),
}

/// `git for-each-repo --config=<config> [--] <arguments>`.
pub fn for_each_repo(args: &[String]) -> Result<ExitCode> {
    let (config_key, keep_going, rest) = match parse_options(args) {
        Parsed::Exit(code) => return Ok(code),
        Parsed::Ok {
            config_key,
            keep_going,
            rest,
        } => (config_key, keep_going, rest),
    };

    // `die(_("missing --config=<config>"))` — no usage block, exit 128.
    let Some(config_key) = config_key else {
        eprintln!("fatal: missing --config=<config>");
        return Ok(ExitCode::from(128));
    };

    let paths = match lookup_paths(&config_key)? {
        Lookup::Bad => return Ok(bad_config(&config_key)),
        // `err > 0`: the key simply has no values. git returns 0 without running.
        Lookup::Missing => return Ok(ExitCode::SUCCESS),
        Lookup::Values(paths) => paths,
    };

    let mut result = ExitCode::SUCCESS;
    for path in paths {
        let code = run_command_on_repo(&path, &rest)?;
        if code != 0 {
            if !keep_going {
                return Ok(ExitCode::from(code));
            }
            result = ExitCode::FAILURE;
        }
    }
    Ok(result)
}

/// `parse_options(..., PARSE_OPT_STOP_AT_NON_OPTION)` over this command's table.
fn parse_options(args: &[String]) -> Parsed {
    let mut config_key: Option<String> = None;
    let mut keep_going = false;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();

        if arg == "--" {
            i += 1;
            break;
        }
        if !arg.starts_with('-') || arg == "-" {
            break;
        }

        if let Some(long) = arg.strip_prefix("--") {
            let (name, value) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            let negated = name.strip_prefix("no-");
            let lookup = negated.unwrap_or(name);

            let Some(resolved) = resolve_option(lookup) else {
                eprint!("error: unknown option `{name}'\n{USAGE}");
                return Parsed::Exit(ExitCode::from(129));
            };

            // Negated forms are pure booleans in `parse_options`, whatever the
            // option's own type is.
            if negated.is_some() {
                if value.is_some() {
                    eprint!("error: option `{name}' takes no value\n{USAGE}");
                    return Parsed::Exit(ExitCode::from(129));
                }
                match resolved {
                    "config" => config_key = None,
                    _ => keep_going = false,
                }
                i += 1;
                continue;
            }

            match resolved {
                "config" => match value {
                    Some(value) => config_key = Some(value.to_string()),
                    None => match args.get(i + 1) {
                        Some(next) => {
                            config_key = Some(next.clone());
                            i += 1;
                        }
                        None => {
                            eprint!("error: option `config' requires a value\n{USAGE}");
                            return Parsed::Exit(ExitCode::from(129));
                        }
                    },
                },
                _ => {
                    if value.is_some() {
                        eprint!("error: option `keep-going' takes no value\n{USAGE}");
                        return Parsed::Exit(ExitCode::from(129));
                    }
                    keep_going = true;
                }
            }
            i += 1;
            continue;
        }

        // Short options: only `-h` exists, and it prints to stdout. Either way
        // the first short switch ends the command with 129.
        let c = arg[1..].chars().next().expect("non-empty after the dash");
        if c == 'h' {
            print!("{USAGE}");
        } else {
            eprint!("error: unknown switch `{c}'\n{USAGE}");
        }
        return Parsed::Exit(ExitCode::from(129));
    }

    Parsed::Ok {
        config_key,
        keep_going,
        rest: args[i..].to_vec(),
    }
}

/// Resolve a long-option name, accepting unique-prefix abbreviations the way
/// `parse_long_opt` does. Exact matches win outright.
fn resolve_option(name: &str) -> Option<&'static str> {
    if let Some(exact) = OPTIONS.iter().find(|o| **o == name) {
        return Some(exact);
    }
    if name.is_empty() {
        return None;
    }
    let mut hits = OPTIONS.iter().filter(|o| o.starts_with(name));
    match (hits.next(), hits.next()) {
        (Some(one), None) => Some(one),
        _ => None,
    }
}

/// What `repo_config_get_string_multi` reported for the requested key.
enum Lookup {
    /// `err < 0` — the key is malformed or one of its entries has no value.
    /// The specific `error:` line has already been printed.
    Bad,
    /// `err > 0` — the key is simply not set.
    Missing,
    /// The values, in config order, already path-interpolated.
    Values(Vec<PathBuf>),
}

/// Read `key` as a multi-valued list of repository paths from the merged config.
fn lookup_paths(key: &str) -> Result<Lookup> {
    let Some(parsed) = parse_config_key(key) else {
        return Ok(Lookup::Bad);
    };

    let config = match gix::discover(".") {
        Ok(repo) => repo.config_snapshot().plumbing().clone(),
        Err(_) => {
            let mut file = gix::config::File::from_globals()?;
            file.append(gix::config::File::from_environment_overrides()?)?;
            file
        }
    };

    let mut raw: Vec<BString> = Vec::new();
    let mut found = false;
    for section in config.sections() {
        let header = section.header();
        if !header.name().to_string().eq_ignore_ascii_case(&parsed.section) {
            continue;
        }
        // Subsection names are compared case-sensitively, section names are not.
        let subsection = header.subsection_name().map(|s| s.to_string());
        if subsection.as_deref() != parsed.subsection.as_deref() {
            continue;
        }

        // `values()` silently skips entries written without `=`; git treats
        // those as a fatal "missing value". Compare against every occurrence of
        // the name to notice one.
        let values = section.values(&parsed.name);
        let occurrences = section
            .value_names()
            .filter(|n| n.eq_ignore_ascii_case(&parsed.name))
            .count();
        if occurrences > 0 {
            found = true;
        }
        if occurrences != values.len() {
            eprintln!("error: missing value for '{key}'");
            return Ok(Lookup::Bad);
        }
        raw.extend(values);
    }

    if !found {
        return Ok(Lookup::Missing);
    }

    let home = gix::path::env::home_dir();
    let context = gix::config::path::interpolate::Context {
        git_install_dir: gix::path::env::system_prefix(),
        home_dir: home.as_deref(),
        ..Default::default()
    };
    let mut paths = Vec::with_capacity(raw.len());
    for value in raw {
        let path = gix::config::Path::from(value).interpolate(context)?;
        paths.push(path);
    }
    Ok(Lookup::Values(paths))
}

/// A config key split the way `git_config_parse_key` normalizes it.
struct ConfigKey {
    /// Everything before the first dot, matched case-insensitively.
    section: String,
    /// Between the first and last dot, matched verbatim; `None` without one.
    subsection: Option<String>,
    /// After the last dot, matched case-insensitively.
    name: String,
}

/// Port of `git_config_parse_key`: validate `key` and split it, printing git's
/// own `error:` line and returning `None` when it is malformed.
fn parse_config_key(key: &str) -> Option<ConfigKey> {
    let bytes = key.as_bytes();
    let last_dot = bytes.iter().rposition(|b| *b == b'.');

    let Some(last_dot) = last_dot.filter(|at| *at != 0) else {
        eprintln!("error: key does not contain a section: {key}");
        return None;
    };
    if last_dot + 1 == bytes.len() {
        eprintln!("error: key does not contain variable name: {key}");
        return None;
    }

    // The validation loop leaves the extended (subsection) part untouched and
    // requires the variable name to start with a letter.
    let baselen = last_dot;
    let mut dot = false;
    for (i, b) in bytes.iter().copied().enumerate() {
        if b == b'.' {
            dot = true;
        }
        if !dot || i > baselen {
            let keychar = b.is_ascii_alphanumeric() || b == b'-';
            if !keychar || (i == baselen + 1 && !b.is_ascii_alphabetic()) {
                eprintln!("error: invalid key: {key}");
                return None;
            }
        } else if b == b'\n' {
            eprintln!("error: invalid key (newline): {key}");
            return None;
        }
    }

    let first_dot = bytes.iter().position(|b| *b == b'.').expect("dot exists");
    Some(ConfigKey {
        section: key[..first_dot].to_ascii_lowercase(),
        subsection: (first_dot != last_dot).then(|| key[first_dot + 1..last_dot].to_string()),
        name: key[last_dot + 1..].to_ascii_lowercase(),
    })
}

/// `usage_msg_optf(_("got bad config --config=%s"), ...)`.
fn bad_config(key: &str) -> ExitCode {
    eprint!("fatal: got bad config --config={key}\n\n{USAGE}");
    ExitCode::from(129)
}

/// `run_command_on_repo()` — run the argument vector inside `path`.
///
/// git builds `git -C <path> <args>`; zvcs has no global `-C`, so the child is
/// this same binary with its working directory set to `path`.
fn run_command_on_repo(path: &std::path::Path, args: &[String]) -> Result<u8> {
    let exe = std::env::current_exe()?;
    let mut child = Command::new(exe);
    child.args(args);
    // `git -C ''` is a documented no-op, so an empty value stays in the cwd.
    if !path.as_os_str().is_empty() {
        child.current_dir(path);
    }

    let status = match child.status() {
        Ok(status) => status,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "fatal: cannot change to '{}': {}",
                path.display(),
                errno_text(&e)
            );
            return Ok(128);
        }
        Err(e) => return Err(e.into()),
    };

    if let Some(code) = status.code() {
        return Ok(code as u8);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return Ok((128 + signal) as u8);
        }
    }
    Ok(1)
}

/// The bare strerror text, without Rust's ` (os error N)` suffix.
fn errno_text(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.find(" (os error ") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}
