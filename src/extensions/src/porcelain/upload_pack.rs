//! `git upload-pack` — the *server* half of `git fetch`.
//! **The protocol itself is not ported: every serving path bails.**
//!
//! `upload-pack` is invoked by `git fetch-pack` over a transport. It writes a
//! ref advertisement, reads `want`/`have` negotiation from stdin, and streams a
//! generated pack back. What is ported here is the surface that is byte-
//! verifiable *without* speaking the protocol — argument parsing and repository
//! resolution (all checked against git 2.55.0 on Darwin):
//!
//!   * `-h` → the 368-byte usage block on **stdout**, exit 129, before any
//!     repository is touched (so it works outside a repository).
//!   * no `<directory>`, or more than one → the 458-byte usage block (the
//!     longer variant, which also lists the hidden `--advertise-refs` alias) on
//!     **stderr**, exit 129.
//!   * an unknown long option → ``error: unknown option `<name>'`` followed by
//!     the short usage block, both on **stderr**, exit 129.
//!   * an unknown short switch → ``error: unknown switch `<c>'`` followed by the
//!     short usage block, both on **stderr**, exit 129.
//!   * an ambiguous abbreviation → ``error: ambiguous option: <name> (could be
//!     --<a> or --<b>)`` on **stderr** with the short usage block on **stdout**,
//!     exit 129. The split across the two streams is git's, not a mistake here.
//!   * `--timeout` diagnostics — missing value, non-numeric value, empty value,
//!     and out-of-`i32`-range value — each on **stderr** with no usage block,
//!     exit 129.
//!   * `--strict=<v>` and friends → ``error: option `<name>' takes no value``,
//!     exit 129.
//!   * `<directory>` that resolves to no repository → `fatal: '<directory>' does
//!     not appear to be a git repository` on **stderr**, exit 128, quoting the
//!     argument exactly as given.
//!
//! Once a repository *does* resolve, this bails. The missing substrate,
//! concretely:
//!
//!   1. **There is no server-side protocol implementation in the vendored
//!      crates.** `src/ported/gix-protocol/src/` contains `handshake`, `fetch`,
//!      `ls_refs` and `command` — all of it the *client* talking to a remote.
//!      Its `Cargo.toml` gates everything behind `blocking-client` /
//!      `async-client`; there is no server feature and no server module. The
//!      only mentions of "upload-pack" under `gix-transport/src` are call sites
//!      that *spawn* someone else's `upload-pack`. Nothing implements the
//!      `want`/`have`/`ACK`/`NAK` state machine from the serving side, shallow
//!      /deepen handling, or `ref-in-want`.
//!   2. **The capability advertisement is a function of the git binary, not of
//!      the repository.** Every advertisement line stock git emits ends with
//!      `agent=git/2.55.0-Darwin` — the installed git's version string and
//!      platform. It is not derivable from the repository or from the vendored
//!      crates, and hardcoding one build's value would produce output that
//!      matches on exactly one machine. The rest of the list is equally
//!      environmental: `no-done` is advertised for `--advertise-refs` but not
//!      for the full v0 exchange, and `filter`, `allow-tip-sha1-in-want`,
//!      `allow-reachable-sha1-in-want` and `ref-in-want` each appear only when
//!      the matching `uploadpack.*` config is set.
//!   3. **Pack generation is not wired to a protocol.** `gix-pack` has
//!      `data::output` (`count`, `entry`) for building a pack, but nothing
//!      turns a negotiated want/have set into a side-band-multiplexed stream
//!      with progress on band 2, and there is no thin-pack or `include-tag`
//!      support on the sending side.
//!
//! These paths are deliberately not approximated. An `upload-pack` that exited 0
//! having written a plausible-looking but wrong advertisement would corrupt the
//! fetch of whoever ran it, while looking like a success to a harness that
//! compares exit codes.

use anyhow::{bail, Result};
use std::path::PathBuf;
use std::process::ExitCode;

