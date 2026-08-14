//! `git commit-graph` — manage the serialized commit-graph file.
//!
//! Both subcommands are implemented, including the file format writer.
//!
//! * `git commit-graph verify [--object-dir <dir>] [--shallow] [--[no-]progress]`
//!   Opens `<object-dir>/info/commit-graph` (or the split chain under
//!   `<object-dir>/info/commit-graphs/`) through `gix_commitgraph` and runs
//!   `Graph::verify_integrity`. `--shallow` verifies only the tip file of a
//!   split chain, via `gix_commitgraph::File::traverse`. On success stdout is
//!   empty and the exit code is 0. When no commit-graph file exists at all git
//!   treats that as success too, and so does this.
//! * `git commit-graph write [--object-dir <dir>] [--append] [--reachable |
//!   --stdin-packs | --stdin-commits] [--[no-]progress] <split-options>`
//!   Collects the commit set the way git does — from every pack in the object
//!   directory by default, from all refs under `--reachable`, from stdin under
//!   `--stdin-commits` / `--stdin-packs` — closes it over all ancestors, then
//!   serializes `OIDF`/`OIDL`/`CDAT`/`GDA2` (plus `GDO2`, `EDGE` and the
//!   changed-path `BIDX`/`BDAT` pair when needed) into
//!   `<object-dir>/info/commit-graph`. As in git, an empty commit
//!   set writes no file and exits 0, and a successful non-split write removes
//!   any existing split chain.
//!
//! `core.commitGraph=false` turns the feature off outright: `write_commit_graph()`
//! opens with that check, so a `write` under it warns `attempting to write a
//! commit-graph, but 'core.commitGraph' is disabled`, leaves the object
//! directory untouched, and still exits 0. The key defaults to true.
//!
//! `commitGraph.generationVersion` selects the generation-number version, as in
//! git: value 2 (git's default, and the value used when the key is unset) writes
//! the corrected-commit-date `GDA2` chunk (plus `GDO2` on overflow); any other
//! value — 0, 1, 3, negatives — keeps only the topological level already stored
//! in the `CDAT` generation bits and omits the corrected-date chunks. git gates
//! this on the value being exactly 2, and a non-numeric value is fatal.
//!
//! `--changed-paths` writes the changed-path Bloom filters: for each commit, the
//! paths it changed against its first parent — plus every leading directory of
//! each — hashed by [`gix::commitgraph::bloom`], a port of git's `bloom.c`, into
//! the `BIDX` offset table and the `BDAT` filter data. Four keys steer them, as
//! in git: `commitGraph.changedPaths` turns them on without the flag,
//! `commitGraph.changedPathsVersion` (-1, 0, 1 or 2) picks which murmur3 hashes
//! them and, at 0, refuses to read existing ones at all,
//! `commitGraph.readChangedPaths` is the deprecated boolean spelling of that
//! last behavior, and `commitGraph.maxNewFilters` bounds how many filters one
//! write may compute. A write that neither asks for filters nor forbids them
//! keeps whatever the graph it replaces already had.
//!
//! The vendored `gix-commitgraph` writes nothing (`src/{init,access,verify}.rs`,
//! `src/file/`, plus the `bloom` port it gained for the filters above), so the
//! serializer here is written against git's
//! `commit-graph-format.txt` layout and validated against files produced by
//! stock git: header `CGPH`, version 1, `<hash-version>`, `<chunk-count>`,
//! `<base-graph-count>`; a chunk lookup of `(id, u64 offset)` pairs terminated
//! by a zero id; `CDAT` entries of `<tree><parent1><parent2><generation<<34 |
//! committer-date>`; `NO_PARENT` = `0x70000000`; the extra-edge marker and the
//! last-extra-edge marker both `0x80000000`; `GDA2` holding the corrected
//! commit date *offset* per commit.
//!
//! `--split` writes the *first* layer of a chain: the graph goes to
//! `info/commit-graphs/graph-<checksum>.graph` and `commit-graph-chain` names
//! it. A one-layer chain has no base graph, so its bytes are exactly the
//! single-file graph's — verified byte-for-byte against git 2.55.0 — and only
//! the naming differs. `--split=no-merge` and `--split=replace` reach the same
//! layer, there being nothing to merge or replace.
//!
//! Deliberately not implemented — this bails rather than writing a file that
//! only looks right:
//!   * `--split` over a commit-graph that already exists. Folding one into the
//!     new tip needs `split_graph_merge_strategy()`'s layer accounting, the
//!     `BASE` chunk, parent positions renumbered across the whole chain, and
//!     `expire_commit_graphs()` — none of which the vendored crate models.
//!
//! `--max-commits`, `--size-multiple` and `--expire-time` only steer the merge
//! strategy above; git accepts and ignores them for a non-split write, and so
//! does this, after validating their values so the exit codes still agree.
//!
//! Progress goes to stderr in git and is not emitted here. Verification
//! *failure* text is gix's diagnostic, not git's wording; only the success path
//! is byte-identical. Exit codes follow git: 1 when the file fails to parse or
//! verify, 128 for `fatal:`, 129 for usage errors.

use anyhow::{bail, Result};
use gix::hash::ObjectId;
use gix::odb::pack;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Top-level usage, byte-for-byte as git 2.55 emits it.
/// `cmd_commit_graph()`'s top-level option table (builtin/commit-graph.c:347),
/// which is `parse_options_concat(builtin_commit_graph_options, common_opts)`.
///
/// The two `OPT_SUBCOMMAND` entries carry no long *option* name — `parse_long_opt()`
/// skips `OPTION_SUBCOMMAND` outright (parse-options.c:542-543) — so `--object-dir`
/// from `common_opts` is the only name this level resolves. The `verify` and `write`
/// tables belong to those subcommands and are reached only after one is named.
const LONG_OPTS: &[super::LongOpt] = &[
    super::LongOpt { name: "object-dir",                  neg: true,  arg: super::Arg::Required },
];

const TOP_USAGE: &str = "\
usage: git commit-graph verify [--object-dir <dir>] [--shallow] [--[no-]progress]
   or: git commit-graph write [--object-dir <dir>] [--append]
                              [--split[=<strategy>]] [--reachable | --stdin-packs | --stdin-commits]
                              [--changed-paths] [--[no-]max-new-filters <n>] [--[no-]progress]
                              <split-options>

    --[no-]object-dir <dir>
                          the object directory to store the graph

";

/// `git commit-graph verify -h`.
const VERIFY_USAGE: &str = "\
usage: git commit-graph verify [--object-dir <dir>] [--shallow] [--[no-]progress]

    --[no-]object-dir <dir>
                          the object directory to store the graph
    --[no-]shallow        if the commit-graph is split, only verify the tip file
    --[no-]progress       force progress reporting

";

