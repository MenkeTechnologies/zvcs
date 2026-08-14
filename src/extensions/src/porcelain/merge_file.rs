//! `git merge-file` — three-way file merge, a work-alike of RCS `merge`.
//!
//! Incorporates the changes that lead from `<base>` to `<other>` into
//! `<current>`. The result replaces `<current>` in place, or goes to stdout
//! with `-p`. The process exit code is the number of conflicts (capped at
//! 127), `0` for a clean merge, `255` for an input error and `129` for a
//! usage error — matching stock git.
//!
//! Covered flags: `-p`/`--stdout`, `-q`/`--quiet`, `-L <label>` (up to three),
//! `--diff3`, `--zdiff3`, `--ours`, `--theirs`, `--union`, `--marker-size=<n>`,
//! `--diff-algorithm=<myers|minimal|patience|histogram>`, `--object-id`, `--`
//! and the `--no-` negations parse-options accepts, plus the
//! `merge.conflictStyle` config default. Long options may be abbreviated to any
//! unique prefix, as [`super::resolve_long`] resolves them.
//!
//! `merge.conflictStyle` is read and validated the way git's `git_xmerge_config`
//! does: it runs before option parsing, so an unknown value (`Diff3`, `zealous`,
//! empty, …) is fatal (exit 128) even when a `--diff3`/`--zdiff3` flag would
//! have overridden it, when `-h` is present, or when the operands are wrong. The
//! one deviation is the `fatal:` line for a file-backed value — `gix_config`
//! does not expose the config line number, so it reads
//! `bad config variable 'merge.conflictstyle' in file '<path>'` where git
//! appends ` at line <n>`. As before, the value is consulted only inside a
//! repository; a global-only setting outside one still resolves to `merge`.
//!
//! `patience` is accepted as a *value* exactly as git accepts it, so command
//! lines that name it and then fail for an unrelated reason (a bad operand
//! count, a later bad `--diff-algorithm`, a missing file) fail identically to
//! git. Only a merge that would actually be computed with patience is refused:
//! the vendored `imara-diff` has no patience implementation, and silently
//! substituting another algorithm would change the merge result.
//!
//! The three-way line merge itself is the vendored `gix-merge` crate's built-in
//! text driver (`blob/builtin_driver/text`), which is a port of git's
//! `xdiff/xmerge.c`. This command is the one caller that runs it at
//! `XDL_MERGE_ZEALOUS_ALNUM`; `builtin/merge-file.c` picks that level while
//! `merge-ll.c` — everything reached through `git merge` — picks
//! `XDL_MERGE_ZEALOUS`, so the two really can disagree on the same inputs.

use anyhow::Result;
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::config::Source;
use gix::diff::blob::{Algorithm, InternedInput};
use gix::merge::blob::builtin_driver::text::{Conflict, ConflictStyle, Labels, Level, Merge, Rendering};

use super::{Arg, LongOpt};

/// `cmd_merge_file()`'s `struct option options[]` (builtin/merge-file.c), in
/// table order, as [`super::resolve_long`] reads it. `--diff-algorithm` is the
/// only `PARSE_OPT_NONEG` entry; `-L`, `-p` and `-q` reach the same slots by
/// their short names.
const LONG_OPTS: &[LongOpt] = &[
    LongOpt { name: "stdout", neg: true, arg: Arg::None },
    LongOpt { name: "object-id", neg: true, arg: Arg::None },
    LongOpt { name: "diff3", neg: true, arg: Arg::None },
    LongOpt { name: "zdiff3", neg: true, arg: Arg::None },
    LongOpt { name: "ours", neg: true, arg: Arg::None },
    LongOpt { name: "theirs", neg: true, arg: Arg::None },
    LongOpt { name: "union", neg: true, arg: Arg::None },
    LongOpt { name: "diff-algorithm", neg: false, arg: Arg::Required },
    LongOpt { name: "marker-size", neg: true, arg: Arg::Required },
    LongOpt { name: "quiet", neg: true, arg: Arg::None },
];

