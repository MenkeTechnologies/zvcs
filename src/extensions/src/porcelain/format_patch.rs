//! `git format-patch` — render commits as mbox-style e-mail patches.
//!
//! Each message is built the way stock git builds it: the fixed
//! `From <oid> Mon Sep 17 00:00:00 2001` magic line, the `From:`/`Date:`/
//! `Subject:` headers (RFC2822 date, RFC2047 q-encoded headers when needed,
//! RFC822 wrapping), the commit body, the three-dash separator, the
//! `--stat`+`--summary` block at git's 72-column mail width, the patch itself,
//! and the `-- \n<version>\n\n` signature.
//!
//! The diffstat is a faithful port of git's `show_stats()` (diff.c) including
//! `scale_linear()` graph scaling and the name-column ellipsis, and the summary
//! lines are a port of `diff_summary()`. The patch body reuses the same default
//! diff settings the rest of this crate uses: Myers with the indent (slider)
//! heuristic, three lines of context, `@@`-header function context, and the
//! `\ No newline at end of file` marker.
//!
//! Covered:
//!   * revision selection — `<since>` (implicit `<since>..HEAD`), `<a>..<b>`,
//!     `<a>...<b>`, `^<rev>`, `-<n>`, `--root`; merges are excluded as git does.
//!   * revision errors — git's own `fatal: ambiguous argument …` / `bad object`
//!     / `bad revision` / `Invalid revision range` text on stderr with exit 128,
//!     and a positional that names an existing path is a pathspec, not an error.
//!   * output — file-per-patch (default, names printed to stdout) or `--stdout`.
//!   * flags — `--stdout`, `-o`/`--output-directory`, `-<n>`/`--max-count`,
//!     `--skip`, `--reverse`, `--min-parents`/`--max-parents`/`--no-merges`,
//!     `-n`/`--numbered`, `-N`/`--no-numbered`, `--start-number`,
//!     `--numbered-files`, `--suffix`, `--subject-prefix`, `--rfc`,
//!     `-v`/`--reroll-count`, `--signature`/`--no-signature`, `--zero-commit`,
//!     `-p`/`--no-stat`, `--root`, `-q`/`--quiet`, `--filename-max-length`,
//!     `--cover-letter`, `-U`/`--unified`, `-a`/`--text`, `--minimal`,
//!     `--histogram`, `--diff-algorithm=myers|minimal|histogram`.
//!
//! Flags git accepts that are *not* ported are recorded during parsing and
//! rejected only once it is clear a patch would actually be emitted. That
//! ordering is deliberate: git validates option values first (exit 129), then
//! resolves revisions (exit 128), and only then renders. Rejecting early would
//! report a porting gap for an invocation git itself refuses, so the two
//! implementations would disagree about *why* they failed. Nothing is silently
//! ignored: if the commit list is non-empty the unported flag is still fatal.
//!
//! Not covered — these `bail!` rather than emit output that would diverge:
//!   * binary files, unless `-a`/`--text` is given. format-patch implies
//!     `--binary`, i.e. a base85 `GIT binary patch` payload; that encoder is not
//!     ported.
//!   * pathspec-limited output. A pathspec is parsed and honoured to the extent
//!     that it never becomes a bogus revision error, but limiting the walk and
//!     the patch to it is not ported, so a pathspec that reaches a non-empty
//!     commit list is fatal.
//!   * threading, MIME attach/inline, signoff, `--keep-subject`, extra headers
//!     (`--to`/`--cc`/`--in-reply-to`), notes, interdiff and range-diff,
//!     `--ignore-if-in-upstream`, the alternate diffstat formats (`--numstat`,
//!     `--shortstat`, `--dirstat`, `--compact-summary`, `--stat=<width>`),
//!     whitespace-insensitive diffing, `-I<regex>` (no regex engine is vendored),
//!     patience diff (imara-diff has Myers, MyersMinimal and Histogram only),
//!     and rename/copy detection.
//!
//! Known deviation, stated rather than hidden: rename/copy detection is
//! disabled (as elsewhere in this crate), so a commit that renames a file
//! renders as a delete plus an add instead of git's `rename from`/`rename to`
//! and `old => new` stat line. Column widths are computed in Unicode scalar
//! values, so East-Asian wide characters in a path measure 1 rather than 2. The
//! cover letter's shortlog does not wrap long subjects at 76 columns.

use anyhow::{anyhow, bail, Result};
use std::io::Write;
use std::process::ExitCode;

use gix::bstr::{BStr, ByteSlice};
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, InternedInput, UnifiedDiff};
use gix::hash::ObjectId;
use gix::object::tree::diff::ChangeDetached;
use gix::prelude::ObjectIdExt;
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;

/// The version reported in the trailing `-- \n<version>\n` signature. Stock git
/// emits its own `git_version_string` here, so this constant is what makes the
/// signature line comparable; override per-invocation with `--signature=<s>`,
/// `--no-signature`, or the `format.signature` config key.
const SIGNATURE_VERSION: &str = "2.55.0";

/// git's `MAIL_DEFAULT_WRAP` — the diffstat width used by format-patch.
const MAIL_DEFAULT_WRAP: i64 = 72;

/// git's `FORMAT_PATCH_NAME_MAX_DEFAULT`.
const NAME_MAX_DEFAULT: usize = 64;

/// Header wrap column for `From:`/`Subject:` (RFC2822 §2.1.1).
const HEADER_MAX_LENGTH: i64 = 78;

/// The charset name used for RFC2047 encoding and the 8-bit MIME header.
const ENCODING: &str = "UTF-8";

/// git's placeholder subject and body in a generated cover letter.
const COVER_SUBJECT: &str = "*** SUBJECT HERE ***";
const COVER_BLURB: &str = "*** BLURB HERE ***";

struct Opts {
    // Output shape.
    to_stdout: bool,
    outdir: Option<String>,
    numbered: Option<bool>,
    start_number: usize,
    numbered_files: bool,
    suffix: String,
    subject_prefix: String,
    reroll: Option<String>,
    signature: String,
    zero_commit: bool,
    no_stat: bool,
    quiet: bool,
    name_max: usize,
    cover_letter: bool,

    // Revision selection.
    root: bool,
    max_count: Option<usize>,
    skip: usize,
    reverse: bool,
    min_parents: usize,
    max_parents: Option<usize>,
    revs: Vec<String>,
    paths: Vec<String>,

    // Diff rendering.
    context: u32,
    algorithm: Algorithm,
    text: bool,

    /// Flags git accepts that this module has not ported, in the spelling the
    /// caller used. Reported only when a patch would actually be emitted.
    deferred: Vec<String>,
}

pub fn format_patch(args: &[String]) -> Result<ExitCode> {
    let repo = gix::discover(".")?;

    let mut opts = match parse(&repo, args)? {
        Parsed::Ready(opts) => *opts,
        Parsed::Exit(code) => return Ok(code),
    };

    // git: "Make sure 0000-$sub.patch gives non-negative length for $sub".
    let floor = "0000-".len() + opts.suffix.len();
    if opts.name_max <= floor {
        opts.name_max = floor;
    }
    if let Some(r) = &opts.reroll {
        opts.subject_prefix.push_str(&format!(" v{r}"));
    }

    let (commits, paths) = match select_commits(&repo, &opts)? {
        Selected::Commits { commits, paths } => (commits, paths),
        Selected::Exit(code) => return Ok(code),
    };
    if commits.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    // Everything below emits bytes, so an unported flag can no longer be
    // deferred: it would change what those bytes are.
    if let Some(flag) = opts.deferred.first() {
        bail!("unsupported flag {flag:?}");
    }
    if let Some(path) = paths.first() {
        bail!("pathspec-limited format-patch is not supported (got {path:?})");
    }

    // Auto-numbering kicks in for a series; -n/-N override it. A cover letter
    // always numbers, since it is itself patch 0 of the series.
    let total = commits.len();
    let numbered = opts.numbered.unwrap_or(total > 1 || opts.cover_letter);
    let printed_total = if numbered {
        total + opts.start_number - 1
    } else {
        0
    };

    let mut stdout = std::io::stdout().lock();
    let mut buffered: Vec<u8> = Vec::new();

    if opts.cover_letter {
        let mut msg: Vec<u8> = Vec::new();
        render_cover_letter(&repo, &commits, printed_total, &opts, &mut msg)?;
        emit_message(&mut buffered, &msg, cover_filename(&opts), &opts)?;
    }

    for (idx, id) in commits.iter().enumerate() {
        let commit = repo.find_object(*id)?.try_into_commit()?;
        let nr = idx + opts.start_number;

        let mut msg: Vec<u8> = Vec::new();
        render_message(&repo, &commit, nr, printed_total, &opts, &mut msg)?;

        // git puts one extra blank line between patches in the mbox stream; the
        // cover letter is not separated that way.
        if opts.to_stdout && idx > 0 {
            buffered.push(b'\n');
        }
        emit_message(&mut buffered, &msg, patch_filename(&commit, nr, &opts)?, &opts)?;
    }

    match stdout.write_all(&buffered).and_then(|()| stdout.flush()) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        // A downstream `| head` closing the pipe is not an error.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(ExitCode::SUCCESS),
        Err(e) => Err(e.into()),
    }
}