/// `git commit-graph write -h`.
const WRITE_USAGE: &str = "\
usage: git commit-graph write [--object-dir <dir>] [--append]
                              [--split[=<strategy>]] [--reachable | --stdin-packs | --stdin-commits]
                              [--changed-paths] [--[no-]max-new-filters <n>] [--[no-]progress]
                              <split-options>

    --[no-]object-dir <dir>
                          the object directory to store the graph
    --[no-]reachable      start walk at all refs
    --[no-]stdin-packs    scan pack-indexes listed by stdin for commits
    --[no-]stdin-commits  start walk at commits listed by stdin
    --[no-]append         include all commits already in the commit-graph file
    --[no-]changed-paths  enable computation for changed paths
    --split[=...]         allow writing an incremental commit-graph file
    --[no-]max-commits <n>
                          maximum number of commits in a non-base split commit-graph
    --[no-]size-multiple <n>
                          maximum ratio between two levels of a split commit-graph
    --[no-]expire-time <expiry-date>
                          only expire files older than a given date-time
    --[no-]max-new-filters ...
                          maximum number of changed-path Bloom filters to compute
    --[no-]progress       force progress reporting

";

// Format constants, mirroring `gix_commitgraph::file` (which keeps them private)
// and git's `commit-graph.h`.
const SIGNATURE: &[u8; 4] = b"CGPH";
const HEADER_LEN: usize = 8;
const CHUNK_LOOKUP_ENTRY_LEN: usize = 12;
const FAN_LEN: usize = 256;
const NO_PARENT: u32 = 0x7000_0000;
const EXTENDED_EDGES_MASK: u32 = 0x8000_0000;
const LAST_EXTENDED_EDGE_MASK: u32 = 0x8000_0000;
/// Generation numbers are stored in the top 30 bits of the `CDAT` date word.
const GENERATION_MAX: u32 = 0x3FFF_FFFF;
/// Committer dates occupy the low 34 bits of that same word.
const DATE_MASK: u64 = 0x0003_FFFF_FFFF;
/// A corrected-date offset wider than this moves into the `GDO2` chunk.
const OFFSET_MAX: u64 = 0x7FFF_FFFF;
/// Set in a `GDA2` slot to mean "the offset lives at this index in `GDO2`".
const OFFSET_OVERFLOW_MASK: u32 = 0x8000_0000;

/// `git commit-graph` — dispatch on the subcommand after the shared options.
///
/// git parses the leading options with `parse_options` in `OPT_SUBCOMMAND`
/// mode: only `--object-dir` is recognised before the subcommand name, anything
/// else is an unknown option, and `--` ends the search without having found a
/// subcommand.
pub fn commit_graph(args: &[String]) -> Result<ExitCode> {
    let mut object_dir: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        // Respell a unique abbreviation as the name it resolves to, so an
        // abbreviation lands on the arm its full spelling lands on.
        let canonical;
        let a = match super::canonical_long(a, LONG_OPTS) {
            super::Long::Name(name) => {
                canonical = name;
                canonical.as_ref()
            }
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(a, &first, &second, TOP_USAGE))
            }
        };
        match a {
            // `--help-all` renders `USAGE_FULL`, identical to the `-h` block:
            // no entry of this table is `PARSE_OPT_HIDDEN`.
            "-h" | "--help-all" => {
                print!("{TOP_USAGE}");
                return Ok(ExitCode::from(129));
            }
            // `--` stops option parsing; no subcommand was seen by then.
            "--" => return Ok(usage_error(Some("need a subcommand"), TOP_USAGE)),
            "--object-dir" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    return Ok(missing_value("object-dir"));
                };
                object_dir = Some(v.clone());
            }
            "--no-object-dir" => object_dir = None,
            "verify" => return verify(&args[i + 1..], object_dir),
            "write" => return write_graph(&args[i + 1..], object_dir),
            s if s.starts_with("--object-dir=") => {
                object_dir = Some(s["--object-dir=".len()..].to_string());
            }
            s if s.starts_with("--") => return Ok(unknown_option(&s[2..], TOP_USAGE)),
            s if s.len() > 1 && s.starts_with('-') => {
                return Ok(unknown_switch(s, TOP_USAGE))
            }
            other => {
                eprint!("error: unknown subcommand: `{other}'\n{TOP_USAGE}");
                return Ok(ExitCode::from(129));
            }
        }
        i += 1;
    }

    Ok(usage_error(Some("need a subcommand"), TOP_USAGE))
}

// --- shared error shapes ---------------------------------------------------

/// parse-options' failure shape: an optional `error: <msg>` line followed by the
/// usage block, both on stderr, exit 129. A stray positional prints usage alone.
fn usage_error(msg: Option<&str>, usage: &str) -> ExitCode {
    match msg {
        Some(m) => eprint!("error: {m}\n{usage}"),
        None => eprint!("{usage}"),
    }
    ExitCode::from(129)
}

fn unknown_option(name: &str, usage: &str) -> ExitCode {
    usage_error(Some(&format!("unknown option `{name}'")), usage)
}

/// The short-option form of the above: git names the offending character, so the
/// second character of the argument is taken rather than a byte slice of it.
fn unknown_switch(arg: &str, usage: &str) -> ExitCode {
    let c = arg.chars().nth(1).unwrap_or('-');
    usage_error(Some(&format!("unknown switch `{c}'")), usage)
}

/// A missing option argument prints no usage block, unlike every other 129 path.
fn missing_value(name: &str) -> ExitCode {
    eprintln!("error: option `{name}' requires a value");
    ExitCode::from(129)
}

fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

/// `error: <msg>` with exit 1 — git's shape for a bad object fed to `write`.
fn error_exit(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::from(1)
}

/// git's `config_error_nonbool`/`git_config_int` shape for a numeric config
/// value that will not parse: `fatal: bad numeric config value '<raw>' for
/// '<lowercased-key>': <reason>`, exit 128. git reports `out of range` when the
/// magnitude overflows and `invalid unit` otherwise.
fn bad_numeric_config(raw: &str, lowercased_key: &str) -> ExitCode {
    let reason = if is_overflowing_integer(raw) {
        "out of range"
    } else {
        "invalid unit"
    };
    fatal(&format!(
        "bad numeric config value '{raw}' for '{lowercased_key}': {reason}"
    ))
}

/// Whether `raw` is syntactically an integer (optional sign, digits, optional
/// single `k`/`m`/`g`/`t` scale suffix) whose value cannot fit — the only way a
/// well-formed integer still fails to parse, which git reports as `out of range`.
fn is_overflowing_integer(raw: &str) -> bool {
    let digits = match raw.as_bytes().last() {
        Some(c) if matches!(c.to_ascii_lowercase(), b'k' | b'm' | b'g' | b't') => {
            &raw[..raw.len() - 1]
        }
        _ => raw,
    };
    let digits = digits.strip_prefix(['+', '-']).unwrap_or(digits);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// Split `--name=value`, returning the value when `arg` names `name`.
fn long_value<'a>(arg: &'a str, name: &str) -> Option<&'a str> {
    arg.strip_prefix("--")
        .and_then(|rest| rest.strip_prefix(name))
        .and_then(|rest| rest.strip_prefix('='))
}

