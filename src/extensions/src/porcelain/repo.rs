//! `git repo` — retrieve information about the repository (experimental in git).
//!
//! Covered: the whole `git repo info` surface — `--format=(lines|nul)`,
//! `--format <v>` as two arguments, `-z`, `--all`/`--no-all`, `--keys`/`--no-keys`,
//! `--`, `-h`, every documented info key, and all of git's failure paths with
//! their exact messages and exit codes (129 for parse-options failures, 128 for
//! `fatal:` diagnostics, 255 for an unknown key). Keys are emitted in the order
//! requested, duplicates included, streamed so that values preceding an unknown
//! key still reach stdout exactly as git does. The top-level dispatcher (`-h`,
//! missing subcommand, unknown subcommand) is reproduced byte-for-byte too.
//!
//! NOT covered: `git repo structure`. Its flags, `-h` text and `fatal: invalid
//! format` path are honoured, but the report itself bails. Producing it requires
//! a full reachability walk that reports, per object type, inflated size *and*
//! on-disk size — the latter being the packed entry's compressed, possibly
//! delta'd footprint. The vendored `gix-odb` exposes decompressed objects
//! through `find()` but no per-object on-disk entry size across the pack/loose
//! boundary, and git's tie-breaking for the `max_size_oid` / `max_parents_oid` /
//! `max_entries_oid` fields is unspecified in the documentation. Guessing either
//! would emit a plausible table that silently disagrees with git, so it bails.
//!
//! Nothing here writes to the repository, so post-command state is unchanged.
//! `references.format` is read from `extensions.refStorage` (only honoured at
//! `core.repositoryFormatVersion >= 1`, as git does) rather than from the ref
//! store itself, because `gix::RefStore` is `gix_ref::file::Store` and has no
//! reftable backend to report. Running outside a repository propagates the
//! discovery error to the central handler rather than emitting git's own
//! `fatal: not a git repository` / exit 128, matching every other module here.

use anyhow::Result;
use std::io::Write;
use std::process::ExitCode;

/// Top-level usage block, byte-for-byte (185 bytes) including the trailing blank
/// line. Printed on `-h` (stdout) and after an `error:` line for a bad verb.
const USAGE_TOP: &str = "usage: git repo info [--format=(lines|nul) | -z] [--all | <key>...]\n\
                         \x20  or: git repo info --keys [--format=(lines|nul) | -z]\n\
                         \x20  or: git repo structure [--format=(table|lines|nul) | -z]\n\
                         \n";

/// `git repo info` usage block, byte-for-byte (301 bytes). Option help starts at
/// column 26, matching git's `usage_with_options()` layout.
const USAGE_INFO: &str = "usage: git repo info [--format=(lines|nul) | -z] [--all | <key>...]\n\
                          \x20  or: git repo info --keys [--format=(lines|nul) | -z]\n\
                          \n\
                          \x20   --format <format>     output format\n\
                          \x20   -z                    synonym for --format=nul\n\
                          \x20   --[no-]all            print all keys/values\n\
                          \x20   --[no-]keys           show keys\n\
                          \n";

/// `git repo structure` usage block, byte-for-byte (193 bytes).
const USAGE_STRUCTURE: &str = "usage: git repo structure [--format=(table|lines|nul) | -z]\n\
                               \n\
                               \x20   --format <format>     output format\n\
                               \x20   -z                    synonym for --format=nul\n\
                               \x20   --[no-]progress       show progress\n\
                               \n";

/// The info keys git knows about, in the order `--keys` and `--all` emit them.
const KEYS: [&str; 4] = [
    "layout.bare",
    "layout.shallow",
    "object.format",
    "references.format",
];

/// Output shape shared by `info` and `structure`.
#[derive(Clone, Copy, PartialEq)]
enum Format {
    /// `key=value` per line, values c-quoted when they contain unusual bytes.
    Lines,
    /// `key\nvalue\0`, values never quoted.
    Nul,
    /// The human-readable table; `structure` only, and its default.
    Table,
}

