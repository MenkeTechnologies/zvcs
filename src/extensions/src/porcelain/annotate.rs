//! `git annotate` — `git blame` rendered in the CVS-compatible output format
//! (`builtin/blame.c`'s `OUTPUT_ANNOTATE_COMPAT` path), backed by `gix-blame`.
//!
//! Upstream `git annotate` is literally `git blame -c`: `builtin/annotate.c`
//! splices `-c` in front of the user's argv and calls `cmd_blame()`. So the
//! accepted option set is blame's *entire* option set, and every option must at
//! least be recognized — rejecting one changes the exit code from git's on
//! paths that never even reach the blame walk.
//!
//! Option recognition is therefore total: every option in `git annotate -h` is
//! parsed here, and anything else that starts with `-` is a usage error (129)
//! exactly like `parse_options()`. Options whose *effect* cannot be reproduced
//! on top of `gix-blame` (see `Unimplementable` below) are still parsed, still
//! participate in the error-precedence ladder, and only refuse at the point
//! where they would have changed the bytes on stdout.

use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::io::Write;
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::process::ExitCode;

use gix::bstr::ByteSlice;
use gix::hash::ObjectId;

/// git's smallest permitted abbreviation length.
const MINIMUM_ABBREV: usize = 4;

/// The first line of `git annotate -h`. stderr is not part of the parity
/// contract, but emitting the usage line keeps the shape of git's 129 paths.
const USAGE: &str = "usage: git annotate [<options>] [<rev-opts>] [<rev>] [--] <file>";