/// git's `OPT_INTEGER` scanner: a decimal number with an optional `k`/`m`/`g`
/// suffix. Only the validity of the value matters here — the values themselves
/// steer split writes, which are not produced.
fn parse_scaled_int(name: &str, raw: &str) -> std::result::Result<(), ExitCode> {
    let (digits, scale) = match raw.chars().last() {
        Some(c) if matches!(c.to_ascii_lowercase(), 'k' | 'm' | 'g') => (&raw[..raw.len() - 1], true),
        _ => (raw, false),
    };
    let ok = !digits.is_empty()
        && digits.parse::<i64>().is_ok()
        && (!scale || !digits.starts_with('-'));
    if ok {
        Ok(())
    } else {
        eprintln!("error: option `{name}' expects an integer value with an optional k/m/g suffix");
        Err(ExitCode::from(129))
    }
}

// --- verify ----------------------------------------------------------------

/// `git commit-graph verify` — read the commit-graph and check it against itself.
fn verify(args: &[String], inherited_object_dir: Option<String>) -> Result<ExitCode> {
    let mut object_dir = inherited_object_dir;
    let mut shallow = false;
    let mut end_of_opts = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if end_of_opts {
            // `verify` takes no positionals.
            return Ok(usage_error(None, VERIFY_USAGE));
        }
        match a {
            // `--help-all` renders `USAGE_FULL`, identical to the `-h` block:
            // no entry of this subcommand's table is `PARSE_OPT_HIDDEN`.
            "-h" | "--help-all" => {
                print!("{VERIFY_USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--" => end_of_opts = true,
            "--object-dir" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    return Ok(missing_value("object-dir"));
                };
                object_dir = Some(v.clone());
            }
            "--no-object-dir" => object_dir = None,
            "--shallow" => shallow = true,
            "--no-shallow" => shallow = false,
            // Progress is written to stderr by git and only when forced or on a
            // tty; nothing is emitted here either way.
            "--progress" | "--no-progress" => {}
            s if s.starts_with("--object-dir=") => {
                object_dir = Some(s["--object-dir=".len()..].to_string());
            }
            s if s.starts_with("--") => return Ok(unknown_option(&s[2..], VERIFY_USAGE)),
            s if s.len() > 1 && s.starts_with('-') => {
                return Ok(unknown_switch(s, VERIFY_USAGE))
            }
            _ => return Ok(usage_error(None, VERIFY_USAGE)),
        }
        i += 1;
    }

    let repo = gix::discover(".")?;
    let objects = match object_directory(&repo, object_dir.as_deref()) {
        Ok(p) => p,
        Err(code) => return Ok(code),
    };

    let info = objects.join("info");
    let single = info.join("commit-graph");
    let chain = info.join("commit-graphs").join("commit-graph-chain");
    // No commit-graph at all is not an error for git: it verifies nothing and succeeds.
    if !single.is_file() && !chain.is_file() {
        return Ok(ExitCode::SUCCESS);
    }

    // `--shallow` on a split chain checks the tip file only. The chain file
    // lists base graphs first and the tip last.
    if shallow && !single.is_file() {
        let Some(tip) = chain_tip(&chain) else {
            eprintln!("error: could not read commit-graph chain file");
            return Ok(ExitCode::from(1));
        };
        return Ok(match gix::commitgraph::File::at(&tip) {
            Ok(f) => match f.traverse(|_| Ok(())) {
                Ok(_) => ExitCode::SUCCESS,
                Err(e) => error_exit(&e.to_string()),
            },
            Err(e) => error_exit(&e.to_string()),
        });
    }

    let graph = match gix::commitgraph::at(&info) {
        Ok(g) => g,
        Err(e) => return Ok(error_exit(&e.to_string())),
    };

    match graph.verify_integrity(|_| -> Result<(), std::convert::Infallible> { Ok(()) }) {
        Ok(_) => Ok(ExitCode::SUCCESS),
        Err(e) => {
            eprintln!("{e}");
            Ok(ExitCode::from(1))
        }
    }
}

/// The last entry of a `commit-graph-chain` file, resolved to its `.graph` path.
fn chain_tip(chain: &Path) -> Option<PathBuf> {
    let body = std::fs::read_to_string(chain).ok()?;
    let last = body.lines().map(str::trim).rfind(|l| !l.is_empty())?;
    Some(
        chain
            .parent()?
            .join(format!("graph-{last}.graph")),
    )
}

// --- write -----------------------------------------------------------------

/// Where the commit set comes from. git allows at most one of these.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Source {
    /// Every commit in every pack of the object directory (git's default).
    Packs,
    /// Every commit reachable from any ref.
    Reachable,
    /// Pack index names on stdin.
    StdinPacks,
    /// Commit ids on stdin.
    StdinCommits,
}