/// Append one rendered message to the mbox stream, or write it to its file and
/// note the name for stdout.
fn emit_message(buffered: &mut Vec<u8>, msg: &[u8], name: String, opts: &Opts) -> Result<()> {
    if opts.to_stdout {
        buffered.extend_from_slice(msg);
        return Ok(());
    }
    let path = match &opts.outdir {
        Some(dir) => {
            std::fs::create_dir_all(dir)?;
            format!("{dir}/{name}")
        }
        None => name.clone(),
    };
    if !opts.quiet {
        let shown = match &opts.outdir {
            Some(_) => path.clone(),
            None => name,
        };
        writeln!(buffered, "{shown}")?;
    }
    std::fs::write(&path, msg)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

enum Parsed {
    Ready(Box<Opts>),
    /// git refused the command line itself; the message is already on stderr.
    Exit(ExitCode),
}

/// Flags whose effect is already this module's behavior, so accepting them
/// changes nothing: the slider heuristic is what the blob diff runs, rename
/// detection is off, color and progress are never rendered, `a/`+`b/` are the
/// prefixes emitted, the stat+summary block is the default, and format-patch
/// implies `--binary` (binary content is rejected either way).
const NO_OP: &[&str] = &[
    "--indent-heuristic",
    "--no-renames",
    "--rename-empty",
    "--no-rename-empty",
    "--no-color",
    "--no-textconv",
    "--no-ext-diff",
    "--progress",
    "--no-progress",
    "--no-signoff",
    "--no-attach",
    "--no-thread",
    "--no-notes",
    "--no-base",
    "--no-encode-email-headers",
    "--no-force-in-body-from",
    "--no-relative",
    "--default-prefix",
    "--ita-invisible-in-index",
    "--binary",
    "--stat",
    "--summary",
];

/// Flags git accepts that this module has not ported. Matched as `--flag` or
/// `--flag=<value>`; see the module header for what each of them would change.
const DEFERRED: &[&str] = &[
    "-k",
    "--keep-subject",
    "-s",
    "--signoff",
    "--attach",
    "--inline",
    "--thread",
    "--in-reply-to",
    "--to",
    "--cc",
    "--add-header",
    "--from",
    "--force-in-body-from",
    "--encode-email-headers",
    "--notes",
    "--base",
    "--interdiff",
    "--range-diff",
    "--creation-factor",
    "--signature-file",
    "--description-file",
    "--cover-from-description",
    "--commit-list-format",
    "--always",
    "--ignore-if-in-upstream",
    "--numstat",
    "--shortstat",
    "--compact-summary",
    "--dirstat",
    "--dirstat-by-file",
    "--cumulative",
    "--stat-width",
    "--stat-name-width",
    "--stat-count",
    "--patience",
    "--no-indent-heuristic",
    "--full-index",
    "--no-binary",
    "--abbrev",
    "--break-rewrites",
    "--find-renames",
    "--find-copies",
    "--find-copies-harder",
    "--irreversible-delete",
    "--skip-to",
    "--rotate-to",
    "--ignore-cr-at-eol",
    "--ignore-space-at-eol",
    "--ignore-space-change",
    "--ignore-all-space",
    "--ignore-blank-lines",
    "--ignore-matching-lines",
    "--inter-hunk-context",
    "--function-context",
    "--textconv",
    "--no-prefix",
    "--line-prefix",
    "--output-indicator-new",
    "--output-indicator-old",
    "--output-indicator-context",
    "--anchored",
    "--no-walk",
    "--first-parent",
    "--topo-order",
    "--date-order",
    "--author-date-order",
    "-b",
    "-w",
    "-D",
    "-W",
];

/// Short options that carry an attached value, e.g. `-I^$` or `-M50%`.
const DEFERRED_SHORT: &[&str] = &["-I", "-l", "-M", "-C", "-B", "-O", "-S", "-G", "-X"];

/// True when `arg` is exactly `name` or the `name=<value>` form.
fn is_flag(arg: &str, name: &str) -> bool {
    arg == name || arg.strip_prefix(name).is_some_and(|r| r.starts_with('='))
}

fn parse(repo: &gix::Repository, args: &[String]) -> Result<Parsed> {
    let mut o = Opts {
        to_stdout: false,
        outdir: None,
        numbered: None,
        start_number: 1,
        numbered_files: false,
        suffix: ".patch".to_owned(),
        subject_prefix: "PATCH".to_owned(),
        reroll: None,
        signature: repo
            .config_snapshot()
            .string("format.signature")
            .as_ref()
            .and_then(|v| v.to_str().ok().map(str::to_owned))
            .unwrap_or_else(|| SIGNATURE_VERSION.to_owned()),
        zero_commit: false,
        no_stat: false,
        quiet: false,
        name_max: NAME_MAX_DEFAULT,
        cover_letter: false,
        root: false,
        max_count: None,
        skip: 0,
        reverse: false,
        min_parents: 0,
        // format-patch sets `rev.max_parents = 1`: merges never get a patch.
        max_parents: Some(1),
        revs: Vec::new(),
        paths: Vec::new(),
        context: 3,
        algorithm: Algorithm::Myers,
        text: false,
        deferred: Vec::new(),
    };

    let mut i = 0;
    let mut pathspec_mode = false;
    while i < args.len() {
        let a = args[i].as_str();
        if pathspec_mode {
            o.paths.push(a.to_owned());
            i += 1;
            continue;
        }
        match a {
            "--" => pathspec_mode = true,
            "--stdout" => o.to_stdout = true,
            "-o" | "--output-directory" => {
                i += 1;
                o.outdir = Some(value_at(args, i, a)?);
            }
            "-n" | "--numbered" => o.numbered = Some(true),
            "-N" | "--no-numbered" => o.numbered = Some(false),
            "--start-number" => {
                i += 1;
                o.start_number = parse_num(&value_at(args, i, a)?)?;
            }
            "--numbered-files" => o.numbered_files = true,
            "--subject-prefix" => {
                i += 1;
                o.subject_prefix = value_at(args, i, a)?;
            }
            "--suffix" => {
                i += 1;
                o.suffix = value_at(args, i, a)?;
            }
            "-v" | "--reroll-count" => {
                i += 1;
                o.reroll = Some(value_at(args, i, a)?);
            }
            "--signature" => {
                i += 1;
                o.signature = value_at(args, i, a)?;
            }
            "--no-signature" => o.signature.clear(),
            "--zero-commit" => o.zero_commit = true,
            "--no-zero-commit" => o.zero_commit = false,
            "-p" | "--no-stat" => o.no_stat = true,
            "--root" => o.root = true,
            "-q" | "--quiet" => o.quiet = true,
            "--filename-max-length" => {
                i += 1;
                o.name_max = parse_num(&value_at(args, i, a)?)?;
            }
            "--cover-letter" => o.cover_letter = true,
            "--no-cover-letter" => o.cover_letter = false,
            "--rfc" => o.subject_prefix = "RFC PATCH".to_owned(),
            "--reverse" => o.reverse = true,
            "--no-merges" => o.max_parents = Some(1),
            "--minimal" => o.algorithm = Algorithm::MyersMinimal,
            "--histogram" => o.algorithm = Algorithm::Histogram,
            "-a" | "--text" => o.text = true,
            // At the top of the worktree `--relative` neither strips a prefix
            // nor filters by directory, so there it is genuinely a no-op.
            "--relative" => {
                if !at_worktree_top(repo) {
                    o.deferred.push(a.to_owned());
                }
            }
            s if s.starts_with("--output-directory=") => {
                o.outdir = Some(s["--output-directory=".len()..].to_owned());
            }
            s if s.starts_with("--start-number=") => {
                o.start_number = parse_num(&s["--start-number=".len()..])?;
            }
            s if s.starts_with("--subject-prefix=") => {
                o.subject_prefix = s["--subject-prefix=".len()..].to_owned();
            }
            s if s.starts_with("--suffix=") => o.suffix = s["--suffix=".len()..].to_owned(),
            s if s.starts_with("--reroll-count=") => {
                o.reroll = Some(s["--reroll-count=".len()..].to_owned());
            }
            s if s.starts_with("--signature=") => {
                o.signature = s["--signature=".len()..].to_owned();
            }
            s if s.starts_with("--filename-max-length=") => {
                o.name_max = parse_num(&s["--filename-max-length=".len()..])?;
            }
            s if s.starts_with("--rfc=") => {
                o.subject_prefix = format!("{} PATCH", &s["--rfc=".len()..]);
            }
            s if s.starts_with("--max-count=") => {
                o.max_count = Some(parse_num(&s["--max-count=".len()..])?);
            }
            s if s.starts_with("--skip=") => o.skip = parse_num(&s["--skip=".len()..])?,
            s if s.starts_with("--min-parents=") => {
                o.min_parents = parse_num(&s["--min-parents=".len()..])?;
            }
            s if s.starts_with("--max-parents=") => {
                o.max_parents = Some(parse_num(&s["--max-parents=".len()..])?);
            }
            s if s.starts_with("--unified=") => {
                o.context = parse_num(&s["--unified=".len()..])? as u32;
            }
            s if s.len() > 2 && s.starts_with("-U") && s[2..].bytes().all(|c| c.is_ascii_digit()) => {
                o.context = parse_num(&s[2..])? as u32;
            }
            s if s.starts_with("--diff-algorithm=") => {
                match &s["--diff-algorithm=".len()..] {
                    "myers" => o.algorithm = Algorithm::Myers,
                    "minimal" => o.algorithm = Algorithm::MyersMinimal,
                    "histogram" => o.algorithm = Algorithm::Histogram,
                    // imara-diff has no patience implementation.
                    "patience" => o.deferred.push(a.to_owned()),
                    _ => {
                        eprintln!(
                            "error: option diff-algorithm accepts \"myers\", \"minimal\", \
                             \"patience\" and \"histogram\""
                        );
                        return Ok(Parsed::Exit(ExitCode::from(129)));
                    }
                }
            }
            s if s.starts_with("--thread=") => match &s["--thread=".len()..] {
                "shallow" | "deep" => o.deferred.push(a.to_owned()),
                // git rejects the value with a bare usage exit and no message.
                _ => return Ok(Parsed::Exit(ExitCode::from(129))),
            },
            s if s.starts_with("--cover-from-description=") => {
                let v = &s["--cover-from-description=".len()..];
                match v {
                    "message" | "subject" | "auto" | "none" => o.deferred.push(a.to_owned()),
                    _ => {
                        return Ok(Parsed::Exit(fatal(&format!(
                            "{v}: invalid cover from description mode"
                        ))))
                    }
                }
            }
            // "none" is what this module does: submodule changes are shown.
            s if s.starts_with("--ignore-submodules=") => {
                if &s["--ignore-submodules=".len()..] != "none" {
                    o.deferred.push(a.to_owned());
                }
            }
            s if s.starts_with("--src-prefix=") => {
                if &s["--src-prefix=".len()..] != "a/" {
                    o.deferred.push(a.to_owned());
                }
            }
            s if s.starts_with("--dst-prefix=") => {
                if &s["--dst-prefix=".len()..] != "b/" {
                    o.deferred.push(a.to_owned());
                }
            }
            // Colour is never emitted, so the flags that ask for none agree.
            s if s.starts_with("--color=") => {
                if !matches!(&s["--color=".len()..], "never" | "auto") {
                    o.deferred.push(a.to_owned());
                }
            }
            s if s.starts_with("--stat=") || s.starts_with("--relative=") => {
                o.deferred.push(a.to_owned());
            }
            s if s.len() > 2 && s.starts_with("-o") => o.outdir = Some(s[2..].to_owned()),
            s if s.len() > 2
                && s.starts_with("-v")
                && s[2..].bytes().all(|c| c.is_ascii_digit()) =>
            {
                o.reroll = Some(s[2..].to_owned());
            }
            // `-<n>` is a commit count, unlike `-n` which means --numbered.
            s if s.len() > 1
                && s.starts_with('-')
                && s[1..].bytes().all(|c| c.is_ascii_digit()) =>
            {
                o.max_count = Some(parse_num(&s[1..])?);
            }
            s if NO_OP.contains(&s) => {}
            s if DEFERRED.iter().any(|f| is_flag(s, f))
                || DEFERRED_SHORT.iter().any(|f| s.starts_with(f)) =>
            {
                o.deferred.push(s.to_owned());
            }
            s if s.starts_with('-') => bail!("unsupported flag {s:?}"),
            s => o.revs.push(s.to_owned()),
        }
        i += 1;
    }

    Ok(Parsed::Ready(Box::new(o)))
}

fn parse_num(s: &str) -> Result<usize> {
    s.parse::<usize>()
        .map_err(|_| anyhow!("invalid number `{s}`"))
}

/// The value slot of a two-token option, e.g. the `<dir>` in `-o <dir>`.
fn value_at(args: &[String], i: usize, name: &str) -> Result<String> {
    args.get(i)
        .cloned()
        .ok_or_else(|| anyhow!("option `{name}` requires a value"))
}

/// Whether the process runs at the root of the worktree, where a relative
/// pathspec prefix is empty.
fn at_worktree_top(repo: &gix::Repository) -> bool {
    let (Some(workdir), Ok(cwd)) = (repo.workdir(), std::env::current_dir()) else {
        return false;
    };
    match (workdir.canonicalize(), cwd.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Revision selection
// ---------------------------------------------------------------------------

/// git exits 128 on a fatal error; `anyhow::bail!` would collapse that to 1, so
/// the message goes to stderr here and the code is returned explicitly.
fn fatal(msg: &str) -> ExitCode {
    eprintln!("fatal: {msg}");
    ExitCode::from(128)
}

/// git's `die_verify_filename()` wording for an argument that is neither a
/// revision nor an existing path.
fn ambiguous(spec: &str) -> ExitCode {
    fatal(&format!(
        "ambiguous argument '{spec}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'"
    ))
}

/// A full-length object name is reported as a missing object rather than as an
/// ambiguous argument — git can tell it was meant to be an object id.
fn is_full_oid(spec: &str, hexsz: usize) -> bool {
    spec.len() == hexsz && spec.bytes().all(|b| b.is_ascii_hexdigit())
}

enum Selected {
    Commits {
        commits: Vec<ObjectId>,
        /// Pathspecs, including positionals that turned out to name a path.
        paths: Vec<String>,
    },
    Exit(ExitCode),
}

/// Resolve the revision arguments into the commits to format, oldest first and
/// with merges dropped (git sets `rev.max_parents = 1`).
///
/// A lone revision with neither `-<n>` nor `--root` is git's traditional
/// `format-patch <since>` shorthand for `<since>..HEAD`; anything else is an
/// ordinary walk over the given tips and exclusions. With no tips at all git
/// formats nothing unless `-<n>` or `--root` asked for HEAD implicitly.
fn select_commits(repo: &gix::Repository, o: &Opts) -> Result<Selected> {
    let hexsz = repo.object_hash().len_in_hex();
    let resolve = |spec: &str| -> Option<ObjectId> {
        repo.rev_parse_single(BStr::new(spec))
            .ok()
            .and_then(|id| id.object().ok())
            .and_then(|obj| obj.peel_to_commit().ok())
            .map(|c| c.id)
    };
    // An empty side of a range means HEAD, as in `..main` or `main..`.
    // A named fn, not a closure: closure inference unifies the input and output
    // lifetimes into one variable that cannot outlive the call.
    fn or_head(s: &str) -> &str {
        if s.is_empty() {
            "HEAD"
        } else {
            s
        }
    }

    let mut tips: Vec<ObjectId> = Vec::new();
    let mut hidden: Vec<ObjectId> = Vec::new();
    let mut paths: Vec<String> = o.paths.clone();
    let mut plain_tips = 0usize;

    for spec in &o.revs {
        // A missing side of a range is reported against the whole range: as a
        // missing object when it was spelled as one, else as an ambiguous
        // argument, exactly as git's `setup_revisions()` does.
        let range_error = |side: &str| -> ExitCode {
            if is_full_oid(side, hexsz) {
                fatal(&format!("Invalid revision range {spec}"))
            } else {
                ambiguous(spec)
            }
        };

        if let Some((left, right)) = spec.split_once("...") {
            let (left, right) = (or_head(left), or_head(right));
            let Some(a) = resolve(left) else {
                return Ok(Selected::Exit(range_error(left)));
            };
            let Some(b) = resolve(right) else {
                return Ok(Selected::Exit(range_error(right)));
            };
            // `a...b` is everything reachable from either tip but not both.
            for base in repo.merge_bases_many(a, &[b])? {
                hidden.push(base.detach());
            }
            tips.push(a);
            tips.push(b);
        } else if let Some((left, right)) = spec.split_once("..") {
            let (left, right) = (or_head(left), or_head(right));
            let Some(a) = resolve(left) else {
                return Ok(Selected::Exit(range_error(left)));
            };
            let Some(b) = resolve(right) else {
                return Ok(Selected::Exit(range_error(right)));
            };
            hidden.push(a);
            tips.push(b);
        } else if let Some(rest) = spec.strip_prefix('^') {
            match resolve(rest) {
                Some(id) => hidden.push(id),
                None if is_full_oid(rest, hexsz) => {
                    return Ok(Selected::Exit(fatal(&format!("bad object {rest}"))))
                }
                // An exclusion is never retried as a filename.
                None => return Ok(Selected::Exit(fatal(&format!("bad revision '{spec}'")))),
            }
        } else {
            match resolve(spec) {
                Some(id) => {
                    tips.push(id);
                    plain_tips += 1;
                }
                // git falls back to treating the argument as a pathspec when it
                // names something that exists in the worktree.
                None if std::path::Path::new(spec).exists() => paths.push(spec.clone()),
                None if is_full_oid(spec, hexsz) => {
                    return Ok(Selected::Exit(fatal(&format!("bad object {spec}"))))
                }
                None => return Ok(Selected::Exit(ambiguous(spec))),
            }
        }
    }

    // `format-patch <since>` prepares what the other side does not have yet.
    if plain_tips == 1 && tips.len() == 1 && o.max_count.is_none() && !o.root {
        let since = tips.pop().expect("a single tip was just counted");
        hidden.push(since);
        tips.push(repo.head_id()?.detach());
    } else if tips.is_empty() && (o.max_count.is_some() || o.root) {
        // `format-patch -3` and `format-patch --root` walk from HEAD; with no
        // revision argument at all git formats nothing.
        tips.push(repo.head_id()?.detach());
    }

    if tips.is_empty() {
        return Ok(Selected::Commits {
            commits: Vec::new(),
            paths,
        });
    }

    let mut platform = repo
        .rev_walk(tips)
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst));
    if !hidden.is_empty() {
        platform = platform.with_hidden(hidden);
    }

    let mut out: Vec<ObjectId> = Vec::new();
    let mut skipped = 0usize;
    for info in platform.all()? {
        let info = info?;
        let parents = repo
            .find_object(info.id)?
            .try_into_commit()?
            .parent_ids()
            .count();
        if parents < o.min_parents {
            continue;
        }
        if o.max_parents.is_some_and(|max| parents > max) {
            continue;
        }
        if skipped < o.skip {
            skipped += 1;
            continue;
        }
        if o.max_count.is_some_and(|max| out.len() >= max) {
            break;
        }
        out.push(info.id);
    }
    // The walk is newest-first; git emits oldest-first unless asked to reverse.
    if !o.reverse {
        out.reverse();
    }
    Ok(Selected::Commits {
        commits: out,
        paths,
    })
}

// ---------------------------------------------------------------------------
// Message rendering
// ---------------------------------------------------------------------------

/// `[v<n>-]NNNN-<sanitized subject><suffix>`, or the bare number under
/// `--numbered-files`. Port of `fmt_output_subject()` (log-tree.c).
fn patch_filename(commit: &gix::Commit<'_>, nr: usize, opts: &Opts) -> Result<String> {
    if opts.numbered_files {
        return Ok(nr.to_string());
    }
    let msg = skip_blank_lines(commit.message_raw()?);
    // git's `%f` sanitizes only the first line of the subject.
    let first_line = &msg[..one_line(msg)];
    Ok(numbered_filename(nr, trim_end_ws(first_line), opts))
}

/// The cover letter is always patch zero, whatever `--start-number` moved the
/// rest of the series to.
fn cover_filename(opts: &Opts) -> String {
    if opts.numbered_files {
        return "0".to_owned();
    }
    numbered_filename(0, b"cover letter", opts)
}

fn numbered_filename(nr: usize, subject: &[u8], opts: &Opts) -> String {
    let mut name = String::new();
    if let Some(r) = &opts.reroll {
        sanitize_subject(&mut name, format!("v{r}").as_bytes());
        name.push('-');
    }
    name.push_str(&format!("{nr:04}-"));
    sanitize_subject(&mut name, subject);

    let max = opts.name_max - (opts.suffix.len() + 1);
    if name.len() > max {
        // `sanitize_subject` only emits ASCII, so this is a char boundary.
        name.truncate(max);
    }
    name.push_str(&opts.suffix);
    name
}

/// Port of `format_sanitized_subject()` (pretty.c): collapse everything that is
/// not `[A-Za-z0-9._]` into single dashes, fold runs of dots, and trim trailing
/// `.`/`-`.
fn sanitize_subject(out: &mut String, msg: &[u8]) {
    let start_len = out.len();
    let mut space = 2u8;
    let mut i = 0;
    while i < msg.len() {
        let c = msg[i];
        if c.is_ascii_alphanumeric() || c == b'.' || c == b'_' {
            if space == 1 {
                out.push('-');
            }
            space = 0;
            out.push(c as char);
            if c == b'.' {
                while i + 1 < msg.len() && msg[i + 1] == b'.' {
                    i += 1;
                }
            }
        } else {
            space |= 1;
        }
        i += 1;
    }
    while out.len() > start_len && (out.ends_with('.') || out.ends_with('-')) {
        out.pop();
    }
}

/// Render one complete mail message: magic `From` line, headers, body, and —
/// when the commit changes anything — the three-dash separator, stat/summary
/// block and patch, followed by the signature.
fn render_message(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    nr: usize,
    total: usize,
    opts: &Opts,
    out: &mut Vec<u8>,
) -> Result<()> {
    write_from_line(out, commit.id, opts)?;

    // Headers and body are built in one buffer because git's wrapping and the
    // final `strbuf_rtrim` both depend on what is already in it.
    let mut sb = String::new();
    let raw = commit.message_raw()?;
    let need_8bit = raw.iter().any(|&b| b >= 0x80);

    let author = commit.author()?;
    let author_name = author.name.to_str().map_err(|_| {
        anyhow!("author name is not valid UTF-8; RFC2047 encoding needs a known charset")
    })?;
    let author_mail = author.email.to_str().map_err(|_| {
        anyhow!("author email is not valid UTF-8; RFC2047 encoding needs a known charset")
    })?;
    let date = author
        .time()?
        .format(gix::date::time::format::GIT_RFC2822)?;
    write_identity_headers(&mut sb, author_name, author_mail, &date);

    // Subject: — the first paragraph, folded onto one logical line.
    let msg = skip_blank_lines(raw);
    let (title, rest) = format_subject(msg);
    let title = title
        .to_str()
        .map_err(|_| anyhow!("commit subject is not valid UTF-8"))?
        .to_owned();
    write_subject(&mut sb, &title, nr, total, opts);

    if need_8bit {
        sb.push_str("MIME-Version: 1.0\n");
        sb.push_str(&format!("Content-Type: text/plain; charset={ENCODING}\n"));
        sb.push_str("Content-Transfer-Encoding: 8bit\n");
    }
    sb.push('\n');

    // Body — the remaining paragraphs, right-trimmed line by line.
    let beginning_of_body = sb.len();
    let mut body: Vec<u8> = Vec::new();
    pp_remainder(rest, &mut body);
    sb.push_str(
        body.to_str()
            .map_err(|_| anyhow!("commit message is not valid UTF-8"))?,
    );
    while sb.ends_with([' ', '\t', '\n', '\r']) {
        sb.pop();
    }
    sb.push('\n');
    if sb.len() <= beginning_of_body {
        sb.push('\n');
    }
    out.extend_from_slice(sb.as_bytes());

    // The patch itself.
    let new_tree = commit.tree()?;
    let parents: Vec<_> = commit.parent_ids().collect();
    let old_tree = match parents.first() {
        Some(pid) => Some(pid.object()?.try_into_commit()?.tree()?),
        None => None,
    };
    let abbrev = new_tree.id().shorten()?.hex_len();
    let changes = tree_changes(repo, old_tree.as_ref(), Some(&new_tree))?;

    if !changes.is_empty() {
        let mut patch: Vec<u8> = Vec::new();
        let mut stats: Vec<StatEntry> = Vec::new();
        for change in &changes {
            stats.push(emit_change(repo, &mut patch, change, abbrev, opts)?);
        }

        if opts.no_stat {
            out.push(b'\n');
        } else {
            out.extend_from_slice(b"---\n");
            emit_stats(out, &stats)?;
            emit_summary(out, &changes)?;
            out.push(b'\n');
        }
        out.extend_from_slice(&patch);
    }

    write_signature(out, opts);
    Ok(())
}

/// Port of `make_cover_letter()` (log-tree.c): the placeholder subject and
/// blurb, a shortlog of the series, and the diffstat of the whole range.
///
/// The magic `From` line names the newest commit of the series, and the
/// identity is the committer's — the cover letter is written now, by whoever
/// runs the command, not by the author of any one patch.
fn render_cover_letter(
    repo: &gix::Repository,
    commits: &[ObjectId],
    total: usize,
    opts: &Opts,
    out: &mut Vec<u8>,
) -> Result<()> {
    // The series is oldest-first unless `--reverse` flipped it; either way the
    // newest commit is the one without a descendant inside the series.
    let newest = if opts.reverse {
        *commits.first().expect("a non-empty series")
    } else {
        *commits.last().expect("a non-empty series")
    };
    write_from_line(out, newest, opts)?;

    let mut sb = String::new();
    let (name, mail, date) = match repo.committer().transpose()? {
        Some(sig) => (
            sig.name.to_str()?.to_owned(),
            sig.email.to_str()?.to_owned(),
            sig.time()?.format(gix::date::time::format::GIT_RFC2822)?,
        ),
        // No committer identity configured: fall back to the series' author so
        // the cover letter is still a well-formed message.
        None => {
            let commit = repo.find_object(newest)?.try_into_commit()?;
            let author = commit.author()?;
            (
                author.name.to_str()?.to_owned(),
                author.email.to_str()?.to_owned(),
                author.time()?.format(gix::date::time::format::GIT_RFC2822)?,
            )
        }
    };
    write_identity_headers(&mut sb, &name, &mail, &date);
    write_subject(&mut sb, COVER_SUBJECT, 0, total, opts);
    sb.push('\n');
    sb.push_str(COVER_BLURB);
    sb.push_str("\n\n");
    out.extend_from_slice(sb.as_bytes());

    emit_shortlog(repo, commits, out)?;
    out.push(b'\n');

    // The range's combined diffstat needs a base to diff against, which a root
    // commit does not have; git omits the block in that case.
    let first = repo
        .find_object(*commits.first().expect("a non-empty series"))?
        .try_into_commit()?;
    let base = match first.parent_ids().next() {
        Some(pid) => Some(pid.object()?.try_into_commit()?.tree()?),
        None => None,
    };
    if let (Some(base), false) = (base, opts.no_stat) {
        let newest_tree = repo.find_object(newest)?.try_into_commit()?.tree()?;
        let abbrev = newest_tree.id().shorten()?.hex_len();
        let changes = tree_changes(repo, Some(&base), Some(&newest_tree))?;
        if !changes.is_empty() {
            let mut discard: Vec<u8> = Vec::new();
            let mut stats: Vec<StatEntry> = Vec::new();
            for change in &changes {
                stats.push(emit_change(repo, &mut discard, change, abbrev, opts)?);
            }
            emit_stats(out, &stats)?;
            emit_summary(out, &changes)?;
            out.push(b'\n');
        }
    }

    write_signature(out, opts);
    Ok(())
}

/// git's shortlog as the cover letter embeds it: one `Name (count):` group per
/// author, most commits first, each subject indented by two spaces.
fn emit_shortlog(repo: &gix::Repository, commits: &[ObjectId], out: &mut Vec<u8>) -> Result<()> {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for id in commits {
        let commit = repo.find_object(*id)?.try_into_commit()?;
        let author = commit.author()?.name.to_str()?.to_owned();
        let msg = skip_blank_lines(commit.message_raw()?);
        let (title, _) = format_subject(msg);
        let title = title.to_str()?.to_owned();
        match groups.iter_mut().find(|(name, _)| *name == author) {
            Some((_, subjects)) => subjects.push(title),
            None => groups.push((author, vec![title])),
        }
    }
    // Ties keep author order stable by name, as git's string list does.
    groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));

    for (i, (name, subjects)) in groups.iter().enumerate() {
        if i > 0 {
            out.push(b'\n');
        }
        writeln!(out, "{name} ({}):", subjects.len())?;
        for s in subjects {
            writeln!(out, "  {s}")?;
        }
    }
    Ok(())
}