/// `git repo` — report metadata about the current repository.
///
/// Supported forms (matching stock git byte-for-byte, including exit codes):
///   * `git repo info [--format=(lines|nul) | -z] [--all | <key>...]`
///   * `git repo info --keys [--format=(lines|nul) | -z]`
///   * `git repo -h`, `git repo info -h`, `git repo structure -h`
///
/// `git repo structure` parses its arguments and rejects a bad `--format`
/// exactly as git does, then bails rather than emitting an approximated report.
pub fn repo(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0.
    let args = match args.first().map(String::as_str) {
        Some("repo") => &args[1..],
        _ => args,
    };

    let Some(first) = args.first().map(String::as_str) else {
        // git's PARSE_OPT_SUBCOMMAND handling when nothing follows the verb.
        eprint!("error: need a subcommand\n{USAGE_TOP}");
        return Ok(ExitCode::from(129));
    };

    match first {
        "-h" => {
            // parse-options writes `-h` output to stdout and still exits 129.
            print!("{USAGE_TOP}");
            Ok(ExitCode::from(129))
        }
        "info" => info(&args[1..]),
        "structure" => structure(&args[1..]),
        // An option in the subcommand slot is reported as an option, not a verb.
        s if s.starts_with("--") => Ok(top_error(&format!("unknown option `{}'", &s[2..]))),
        s if s.len() > 1 && s.starts_with('-') => {
            // git's parse-options reports only the first offending short switch.
            let c = s[1..].chars().next().expect("len > 1");
            Ok(top_error(&format!("unknown switch `{c}'")))
        }
        s => Ok(top_error(&format!("unknown subcommand: `{s}'"))),
    }
}

/// `git repo info` — print the requested key/value pairs.
fn info(args: &[String]) -> Result<ExitCode> {
    let mut format: Option<Format> = None;
    let mut all = false;
    let mut keys_only = false;
    let mut requested: Vec<String> = Vec::new();
    let mut end_of_opts = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if end_of_opts {
            requested.push(a.to_string());
            i += 1;
            continue;
        }
        match a {
            "--" => end_of_opts = true,
            "-h" => {
                print!("{USAGE_INFO}");
                return Ok(ExitCode::from(129));
            }
            "-z" => format = Some(Format::Nul),
            "--all" => all = true,
            "--no-all" => all = false,
            "--keys" => keys_only = true,
            "--no-keys" => keys_only = false,
            "--format" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    // `error()` without a usage block, exactly as parse-options does.
                    eprintln!("error: option `format' requires a value");
                    return Ok(ExitCode::from(129));
                };
                format = Some(match parse_format(v, false) {
                    Some(f) => f,
                    None => return Ok(invalid_format(v)),
                });
            }
            s if s.starts_with("--format=") => {
                let v = &s["--format=".len()..];
                format = Some(match parse_format(v, false) {
                    Some(f) => f,
                    None => return Ok(invalid_format(v)),
                });
            }
            s if s.starts_with("--all=") => {
                eprintln!("error: option `all' takes no value");
                return Ok(ExitCode::from(129));
            }
            s if s.starts_with("--keys=") => {
                eprintln!("error: option `keys' takes no value");
                return Ok(ExitCode::from(129));
            }
            s if s.starts_with("--") => {
                return Ok(usage_error(USAGE_INFO, &format!("unknown option `{}'", &s[2..])));
            }
            s if s.len() > 1 && s.starts_with('-') => {
                // Clustered short switches; `-z` is the only one, `-h` wins early.
                for c in s[1..].chars() {
                    match c {
                        'z' => format = Some(Format::Nul),
                        'h' => {
                            print!("{USAGE_INFO}");
                            return Ok(ExitCode::from(129));
                        }
                        _ => return Ok(usage_error(USAGE_INFO, &format!("unknown switch `{c}'"))),
                    }
                }
            }
            s => requested.push(s.to_string()),
        }
        i += 1;
    }

    // git validates the format first, then the flag combinations, `--keys` before
    // `--all`; both of the latter are `die()` and so exit 128 with no usage block.
    if keys_only && (all || !requested.is_empty()) {
        eprintln!("fatal: --keys cannot be used with a <key> or --all");
        return Ok(ExitCode::from(128));
    }
    if all && !requested.is_empty() {
        eprintln!("fatal: --all and <key> cannot be used together");
        return Ok(ExitCode::from(128));
    }

    let format = format.unwrap_or(Format::Lines);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    if keys_only {
        // `--keys` still requires a repository, matching git's RUN_SETUP.
        gix::discover(".")?;
        for key in KEYS {
            match format {
                Format::Nul => write!(out, "{key}\0")?,
                _ => writeln!(out, "{key}")?,
            }
        }
        out.flush()?;
        return Ok(ExitCode::SUCCESS);
    }

    let wanted: Vec<&str> = if all {
        KEYS.to_vec()
    } else {
        requested.iter().map(String::as_str).collect()
    };

    // Nothing asked for means nothing to look up — and git still exits 0.
    if wanted.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    let repo = gix::discover(".")?;
    for key in wanted {
        let Some(value) = value_of(&repo, key) else {
            // Values already written stay on stdout; git returns -1 here, which
            // the process exits with as 255.
            out.flush()?;
            eprintln!("error: key '{key}' not found");
            return Ok(ExitCode::from(255));
        };
        match format {
            Format::Nul => write!(out, "{key}\n{value}\0")?,
            _ => writeln!(out, "{key}={}", quote_c_style(value.as_bytes()))?,
        }
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Resolve one documented info key, or `None` if git wouldn't recognise it.
fn value_of(repo: &gix::Repository, key: &str) -> Option<String> {
    match key {
        "layout.bare" => Some(repo.is_bare().to_string()),
        // git's `is_repository_shallow()`: the shallow file exists and is non-empty.
        "layout.shallow" => Some(repo.is_shallow().to_string()),
        // `gix_hash::Kind`'s Display is already git's own `sha1`/`sha256` spelling.
        "object.format" => Some(repo.object_hash().to_string()),
        "references.format" => Some(reference_format(repo)),
        _ => None,
    }
}

/// git's `ref_storage_format_to_name()`: the `extensions.refStorage` value, which
/// is only consulted once `core.repositoryFormatVersion` is at least 1, and
/// otherwise defaults to `files`.
fn reference_format(repo: &gix::Repository) -> String {
    let config = repo.config_snapshot();
    if config
        .integer("core.repositoryFormatVersion")
        .unwrap_or(0)
        < 1
    {
        return "files".to_string();
    }
    match config.string("extensions.refStorage") {
        Some(v) => String::from_utf8_lossy(&v).to_lowercase(),
        None => "files".to_string(),
    }
}

/// `git repo structure` — argument handling only; see the module docs for why the
/// report itself is not produced.
fn structure(args: &[String]) -> Result<ExitCode> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-h" => {
                print!("{USAGE_STRUCTURE}");
                return Ok(ExitCode::from(129));
            }
            "-z" | "--progress" | "--no-progress" | "--" => {}
            "--format" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    eprintln!("error: option `format' requires a value");
                    return Ok(ExitCode::from(129));
                };
                if parse_format(v, true).is_none() {
                    return Ok(invalid_format(v));
                }
            }
            s if s.starts_with("--format=") => {
                let v = &s["--format=".len()..];
                if parse_format(v, true).is_none() {
                    return Ok(invalid_format(v));
                }
            }
            s if s.starts_with("--") => {
                return Ok(usage_error(
                    USAGE_STRUCTURE,
                    &format!("unknown option `{}'", &s[2..]),
                ));
            }
            s if s.len() > 1 && s.starts_with('-') => {
                let c = s[1..].chars().next().expect("len > 1");
                if c != 'z' {
                    return Ok(usage_error(USAGE_STRUCTURE, &format!("unknown switch `{c}'")));
                }
            }
            // `structure` takes no positionals; git's bare `usage()` fires here.
            _ => {
                eprintln!("usage: too many arguments");
                return Ok(ExitCode::from(129));
            }
        }
        i += 1;
    }

    anyhow::bail!(
        "`repo structure` needs per-object on-disk (packed entry) sizes, which the vendored gix-odb does not expose (ported: repo info, repo structure argument checking)"
    )
}