/// `git commit-graph write` — collect commits and serialize the graph file.
fn write_graph(args: &[String], inherited_object_dir: Option<String>) -> Result<ExitCode> {
    let mut object_dir = inherited_object_dir;
    let mut reachable = false;
    let mut stdin_packs = false;
    let mut stdin_commits = false;
    let mut append = false;
    // git's tri-state: `--[no-]changed-paths` always beats
    // `commitGraph.changedPaths`, and "nobody said" additionally means "keep
    // whatever the existing graph already has".
    let mut changed_paths: Option<bool> = None;
    // `--max-new-filters`, defaulting to `commitGraph.maxNewFilters`; git's
    // `opts->max_new_filters`, where a negative value means "no bound".
    let mut max_new_filters: Option<i64> = None;
    let mut split = false;
    let mut end_of_opts = false;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if end_of_opts {
            return Ok(usage_error(None, WRITE_USAGE));
        }
        match a {
            // `--help-all` renders `USAGE_FULL`, identical to the `-h` block:
            // no entry of this subcommand's table is `PARSE_OPT_HIDDEN`.
            "-h" | "--help-all" => {
                print!("{WRITE_USAGE}");
                return Ok(ExitCode::from(129));
            }
            "--" => end_of_opts = true,
            "--object-dir" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    return Ok(missing_value("object-dir"));
                };
                object_dir = Some(v.clone());
            }
            "--no-object-dir" => object_dir = None,
            "--reachable" => reachable = true,
            "--no-reachable" => reachable = false,
            "--stdin-packs" => stdin_packs = true,
            "--no-stdin-packs" => stdin_packs = false,
            "--stdin-commits" => stdin_commits = true,
            "--no-stdin-commits" => stdin_commits = false,
            "--append" => append = true,
            "--no-append" => append = false,
            "--changed-paths" => changed_paths = Some(true),
            "--no-changed-paths" => changed_paths = Some(false),
            "--progress" | "--no-progress" => {}
            "--split" => split = true,
            // `--max-new-filters` bounds how many filters one run may compute;
            // git's `--no-max-new-filters` restores the unbounded default.
            "--no-max-new-filters" => max_new_filters = None,
            "--max-new-filters" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    return Ok(missing_value("max-new-filters"));
                };
                // `write_option_max_new_filters()` reads the value with C `strtol`, so
                // overflow saturates instead of failing and only trailing junk is an
                // error.
                match crate::optint::strtol_all(v) {
                    Some(n) => max_new_filters = Some(n),
                    None => {
                        eprintln!("error: option `max-new-filters' expects a numerical value");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            // Split-only knobs: validated, then ignored, exactly as git does for
            // a non-split write.
            "--max-commits" | "--size-multiple" => {
                let name = &a[2..];
                i += 1;
                let Some(v) = args.get(i) else {
                    return Ok(missing_value(name));
                };
                if let Err(code) = parse_scaled_int(name, v) {
                    return Ok(code);
                }
            }
            "--no-max-commits" | "--no-size-multiple" | "--no-expire-time" => {}
            "--expire-time" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    return Ok(missing_value("expire-time"));
                };
                // `OPT_EXPIRY_DATE` (parse-options-cb.c:45) is `parse_expiry_date()`.
                if crate::date::parse_expiry_date(v).is_none() {
                    return Ok(fatal(&format!("malformed expiration date '{v}'")));
                }
            }
            s if s.starts_with("--object-dir=") => {
                object_dir = Some(s["--object-dir=".len()..].to_string());
            }
            s if s.starts_with("--split=") => {
                split = true;
                let strategy = &s["--split=".len()..];
                if !matches!(strategy, "no-merge" | "replace") {
                    return Ok(fatal(&format!("unrecognized --split argument, {strategy}")));
                }
            }
            s if long_value(s, "max-new-filters").is_some() => {
                let v = long_value(s, "max-new-filters").unwrap_or_default();
                // `write_option_max_new_filters()` reads the value with C `strtol`, so
                // overflow saturates instead of failing and only trailing junk is an
                // error.
                match crate::optint::strtol_all(v) {
                    Some(n) => max_new_filters = Some(n),
                    None => {
                        eprintln!("error: option `max-new-filters' expects a numerical value");
                        return Ok(ExitCode::from(129));
                    }
                }
            }
            s if long_value(s, "max-commits").is_some() => {
                let v = long_value(s, "max-commits").unwrap_or_default();
                if let Err(code) = parse_scaled_int("max-commits", v) {
                    return Ok(code);
                }
            }
            s if long_value(s, "size-multiple").is_some() => {
                let v = long_value(s, "size-multiple").unwrap_or_default();
                if let Err(code) = parse_scaled_int("size-multiple", v) {
                    return Ok(code);
                }
            }
            s if long_value(s, "expire-time").is_some() => {
                let v = long_value(s, "expire-time").unwrap_or_default();
                // `OPT_EXPIRY_DATE` (parse-options-cb.c:45) is `parse_expiry_date()`.
                if crate::date::parse_expiry_date(v).is_none() {
                    return Ok(fatal(&format!("malformed expiration date '{v}'")));
                }
            }
            s if s.starts_with("--") => return Ok(unknown_option(&s[2..], WRITE_USAGE)),
            s if s.len() > 1 && s.starts_with('-') => {
                return Ok(unknown_switch(s, WRITE_USAGE))
            }
            _ => return Ok(usage_error(None, WRITE_USAGE)),
        }
        i += 1;
    }

    if usize::from(reachable) + usize::from(stdin_packs) + usize::from(stdin_commits) > 1 {
        return Ok(fatal(
            "use at most one of --reachable, --stdin-commits, or --stdin-packs",
        ));
    }

    let repo = gix::discover(".")?;

    // `commitGraph.generationVersion` selects the generation-number version.
    // Version 2 (git's default) additionally records the corrected commit date
    // in the `GDA2` chunk (and `GDO2` on overflow); version 1 stores only the
    // topological level, which already lives in the `CDAT` generation bits, so
    // the corrected-date chunks are omitted. git gates this on the value being
    // exactly 2: 0, 1, 3 and negatives all drop the chunk, an absent key means
    // 2, and a non-numeric value is fatal.
    let write_generation_data = match repo
        .config_snapshot()
        .try_integer("commitGraph.generationVersion")
    {
        Ok(v) => v.unwrap_or(2) == 2,
        Err(_) => {
            let raw = repo
                .config_snapshot()
                .string("commitGraph.generationVersion")
                .map(|v| v.to_string())
                .unwrap_or_default();
            return Ok(bad_numeric_config(&raw, "commitgraph.generationversion"));
        }
    };

    // `commitGraph.readChangedPaths` is the deprecated spelling: true means
    // `changedPathsVersion = -1`, false means `0`, and an explicit
    // `changedPathsVersion` wins over either. git resolves both in
    // `prepare_repo_settings()`.
    let read_changed_paths = repo
        .config_snapshot()
        .boolean("commitGraph.readChangedPaths")
        .unwrap_or(true);
    let changed_paths_version = match repo
        .config_snapshot()
        .try_integer("commitGraph.changedPathsVersion")
    {
        Ok(Some(v)) => v,
        Ok(None) => {
            if read_changed_paths {
                -1
            } else {
                0
            }
        }
        Err(_) => {
            let raw = repo
                .config_snapshot()
                .string("commitGraph.changedPathsVersion")
                .map(|v| v.to_string())
                .unwrap_or_default();
            return Ok(bad_numeric_config(&raw, "commitgraph.changedpathsversion"));
        }
    };

    let objects = match object_directory(&repo, object_dir.as_deref()) {
        Ok(p) => p,
        Err(code) => return Ok(code),
    };

    // What `--split` is *not*: the strategies that fold an existing graph into
    // the new tip. `split_graph_merge_strategy()` decides how many layers of the
    // old chain survive, `write_commit_graph_file()` writes their checksums into
    // a `BASE` chunk and renumbers every parent position across the whole chain,
    // and `expire_commit_graphs()` removes what is left over. None of that is
    // modelled, so a repository that already has a graph to build on is refused
    // rather than answered with a chain that only looks right. Writing the
    // *first* layer of a chain needs none of it: with no base graph the layer's
    // bytes are exactly the single-file graph's — verified byte-for-byte against
    // git 2.55.0 — and only the naming and the chain file differ.
    if split
        && (objects.join("info").join("commit-graph").exists()
            || objects
                .join("info")
                .join("commit-graphs")
                .join("commit-graph-chain")
                .exists())
    {
        bail!(
            "unsupported flag \"--split\" over an existing commit-graph: the chain merge, \
             BASE chunk and expiry strategies are not modelled by the read-only vendored \
             gix-commitgraph"
        );
    }

    // `write_commit_graph()`'s first act is to refuse outright when the feature
    // is switched off, so `-c core.commitGraph=false commit-graph write` warns,
    // leaves the object directory untouched, and still succeeds. The default is
    // on (`prepare_repo_settings()` reads it with a fallback of 1).
    if !repo
        .config_snapshot()
        .boolean("core.commitGraph")
        .unwrap_or(true)
    {
        eprintln!(
            "warning: attempting to write a commit-graph, but 'core.commitGraph' is disabled"
        );
        return Ok(ExitCode::SUCCESS);
    }

    // git's `write_commit_graph()` refuses to write at all — not just to write
    // filters — when the version is outside the range it understands, and warns
    // rather than failing.
    if !(-1..=2).contains(&changed_paths_version) {
        eprintln!(
            "warning: attempting to write a commit-graph, but \
             'commitGraph.changedPathsVersion' ({changed_paths_version}) is not supported"
        );
        return Ok(ExitCode::SUCCESS);
    }

    // `--max-new-filters` beats `commitGraph.maxNewFilters`, which git reads as
    // the option's default in `git_commit_graph_write_config()`.
    if max_new_filters.is_none() {
        match repo.config_snapshot().try_integer("commitGraph.maxNewFilters") {
            Ok(v) => max_new_filters = v,
            Err(_) => {
                let raw = repo
                    .config_snapshot()
                    .string("commitGraph.maxNewFilters")
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                return Ok(bad_numeric_config(&raw, "commitgraph.maxnewfilters"));
            }
        }
    }

    // Whether this run carries filters. `--[no-]changed-paths` decides outright;
    // failing that `commitGraph.changedPaths` does; failing that git keeps
    // filters that the graph being replaced already had.
    //
    // Version 0 is "read no filters at all", and git enforces that by never
    // looking at `BIDX`/`BDAT` — so a graph whose filters are invisible cannot
    // pass them on, and rewriting it drops them.
    let existing = gix::commitgraph::at(objects.join("info")).ok();
    let inherited = if changed_paths_version == 0 {
        None
    } else {
        existing.as_ref().and_then(bloom_settings_of)
    };
    let write_changed_paths = match changed_paths {
        Some(explicit) => explicit,
        None => {
            repo.config_snapshot()
                .boolean("commitGraph.changedPaths")
                .unwrap_or(false)
                || inherited.is_some()
        }
    };

    // git only propagates the hash version from the old graph when nothing
    // asked for one, but always inherits the sizing so that the filters already
    // on disk and the ones about to be written can share a chunk.
    let mut bloom_settings = gix::commitgraph::bloom::Settings::default();
    if write_changed_paths {
        let mut hash_version = changed_paths_version;
        if let Some(old) = inherited {
            if hash_version == -1 {
                hash_version = i64::from(old.hash_version);
            }
            bloom_settings.bits_per_entry = old.bits_per_entry;
            bloom_settings.num_hashes = old.num_hashes;
        }
        // git's final clamp: only 2 means version 2, and -1 or 0 both settle on
        // 1, which is what an unconfigured repository writes.
        bloom_settings.hash_version = if hash_version == 2 { 2 } else { 1 };
    }

    let source = if reachable {
        Source::Reachable
    } else if stdin_packs {
        Source::StdinPacks
    } else if stdin_commits {
        Source::StdinCommits
    } else {
        Source::Packs
    };

    let mut seeds = match collect_seeds(&repo, &objects, source) {
        Ok(s) => s,
        Err(code) => return Ok(code),
    };

    if append {
        let info = objects.join("info");
        if let Ok(existing) = gix::commitgraph::at(&info) {
            seeds.extend(existing.iter_ids().map(|id| id.to_owned()));
        }
    }

    // git closes the set over parents: every parent must have a slot in the
    // file, since the format can only encode a parent as a position within it.
    let entries = match close_over_ancestors(&repo, seeds) {
        Ok(e) => e,
        Err(code) => return Ok(code),
    };

    // No commits means no file, and git leaves any existing one alone.
    if entries.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    // The filters must line up with the entries as they will be stored, and
    // `serialize` writes them in the order it is given, so they are computed
    // against the same slice.
    let filters = if write_changed_paths {
        // Filters can only be carried over from a graph this run was allowed to
        // read, which version 0 forbids.
        let reuse_from = inherited.is_some().then_some(existing.as_ref()).flatten();
        Some(compute_bloom_filters(
            &repo,
            &entries,
            &bloom_settings,
            reuse_from,
            max_new_filters,
            write_generation_data,
        ))
    } else {
        None
    };

    let bytes = match serialize(
        repo.object_hash(),
        &entries,
        write_generation_data,
        filters.as_ref().map(|f| (f.as_slice(), &bloom_settings)),
    ) {
        Ok(b) => b,
        Err(code) => return Ok(code),
    };
    if split {
        install_split(&objects, &bytes, repo.object_hash())?;
    } else {
        install(&objects, &bytes)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// The `BDAT` header of the graph being replaced, which decides whether this
/// run inherits filters and how they are sized.
fn bloom_settings_of(graph: &gix::commitgraph::Graph) -> Option<gix::commitgraph::bloom::Settings> {
    graph.bloom_filter_settings()
}

/// A changed-path Bloom filter for every entry, in the order `serialize` will
/// store them; git's `compute_bloom_filters()`.
///
/// A commit's filter holds the paths it changed against its first parent, or
/// against the empty tree when it is a root — git's `diff_tree_oid(NULL, ...)`.
/// Rename detection is off and the diff is recursive, which is what
/// `get_or_compute_bloom_filter()` sets up, because a filter is about paths and
/// not about what happened to them.
///
/// `max_new_filters` bounds how many filters one run may *compute*, and git
/// spends that budget in ascending generation order — oldest history first —
/// rather than in the order the entries happen to be stored. A commit left
/// unfiltered when the budget runs out is stored with a length of zero, which
/// is how the format says "this commit has no filter": a reader sees an empty
/// range and falls back to a real tree diff rather than trusting an answer.
fn compute_bloom_filters(
    repo: &gix::Repository,
    entries: &[Entry],
    settings: &gix::commitgraph::bloom::Settings,
    existing: Option<&gix::commitgraph::Graph>,
    max_new_filters: Option<i64>,
    write_generation_data: bool,
) -> Vec<gix::commitgraph::bloom::Filter> {
    use gix::commitgraph::bloom;

    // A negative bound is git's "no bound at all"; an absent one likewise,
    // since `commitGraph.maxNewFilters` and `--max-new-filters` share this.
    let budget = match max_new_filters {
        Some(n) if n >= 0 => Some(n as usize),
        _ => None,
    };
    // Filters already on disk can only be reused when they were built the same
    // way, which is exactly what git checks before propagating them.
    let reusable = existing.filter(|g| g.bloom_filter_settings().as_ref() == Some(settings));

    // git's `commit_gen_cmp`: lowest generation first, with the commit date
    // breaking ties. The generation it sorts on is the corrected commit date,
    // which only exists when generation data is being written; without it every
    // commit compares equal and the date alone decides.
    let mut order: Vec<usize> = (0..entries.len()).collect();
    let corrected = write_generation_data
        .then(|| parent_positions(entries).ok().map(|p| generations(entries, &p).1))
        .flatten();
    order.sort_by_key(|&i| (corrected.as_ref().map_or(0, |c| c[i]), entries[i].time));

    let mut computed = 0usize;
    let mut out = vec![bloom::Filter { data: Vec::new() }; entries.len()];
    for i in order {
        let entry = &entries[i];
        if let Some(filter) = reusable
            .and_then(|g| g.lookup(entry.id))
            .and_then(|pos| g_bloom_at(reusable, pos))
        {
            out[i] = filter;
            continue;
        }
        // Out of budget: leave the zero-length filter already in place, which
        // is what git writes for a commit it declined to compute.
        if budget.is_some_and(|max| computed >= max) {
            continue;
        }
        computed += 1;
        out[i] = match changed_paths_of(repo, entry) {
            Some(paths) => {
                let refs: Vec<&[u8]> = paths.iter().map(|p| p.as_slice()).collect();
                bloom::compute(&refs, settings)
            }
            // A diff we could not take is not a diff that found nothing, so the
            // filter has to admit every path rather than deny them all.
            None => bloom::Filter::truncated_large(),
        };
    }
    out
}

/// The filter stored for `pos`, if the graph has one there.
fn g_bloom_at(
    graph: Option<&gix::commitgraph::Graph>,
    pos: gix::commitgraph::Position,
) -> Option<gix::commitgraph::bloom::Filter> {
    graph?.bloom_filter_at(pos)
}

/// The paths one commit changed against its first parent, or `None` when the
/// diff cannot be taken.
///
/// git diffs against the first parent only, and against the empty tree for a
/// root commit, so a merge is described by what it changed along its first
/// line of history.
fn changed_paths_of(repo: &gix::Repository, entry: &Entry) -> Option<Vec<Vec<u8>>> {
    let new_tree = repo.find_tree(entry.tree).ok()?;
    let old_tree = match entry.parents.first() {
        Some(parent) => Some(
            repo.find_commit(*parent)
                .ok()?
                .tree()
                .ok()?,
        ),
        None => None,
    };
    let changes = repo
        .diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), gix::diff::Options::default())
        .ok()?;
    Some(changes.iter().map(|c| change_path(c).to_vec()).collect())
}