const USAGE: &str = "\
usage: git merge-file [<options>] [-L <name1> [-L <orig> [-L <name2>]]] <file1> <orig-file> <file2>

    -p, --[no-]stdout     send results to standard output
    --[no-]object-id      use object IDs instead of filenames
    --[no-]diff3          use a diff3 based merge
    --[no-]zdiff3         use a zealous diff3 based merge
    --[no-]ours           for conflicts, use our version
    --[no-]theirs         for conflicts, use their version
    --[no-]union          for conflicts, use a union version
    --diff-algorithm <algorithm>
                          choose a diff algorithm
    --[no-]marker-size <n>
                          for conflicts, use this marker size
    -q, --[no-]quiet      do not warn about conflicts
    -L <name>             set labels for file1/orig-file/file2

";


/// `git merge-file` — see the module docs for the covered surface.
pub fn merge_file(args: &[String]) -> Result<ExitCode> {
    let argv = args;

    // git reads and validates config at the very start of `cmd_merge_file`,
    // before option parsing, `-h`, or the operand count check, so an invalid
    // `merge.conflictStyle` is fatal (exit 128) regardless of the command line.
    let repo = gix::discover(".").ok();
    let config_style = match conflict_style_config(repo.as_ref()) {
        Ok(style) => style,
        Err(code) => return Ok(code),
    };

    let mut to_stdout = false;
    let mut quiet = false;
    let mut object_id = false;
    // `--ours`/`--theirs`/`--union` all write the same slot in git, so the
    // last one on the command line wins; `--no-<x>` clears it.
    let mut favor: Option<Conflict> = None;
    // Same for the two style flags.
    let mut style: Option<ConflictStyle> = None;
    // git stores this in a C `int` and only clamps it (to 7) at merge time.
    let mut marker_size: i64 = 7;
    let mut algorithm = DiffAlgorithm::Imara(Algorithm::Myers);
    let mut label_args: Vec<String> = Vec::new();
    let mut operands: Vec<&str> = Vec::new();
    let mut no_more_opts = false;

    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        i += 1;

        if no_more_opts || arg == "-" || !arg.starts_with('-') {
            operands.push(arg);
            continue;
        }
        if arg == "--" {
            no_more_opts = true;
            continue;
        }

        let resolved = match super::canonical_long(arg, LONG_OPTS) {
            super::Long::Name(name) => name,
            super::Long::Ambiguous(first, second) => {
                return Ok(super::ambiguous_option(arg, &first, &second, USAGE))
            }
        };
        let arg = resolved.as_ref();

        if let Some(long) = arg.strip_prefix("--") {
            let (name, value) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            // Boolean options reject `--opt=v` the way parse-options does.
            if value.is_some() && !matches!(name, "marker-size" | "diff-algorithm") {
                return Ok(option_error(&format!("option `{name}' takes no value")));
            }
            match name {
                "stdout" => to_stdout = true,
                "no-stdout" => to_stdout = false,
                "quiet" => quiet = true,
                "no-quiet" => quiet = false,
                "object-id" => object_id = true,
                "no-object-id" => object_id = false,
                "diff3" => style = Some(ConflictStyle::Diff3),
                "zdiff3" => style = Some(ConflictStyle::ZealousDiff3),
                "no-diff3" | "no-zdiff3" => style = None,
                "ours" => favor = Some(Conflict::ResolveWithOurs),
                "theirs" => favor = Some(Conflict::ResolveWithTheirs),
                "union" => favor = Some(Conflict::ResolveWithUnion),
                "no-ours" | "no-theirs" | "no-union" => favor = None,
                "marker-size" => {
                    let Some(v) = value.or_else(|| next_value(argv, &mut i)) else {
                        return Ok(option_error("option `marker-size' requires a value"));
                    };
                    match parse_int_arg(v) {
                        Ok(Some(n)) => marker_size = n,
                        Ok(None) => return Ok(option_error(&format!(
                            "value {v} for option `marker-size' not in range [-2147483648,2147483647]"
                        ))),
                        Err(()) => return Ok(option_error(
                            "option `marker-size' expects an integer value with an optional k/m/g suffix",
                        )),
                    }
                }
                "no-marker-size" => marker_size = 0,
                "diff-algorithm" => {
                    let Some(v) = value.or_else(|| next_value(argv, &mut i)) else {
                        return Ok(option_error("option `diff-algorithm' requires a value"));
                    };
                    algorithm = match v {
                        "myers" | "default" => DiffAlgorithm::Imara(Algorithm::Myers),
                        "minimal" => DiffAlgorithm::Imara(Algorithm::MyersMinimal),
                        "histogram" => DiffAlgorithm::Imara(Algorithm::Histogram),
                        "patience" => DiffAlgorithm::Patience,
                        _ => {
                            return Ok(option_error(
                                "option diff-algorithm accepts \"myers\", \"minimal\", \"patience\" and \"histogram\"",
                            ))
                        }
                    };
                }
                other => return Ok(usage_error(&format!("unknown option `{other}'"))),
            }
            continue;
        }

        // Short options, grouped left to right. `-L` consumes the rest of the
        // token as its value, or the next argument if the token ends there.
        let chars = arg[1..].char_indices();
        for (at, c) in chars {
            match c {
                'p' => to_stdout = true,
                'q' => quiet = true,
                'h' => {
                    print!("{USAGE}");
                    return Ok(ExitCode::from(129));
                }
                'L' => {
                    let rest = &arg[1 + at + c.len_utf8()..];
                    let value = if rest.is_empty() {
                        match next_value(argv, &mut i) {
                            Some(v) => v.to_string(),
                            None => return Ok(option_error("switch `L' requires a value")),
                        }
                    } else {
                        rest.to_string()
                    };
                    label_args.push(value);
                    break;
                }
                _ => return Ok(usage_error(&format!("unknown switch `{c}'"))),
            }
        }
    }

    if operands.len() != 3 || label_args.len() > 3 {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(129));
    }

    // `--object-id` additionally requires the repository the config came from.
    if object_id && repo.is_none() {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    }

    // A CLI style flag (`--diff3`/`--zdiff3`) overrides the validated
    // `merge.conflictStyle` default.
    let style = style.unwrap_or(config_style);
    // git's `xdl_merge` substitutes the default for any non-positive size, and
    // imposes no upper bound — it is passed separately below because
    // `Conflict::Keep` can only hold a `NonZeroU8`.
    let marker_size = if marker_size <= 0 { 7 } else { marker_size as usize };
    let conflict = favor.unwrap_or(Conflict::Keep {
        style,
        marker_size: Conflict::DEFAULT_MARKER_SIZE.try_into().expect("non-zero"),
    });

    // Read the three operands, in the order git reports errors for them.
    let mut contents: Vec<Vec<u8>> = Vec::with_capacity(3);
    for operand in &operands {
        let content = if object_id {
            match read_blob(repo.as_ref().expect("checked above"), operand, quiet) {
                Ok(content) => content,
                Err(code) => return Ok(code),
            }
        } else {
            match read_file(operand, quiet) {
                Ok(content) => content,
                Err(code) => return Ok(code),
            }
        };
        // git's `buffer_is_binary` only sniffs the first 8000 bytes for NUL, so
        // content with a NUL past that point really is merged as text.
        if content[..content.len().min(8000)].contains(&0) {
            if !quiet {
                eprintln!("error: Cannot merge binary files: {operand}");
            }
            return Ok(ExitCode::from(255));
        }
        contents.push(content);
    }

    // Unlabelled operands annotate conflicts with the spelling used on the CLI.
    let labels = Labels {
        current: Some(label_args.first().map_or(operands[0], String::as_str).as_bytes().as_bstr()),
        ancestor: Some(label_args.get(1).map_or(operands[1], String::as_str).as_bytes().as_bstr()),
        other: Some(label_args.get(2).map_or(operands[2], String::as_str).as_bytes().as_bstr()),
    };

    // Every failure git reports before it diffs has been reproduced by now, so
    // this is the first point at which the missing algorithm actually matters.
    let algorithm = match algorithm {
        DiffAlgorithm::Imara(algorithm) => algorithm,
        DiffAlgorithm::Patience => anyhow::bail!(
            "--diff-algorithm=patience is unsupported (ported: myers, minimal, histogram)"
        ),
    };

    // `builtin/merge-file.c` sets `xmp.level = XDL_MERGE_ZEALOUS_ALNUM`, one level
    // above what `git merge` runs at. `xmp.style` is set independently of
    // `xmp.favor`, so `--zdiff3 --union` still refines regions the zdiff3 way.
    let mut merged = Vec::new();
    let mut input = InternedInput::default();
    let merge = Merge::new(&mut input, &contents[0], &contents[1], &contents[2], algorithm);
    let (_resolution, conflicts) = merge.run_with(
        &mut merged,
        labels,
        Rendering {
            conflict,
            style: Some(style),
            level: Level::ZealousAlnum,
            marker_size: Some(marker_size),
        },
    );

    if to_stdout {
        std::io::stdout().write_all(&merged)?;
    } else if object_id {
        let id = repo.as_ref().expect("checked above").write_blob(&merged)?;
        println!("{}", id.detach().to_hex());
    } else {
        std::fs::write(operands[0], &merged)?;
    }

    Ok(ExitCode::from(conflicts.min(127) as u8))
}