/// The usage block git prints for `-h` and for option errors: 368 bytes, with
/// the hidden `--advertise-refs` alias omitted.
const USAGE_SHORT: &str = concat!(
    "usage: git-upload-pack [--[no-]strict] [--timeout=<n>] [--stateless-rpc]\n",
    "                       [--advertise-refs] <directory>\n",
    "\n",
    "    --[no-]stateless-rpc  quit after a single request/response exchange\n",
    "    --[no-]strict         do not try <directory>/.git/ if <directory> is no Git directory\n",
    "    --[no-]timeout <n>    interrupt transfer after <n> seconds of inactivity\n",
    "\n",
);

/// The usage block git prints when the `<directory>` count is wrong: 458 bytes.
/// This is the explicit `usage_with_options()` call in the command itself, which
/// unlike the `-h`/error path also lists the hidden `--advertise-refs` alias.
const USAGE_FULL: &str = concat!(
    "usage: git-upload-pack [--[no-]strict] [--timeout=<n>] [--stateless-rpc]\n",
    "                       [--advertise-refs] <directory>\n",
    "\n",
    "    --[no-]stateless-rpc  quit after a single request/response exchange\n",
    "    --[no-]advertise-refs ...\n",
    "                          alias of --http-backend-info-refs\n",
    "    --[no-]strict         do not try <directory>/.git/ if <directory> is no Git directory\n",
    "    --[no-]timeout <n>    interrupt transfer after <n> seconds of inactivity\n",
    "\n",
);

/// The long options, in the order they appear in git's option table. The order
/// is load-bearing: ambiguous abbreviations are reported against it.
const LONG_OPTS: [&str; 5] = [
    "stateless-rpc",
    "http-backend-info-refs",
    "advertise-refs",
    "strict",
    "timeout",
];

/// Index into [`LONG_OPTS`] for `--strict`, the only option this module acts on.
const OPT_STRICT: usize = 3;

/// Index into [`LONG_OPTS`] for the only option that takes a value.
const OPT_TIMEOUT: usize = 4;

/// The suffixes git's integer parser accepts, and the factor each applies.
const MAGNITUDES: [(char, i128); 6] = [
    ('k', 1024),
    ('K', 1024),
    ('m', 1024 * 1024),
    ('M', 1024 * 1024),
    ('g', 1024 * 1024 * 1024),
    ('G', 1024 * 1024 * 1024),
];

/// One resolved long option: which entry of [`LONG_OPTS`], and whether it was
/// spelled with the `no-` prefix.
#[derive(Clone, Copy)]
struct Resolved {
    index: usize,
    negated: bool,
}

impl Resolved {
    /// The canonical spelling git uses when naming this option in a diagnostic,
    /// i.e. the full long name including any `no-` prefix, without dashes.
    fn name(self) -> String {
        if self.negated {
            format!("no-{}", LONG_OPTS[self.index])
        } else {
            LONG_OPTS[self.index].to_owned()
        }
    }
}