/// The post-image path of a change, which is the side git enters into the
/// filter. Rename detection is off, so both sides always agree.
fn change_path(change: &gix::object::tree::diff::ChangeDetached) -> &[u8] {
    use gix::object::tree::diff::ChangeDetached;
    match change {
        ChangeDetached::Addition { location, .. }
        | ChangeDetached::Deletion { location, .. }
        | ChangeDetached::Modification { location, .. }
        | ChangeDetached::Rewrite { location, .. } => location,
    }
}

/// The topological level stored in `CDAT` and the corrected commit date stored
/// in `GDA2`, for every entry; git's `compute_topological_levels()` and
/// `compute_generation_numbers()`.
///
/// Both are "one more than the largest among my parents", differing only in
/// what they count: the level counts commits, the corrected date counts from
/// each commit's own timestamp so that it never decreases along a parent edge.
/// An explicit stack keeps a deep history from overflowing the real one.
fn generations(entries: &[Entry], parents: &[Vec<u32>]) -> (Vec<u32>, Vec<u64>) {
    let n = entries.len();
    let mut level = vec![0u32; n];
    let mut corrected = vec![0u64; n];
    let mut stack: Vec<(usize, bool)> = Vec::new();
    for start in 0..n {
        if level[start] != 0 {
            continue;
        }
        stack.push((start, false));
        while let Some((idx, expanded)) = stack.pop() {
            if level[idx] != 0 {
                continue;
            }
            if expanded {
                let mut l = 1u32;
                let mut c = entries[idx].time;
                for &p in &parents[idx] {
                    let p = p as usize;
                    l = l.max(level[p].saturating_add(1));
                    c = c.max(corrected[p].saturating_add(1));
                }
                level[idx] = l.min(GENERATION_MAX);
                corrected[idx] = c;
            } else {
                stack.push((idx, true));
                for &p in &parents[idx] {
                    if level[p as usize] == 0 {
                        stack.push((p as usize, false));
                    }
                }
            }
        }
    }
    (level, corrected)
}