/// The mbox magic line. `--zero-commit` replaces the commit name with zeroes.
fn write_from_line(out: &mut Vec<u8>, id: ObjectId, opts: &Opts) -> Result<()> {
    let name = if opts.zero_commit {
        ObjectId::null(id.kind()).to_hex().to_string()
    } else {
        id.to_hex().to_string()
    };
    writeln!(out, "From {name} Mon Sep 17 00:00:00 2001")?;
    Ok(())
}

/// `From:` — RFC2047 when non-ASCII, RFC822 quoting for specials, else wrapped —
/// followed by `Date:`.
fn write_identity_headers(sb: &mut String, name: &str, mail: &str, date: &str) {
    sb.push_str("From: ");
    let mut max_length = HEADER_MAX_LENGTH;
    if needs_rfc2047_encoding(name) {
        add_rfc2047(sb, name, true);
        max_length = 76;
    } else if name.bytes().any(is_rfc822_special) {
        let quoted = rfc822_quoted(name);
        wrap_text(sb, &quoted, -6, 1, max_length);
    } else {
        wrap_text(sb, name, -6, 1, max_length);
    }
    if max_length < last_line_length(sb) + 2 + mail.len() as i64 + 1 {
        sb.push('\n');
    }
    sb.push_str(&format!(" <{mail}>\n"));
    sb.push_str(&format!("Date: {date}\n"));
}