/// `git annotate` — line-by-line last-modifying commit in the CVS-compatible
/// output format, backed by `gix-blame`.
///
/// `builtin/blame.c:emit_other()` renders each line as
/// `"%.*s\t(%10s\t%10s\t%d)"` — object name, author (or `<email>` under `-e`),
/// author date, and the final line number — followed immediately by the line
/// content with no separating space. Boundary commits get no `^` marker in this
/// mode; they are only distinguishable via `-b`, which blanks the hash column.
///
/// # Error precedence
///
/// git decides failures in a fixed order, and matching the *order* is what
/// makes the exit codes line up. Verified against git 2.55.0:
///
/// | # | condition                     | code | message                                    |
/// |---|-------------------------------|------|--------------------------------------------|
/// | 1 | unknown option / bad value    | 129  | `usage: …` / `error: option …`             |
/// | 2 | no `<file>` positional        | 129  | `usage: …`                                 |
/// | 3 | `--ignore-revs-file` unreadable | 128 | `could not open object name list: <f>`     |
/// | 4 | `--ignore-rev` unresolvable   | 128  | `cannot find revision <r> to ignore`       |
/// | 5 | `--reverse` with no rev range | 128  | `No commit to dig up from?`                |
/// | 6 | path absent from `<rev>`      | 128  | `no such path '<p>' in <rev>`              |
/// | 7 | `-L` out of range / no match  | 128  | `file <p> has only <n> lines` / `no match` |
///
/// Notably `-L` is validated *after* the path resolves (`-L:x does-not-exist`
/// reports the path, not the funcname), and `--ignore-revs-file` outranks
/// `--reverse` (`--reverse --ignore-revs-file=missing f` reports the file).
pub fn annotate(args: &[String]) -> Result<ExitCode> {
    // `args[0]` is the subcommand itself when dispatched; tolerate its absence.
    let rest = match args.first() {
        Some(a) if a == "annotate" => &args[1..],
        _ => args,
    };
    let mut opts = match Options::parse(rest) {
        Parsed::Options(opts) => *opts,
        // Precedence 1 and 2: both of git's 129 paths.
        Parsed::Usage(msg) => {
            if let Some(msg) = msg {
                eprintln!("{msg}");
            }
            eprintln!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
    };

    let repo = gix::discover(".")?;

    // Command-line flags win; otherwise fall back to the two blame config knobs
    // git honours here (`blame.blankBoundary`, `blame.showRoot`).
    {
        let config = repo.config_snapshot();
        if opts.blank_boundary.is_none() {
            opts.blank_boundary = config.boolean("blame.blankBoundary");
        }
        if opts.show_root.is_none() {
            opts.show_root = config.boolean("blame.showRoot");
        }
    }
    let blank_boundary = opts.blank_boundary.unwrap_or(false);
    let show_root = opts.show_root.unwrap_or(false);

    // blame's argument DWIM (`builtin/blame.c`): split the positionals into the
    // revision arguments and the single `<file>`. This is resolution-dependent
    // (the 2-positional case asks whether the trailing arg *is a rev*), which is
    // why it runs here rather than in the pure parser.
    let rev_args = match dwim_positionals(&repo, &opts.pre, opts.post.as_deref()) {
        DwimResult::Usage => {
            eprintln!("{USAGE}");
            return Ok(ExitCode::from(129));
        }
        DwimResult::Plan { rev_args, file } => {
            opts.file = file;
            rev_args
        }
    };

    // `setup_revisions()` resolves each rev argument; the first that names no
    // object is git's "bad revision" (128). git does this *before* it opens
    // `--ignore-revs-file` (verified: `-C40 --ignore-revs-file=missing HEAD
    // does-not-exist <file>` reports the bad revision, not the missing file),
    // so it precedes precedence 3.
    let mut suspects: Vec<ObjectId> = Vec::with_capacity(rev_args.len());
    for rev in &rev_args {
        match resolve_commit(&repo, rev) {
            Some(id) => suspects.push(id),
            None => {
                eprintln!("fatal: bad revision '{rev}'");
                return Ok(ExitCode::from(128));
            }
        }
    }

    // Precedence 3: the revs-file is read before anything else touches the odb.
    let mut ignore_revs: Vec<String> = Vec::new();
    for path in &opts.ignore_revs_files {
        let body = match std::fs::read_to_string(path) {
            Ok(body) => body,
            Err(_) => {
                eprintln!("fatal: could not open object name list: {path}");
                return Ok(ExitCode::from(128));
            }
        };
        // git's `oidset` file format: one object name per line, `#` comments.
        for line in body.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if !line.is_empty() {
                ignore_revs.push(line.to_string());
            }
        }
    }
    ignore_revs.extend(opts.ignore_revs.iter().cloned());

    // Precedence 4: every ignored rev must resolve, even if no line uses it.
    let mut ignored: Vec<ObjectId> = Vec::new();
    for rev in &ignore_revs {
        match resolve_commit(&repo, rev) {
            Some(id) => ignored.push(id),
            None => {
                eprintln!("fatal: cannot find revision {rev} to ignore");
                return Ok(ExitCode::from(128));
            }
        }
    }

    // Precedence 5: `--reverse` needs a rev *range* to dig forward through. A
    // bare `git annotate --reverse <file>` has none, and git says so before it
    // ever looks at the path.
    if opts.reverse && rev_args.is_empty() {
        eprintln!("fatal: No commit to dig up from?");
        return Ok(ExitCode::from(128));
    }

    // Blame digs from exactly one commit. Two positive revs is git's "More than
    // one commit to dig from", reported *after* the ignore files are read
    // (verified: `--ignore-revs-file=missing A B <file>` opens the file first),
    // so it lands here rather than beside the bad-revision check above.
    if suspects.len() > 1 {
        eprintln!(
            "fatal: More than one commit to dig from {} and {}?",
            rev_args[1], rev_args[0]
        );
        return Ok(ExitCode::from(128));
    }

    // The suspect commit (default HEAD, peeled to a commit above). Record the
    // explicit rev string so the "no such path" messages quote HEAD vs a named
    // revision exactly as git does.
    let suspect = match suspects.first() {
        Some(id) => {
            opts.rev = Some(rev_args[0].clone());
            *id
        }
        None => repo.head_id()?.detach(),
    };

    // Translate the user's path (relative to CWD) into a repo-root-relative path.
    let rel_path = repo_relative_path(&repo, &opts.file)?;

    // Precedence 6: resolve the path against the suspect's tree explicitly,
    // rather than inferring it from a blame failure, so that `-L` validation
    // (precedence 7) can run strictly afterwards the way git orders them.
    let blob = match repo
        .find_commit(suspect)
        .ok()
        .and_then(|c| c.tree().ok())
        .and_then(|t| t.lookup_entry_by_path(&rel_path).ok().flatten())
        .and_then(|e| e.object().ok())
        .filter(|o| o.kind == gix::object::Kind::Blob)
    {
        Some(obj) => obj.into_blob().data.clone(),
        None => {
            // git quotes the path only when no explicit revision was given.
            match &opts.rev {
                Some(rev) => eprintln!("fatal: no such path {} in {rev}", opts.file),
                None => eprintln!("fatal: no such path '{}' in HEAD", opts.file),
            }
            return Ok(ExitCode::from(128));
        }
    };

    // Precedence 7: `-L` specs are resolved against the blob just found.
    let file_lines = count_lines(&blob);
    let mut ranges: Vec<RangeInclusive<u32>> = Vec::new();
    for spec in &opts.line_specs {
        match resolve_line_spec(spec, &blob, file_lines, &opts.file) {
            Ok(range) => ranges.push(range),
            Err(LineSpecError::Fatal(msg)) => {
                eprintln!("fatal: {msg}");
                return Ok(ExitCode::from(128));
            }
            Err(LineSpecError::Unimplementable(what)) => {
                bail!("annotate: {what} is not yet ported");
            }
        }
    }

    // Everything that would change stdout but cannot be reproduced faithfully
    // on top of `gix-blame` refuses here — after the whole error ladder above,
    // so the exit codes on failing paths still match git, and before a single
    // byte of output, so a wrong answer is never printed.
    if let Some(what) = opts.unimplementable() {
        bail!("annotate: {what} is not yet ported");
    }

    let ranges = if ranges.is_empty() {
        gix::blame::BlameRanges::default()
    } else {
        gix::blame::BlameRanges::from_one_based_inclusive_ranges(ranges)
            .map_err(|e| anyhow!("{e}"))?
    };
    let blame_options = gix::repository::blame_file::Options {
        diff_algorithm: opts.diff_algorithm,
        ranges,
        since: None,
        rewrites: Some(gix::diff::Rewrites::default()),
    };

    let outcome = match repo.blame_file(rel_path.as_bytes().as_bstr(), suspect, blame_options) {
        Ok(outcome) => outcome,
        Err(_) => {
            match &opts.rev {
                Some(rev) => eprintln!("fatal: no such path {} in {rev}", opts.file),
                None => eprintln!("fatal: no such path '{}' in HEAD", opts.file),
            }
            return Ok(ExitCode::from(128));
        }
    };

    // Materialize every output line; the compat format has no computed column
    // widths, so a single pass over the entries is all that is required.
    struct Line {
        commit_id: ObjectId,
        final_no: u32,
        content: Vec<u8>,
    }
    let mut lines: Vec<Line> = Vec::new();
    for (entry, tokens) in outcome.entries_with_lines() {
        let blamed_start = entry.start_in_blamed_file;
        for (i, token) in tokens.into_iter().enumerate() {
            // Line tokens include their trailing '\n'; strip exactly one so the
            // newline we append reproduces the original terminator.
            let mut content = token.to_vec();
            if content.last() == Some(&b'\n') {
                content.pop();
            }
            lines.push(Line {
                commit_id: entry.commit_id,
                final_no: blamed_start + i as u32 + 1,
                content,
            });
        }
    }

    // `--ignore-rev` only alters the answer once a line actually lands on an
    // ignored commit; git then re-attributes it to the parent using
    // `guess_line_blames()`, a walk-level heuristic `gix-blame` has no hook for.
    // Ignored revs that no line touches are a no-op, which is the common case
    // and is served exactly.
    if !ignored.is_empty() && lines.iter().any(|l| ignored.contains(&l.commit_id)) {
        bail!("annotate: --ignore-rev re-attribution of an ignored commit is not yet ported");
    }

    if lines.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    // Per-commit metadata (author display + date + boundary flag + hex), cached.
    struct CommitInfo {
        author: Vec<u8>,
        date: String,
        boundary: bool,
        hex: String,
    }
    let mut info: HashMap<ObjectId, CommitInfo> = HashMap::new();
    for line in &lines {
        if info.contains_key(&line.commit_id) {
            continue;
        }
        let commit = repo.find_commit(line.commit_id)?;
        let sig = commit.author()?;
        let author = if opts.show_email {
            let email = sig.email.to_vec();
            let mut v = Vec::with_capacity(email.len() + 2);
            v.push(b'<');
            v.extend_from_slice(&email);
            v.push(b'>');
            v
        } else {
            sig.name.to_vec()
        };
        // `-t` reproduces git's raw form (`<seconds> <tz>`); otherwise the
        // default `ISO8601` shape `YYYY-MM-DD HH:MM:SS +ZZZZ`.
        let format: gix::date::time::Format = if opts.raw_timestamp {
            gix::date::time::format::RAW
        } else {
            gix::date::time::format::ISO8601.into()
        };
        let date = sig
            .time()
            .map(|t| t.format_or_unix(format))
            .unwrap_or_else(|_| sig.time.to_string());
        // Only root commits are marked UNINTERESTING here; `--root` clears that,
        // and the flag is inert unless `-b` blanks the hash column.
        let boundary = !show_root && commit.parent_ids().next().is_none();
        info.insert(
            line.commit_id,
            CommitInfo {
                author,
                date,
                boundary,
                hex: line.commit_id.to_hex().to_string(),
            },
        );
    }

    // Abbreviation length, following git's rule: config/`--abbrev`, clamped, then
    // +1 for the boundary-marker slot (`-l` forces the full hash). Compat mode
    // never prints the `^`, but it still uses the widened length.
    let hexsz = repo.object_hash().len_in_hex();
    let mut length = if opts.long {
        hexsz
    } else {
        match opts.abbrev {
            // `--abbrev=0` means "do not abbreviate" (verified: prints 40 hex).
            Some(0) => hexsz,
            Some(n) => n.clamp(MINIMUM_ABBREV, hexsz),
            None => configured_abbrev(&repo, hexsz).clamp(MINIMUM_ABBREV, hexsz),
        }
    };
    if length < hexsz {
        length += 1;
    }

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut buf: Vec<u8> = Vec::with_capacity(128);

    for line in &lines {
        let ci = &info[&line.commit_id];
        buf.clear();

        // Object name column — blanked for boundary commits under `-b`.
        if ci.boundary && blank_boundary {
            buf.resize(buf.len() + length, b' ');
        } else {
            buf.extend_from_slice(&ci.hex.as_bytes()[..length]);
        }

        // `\t(%10s\t%10s\t%d)` then the content, with no separating space.
        buf.push(b'\t');
        buf.push(b'(');
        pad_left(&mut buf, &ci.author, 10);
        buf.push(b'\t');
        pad_left(&mut buf, ci.date.as_bytes(), 10);
        buf.push(b'\t');
        buf.extend_from_slice(line.final_no.to_string().as_bytes());
        buf.push(b')');
        buf.extend_from_slice(&line.content);
        buf.push(b'\n');

        out.write_all(&buf)?;
    }

    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Outcome of parsing: either options, or one of git's two 129 exits (with the
/// `error:` line git would have printed ahead of the usage block, if any).
enum Parsed {
    Options(Box<Options>),
    Usage(Option<String>),
}

/// Parsed command line. Every field corresponds to an option `git annotate -h`
/// lists, plus the `--reverse` rev-list option blame forwards to
/// `setup_revisions()`.
struct Options {
    /// Resolved in `annotate()` from the DWIM below; `None` when the suspect is
    /// the implicit `HEAD` (governs whether "no such path" messages quote HEAD).
    rev: Option<String>,
    /// Resolved in `annotate()` from the DWIM below.
    file: String,
    /// Positional args before a standalone `--`.
    pre: Vec<String>,
    /// Positional args after a standalone `--`; `None` when no `--` was seen.
    /// blame's DWIM (`builtin/blame.c`) branches on the presence and count of
    /// these, so the pre/post split has to survive parsing intact.
    post: Option<Vec<String>>,
    /// Raw `-L` specs, in order. Resolution needs the file, so it happens late.
    line_specs: Vec<String>,
    long: bool,
    raw_timestamp: bool,
    show_email: bool,
    /// `None` = unspecified on the command line, defer to `blame.blankBoundary`.
    blank_boundary: Option<bool>,
    /// `None` = unspecified on the command line, defer to `blame.showRoot`.
    show_root: Option<bool>,
    abbrev: Option<usize>,
    diff_algorithm: Option<gix::diff::blob::Algorithm>,
    ignore_revs: Vec<String>,
    ignore_revs_files: Vec<String>,
    reverse: bool,
    porcelain: bool,
    line_porcelain: bool,
    incremental: bool,
    show_stats: bool,
    ignore_whitespace: bool,
    find_moves: bool,
    find_copies: bool,
    contents: Option<String>,
    revs_file: Option<String>,
}

impl Options {
    /// The option, if any, that would change stdout but cannot be reproduced on
    /// top of `gix-blame`. Named rather than silently ignored: emitting the
    /// unmodified output under one of these flags would be a wrong answer
    /// dressed as a right one.
    fn unimplementable(&self) -> Option<&'static str> {
        if self.incremental {
            return Some("--incremental output");
        }
        if self.line_porcelain {
            return Some("--line-porcelain output");
        }
        if self.porcelain {
            return Some("--porcelain output");
        }
        if self.show_stats {
            // git prints its own walk counters (`num read blob` / `num get
            // patch` / `num commits`); gix-blame's `Statistics` counts
            // different events and cannot be mapped onto them.
            return Some("--show-stats counters");
        }
        if self.contents.is_some() {
            return Some("--contents");
        }
        if self.revs_file.is_some() {
            return Some("-S <revs-file>");
        }
        if self.ignore_whitespace {
            // xdiff's XDF_IGNORE_WHITESPACE has no counterpart in imara-diff,
            // which is the tokenizer `gix-blame` diffs through.
            return Some("-w");
        }
        if self.find_moves {
            return Some("-M line-move detection");
        }
        if self.find_copies {
            return Some("-C line-copy detection");
        }
        if self.reverse {
            // Reachable only with an explicit rev; the range-less form is a
            // 128 above.
            return Some("--reverse");
        }
        None
    }

    fn parse(args: &[String]) -> Parsed {
        let mut o = Options {
            rev: None,
            file: String::new(),
            pre: Vec::new(),
            post: None,
            line_specs: Vec::new(),
            long: false,
            raw_timestamp: false,
            show_email: false,
            blank_boundary: None,
            show_root: None,
            abbrev: None,
            diff_algorithm: None,
            ignore_revs: Vec::new(),
            ignore_revs_files: Vec::new(),
            reverse: false,
            porcelain: false,
            line_porcelain: false,
            incremental: false,
            show_stats: false,
            ignore_whitespace: false,
            find_moves: false,
            find_copies: false,
            contents: None,
            revs_file: None,
        };
        // `pre` = positionals before a standalone `--`; `post` = everything
        // after it (only the *first* standalone `--` separates — a later `--`
        // is an ordinary pathspec, exactly like git's argv scan).
        let mut pre: Vec<String> = Vec::new();
        let mut post: Option<Vec<String>> = None;

        // Fetch the value of an option written as a separate argument; a
        // missing value is `parse_options()`'s "requires a value" usage error.
        macro_rules! value {
            ($i:ident) => {
                match args.get($i + 1) {
                    Some(v) => {
                        $i += 1;
                        v.clone()
                    }
                    None => return Parsed::Usage(None),
                }
            };
        }

        let mut i = 0;
        while i < args.len() {
            let a = args[i].as_str();
            if let Some(p) = post.as_mut() {
                p.push(a.to_string());
                i += 1;
                continue;
            }
            match a {
                "--" => post = Some(Vec::new()),

                // Output-shape flags the compat renderer honours.
                "-l" => o.long = true,
                "-t" => o.raw_timestamp = true,
                "-e" | "--show-email" => o.show_email = true,
                "--no-show-email" => o.show_email = false,
                "-b" => o.blank_boundary = Some(true),
                "--root" => o.show_root = Some(true),
                "--no-root" => o.show_root = Some(false),

                // Inert in the compat renderer — verified byte-identical to the
                // bare invocation, because `emit_other()` never consults them.
                "-c" | "-f" | "--show-name" | "--no-show-name" | "-n" | "--show-number"
                | "--no-show-number" | "-s" | "--score-debug" | "--no-score-debug"
                | "--progress" | "--no-progress" | "--color-lines" | "--no-color-lines"
                | "--color-by-age" | "--no-color-by-age" => {}

                // Recognized, effectful, and refused later (see `unimplementable`).
                "-p" | "--porcelain" => o.porcelain = true,
                "--no-porcelain" => o.porcelain = false,
                "--line-porcelain" => o.line_porcelain = true,
                "--no-line-porcelain" => o.line_porcelain = false,
                "--incremental" => o.incremental = true,
                "--no-incremental" => o.incremental = false,
                "--show-stats" => o.show_stats = true,
                "--no-show-stats" => o.show_stats = false,
                "-w" => o.ignore_whitespace = true,
                "--reverse" => o.reverse = true,
                "--no-reverse" => o.reverse = false,

                "-L" => {
                    let spec = value!(i);
                    if !line_spec_is_wellformed(&spec) {
                        return Parsed::Usage(None);
                    }
                    o.line_specs.push(spec);
                }

                // git declares this as `--[no-]abbrev[=<n>]`: the value is
                // optional and is never taken from the following argument, so a
                // bare `--abbrev` just means "use the configured default".
                "--abbrev" => o.abbrev = None,
                "--no-abbrev" => o.abbrev = Some(usize::MAX),

                "--diff-algorithm" => {
                    let v = value!(i);
                    match parse_diff_algorithm(&v) {
                        Some(algo) => o.diff_algorithm = Some(algo),
                        None => return Parsed::Usage(Some(DIFF_ALGORITHM_ERROR.to_string())),
                    }
                }
                "--ignore-rev" => o.ignore_revs.push(value!(i)),
                "--no-ignore-rev" => o.ignore_revs.clear(),
                "--ignore-revs-file" => o.ignore_revs_files.push(value!(i)),
                "--no-ignore-revs-file" => o.ignore_revs_files.clear(),
                "--contents" => o.contents = Some(value!(i)),
                "--no-contents" => o.contents = None,
                "-S" => o.revs_file = Some(value!(i)),

                // `-M`/`-C` take an optional attached score; `-C` repeats to
                // widen the search (`-CC`, `-CCC`).
                _ if is_move_or_copy(a, b'M') => o.find_moves = true,
                _ if is_move_or_copy(a, b'C') => o.find_copies = true,

                _ if a.starts_with("-L") => {
                    let spec = a[2..].to_string();
                    if !line_spec_is_wellformed(&spec) {
                        return Parsed::Usage(None);
                    }
                    o.line_specs.push(spec);
                }
                _ if a.starts_with("--abbrev=") => {
                    let v = &a["--abbrev=".len()..];
                    match v.parse() {
                        Ok(n) => o.abbrev = Some(n),
                        Err(_) => {
                            return Parsed::Usage(Some(
                                "error: option `abbrev' expects a numerical value".to_string(),
                            ))
                        }
                    }
                }
                _ if a.starts_with("--diff-algorithm=") => {
                    match parse_diff_algorithm(&a["--diff-algorithm=".len()..]) {
                        Some(algo) => o.diff_algorithm = Some(algo),
                        None => return Parsed::Usage(Some(DIFF_ALGORITHM_ERROR.to_string())),
                    }
                }
                _ if a.starts_with("--ignore-rev=") => {
                    o.ignore_revs.push(a["--ignore-rev=".len()..].to_string());
                }
                _ if a.starts_with("--ignore-revs-file=") => {
                    o.ignore_revs_files
                        .push(a["--ignore-revs-file=".len()..].to_string());
                }
                _ if a.starts_with("--contents=") => {
                    o.contents = Some(a["--contents=".len()..].to_string());
                }
                _ if a.starts_with("-S") => o.revs_file = Some(a[2..].to_string()),

                // Anything else beginning with `-` is `parse_options()`'s
                // "unknown option" — exit 129, never a silent positional. This
                // is what makes an argument like `"-- README.md"` (a single
                // argv entry, `--` and a path glued together) a usage error.
                _ if a.starts_with('-') && a.len() > 1 => return Parsed::Usage(None),

                _ => pre.push(a.to_string()),
            }
            i += 1;
        }

        // The rev/path split (blame's DWIM) is resolution-dependent — it turns
        // on whether a positional resolves to an object — so it happens in
        // `annotate()` after the repo is open, not here.
        o.pre = pre;
        o.post = post;

        Parsed::Options(Box::new(o))
    }
}