/// Parent positions per commit, or the id of a parent that has no slot.
///
/// The format encodes a parent as a position within the same file, so a parent
/// outside the entry set cannot be stored at all.
fn parent_positions(entries: &[Entry]) -> std::result::Result<Vec<Vec<u32>>, ObjectId> {
    let mut position: HashMap<ObjectId, u32> = HashMap::with_capacity(entries.len());
    for (i, e) in entries.iter().enumerate() {
        position.insert(e.id, i as u32);
    }
    entries
        .iter()
        .map(|e| {
            e.parents
                .iter()
                .map(|p| position.get(p).copied().ok_or(*p))
                .collect()
        })
        .collect()
}

/// A commit as it will be stored, before positions are assigned.
struct Entry {
    id: ObjectId,
    tree: ObjectId,
    parents: Vec<ObjectId>,
    /// Committer date in seconds since the epoch, clamped at zero as the format
    /// stores it unsigned.
    time: u64,
}

/// Resolve the starting commit ids for `source`.
fn collect_seeds(
    repo: &gix::Repository,
    objects: &Path,
    source: Source,
) -> std::result::Result<Vec<ObjectId>, ExitCode> {
    let mut seeds = Vec::new();
    match source {
        Source::Reachable => {
            // The platform is bound to a local: `all()` borrows from it.
            let platform = repo.references().map_err(|e| error_exit(&e.to_string()))?;
            let refs = platform.all().map_err(|e| error_exit(&e.to_string()))?;
            for reference in refs {
                let Ok(reference) = reference else { continue };
                // Peel through annotated tags: a tag object is not a graph entry,
                // the commit it names is.
                let Ok(id) = reference.into_fully_peeled_id() else {
                    continue;
                };
                let id = id.detach();
                if matches!(repo.try_find_header(id), Ok(Some(h)) if h.kind() == gix::objs::Kind::Commit)
                {
                    seeds.push(id);
                }
            }
        }
        Source::Packs => {
            for idx in open_pack_indices(objects, repo.object_hash()) {
                collect_pack_commits(repo, &idx, &mut seeds);
            }
        }
        Source::StdinPacks => {
            let pack_dir = objects.join("pack");
            for name in read_stdin_lines()? {
                let path = if Path::new(&name).is_absolute() {
                    PathBuf::from(&name)
                } else {
                    pack_dir.join(&name)
                };
                match pack::index::File::at(&path, repo.object_hash()) {
                    Ok(idx) => collect_pack_commits(repo, &idx, &mut seeds),
                    Err(_) => {
                        return Err(error_exit(&format!(
                            "error adding pack {}",
                            path.display()
                        )))
                    }
                }
            }
        }
        Source::StdinCommits => {
            for line in read_stdin_lines()? {
                let Ok(id) = ObjectId::from_hex(line.as_bytes()) else {
                    return Err(error_exit(&format!("unexpected non-hex object ID: {line}")));
                };
                match repo.try_find_header(id) {
                    Ok(Some(h)) => {
                        // git peels through tags and silently drops anything that
                        // is not a commit in the end.
                        if h.kind() == gix::objs::Kind::Commit {
                            seeds.push(id);
                        } else if h.kind() == gix::objs::Kind::Tag {
                            if let Ok(tag) = repo.find_object(id) {
                                if let Ok(peeled) = tag.peel_to_kind(gix::objs::Kind::Commit) {
                                    seeds.push(peeled.id);
                                }
                            }
                        }
                    }
                    _ => return Err(error_exit(&format!("invalid object: {id}"))),
                }
            }
        }
    }
    Ok(seeds)
}