/// Consume the next argument as an option value, advancing the cursor.
fn next_value<'a>(argv: &'a [String], i: &mut usize) -> Option<&'a str> {
    let value = argv.get(*i)?.as_str();
    *i += 1;
    Some(value)
}

/// Which diff implementation the merge should run on.
///
/// `patience` is a value git accepts, so it has to survive option parsing even
/// though nothing can execute it; see the module docs.
#[derive(Copy, Clone, Eq, PartialEq)]
enum DiffAlgorithm {
    Imara(Algorithm),
    Patience,
}

/// Report an unrecognised option the way parse-options does: reason *and*
/// usage on stderr, exit 129.
fn usage_error(reason: &str) -> ExitCode {
    eprintln!("error: {reason}");
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// Report a bad option *value*. parse-options prints no usage block for these
/// — only the reason — and still exits 129.
fn option_error(reason: &str) -> ExitCode {
    eprintln!("error: {reason}");
    ExitCode::from(129)
}

/// Port of git's `git_parse_signed` as `OPT_INTEGER` uses it: `strtoimax` with
/// base detection (`0x` hex, leading `0` octal), then one optional `k`/`m`/`g`
/// suffix, then end of string.
///
/// `Err(())` is malformed input, `Ok(None)` a value outside a C `int`.
fn parse_int_arg(text: &str) -> std::result::Result<Option<i64>, ()> {
    let bytes = text.as_bytes();
    let mut at = 0;
    while matches!(bytes.get(at), Some(c) if c.is_ascii_whitespace()) {
        at += 1;
    }
    let negative = match bytes.get(at) {
        Some(b'-') => {
            at += 1;
            true
        }
        Some(b'+') => {
            at += 1;
            false
        }
        _ => false,
    };

    let radix = if bytes[at..].starts_with(b"0x") || bytes[at..].starts_with(b"0X") {
        at += 2;
        16
    } else if bytes.get(at) == Some(&b'0') {
        8
    } else {
        10
    };

    let digits_start = at;
    let mut value: i64 = 0;
    while let Some(digit) = bytes.get(at).and_then(|c| (*c as char).to_digit(radix)) {
        let Some(next) = value
            .checked_mul(i64::from(radix))
            .and_then(|v| v.checked_add(i64::from(digit)))
        else {
            return Ok(None);
        };
        value = next;
        at += 1;
    }
    if at == digits_start {
        return Err(());
    }

    let factor: i64 = match bytes.get(at) {
        Some(b'k' | b'K') => 1024,
        Some(b'm' | b'M') => 1024 * 1024,
        Some(b'g' | b'G') => 1024 * 1024 * 1024,
        _ => 1,
    };
    if factor != 1 {
        at += 1;
    }
    if at != bytes.len() {
        return Err(());
    }

    let Some(value) = value.checked_mul(factor) else {
        return Ok(None);
    };
    let value = if negative { -value } else { value };
    if value < i64::from(i32::MIN) || value > i64::from(i32::MAX) {
        return Ok(None);
    }
    Ok(Some(value))
}

/// The `merge.conflictStyle` default, validated the way git's
/// `git_xmerge_config` validates it: every occurrence in the merged
/// configuration is checked, an unknown value is fatal (exit 128), the last
/// valid value wins, and an absent value resolves to `merge`. Value matching is
/// case-sensitive (`Diff3` is rejected), matching git.
///
/// Config is read only when inside a repository — the same scope the previous
/// default honored — so a global-only `merge.conflictStyle` outside a repo
/// keeps resolving to `merge` rather than being consulted (or validated) here.
pub(super) fn conflict_style_config(
    repo: Option<&gix::Repository>,
) -> std::result::Result<ConflictStyle, ExitCode> {
    let Some(repo) = repo else {
        return Ok(ConflictStyle::Merge);
    };
    let snapshot = repo.config_snapshot();
    let config = snapshot.plumbing();

    let mut chosen = ConflictStyle::Merge;
    // `sections()` yields in merged configuration order, so a later value wins.
    for section in config.sections() {
        let header = section.header();
        if header.subsection_name().is_some()
            || !header.name().to_string().eq_ignore_ascii_case("merge")
        {
            continue;
        }
        for value in section.body().values("conflictStyle") {
            chosen = match value.as_slice() {
                b"merge" => ConflictStyle::Merge,
                b"diff3" => ConflictStyle::Diff3,
                b"zdiff3" => ConflictStyle::ZealousDiff3,
                other => {
                    let value = String::from_utf8_lossy(other);
                    return Err(style_fatal(&value, section.meta()));
                }
            };
        }
    }
    Ok(chosen)
}

/// Report an unknown `merge.conflictStyle` value the way git's
/// `git_xmerge_config` does — the `error:` reason, then a `fatal:` line naming
/// where the value came from — and yield exit 128.
///
/// gix does not expose the config line number, so a file-backed section omits
/// git's trailing ` at line <n>`; the command-line/environment forms match.
fn style_fatal(value: &str, meta: &gix::config::file::Metadata) -> ExitCode {
    eprintln!("error: unknown style '{value}' given for 'merge.conflictstyle'");
    let origin = match meta.source {
        Source::Cli | Source::Env => {
            "unable to parse 'merge.conflictstyle' from command-line config".to_string()
        }
        _ => match &meta.path {
            Some(path) => {
                format!("bad config variable 'merge.conflictstyle' in file '{}'", path.display())
            }
            None => "bad config variable 'merge.conflictstyle'".to_string(),
        },
    };
    eprintln!("fatal: {origin}");
    ExitCode::from(128)
}

/// Read one operand from the worktree, mirroring git's `stat` then `open`
/// error messages. Returns the exit code to use on failure.
fn read_file(path: &str, quiet: bool) -> std::result::Result<Vec<u8>, ExitCode> {
    if let Err(err) = std::fs::metadata(path) {
        if !quiet {
            eprintln!("error: Could not stat {path}: {}", errno_text(&err));
        }
        return Err(ExitCode::from(255));
    }
    std::fs::read(path).map_err(|err| {
        if !quiet {
            eprintln!("error: Could not open {path}: {}", errno_text(&err));
        }
        ExitCode::from(255)
    })
}

/// The bare `strerror` text, without Rust's trailing ` (os error N)`.
fn errno_text(err: &std::io::Error) -> String {
    let text = err.to_string();
    match text.split_once(" (os error ") {
        Some((head, _)) => head.to_string(),
        None => text,
    }
}

/// Read one operand as a blob for `--object-id`.
///
/// A raw hex object id is looked up directly, so naming a non-blob reproduces
/// git's `unable to read blob object` failure; any other revision spec must
/// resolve to a blob, as git's blob-context lookup requires.
fn read_blob(
    repo: &gix::Repository,
    spec: &str,
    quiet: bool,
) -> std::result::Result<Vec<u8>, ExitCode> {
    let missing = || {
        if !quiet {
            eprintln!("error: object '{spec}' does not exist");
        }
        ExitCode::from(255)
    };
    let is_hex = spec.len() >= 4 && spec.chars().all(|c| c.is_ascii_hexdigit());

    let object = match repo.rev_parse_single(spec) {
        Ok(id) => id.object().map_err(|_| missing())?,
        Err(_) => return Err(missing()),
    };
    if object.kind != gix::object::Kind::Blob {
        if is_hex {
            if !quiet {
                eprintln!("fatal: unable to read blob object {}", object.id.to_hex());
            }
            return Err(ExitCode::from(128));
        }
        return Err(missing());
    }
    // `gix::Object` implements Drop, so its buffer cannot be moved out.
    Ok(object.data.clone())
}