const DIFF_ALGORITHM_ERROR: &str =
    "error: option diff-algorithm accepts \"myers\", \"minimal\", \"patience\" and \"histogram\"";

/// Map git's `--diff-algorithm` names onto `imara-diff`'s algorithms.
///
/// `patience` has no distinct implementation in `imara-diff`; its `Histogram`
/// *is* a patience diff that uses a histogram to pick the LCS, which is why
/// `gix` itself resolves `diff.algorithm=patience` to `Histogram` under lenient
/// config (`gix/src/config/tree/sections/diff.rs:9`).
fn parse_diff_algorithm(name: &str) -> Option<gix::diff::blob::Algorithm> {
    use gix::diff::blob::Algorithm;
    match name.to_ascii_lowercase().as_str() {
        "myers" | "default" => Some(Algorithm::Myers),
        "minimal" => Some(Algorithm::MyersMinimal),
        "histogram" | "patience" => Some(Algorithm::Histogram),
        _ => None,
    }
}

/// `-M[<score>]` / `-C[<score>]`, where `-C` may repeat to widen the search.
fn is_move_or_copy(arg: &str, letter: u8) -> bool {
    let bytes = arg.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'-' || bytes[1] != letter {
        return false;
    }
    let mut rest = &bytes[2..];
    // `-CC`, `-CCC`: repeated letters before an optional score.
    while rest.first() == Some(&letter) {
        rest = &rest[1..];
    }
    rest.iter().all(u8::is_ascii_digit)
}