/// `Subject: [<prefix> n/total] <title>`, with the numbering git uses.
fn write_subject(sb: &mut String, title: &str, nr: usize, total: usize, opts: &Opts) {
    if total > 0 {
        let width = decimal_width(total as u64);
        let sep = if opts.subject_prefix.is_empty() {
            ""
        } else {
            " "
        };
        sb.push_str(&format!(
            "Subject: [{}{sep}{:0width$}/{total}] ",
            opts.subject_prefix, nr
        ));
    } else if !opts.subject_prefix.is_empty() {
        sb.push_str(&format!("Subject: [{}] ", opts.subject_prefix));
    } else {
        sb.push_str("Subject: ");
    }
    if needs_rfc2047_encoding(title) {
        add_rfc2047(sb, title, false);
    } else {
        let consumed = -last_line_length(sb);
        wrap_text(sb, title, consumed, 1, HEADER_MAX_LENGTH);
    }
    sb.push('\n');
}

fn write_signature(out: &mut Vec<u8>, opts: &Opts) {
    if opts.signature.is_empty() {
        return;
    }
    out.extend_from_slice(b"-- \n");
    out.extend_from_slice(opts.signature.as_bytes());
    if !opts.signature.ends_with('\n') {
        out.push(b'\n');
    }
    out.push(b'\n');
}