fn read_stdin_lines() -> std::result::Result<Vec<String>, ExitCode> {
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        return Err(fatal("could not read from stdin"));
    }
    Ok(buf
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Every readable `.idx` in `<objects>/pack`, mirroring `prepare_packed_git_one()`.
fn open_pack_indices(objects: &Path, hash: gix::hash::Kind) -> Vec<pack::index::File> {
    let dir = objects.join("pack");
    let mut names: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "idx"))
            .collect(),
        Err(_) => return Vec::new(),
    };
    names.sort();
    names
        .into_iter()
        .filter_map(|p| pack::index::File::at(&p, hash).ok())
        .collect()
}

fn collect_pack_commits(repo: &gix::Repository, idx: &pack::index::File, out: &mut Vec<ObjectId>) {
    for entry in idx.iter() {
        if matches!(repo.try_find_header(entry.oid), Ok(Some(h)) if h.kind() == gix::objs::Kind::Commit)
        {
            out.push(entry.oid);
        }
    }
}

/// Walk from `seeds` through every ancestor, decoding each commit once.
///
/// The result is sorted by object id, which is the order the file stores.
fn close_over_ancestors(
    repo: &gix::Repository,
    seeds: Vec<ObjectId>,
) -> std::result::Result<Vec<Entry>, ExitCode> {
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut out: Vec<Entry> = Vec::new();
    let mut stack = seeds;

    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Ok(object) = repo.find_object(id) else {
            return Err(error_exit(&format!("invalid object: {id}")));
        };
        if object.kind != gix::objs::Kind::Commit {
            continue;
        }
        let Ok(commit) = gix::objs::CommitRef::from_bytes(&object.data, id.kind()) else {
            return Err(error_exit(&format!("unable to parse commit {id}")));
        };
        let time = commit
            .committer()
            .ok()
            .and_then(|sig| sig.time().ok())
            .map_or(0, |t| t.seconds.max(0) as u64);
        let parents: Vec<ObjectId> = commit.parents().collect();
        for p in &parents {
            if !seen.contains(p) {
                stack.push(*p);
            }
        }
        out.push(Entry {
            id,
            tree: commit.tree(),
            parents,
            time,
        });
    }

    out.sort_by_key(|a| a.id);
    Ok(out)
}

/// Serialize `entries` into the on-disk commit-graph representation.
fn serialize(
    hash: gix::hash::Kind,
    entries: &[Entry],
    write_generation_data: bool,
    bloom: Option<(
        &[gix::commitgraph::bloom::Filter],
        &gix::commitgraph::bloom::Settings,
    )>,
) -> std::result::Result<Vec<u8>, ExitCode> {
    let hash_len = hash.len_in_bytes();
    let n = entries.len();

    // A missing parent would be a bug in the ancestor closure, since the format
    // cannot encode an absent parent.
    let parents = parent_positions(entries).map_err(|missing| {
        error_exit(&format!("commit has parent {missing} outside the graph"))
    })?;

    let (level, corrected) = generations(entries, &parents);

    // --- chunks ---
    let mut oidf = Vec::with_capacity(FAN_LEN * 4);
    let mut counts = [0u32; FAN_LEN];
    for e in entries {
        counts[usize::from(e.id.as_bytes()[0])] += 1;
    }
    let mut running = 0u32;
    for c in counts {
        running += c;
        oidf.extend_from_slice(&running.to_be_bytes());
    }

    let mut oidl = Vec::with_capacity(n * hash_len);
    for e in entries {
        oidl.extend_from_slice(e.id.as_bytes());
    }

    let mut edge: Vec<u8> = Vec::new();
    let mut cdat = Vec::with_capacity(n * (hash_len + 16));
    for (idx, e) in entries.iter().enumerate() {
        cdat.extend_from_slice(e.tree.as_bytes());
        let ps = &parents[idx];
        let (p1, p2) = match ps.len() {
            0 => (NO_PARENT, NO_PARENT),
            1 => (ps[0], NO_PARENT),
            2 => (ps[0], ps[1]),
            _ => {
                let first_edge = (edge.len() / 4) as u32;
                for (k, &p) in ps[1..].iter().enumerate() {
                    let last = k + 2 == ps.len();
                    let word = if last { p | LAST_EXTENDED_EDGE_MASK } else { p };
                    edge.extend_from_slice(&word.to_be_bytes());
                }
                (ps[0], first_edge | EXTENDED_EDGES_MASK)
            }
        };
        cdat.extend_from_slice(&p1.to_be_bytes());
        cdat.extend_from_slice(&p2.to_be_bytes());
        let word = (u64::from(level[idx]) << 34) | (e.time & DATE_MASK);
        cdat.extend_from_slice(&word.to_be_bytes());
    }

    // The corrected-date chunks are only emitted under generation-number
    // version 2; version 1 keeps the topological level in `CDAT` and writes no
    // `GDA2`/`GDO2`, so their construction is skipped entirely.
    let (gda2, gdo2) = if write_generation_data {
        let mut gda2 = Vec::with_capacity(n * 4);
        let mut gdo2: Vec<u8> = Vec::new();
        for (idx, e) in entries.iter().enumerate() {
            let offset = corrected[idx].saturating_sub(e.time);
            if offset > OFFSET_MAX {
                let slot = (gdo2.len() / 8) as u32;
                gdo2.extend_from_slice(&offset.to_be_bytes());
                gda2.extend_from_slice(&(slot | OFFSET_OVERFLOW_MASK).to_be_bytes());
            } else {
                gda2.extend_from_slice(&(offset as u32).to_be_bytes());
            }
        }
        (Some(gda2), gdo2)
    } else {
        (None, Vec::new())
    };

    // The changed-path Bloom filters. `BIDX` stores each filter's *end* as a
    // running byte count, so filter `i` is found by subtracting `BIDX[i-1]`;
    // `BDAT` opens with the three settings a reader needs to reproduce a key,
    // then concatenates the filters in the same order. The format requires the
    // two chunks to appear together, so they are built together.
    let (bidx, bdat) = match bloom {
        Some((filters, settings)) => {
            let mut bidx = Vec::with_capacity(n * 4);
            let mut bdat = Vec::with_capacity(12 + n);
            bdat.extend_from_slice(&settings.hash_version.to_be_bytes());
            bdat.extend_from_slice(&settings.num_hashes.to_be_bytes());
            bdat.extend_from_slice(&settings.bits_per_entry.to_be_bytes());
            let mut running = 0u32;
            for filter in filters {
                running += filter.data.len() as u32;
                bidx.extend_from_slice(&running.to_be_bytes());
                bdat.extend_from_slice(&filter.data);
            }
            (Some(bidx), Some(bdat))
        }
        None => (None, None),
    };

    // git's chunk order: OIDF, OIDL, CDAT, GDA2, GDO2, EDGE, BIDX, BDAT.
    let mut chunks: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"OIDF", oidf),
        (b"OIDL", oidl),
        (b"CDAT", cdat),
    ];
    if let Some(gda2) = gda2 {
        chunks.push((b"GDA2", gda2));
    }
    if !gdo2.is_empty() {
        chunks.push((b"GDO2", gdo2));
    }
    if !edge.is_empty() {
        chunks.push((b"EDGE", edge));
    }
    if let (Some(bidx), Some(bdat)) = (bidx, bdat) {
        chunks.push((b"BIDX", bidx));
        chunks.push((b"BDAT", bdat));
    }

    // --- header, chunk lookup, data, trailer ---
    let mut out = Vec::new();
    out.extend_from_slice(SIGNATURE);
    out.push(1); // file version
    out.push(hash as u8); // hash version: 1 = SHA-1, 2 = SHA-256
    out.push(chunks.len() as u8);
    out.push(0); // base graph count: never split here

    let mut offset = (HEADER_LEN + (chunks.len() + 1) * CHUNK_LOOKUP_ENTRY_LEN) as u64;
    for (id, data) in &chunks {
        out.extend_from_slice(*id);
        out.extend_from_slice(&offset.to_be_bytes());
        offset += data.len() as u64;
    }
    out.extend_from_slice(&[0u8; 4]); // terminating chunk id
    out.extend_from_slice(&offset.to_be_bytes());

    for (_, data) in &chunks {
        out.extend_from_slice(data);
    }

    let mut hasher = gix::hash::hasher(hash);
    hasher.update(&out);
    match hasher.try_finalize() {
        Ok(id) => out.extend_from_slice(id.as_bytes()),
        Err(e) => return Err(error_exit(&e.to_string())),
    }
    Ok(out)
}

