//! `git diagnose` — generate a zip archive of diagnostic information.
//!
//! Stock `git diagnose` (`builtin/diagnose.c` + `diagnose.c`) collects a report
//! and packages it, together with copies of repository metadata, into
//! `git-diagnostics-<strftime-suffix>.zip`. Its observable output is not
//! reproducible from gitoxide, for four independent reasons:
//!
//!   * The report's first section is verbatim `git version --build-options`:
//!     the C build configuration of the *stock* binary (`sizeof-long`,
//!     `shell-path`, the SHA-1/SHA-256 backend names, the linked `libcurl` and
//!     `zlib` versions, the compiled-in feature list). A Rust binary has no
//!     honest way to emit those values.
//!   * The `Available space on '<path>': <N> GiB (mount flags 0x…)` line needs
//!     a `statvfs`/`statfs` call. This crate's dependency set is `gix` +
//!     `anyhow` only (`src/extensions/Cargo.toml`); there is no `libc` and no
//!     std API for free space or mount flags.
//!   * The archive itself is a zip written by git's `archive-zip` writer
//!     (deflate streams, DOS timestamps, its own entry ordering). There is no
//!     zip/deflate writer among the vendored `gix*` crates, so the file left
//!     behind — which is post-command state the differential harness compares —
//!     cannot be produced, let alone byte-matched.
//!   * `--mode=all` additionally copies `.git`, `.git/hooks`, `.git/info`,
//!     `.git/logs` and `.git/objects/info` into that archive.
//!
//! The packfile inventory and loose-object counts *would* be reachable through
//! `gix`, but they are a minority of a report whose surrounding lines are not,
//! so emitting them alone would only produce a near-miss. Nothing is fabricated
//! here.
//!
//! Covered, byte-identically with stock git:
//!   * `-h` — the exact usage block on stdout, exit status 129.
//!   * Usage errors — unknown long option, unknown short switch, a missing
//!     option value, and an invalid `--mode` value: the same `error: …` text on
//!     stderr (with the usage block where stock prints it), exit status 129.
//!   * Recognition of the full option table (`-o`/`--output-directory`,
//!     `-s`/`--suffix`, `--mode`, their `--no-` forms, sticky `-o<v>`/`-s<v>`,
//!     `=`-joined long values, unique long-option prefixes, `--`), so no flag is
//!     ever silently ignored. Positional arguments are accepted and ignored,
//!     as stock does.
//!
//! Not covered: every invocation that would actually produce a report `bail!`s,
//! naming the missing substrate. No archive is written and no repository state
//! is touched.
//!
//! Known divergence: stock's ambiguous-prefix diagnostic
//! (`error: ambiguous option: …`) is not reproduced; an ambiguous prefix
//! `bail!`s instead of guessing at the message.

use anyhow::{bail, Result};
use std::process::ExitCode;

/// The usage block, byte-identical to stock `git diagnose -h`.
const USAGE: &str = "\
usage: git diagnose [(-o | --output-directory) <path>] [(-s | --suffix) <format>]
                    [--mode=<mode>]

    -o, --[no-]output-directory <path>
                          specify a destination for the diagnostics archive
    -s, --[no-]suffix <format>
                          specify a strftime format suffix for the filename
    --mode (stats|all)    specify the content of the diagnostic archive
";

/// The long options stock's `parse_options` table exposes, in table order.
/// Prefix matching resolves against this list, as `parse-options` does.
const LONG_OPTS: [&str; 3] = ["output-directory", "suffix", "mode"];

/// `git diagnose` — see the module docs: option parsing and the usage/error
/// paths are faithful; report generation is not implemented and `bail!`s.
pub fn diagnose(args: &[String]) -> Result<ExitCode> {
    let mut i = 0;
    let mut no_more_opts = false;

    while i < args.len() {
        let a = args[i].as_str();
        i += 1;

        if no_more_opts || a == "-" || !a.starts_with('-') {
            // Stock accepts and ignores positional arguments here.
            continue;
        }
        if a == "--" {
            no_more_opts = true;
            continue;
        }

        if let Some(long) = a.strip_prefix("--") {
            // Split `--name=value` before resolving the name.
            let (name, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            let (name, negated) = match name.strip_prefix("no-") {
                Some(rest) => (rest, true),
                None => (name, false),
            };

            let matches: Vec<&str> = LONG_OPTS
                .iter()
                .copied()
                .filter(|o| o.starts_with(name))
                .collect();
            let resolved = match matches.as_slice() {
                [exact] => *exact,
                // An exact hit wins over the longer options it prefixes.
                _ if LONG_OPTS.contains(&name) => name,
                [] => return Ok(usage_error(&format!("unknown option `{long}'"))),
                _ => bail!("ambiguous option `--{long}' (ambiguity diagnostics not ported)"),
            };

            if negated {
                // `--no-<opt>` clears the value and takes no argument.
                if inline.is_some() {
                    return Ok(bare_error(&format!(
                        "option `no-{resolved}' takes no value"
                    )));
                }
                continue;
            }

            // Every option in this table takes a value.
            let value = match inline {
                Some(v) => v.to_string(),
                None => match args.get(i) {
                    Some(v) => {
                        i += 1;
                        v.clone()
                    }
                    None => {
                        return Ok(bare_error(&format!("option `{resolved}' requires a value")))
                    }
                },
            };
            if resolved == "mode" && value != "stats" && value != "all" {
                return Ok(bare_error(&format!("invalid --mode value '{value}'")));
            }
            continue;
        }

        // Short switches: grouped, with a sticky or following value.
        let mut chars = a[1..].chars();
        while let Some(c) = chars.next() {
            match c {
                'h' => {
                    print!("{USAGE}\n");
                    return Ok(ExitCode::from(129));
                }
                'o' | 's' => {
                    let rest: String = chars.by_ref().collect();
                    if rest.is_empty() {
                        if args.get(i).is_none() {
                            return Ok(bare_error(&format!("switch `{c}' requires a value")));
                        }
                        i += 1;
                    }
                }
                _ => return Ok(usage_error(&format!("unknown switch `{c}'"))),
            }
        }
    }

    bail!(
        "generating a diagnostics archive is not supported: needs `git version --build-options` \
         data, a statvfs free-space/mount-flags query, and a zip/deflate writer — none of which \
         exist in gitoxide or this crate's dependency set"
    );
}

/// Stock's usage-error path: `error: <msg>` followed by the usage block, both
/// on stderr, exit status 129.
fn usage_error(msg: &str) -> ExitCode {
    eprint!("error: {msg}\n{USAGE}\n");
    ExitCode::from(129)
}

/// Stock's value-error path: `error: <msg>` on stderr with no usage block,
/// exit status 129.
fn bare_error(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::from(129)
}