/// The file-level changes between two trees, in path order.
///
/// `tree_with_rewrites` reports the directory entry *and* its recursed contents;
/// git's patch format only names blobs and submodules, so tree entries are
/// dropped — keeping one would render a raw tree object as a binary file.
fn tree_changes(
    repo: &gix::Repository,
    old_tree: Option<&gix::Tree<'_>>,
    new_tree: Option<&gix::Tree<'_>>,
) -> Result<Vec<ChangeDetached>> {
    let mut changes =
        repo.diff_tree_to_tree(old_tree, new_tree, gix::diff::Options::default())?;
    changes.retain(|c| !is_tree_entry(c));
    changes.sort_by(|a, b| change_path(a).cmp(change_path(b)));
    Ok(changes)
}

fn is_tree_entry(change: &ChangeDetached) -> bool {
    match change {
        ChangeDetached::Addition { entry_mode, .. }
        | ChangeDetached::Deletion { entry_mode, .. }
        | ChangeDetached::Modification { entry_mode, .. }
        | ChangeDetached::Rewrite { entry_mode, .. } => entry_mode.is_tree(),
    }
}

// ---------------------------------------------------------------------------
// Commit-message plumbing (pretty.c)
// ---------------------------------------------------------------------------

/// git's `get_one_line`: the length of the next line, newline included.
fn one_line(msg: &[u8]) -> usize {
    match msg.iter().position(|&b| b == b'\n') {
        Some(i) => i + 1,
        None => msg.len(),
    }
}