/// `git upload-pack` — argument parsing and repository resolution only; serving
/// the fetch protocol is not ported.
///
/// See the module documentation for the exact set of invocations reproduced
/// byte-for-byte, and for the substrate the rest would need.
pub fn upload_pack(args: &[String]) -> Result<ExitCode> {
    // Dispatch includes the verb at index 0. `upload-pack`'s only positional is
    // `<directory>`, so a leading literal verb is unambiguous only as the verb;
    // strip exactly one. Both spellings git installs are accepted.
    let args = match args.first().map(String::as_str) {
        Some("upload-pack" | "git-upload-pack") => &args[1..],
        _ => args,
    };

    let mut strict = false;
    let mut directories: Vec<&str> = Vec::new();
    let mut end_of_opts = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();

        if end_of_opts {
            directories.push(a);
            i += 1;
            continue;
        }

        if a == "--" {
            end_of_opts = true;
            i += 1;
            continue;
        }

        // A long option, possibly abbreviated, possibly `--name=value`.
        if let Some(body) = a.strip_prefix("--") {
            let (name, inline) = match body.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (body, None),
            };

            let opt = match resolve_long(name) {
                Ok(opt) => opt,
                Err(LongError::Unknown) => {
                    eprint!("error: unknown option `{name}'\n{USAGE_SHORT}");
                    return Ok(ExitCode::from(129));
                }
                // git splits this one across the streams: the diagnostic on
                // stderr, the usage block on stdout.
                Err(LongError::Ambiguous(first, second)) => {
                    eprintln!(
                        "error: ambiguous option: {name} (could be --{first} or --{second})"
                    );
                    print!("{USAGE_SHORT}");
                    return Ok(ExitCode::from(129));
                }
            };

            // Only `--timeout` takes a value, and its negated form does not.
            if opt.index != OPT_TIMEOUT || opt.negated {
                if inline.is_some() {
                    eprintln!("error: option `{}' takes no value", opt.name());
                    return Ok(ExitCode::from(129));
                }
                if opt.index == OPT_STRICT {
                    strict = !opt.negated;
                }
                i += 1;
                continue;
            }

            let value = match inline {
                Some(v) => v,
                None => match args.get(i + 1) {
                    Some(v) => {
                        i += 1;
                        v.as_str()
                    }
                    None => {
                        eprintln!("error: option `{}' requires a value", opt.name());
                        return Ok(ExitCode::from(129));
                    }
                },
            };
            // The value is parsed for its diagnostics only; nothing here can
            // time out, because nothing here reads from the transport.
            if let Err(msg) = parse_timeout(value, &opt.name()) {
                eprintln!("{msg}");
                return Ok(ExitCode::from(129));
            }
            i += 1;
            continue;
        }

        // A short switch cluster. `upload-pack` defines none of its own, so the
        // only one that resolves is parse-options' built-in `-h`.
        // Either way the first letter of the cluster decides, so the rest is
        // never reached.
        if let Some(c) = a.strip_prefix('-').and_then(|s| s.chars().next()) {
            if c == 'h' {
                print!("{USAGE_SHORT}");
                return Ok(ExitCode::from(129));
            }
            eprint!("error: unknown switch `{c}'\n{USAGE_SHORT}");
            return Ok(ExitCode::from(129));
        }

        directories.push(a);
        i += 1;
    }

    // Exactly one `<directory>` is required; anything else is the command's own
    // `usage_with_options()` call, which prints the longer block to stderr.
    if directories.len() != 1 {
        eprint!("{USAGE_FULL}");
        return Ok(ExitCode::from(129));
    }
    let directory = directories[0];

    // Resolution mirrors git's `enter_repo()`: `~` is expanded, and unless
    // `--strict` was given the suffix list is tried in order, so a worktree wins
    // over a sibling bare repository of the same name.
    let expanded = match expand_tilde(directory) {
        Ok(p) => p,
        Err(msg) => bail!("{msg}"),
    };
    let candidates: Vec<PathBuf> = if strict {
        vec![expanded]
    } else {
        let base = expanded.as_os_str().to_owned();
        ["/.git", "", ".git/.git", ".git"]
            .iter()
            .map(|suffix| {
                let mut p = base.clone();
                p.push(suffix);
                PathBuf::from(p)
            })
            .collect()
    };

    // `open_path_as_is` keeps gix from silently appending `/.git` itself, which
    // would make `--strict` accept a worktree root that git rejects.
    let options = gix::open::Options::default().open_path_as_is(true);
    let found = candidates
        .into_iter()
        .any(|c| gix::open_opts(c, options.clone()).is_ok());

    if !found {
        eprintln!("fatal: '{directory}' does not appear to be a git repository");
        return Ok(ExitCode::from(128));
    }

    bail!(
        "upload-pack is not ported: serving a fetch needs the want/have/ACK state machine, \
         shallow handling and side-band pack streaming from the server side, and the vendored \
         gix-protocol is client-only (handshake/fetch/ls_refs behind blocking-client) with no \
         server module; the advertisement is also unreproducible because every line carries \
         `agent=git/<version>-<platform>` of the git binary plus config-dependent capabilities \
         (ported: -h, the usage and option diagnostics, --timeout validation, and the \
         not-a-git-repository failure)"
    )
}

/// Why a long option could not be resolved to a single table entry.
enum LongError {
    /// No entry matched, even as an abbreviation.
    Unknown,
    /// Two or more matched. Carries the two git names in its message, which are
    /// the *last* two matches in table order — that is what git's
    /// `abbrev_option`/`ambiguous_option` pair ends up holding.
    Ambiguous(String, String),
}