/// Accept the format names valid for the subcommand; `table` is `structure`-only.
fn parse_format(value: &str, allow_table: bool) -> Option<Format> {
    match value {
        "lines" => Some(Format::Lines),
        "nul" => Some(Format::Nul),
        "table" if allow_table => Some(Format::Table),
        _ => None,
    }
}

/// git `die()`s on a bad `--format`, so there is no usage block and exit is 128.
fn invalid_format(value: &str) -> ExitCode {
    eprintln!("fatal: invalid format '{value}'");
    ExitCode::from(128)
}

/// parse-options' unknown-option shape: `error: <msg>` then the usage block, both
/// on stderr, exit 129.
fn usage_error(usage: &str, msg: &str) -> ExitCode {
    eprint!("error: {msg}\n{usage}");
    ExitCode::from(129)
}

/// The same shape for the top-level dispatcher.
fn top_error(msg: &str) -> ExitCode {
    eprint!("error: {msg}\n{USAGE_TOP}");
    ExitCode::from(129)
}

/// `quote_c_style()`: emit the bytes verbatim unless they contain a control byte,
/// a quote, a backslash or anything >= 0x80, in which case wrap in double quotes
/// with C-style escapes. Every value git currently reports is plain ASCII, so
/// this is a fidelity guard rather than a hot path.
fn quote_c_style(bytes: &[u8]) -> String {
    let needs = bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
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
            b if b < 0x20 || b == 0x7f || b >= 0x80 => out.push_str(&format!("\\{b:03o}")),
            b => out.push(b as char),
        }
    }
    out.push('"');
    out
}