/// git's `is_blank_line`: right-trim the line and report whether nothing is left.
/// Returns the trimmed slice alongside the verdict.
fn blank_line(line: &[u8]) -> (&[u8], bool) {
    let t = trim_end_ws(line);
    (t, t.is_empty())
}

/// Strip trailing ASCII whitespace (git's `isspace` set).
fn trim_end_ws(mut s: &[u8]) -> &[u8] {
    while let Some(&last) = s.last() {
        if last.is_ascii_whitespace() {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    s
}

/// git's `skip_blank_lines`: advance past leading blank lines.
fn skip_blank_lines(mut msg: &[u8]) -> &[u8] {
    loop {
        let len = one_line(msg);
        if len == 0 {
            return msg;
        }
        if !blank_line(&msg[..len]).1 {
            return msg;
        }
        msg = &msg[len..];
    }
}

/// git's `format_subject` with a `" "` separator: join the first paragraph into
/// one line and return it together with the rest of the message.
fn format_subject(mut msg: &[u8]) -> (Vec<u8>, &[u8]) {
    let mut title: Vec<u8> = Vec::new();
    let mut first = true;
    loop {
        let len = one_line(msg);
        if len == 0 {
            break;
        }
        let (trimmed, is_blank) = blank_line(&msg[..len]);
        if is_blank {
            break;
        }
        msg = &msg[len..];
        if !first {
            title.push(b' ');
        }
        title.extend_from_slice(trimmed);
        first = false;
    }
    (title, msg)
}

/// git's `pp_remainder` with zero indent: skip leading blank lines, then emit
/// every remaining line right-trimmed.
fn pp_remainder(mut msg: &[u8], out: &mut Vec<u8>) {
    let mut first = true;
    loop {
        let len = one_line(msg);
        if len == 0 {
            break;
        }
        let (trimmed, is_blank) = blank_line(&msg[..len]);
        msg = &msg[len..];
        if is_blank && first {
            continue;
        }
        first = false;
        out.extend_from_slice(trimmed);
        out.push(b'\n');
    }
}

// ---------------------------------------------------------------------------
// Header encoding and wrapping (pretty.c, utf8.c)
// ---------------------------------------------------------------------------

/// Bytes already used on the last line of `sb` (git's `last_line_length`).
fn last_line_length(sb: &str) -> i64 {
    match sb.rfind('\n') {
        Some(i) => (sb.len() - i - 1) as i64,
        None => sb.len() as i64,
    }
}

/// git's `needs_rfc2047_encoding`: any non-ASCII byte, a newline, or a literal
/// `=?` sequence forces the encoded-word form.
fn needs_rfc2047_encoding(s: &str) -> bool {
    let b = s.as_bytes();
    for (i, &ch) in b.iter().enumerate() {
        if ch >= 0x80 || ch == b'\n' {
            return true;
        }
        if i + 1 < b.len() && ch == b'=' && b[i + 1] == b'?' {
            return true;
        }
    }
    false
}

/// git's `is_rfc822_special`.
fn is_rfc822_special(ch: u8) -> bool {
    matches!(
        ch,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b':' | b';' | b'@' | b',' | b'.' | b'"' | b'\\'
    )
}

/// git's `add_rfc822_quoted`: wrap in double quotes, backslash-escaping `"`/`\`.
fn rfc822_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// git's `is_rfc2047_special`. `address` selects the stricter `phrase` rules
/// used for the `From:` display name.
fn is_rfc2047_special(ch: u8, address: bool) -> bool {
    if ch >= 0x80 || !(0x20..0x7f).contains(&ch) {
        return true;
    }
    if ch.is_ascii_whitespace() || ch == b'=' || ch == b'?' || ch == b'_' {
        return true;
    }
    if !address {
        return false;
    }
    !(ch.is_ascii_alphanumeric() || matches!(ch, b'!' | b'*' | b'+' | b'-' | b'/'))
}

/// Port of `add_rfc2047()` (pretty.c): q-encoded words, never splitting a
/// multi-byte character, folded at 76 columns.
fn add_rfc2047(sb: &mut String, line: &str, address: bool) {
    const MAX_ENCODED_LENGTH: i64 = 76;
    let mut line_len = last_line_length(sb);

    sb.push_str(&format!("=?{ENCODING}?q?"));
    line_len += ENCODING.len() as i64 + 5;

    for c in line.chars() {
        let mut buf = [0u8; 4];
        let bytes = c.encode_utf8(&mut buf).as_bytes();
        let chrlen = bytes.len() as i64;
        let is_special = chrlen > 1 || is_rfc2047_special(bytes[0], address);
        let encoded_len = if is_special { 3 * chrlen } else { 1 };

        if line_len + encoded_len + 2 > MAX_ENCODED_LENGTH {
            sb.push_str(&format!("?=\n =?{ENCODING}?q?"));
            line_len = ENCODING.len() as i64 + 5 + 1;
        }
        for &b in bytes {
            if is_special {
                sb.push_str(&format!("={b:02X}"));
            } else {
                sb.push(b as char);
            }
        }
        line_len += encoded_len;
    }
    sb.push_str("?=");
}

/// Port of `strbuf_add_wrapped_text()` (utf8.c) for the ASCII inputs that reach
/// it — anything non-ASCII takes the RFC2047 path above, and neither the subject
/// (paragraph joined with spaces) nor a display name can contain a newline, so
/// the original's embedded-newline branch is unreachable here.
///
/// A negative `indent1` means that many columns are already consumed.
fn wrap_text(buf: &mut String, text: &str, indent1: i64, indent2: i64, width: i64) {
    if width <= 0 {
        buf.push_str(text);
        return;
    }
    let b = text.as_bytes();
    let mut indent = indent1;
    let mut w = indent1;
    let mut bol: usize = 0;
    let mut space: Option<usize> = None;
    let mut i: usize = 0;

    if indent < 0 {
        w = -indent;
        space = Some(0);
    }

    loop {
        let c = b.get(i).copied().unwrap_or(0);
        if c == 0 || c.is_ascii_whitespace() {
            if w <= width || space.is_none() {
                // git checks the empty-tail case against `bol`, before the
                // remembered space overrides the copy start.
                if c == 0 && i == bol {
                    return;
                }
                let start = match space {
                    Some(s) => s,
                    None => {
                        if indent > 0 {
                            buf.push_str(&" ".repeat(indent as usize));
                        }
                        bol
                    }
                };
                buf.push_str(&text[start..i]);
                if c == 0 {
                    return;
                }
                space = Some(i);
                if c == b'\t' {
                    w |= 0x07;
                }
                w += 1;
                i += 1;
            } else {
                // Break the line at the last remembered space.
                buf.push('\n');
                let s = space.expect("the else branch requires a remembered space");
                // `*space` reads the NUL terminator in git when the remembered
                // position is the end of the text; that is not whitespace.
                let at_space = b.get(s).copied().unwrap_or(0).is_ascii_whitespace();
                i = s + usize::from(at_space);
                bol = i;
                space = None;
                indent = indent2;
                w = indent2;
            }
            continue;
        }
        w += 1;
        i += 1;
    }
}

// ---------------------------------------------------------------------------
// Diffstat and summary (diff.c)
// ---------------------------------------------------------------------------

/// One diffstat row: the quoted path and its line counts.
struct StatEntry {
    name: String,
    added: u64,
    deleted: u64,
}

/// git's `decimal_width`.
fn decimal_width(mut n: u64) -> usize {
    let mut w = 1;
    while n >= 10 {
        n /= 10;
        w += 1;
    }
    w
}

/// git's `scale_linear`: at least one column for any non-zero change.
fn scale_linear(it: i64, width: i64, max_change: i64) -> i64 {
    if it == 0 {
        return 0;
    }
    1 + (it * (width - 1) / max_change)
}

/// Display width in Unicode scalar values (git measures terminal columns; wide
/// characters are counted as 1 here, see the module note).
fn display_width(s: &str) -> i64 {
    s.chars().count() as i64
}

/// Port of `show_stats()` (diff.c) at format-patch's fixed 72-column mail width,
/// followed by `print_stat_summary_inserts_deletes()`.
fn emit_stats(out: &mut Vec<u8>, files: &[StatEntry]) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }

    let mut max_change: i64 = 0;
    let mut max_len: i64 = 0;
    for f in files {
        max_len = max_len.max(display_width(&f.name));
        max_change = max_change.max((f.added + f.deleted) as i64);
    }

    let mut width = MAIL_DEFAULT_WRAP;
    let number_width = decimal_width(max_change as u64) as i64;
    if width < 16 + 6 + number_width {
        width = 16 + 6 + number_width;
    }

    let mut graph_width = max_change;
    let mut name_width = max_len;
    if name_width + number_width + 6 + graph_width > width {
        if graph_width > width * 3 / 8 - number_width - 6 {
            graph_width = width * 3 / 8 - number_width - 6;
            if graph_width < 6 {
                graph_width = 6;
            }
        }
        if name_width > width - number_width - 6 - graph_width {
            name_width = width - number_width - 6 - graph_width;
        } else {
            graph_width = width - number_width - 6 - name_width;
        }
    }

    let mut adds: u64 = 0;
    let mut dels: u64 = 0;
    for f in files {
        adds += f.added;
        dels += f.deleted;

        // Scale the filename: elide the head, then resume at a path separator.
        let mut len = name_width;
        let mut prefix = "";
        let mut name: &str = &f.name;
        if name_width < display_width(name) {
            prefix = "...";
            len -= 3;
            if len < 0 {
                len = 0;
            }
            let mut name_len = display_width(name);
            let mut off = 0;
            while name_len > len && off < name.len() {
                let c = name[off..]
                    .chars()
                    .next()
                    .expect("off stays on a char boundary");
                off += c.len_utf8();
                name_len -= 1;
            }
            name = &name[off..];
            if let Some(slash) = name.find('/') {
                name = &name[slash..];
            }
        }
        let padding = (len - display_width(name)).max(0) as usize;

        let total = f.added + f.deleted;
        let mut add = f.added as i64;
        let mut del = f.deleted as i64;
        if graph_width <= max_change && max_change > 0 {
            let mut sum = scale_linear(add + del, graph_width, max_change);
            if sum < 2 && add > 0 && del > 0 {
                sum = 2;
            }
            if add < del {
                add = scale_linear(add, graph_width, max_change);
                del = sum - add;
            } else {
                del = scale_linear(del, graph_width, max_change);
                add = sum - del;
            }
        }

        write!(
            out,
            " {prefix}{name}{:padding$} | {:>nw$}{}",
            "",
            total,
            if total > 0 { " " } else { "" },
            nw = number_width as usize,
        )?;
        for _ in 0..add.max(0) {
            out.push(b'+');
        }
        for _ in 0..del.max(0) {
            out.push(b'-');
        }
        out.push(b'\n');
    }

    let n = files.len();
    let mut line = format!(" {n} {} changed", if n == 1 { "file" } else { "files" });
    if adds > 0 || dels == 0 {
        line.push_str(&format!(
            ", {adds} {}",
            if adds == 1 {
                "insertion(+)"
            } else {
                "insertions(+)"
            }
        ));
    }
    if dels > 0 || adds == 0 {
        line.push_str(&format!(
            ", {dels} {}",
            if dels == 1 {
                "deletion(-)"
            } else {
                "deletions(-)"
            }
        ));
    }
    writeln!(out, "{line}")?;
    Ok(())
}