/// Resolve the text after `--` (with any `=value` already split off) against the
/// option table, honouring exact matches, `no-` negation, and unique-prefix
/// abbreviation.
///
/// Each entry is tried plain first and then negated, matching the order git
/// scans in; an exact match returns immediately, so a later exact spelling wins
/// over an earlier abbreviation.
fn resolve_long(name: &str) -> Result<Resolved, LongError> {
    let mut matches: Vec<Resolved> = Vec::new();

    for (index, long) in LONG_OPTS.iter().enumerate() {
        for negated in [false, true] {
            let spelling = if negated {
                format!("no-{long}")
            } else {
                (*long).to_owned()
            };
            if spelling == name {
                return Ok(Resolved { index, negated });
            }
            if !name.is_empty() && spelling.starts_with(name) {
                matches.push(Resolved { index, negated });
            }
        }
    }

    match matches.len() {
        0 => Err(LongError::Unknown),
        1 => Ok(matches[0]),
        n => Err(LongError::Ambiguous(
            matches[n - 2].name(),
            matches[n - 1].name(),
        )),
    }
}

/// Validate a `--timeout` value the way git's `git_parse_int` does, returning
/// the diagnostic line git would print on failure.
///
/// Accepted: optional leading whitespace, an optional sign, a base-0 integer
/// (so `0x10` is hex and `010` is octal), and an optional `k`/`m`/`g` magnitude
/// suffix. The result must fit in an `i32`.
fn parse_timeout(value: &str, name: &str) -> Result<i32, String> {
    if value.is_empty() {
        return Err(format!("error: option `{name}' expects a numerical value"));
    }
    let invalid =
        || format!("error: option `{name}' expects an integer value with an optional k/m/g suffix");

    let rest = value.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    let (negative, rest) = match rest.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, rest.strip_prefix('+').unwrap_or(rest)),
    };

    // Base 0, exactly as strtoimax reads it.
    let (radix, digits) = if let Some(r) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X"))
    {
        (16, r)
    } else if rest.len() > 1 && rest.starts_with('0') {
        (8, &rest[1..])
    } else {
        (10, rest)
    };

    let split = digits
        .find(|c: char| !c.is_digit(radix))
        .unwrap_or(digits.len());
    let (number, tail) = digits.split_at(split);
    if number.is_empty() {
        return Err(invalid());
    }
    let mut magnitude: i128 = match i128::from_str_radix(number, radix) {
        Ok(v) => v,
        // Longer than an i128 can hold; git reports this as out of range.
        Err(_) => return Err(range_error(value, name)),
    };

    // At most one magnitude suffix, and nothing may follow it.
    if !tail.is_empty() {
        let mut chars = tail.chars();
        let suffix = chars.next().expect("tail is non-empty");
        let Some((_, factor)) = MAGNITUDES.iter().find(|(c, _)| *c == suffix) else {
            return Err(invalid());
        };
        if chars.next().is_some() {
            return Err(invalid());
        }
        magnitude *= factor;
    }

    if negative {
        magnitude = -magnitude;
    }
    i32::try_from(magnitude).map_err(|_| range_error(value, name))
}

/// git's out-of-range diagnostic, which quotes the value exactly as written,
/// magnitude suffix included.
fn range_error(value: &str, name: &str) -> String {
    format!(
        "error: value {value} for option `{name}' not in range [{},{}]",
        i32::MIN,
        i32::MAX
    )
}

/// Expand a leading `~` against `$HOME`, as `enter_repo()` does.
///
/// `~<user>` needs a passwd lookup that the vendored crates do not provide, so
/// it is refused rather than passed through unexpanded — silently treating it as
/// a literal path would report "not a git repository" for a directory that
/// exists.
fn expand_tilde(path: &str) -> Result<PathBuf, String> {
    let Some(rest) = path.strip_prefix('~') else {
        return Ok(PathBuf::from(path));
    };
    if !rest.is_empty() && !rest.starts_with('/') {
        return Err(format!(
            "~<user> expansion in {path:?} is not ported: it needs a passwd-database lookup that \
             the vendored crates do not provide"
        ));
    }
    let Some(home) = std::env::var_os("HOME") else {
        return Ok(PathBuf::from(path));
    };
    let mut out = PathBuf::from(home);
    if let Some(tail) = rest.strip_prefix('/') {
        out.push(tail);
    }
    Ok(out)
}