/// Whether a `-L` spec is *syntactically* acceptable to `parse_options()`.
///
/// git rejects a malformed spec at parse time with a bare usage error (`-Lbogus`
/// → 129), but defers every *semantic* check — line 0, a start past the end of
/// the file, a funcname that matches nothing — until the file is in hand, which
/// is why those come out as 128 later.
fn line_spec_is_wellformed(spec: &str) -> bool {
    if spec.starts_with('/') || spec.starts_with(':') || spec.starts_with('^') {
        return true;
    }
    let (start, end) = match spec.split_once(',') {
        Some((s, e)) => (s, Some(e)),
        None => (spec, None),
    };
    if !is_line_number(start) {
        return false;
    }
    match end {
        None => true,
        Some(e) => is_line_number(e),
    }
}

/// An empty, plain, or `+`/`-`-relative line number.
fn is_line_number(s: &str) -> bool {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    digits.is_empty() || digits.bytes().all(|b| b.is_ascii_digit())
}

/// Why a `-L` spec could not be turned into a range.
enum LineSpecError {
    /// git dies with this message and exit 128.
    Fatal(String),
    /// The form is real but unported; refuse rather than answer wrongly.
    Unimplementable(&'static str),
}

/// Resolve one `-L` spec against the file being annotated, reproducing
/// `line-range.c:parse_range_arg()` for the forms git accepts here.
fn resolve_line_spec(
    spec: &str,
    blob: &[u8],
    file_lines: u32,
    path: &str,
) -> Result<RangeInclusive<u32>, LineSpecError> {
    if let Some(name) = spec.strip_prefix(':') {
        return resolve_funcname(name, blob, file_lines);
    }
    if spec.starts_with('/') || spec.starts_with('^') {
        return Err(LineSpecError::Unimplementable(
            "-L/<regex>/ (no regex engine is vendored)",
        ));
    }

    let (start_part, end_part) = match spec.split_once(',') {
        Some((s, e)) => (s, Some(e)),
        None => (spec, None),
    };

    let start: u32 = if start_part.is_empty() {
        1
    } else {
        start_part
            .trim_start_matches('+')
            .parse()
            .map_err(|_| LineSpecError::Fatal(format!("-L invalid line number: {start_part}")))?
    };
    if start == 0 {
        return Err(LineSpecError::Fatal("-L invalid line number: 0".into()));
    }
    if start > file_lines {
        return Err(LineSpecError::Fatal(plural_line_count(path, file_lines)));
    }

    let end: u32 = match end_part {
        // A bare `-L<n>` is `-L<n>,` — to the end of the file (verified: on a
        // 5-line file both `-L2` and `-L2,` print lines 2 through 5).
        None => file_lines,
        Some(e) if e.is_empty() => file_lines,
        Some(e) if e.starts_with('+') => {
            let count: u32 = e[1..]
                .parse()
                .map_err(|_| LineSpecError::Fatal(format!("-L invalid line number: {e}")))?;
            start.saturating_add(count.saturating_sub(1))
        }
        Some(e) if e.starts_with('-') => {
            return Err(LineSpecError::Unimplementable("-L<start>,-<n>"));
        }
        Some(e) => e
            .parse()
            .map_err(|_| LineSpecError::Fatal(format!("-L invalid line number: {e}")))?,
    };

    // git swaps an inverted range rather than rejecting it (`-L2,1` == `-L1,2`).
    let (start, end) = if end < start && end != 0 {
        (end.max(1), start)
    } else {
        (start, end.max(start))
    };
    Ok(start..=end.min(file_lines))
}

/// `-L:<funcname>` — find the first "function line" whose text contains
/// `name`, then extend to just before the next function line.
///
/// `line-range.c` scans for a line that both satisfies xdiff's function-line
/// test and matches `name` as a regex, then walks forward from two lines later
/// to the next function line. Without a vendored regex engine only literal
/// names are handled; anything with metacharacters is refused rather than
/// silently matched as a substring.
fn resolve_funcname(
    name: &str,
    blob: &[u8],
    file_lines: u32,
) -> Result<RangeInclusive<u32>, LineSpecError> {
    if name.is_empty() || name.bytes().any(is_regex_meta) {
        return Err(LineSpecError::Unimplementable(
            "-L:<regex> (no regex engine is vendored)",
        ));
    }
    let lines: Vec<&[u8]> = split_lines(blob);

    let begin = (1..=file_lines).find(|&n| {
        let line = lines[(n - 1) as usize];
        is_funcname_line(line) && line.windows(name.len()).any(|w| w == name.as_bytes())
    });
    let Some(begin) = begin else {
        // The anchor is line 1 for a leading `-L`, which is the only form here.
        return Err(LineSpecError::Fatal(format!(
            "-L parameter '{name}' starting at line 1: no match"
        )));
    };

    let end = (begin + 2..=file_lines)
        .find(|&n| is_funcname_line(lines[(n - 1) as usize]))
        .map_or(file_lines, |n| n - 1);
    Ok(begin..=end.max(begin))
}

/// xdiff's default function-line test (`xdiff/xemit.c:def_ff()`): a line that
/// starts with a letter, `_`, or `$`. Used whenever the path has no userdiff
/// driver with a `funcname` pattern, which is every path in a repo without
/// `.gitattributes` diff drivers.
fn is_funcname_line(line: &[u8]) -> bool {
    matches!(line.first(), Some(&c) if c.is_ascii_alphabetic() || c == b'_' || c == b'$')
}

fn is_regex_meta(b: u8) -> bool {
    matches!(
        b,
        b'.' | b'^'
            | b'$'
            | b'*'
            | b'+'
            | b'?'
            | b'('
            | b')'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'|'
            | b'\\'
    )
}

/// git's `Q_("file %s has only %lu line", "file %s has only %lu lines", lines)`.
fn plural_line_count(path: &str, lines: u32) -> String {
    if lines == 1 {
        format!("file {path} has only 1 line")
    } else {
        format!("file {path} has only {lines} lines")
    }
}

/// Lines of `blob`, without terminators. A trailing incomplete line counts, an
/// empty trailing piece after a final `\n` does not — matching how git counts
/// lines for `-L` bounds.
fn split_lines(blob: &[u8]) -> Vec<&[u8]> {
    let mut out: Vec<&[u8]> = blob.split(|&b| b == b'\n').collect();
    if out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out
}

fn count_lines(blob: &[u8]) -> u32 {
    split_lines(blob).len() as u32
}

/// Outcome of blame's positional DWIM: either a structural usage error (129),
/// or a plan splitting the argv into revision arguments and the single `<file>`.
enum DwimResult {
    Usage,
    Plan { rev_args: Vec<String>, file: String },
}

/// Reproduce `builtin/blame.c`'s DWIM that separates `<rev>`s from `<file>`.
///
/// git strips the path out of the argv *itself* (appending a synthetic `--`)
/// before handing the rest to `setup_revisions()`, so the rule is positional,
/// not "the last non-rev is the file". Reconstructed from git 2.55.0 behaviour:
///
/// * With a standalone `--` (`post` is `Some`):
///   - 1 arg after `--`  → that arg is the file, everything before is revs.
///   - 2 args after `--` → only the `-- <file> <rev>` legacy form, which git
///     accepts *only* as the whole command line (`argc == 4`): nothing may
///     precede the `--`. The file is the first, the rev the second. Anything
///     before `--` makes it a usage error.
///   - 0 or ≥3 args after `--` → usage error.
/// * Without a `--` (`post` is `None`), over the `pre` positionals:
///   - 0 → usage error (no `<file>`).
///   - 1 → the sole positional is the file; the suspect defaults to HEAD.
///   - 2 → if the *trailing* one names an object it is the rev and the leading
///     one is the file (`git annotate <file> <rev>`); otherwise the leading one
///     is the rev and the trailing one is the file (`git annotate <rev> <file>`).
///   - ≥3 → the last is the file; every earlier positional is a rev argument
///     (each resolved by `setup_revisions()`, so a non-object among them is a
///     "bad revision").
fn dwim_positionals(
    repo: &gix::Repository,
    pre: &[String],
    post: Option<&[String]>,
) -> DwimResult {
    match post {
        Some(post) => match post.len() {
            1 => DwimResult::Plan {
                rev_args: pre.to_vec(),
                file: post[0].clone(),
            },
            2 if pre.is_empty() => DwimResult::Plan {
                rev_args: vec![post[1].clone()],
                file: post[0].clone(),
            },
            _ => DwimResult::Usage,
        },
        None => match pre.len() {
            0 => DwimResult::Usage,
            1 => DwimResult::Plan {
                rev_args: Vec::new(),
                file: pre[0].clone(),
            },
            2 => {
                if is_a_rev(repo, &pre[1]) {
                    DwimResult::Plan {
                        rev_args: vec![pre[1].clone()],
                        file: pre[0].clone(),
                    }
                } else {
                    DwimResult::Plan {
                        rev_args: vec![pre[0].clone()],
                        file: pre[1].clone(),
                    }
                }
            }
            n => DwimResult::Plan {
                rev_args: pre[..n - 1].to_vec(),
                file: pre[n - 1].clone(),
            },
        },
    }
}

/// git's `is_a_rev()`: whether `name` resolves to an existing object. Used only
/// for the 2-positional DWIM tie-break.
fn is_a_rev(repo: &gix::Repository, name: &str) -> bool {
    repo.rev_parse_single(name).is_ok()
}

/// Resolve a revision to a commit id, peeling tags.
fn resolve_commit(repo: &gix::Repository, rev: &str) -> Option<ObjectId> {
    repo.rev_parse_single(rev)
        .ok()
        .and_then(|id| id.object().ok())
        .and_then(|obj| obj.peel_to_commit().ok())
        .map(|commit| commit.id().detach())
}

/// git's effective `core.abbrev`: an explicit number, `auto`/absent → derived
/// from the object count, or `no`/`off`/`false` → the full hash length.
fn configured_abbrev(repo: &gix::Repository, hexsz: usize) -> usize {
    match repo
        .config_snapshot()
        .string("core.abbrev")
        .as_ref()
        .and_then(|v| v.to_str().ok().map(str::to_ascii_lowercase))
    {
        None => auto_abbrev(repo, hexsz),
        Some(v) => match v.as_str() {
            "auto" => auto_abbrev(repo, hexsz),
            "no" | "off" | "false" => hexsz,
            other => other
                .parse::<usize>()
                .unwrap_or_else(|_| auto_abbrev(repo, hexsz)),
        },
    }
}

/// Auto abbreviation length: `ceil(log2(objects) / 2)`, floored at 7 — the same
/// heuristic `gix` uses for `core.abbrev = auto`.
fn auto_abbrev(repo: &gix::Repository, hexsz: usize) -> usize {
    let count = repo.objects.packed_object_count().unwrap_or(0);
    let mut len = (64 - count.leading_zeros()) as usize;
    len = len.div_ceil(2);
    len.max(7).min(hexsz)
}

/// Append `field` to `buf` right-justified in at least `width` bytes, matching
/// C's `%*s` (which pads but never truncates, and counts bytes not characters).
fn pad_left(buf: &mut Vec<u8>, field: &[u8], width: usize) {
    buf.resize(buf.len() + width.saturating_sub(field.len()), b' ');
    buf.extend_from_slice(field);
}

/// Turn a CWD-relative user path into a repo-root-relative path, so annotate
/// works from any subdirectory of the worktree (git resolves pathspecs the same
/// way).
fn repo_relative_path(repo: &gix::Repository, user_path: &str) -> Result<String> {
    let joined = match repo.workdir() {
        Some(workdir) => {
            let cwd = std::env::current_dir()?;
            let workdir_abs = workdir
                .canonicalize()
                .unwrap_or_else(|_| workdir.to_path_buf());
            let cwd_abs = cwd.canonicalize().unwrap_or(cwd);
            match cwd_abs.strip_prefix(&workdir_abs) {
                Ok(prefix) => prefix.join(user_path),
                Err(_) => PathBuf::from(user_path),
            }
        }
        None => PathBuf::from(user_path),
    };

    // Normalize `a/../b` style segments the join may have produced.
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for c in joined.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                parts.pop();
            }
            other => parts.push(other.as_os_str().to_os_string()),
        }
    }
    let normalized: PathBuf = parts.iter().collect();

    let s = normalized
        .to_str()
        .ok_or_else(|| anyhow!("path is not valid UTF-8: {user_path}"))?;
    Ok(s.to_string())
}