/// Port of `diff_summary()` (diff.c): the `create`/`delete`/`mode change` lines
/// that follow the diffstat. Rewrites never occur (rename detection is off).
fn emit_summary(out: &mut Vec<u8>, changes: &[ChangeDetached]) -> Result<()> {
    for change in changes {
        match change {
            ChangeDetached::Addition {
                location,
                entry_mode,
                ..
            } => writeln!(
                out,
                " create mode {:06o} {}",
                entry_mode.value(),
                quote_path(location)
            )?,
            ChangeDetached::Deletion {
                location,
                entry_mode,
                ..
            } => writeln!(
                out,
                " delete mode {:06o} {}",
                entry_mode.value(),
                quote_path(location)
            )?,
            ChangeDetached::Modification {
                location,
                previous_entry_mode,
                entry_mode,
                ..
            } => {
                if previous_entry_mode.value() != entry_mode.value() {
                    writeln!(
                        out,
                        " mode change {:06o} => {:06o} {}",
                        previous_entry_mode.value(),
                        entry_mode.value(),
                        quote_path(location)
                    )?;
                }
            }
            ChangeDetached::Rewrite { .. } => bail!("rename/copy detection is not supported"),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Patch body (shared shape with `show`)
// ---------------------------------------------------------------------------

/// Render one file-level change as a `diff --git` block, returning its stat row.
fn emit_change(
    repo: &gix::Repository,
    out: &mut Vec<u8>,
    change: &ChangeDetached,
    abbrev: usize,
    opts: &Opts,
) -> Result<StatEntry> {
    let mut counts = (0u64, 0u64);
    match change {
        ChangeDetached::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            let path: &[u8] = location;
            emit_git_header(out, path);
            writeln!(out, "new file mode {:o}", entry_mode.value())?;
            let is_sub = entry_mode.is_commit();
            let content = content_of(repo, *id, is_sub)?;
            reject_binary(is_sub, &content, path, opts)?;
            let short = short_oid(repo, *id, abbrev, is_sub)?;
            writeln!(out, "index {}..{}", "0".repeat(short.len()), short)?;
            counts = emit_body(out, None, Some(path), &[], &content, opts)?;
        }
        ChangeDetached::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            let path: &[u8] = location;
            emit_git_header(out, path);
            writeln!(out, "deleted file mode {:o}", entry_mode.value())?;
            let is_sub = entry_mode.is_commit();
            let content = content_of(repo, *id, is_sub)?;
            reject_binary(is_sub, &content, path, opts)?;
            let short = short_oid(repo, *id, abbrev, is_sub)?;
            writeln!(out, "index {}..{}", short, "0".repeat(short.len()))?;
            counts = emit_body(out, Some(path), None, &content, &[], opts)?;
        }
        ChangeDetached::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
        } => {
            let path: &[u8] = location;
            emit_git_header(out, path);
            let old_mode = format!("{:o}", previous_entry_mode.value());
            let new_mode = format!("{:o}", entry_mode.value());
            let mode_changed = old_mode != new_mode;
            if mode_changed {
                writeln!(out, "old mode {old_mode}")?;
                writeln!(out, "new mode {new_mode}")?;
            }
            // A pure mode change (identical content) prints no index/hunks.
            if previous_id != id {
                let old_is_sub = previous_entry_mode.is_commit();
                let new_is_sub = entry_mode.is_commit();
                let old_content = content_of(repo, *previous_id, old_is_sub)?;
                let new_content = content_of(repo, *id, new_is_sub)?;
                reject_binary(old_is_sub, &old_content, path, opts)?;
                reject_binary(new_is_sub, &new_content, path, opts)?;
                let old_short = short_oid(repo, *previous_id, abbrev, old_is_sub)?;
                let new_short = short_oid(repo, *id, abbrev, new_is_sub)?;
                // The mode suffix is dropped when `old mode`/`new mode` said it.
                if mode_changed {
                    writeln!(out, "index {old_short}..{new_short}")?;
                } else {
                    writeln!(out, "index {old_short}..{new_short} {new_mode}")?;
                }
                counts = emit_body(
                    out,
                    Some(path),
                    Some(path),
                    &old_content,
                    &new_content,
                    opts,
                )?;
            }
        }
        // Never produced: rewrite tracking is off via Options::default().
        ChangeDetached::Rewrite { .. } => bail!("rename/copy detection is not supported"),
    }
    Ok(StatEntry {
        name: quote_path(change_path(change)),
        added: counts.0,
        deleted: counts.1,
    })
}