/// Put `bytes` at `<objects>/info/commit-graph`, replacing any split chain.
///
/// Written to a sibling temporary and renamed, so a reader never sees a partial
/// file, and left read-only as git leaves it.
fn install(objects: &Path, bytes: &[u8]) -> Result<()> {
    let info = objects.join("info");
    std::fs::create_dir_all(&info)?;

    let tmp = info.join(format!("commit-graph.tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o444));
    }
    let target = info.join("commit-graph");
    // The old file is read-only, which some filesystems refuse to rename over.
    let _ = std::fs::remove_file(&target);
    std::fs::rename(&tmp, &target)?;

    // A non-split write supersedes the chain; git removes its files and leaves
    // the (now empty) directory behind.
    let chain_dir = info.join("commit-graphs");
    if let Ok(entries) = std::fs::read_dir(&chain_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == "commit-graph-chain" || (name.starts_with("graph-") && name.ends_with(".graph"))
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(())
}

/// Install `bytes` as the one layer of a new commit-graph chain.
///
/// A layer lives at `<objects>/info/commit-graphs/graph-<checksum>.graph`, named
/// after its own trailing hash, and `commit-graph-chain` lists the chain base
/// first, tip last — one hash per line. Both are left read-only, as git leaves
/// them. Only the first layer is written here; see the refusal in [`write`] for
/// what a chain that already exists would need.
fn install_split(objects: &Path, bytes: &[u8], hash: gix::hash::Kind) -> Result<()> {
    let dir = objects.join("info").join("commit-graphs");
    std::fs::create_dir_all(&dir)?;

    let Ok(checksum) = gix::hash::ObjectId::try_from(&bytes[bytes.len() - hash.len_in_bytes()..])
    else {
        bail!("commit-graph trailer is not a valid object id");
    };
    let pid = std::process::id();
    for (name, body) in [
        (format!("graph-{checksum}.graph"), bytes.to_vec()),
        ("commit-graph-chain".to_string(), format!("{checksum}\n").into_bytes()),
    ] {
        let tmp = dir.join(format!("{name}.tmp-{pid}"));
        std::fs::write(&tmp, &body)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o444));
        }
        let target = dir.join(&name);
        // The old file is read-only, which some filesystems refuse to rename over.
        let _ = std::fs::remove_file(&target);
        std::fs::rename(&tmp, &target)?;
    }
    Ok(())
}

// --- object directory ------------------------------------------------------

/// Resolve `--object-dir` against the repository, as git's `odb_find_alternate`
/// does: the value must name the repository's own object database or one of its
/// alternates, otherwise git refuses with exit 128.
fn object_directory(
    repo: &gix::Repository,
    dir: Option<&str>,
) -> std::result::Result<PathBuf, ExitCode> {
    let repo_objects = repo.objects.store_ref().path().to_owned();
    let Some(dir) = dir else {
        return Ok(repo_objects);
    };
    if dir.is_empty() {
        return Err(fatal("The empty string is not a valid path"));
    }
    match resolve_object_dir(dir, &repo_objects) {
        Some(p) => Ok(p),
        None => Err(fatal(&format!(
            "could not find object directory matching {dir}"
        ))),
    }
}

/// Map a user-supplied `--object-dir` onto a known object directory.
///
/// git rejects a directory that is neither the repository's own object database
/// nor one of its alternates; mirror that by comparing canonicalised paths
/// against the main objects dir and every entry of `info/alternates`.
fn resolve_object_dir(dir: &str, repo_objects: &Path) -> Option<PathBuf> {
    let want = std::fs::canonicalize(dir).ok()?;
    let mut candidates = vec![repo_objects.to_path_buf()];

    if let Ok(alternates) = std::fs::read_to_string(repo_objects.join("info").join("alternates")) {
        for line in alternates.lines().filter(|l| !l.trim().is_empty()) {
            let p = PathBuf::from(line);
            candidates.push(if p.is_absolute() {
                p
            } else {
                repo_objects.join(p)
            });
        }
    }

    candidates
        .into_iter()
        .find(|c| std::fs::canonicalize(c).is_ok_and(|c| c == want))
        .map(|_| want)
}