/// format-patch implies `--binary`, whose base85 `GIT binary patch` payload is
/// not ported; refuse rather than emit a textual approximation. `-a`/`--text`
/// asks for exactly that textual rendering, so it is honoured.
fn reject_binary(is_submodule: bool, content: &[u8], path: &[u8], opts: &Opts) -> Result<()> {
    if !opts.text && !is_submodule && content.iter().take(8000).any(|&b| b == 0) {
        bail!(
            "binary file {:?}: the GIT binary patch encoding is not ported",
            path.as_bstr()
        );
    }
    Ok(())
}

/// `diff --git a/<path> b/<path>` line, with git's `quote_two()` C-quoting.
fn emit_git_header(out: &mut Vec<u8>, path: &[u8]) {
    out.extend_from_slice(b"diff --git ");
    out.extend_from_slice(&quote_two("a/", path));
    out.push(b' ');
    out.extend_from_slice(&quote_two("b/", path));
    out.push(b'\n');
}

/// Emit the `---`/`+++` headers and hunks, returning `(added, deleted)` line
/// counts. An add/delete of an empty file produces no header lines, like git.
fn emit_body(
    out: &mut Vec<u8>,
    old: Option<&[u8]>,
    new: Option<&[u8]>,
    old_content: &[u8],
    new_content: &[u8],
    opts: &Opts,
) -> Result<(u64, u64)> {
    let mut hunks: Vec<u8> = Vec::new();
    let counts = emit_text_hunks(&mut hunks, old_content, new_content, opts)?;
    if hunks.is_empty() {
        return Ok(counts);
    }

    emit_file_header(out, b"--- ", old, "a/");
    emit_file_header(out, b"+++ ", new, "b/");
    out.extend_from_slice(&hunks);
    Ok(counts)
}

/// One `---`/`+++` line. git appends a tab when the rendered name contains a
/// space, so that a reader can tell where the name ends.
fn emit_file_header(out: &mut Vec<u8>, marker: &[u8], path: Option<&[u8]>, prefix: &str) {
    out.extend_from_slice(marker);
    let name = match path {
        Some(p) => quote_two(prefix, p),
        None => b"/dev/null".to_vec(),
    };
    out.extend_from_slice(&name);
    if name.contains(&b' ') {
        out.push(b'\t');
    }
    out.push(b'\n');
}

/// Compute the unified diff of two blobs, returning the added/deleted line
/// counts the diffstat needs.
fn emit_text_hunks(
    out: &mut Vec<u8>,
    old: &[u8],
    new: &[u8],
    opts: &Opts,
) -> Result<(u64, u64)> {
    let input = InternedInput::new(old, new);
    let diff = diff_with_slider_heuristics(opts.algorithm, &input);
    let before_lines: Vec<&[u8]> = input.before.iter().map(|&t| input.interner[t]).collect();
    let writer = HunkWriter {
        out,
        before_lines,
        added: 0,
        deleted: 0,
    };
    let counts = UnifiedDiff::new(
        &diff,
        &input,
        writer,
        ContextSize::symmetrical(opts.context),
    )
    .consume()?;
    Ok(counts)
}

/// Writes hunks in git's unified-diff style and tallies changed lines.
struct HunkWriter<'a> {
    out: &'a mut Vec<u8>,
    /// Pre-image lines, for resolving each hunk header's function context.
    before_lines: Vec<&'a [u8]>,
    added: u64,
    deleted: u64,
}

impl<'a> HunkWriter<'a> {
    /// Nearest "function" line above the hunk's leading context, mirroring git's
    /// default (no `xfuncname`) heuristic: first byte is a letter, `_`, or `$`.
    fn find_func(&self, before_hunk_start: u32) -> Option<&'a [u8]> {
        let ctx_start = before_hunk_start.saturating_sub(1);
        let mut idx = ctx_start as i64 - 1;
        while idx >= 0 {
            let line = trim_end_ws(self.before_lines[idx as usize]);
            if let Some(&first) = line.first() {
                if first.is_ascii_alphabetic() || first == b'_' || first == b'$' {
                    return Some(line);
                }
            }
            idx -= 1;
        }
        None
    }
}

impl ConsumeHunk for HunkWriter<'_> {
    type Out = (u64, u64);

    fn consume_hunk(
        &mut self,
        header: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        self.out.extend_from_slice(b"@@ -");
        write_range(self.out, header.before_hunk_start, header.before_hunk_len);
        self.out.extend_from_slice(b" +");
        write_range(self.out, header.after_hunk_start, header.after_hunk_len);
        self.out.extend_from_slice(b" @@");
        if let Some(func) = self.find_func(header.before_hunk_start) {
            self.out.push(b' ');
            self.out.extend_from_slice(func);
        }
        self.out.push(b'\n');

        for &(kind, content) in lines {
            self.out.push(match kind {
                DiffLineKind::Context => b' ',
                DiffLineKind::Add => {
                    self.added += 1;
                    b'+'
                }
                DiffLineKind::Remove => {
                    self.deleted += 1;
                    b'-'
                }
            });
            self.out.extend_from_slice(content);
            if !content.ends_with(b"\n") {
                self.out.push(b'\n');
                self.out
                    .extend_from_slice(b"\\ No newline at end of file\n");
            }
        }
        Ok(())
    }

    fn finish(self) -> (u64, u64) {
        (self.added, self.deleted)
    }
}

/// Port of `xdl_emit_hunk_hdr()` (xdiff): the `,len` field is omitted when the
/// hunk spans exactly one line, and an empty side is anchored to the line
/// *before* the change — which is line 0 for a file that is being created.
fn write_range(out: &mut Vec<u8>, start: u32, len: u32) {
    let start = if len == 0 { start.saturating_sub(1) } else { start };
    if len == 1 {
        let _ = write!(out, "{start}");
    } else {
        let _ = write!(out, "{start},{len}");
    }
}

/// The bytes to diff for an entry: a blob comes from the object database; a
/// submodule (commit entry) renders as its `Subproject commit <oid>` line.
fn content_of(repo: &gix::Repository, id: ObjectId, is_submodule: bool) -> Result<Vec<u8>> {
    if is_submodule {
        Ok(format!("Subproject commit {}\n", id.to_hex()).into_bytes())
    } else {
        Ok(repo.find_object(id)?.detach().data)
    }
}

/// Abbreviated object id for the `index` line. Real objects are disambiguated
/// against the odb; a submodule commit (absent here) is plainly truncated.
fn short_oid(
    repo: &gix::Repository,
    id: ObjectId,
    abbrev: usize,
    is_submodule: bool,
) -> Result<String> {
    if is_submodule {
        Ok(id.to_hex_with_len(abbrev).to_string())
    } else {
        Ok(id.attach(repo).shorten()?.to_string())
    }
}

/// The path of a change, for stable diff ordering.
fn change_path(change: &ChangeDetached) -> &[u8] {
    match change {
        ChangeDetached::Addition { location, .. }
        | ChangeDetached::Deletion { location, .. }
        | ChangeDetached::Modification { location, .. }
        | ChangeDetached::Rewrite { location, .. } => location,
    }
}

/// True when `quote_c_style()` would escape any byte of `bytes`.
fn needs_c_quote(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80)
}

fn c_escape_into(out: &mut String, bytes: &[u8]) {
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
}

/// C-style path quoting matching git's default `core.quotePath=true`, used for
/// the stat and summary columns (`quote_c_style`).
fn quote_path(path: impl AsRef<[u8]>) -> String {
    let bytes = path.as_ref();
    if !needs_c_quote(bytes) {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut out = String::from("\"");
    c_escape_into(&mut out, bytes);
    out.push('"');
    out
}

/// git's `quote_two()`: `<prefix><path>` is quoted as a whole when either half
/// needs escaping, so `a/` stays inside the quotes.
fn quote_two(prefix: &str, path: &[u8]) -> Vec<u8> {
    if !needs_c_quote(prefix.as_bytes()) && !needs_c_quote(path) {
        let mut out = prefix.as_bytes().to_vec();
        out.extend_from_slice(path);
        return out;
    }
    let mut out = String::from("\"");
    c_escape_into(&mut out, prefix.as_bytes());
    c_escape_into(&mut out, path);
    out.push('"');
    out.into_bytes()
}
