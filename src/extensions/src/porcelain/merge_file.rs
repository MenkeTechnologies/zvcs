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
//! `--diff-algorithm=<myers|minimal|histogram>`, `--object-id`, `--` and the
//! `--no-` negations parse-options accepts, plus the `merge.conflictStyle`
//! config default. Unique-prefix abbreviation of long options is *not*
//! accepted, and `--diff-algorithm=patience` is refused: the vendored
//! `imara-diff` has no patience implementation, and silently substituting
//! another algorithm would change the merge result.
//!
//! The three-way line merge itself is a port of the built-in text driver from
//! the vendored `gix-merge` crate (`blob/builtin_driver/text`), inlined here
//! because the `gix/merge` feature is not enabled for this build. It carries
//! that crate's known deviations from git's `xdl_merge` around conflict-region
//! line-ending detection, so pathological inputs (mixed CRLF, missing trailing
//! newline inside a conflict) may differ from stock git by a line terminator.

use anyhow::Result;
use std::io::Write;
use std::iter::Peekable;
use std::ops::Range;
use std::process::ExitCode;

use gix::diff::blob::sources::byte_lines;
use gix::diff::blob::{Algorithm, Diff, InternedInput, Interner, Token};

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

/// The fallback line terminator when none can be detected from the input.
const LF: &[u8] = b"\n";

/// How conflicting regions are rendered when they are kept.
#[derive(Copy, Clone, Eq, PartialEq)]
enum ConflictStyle {
    /// `<<<<<<<` / `=======` / `>>>>>>>`, base hidden, hunks minimized.
    Merge,
    /// Adds a `|||||||` base section; hunks are not minimized.
    Diff3,
    /// Like `Diff3`, but our/their hunks are minimized.
    ZealousDiff3,
}

/// What to do when both sides changed the same region.
#[derive(Copy, Clone, Eq, PartialEq)]
enum Conflict {
    Keep { style: ConflictStyle, marker_size: u8 },
    Ours,
    Theirs,
    Union,
}

/// Conflict-marker annotations; default to the operands as spelled on the CLI.
struct Labels<'a> {
    current: Option<&'a [u8]>,
    ancestor: Option<&'a [u8]>,
    other: Option<&'a [u8]>,
}

/// `git merge-file` — see the module docs for the covered surface.
pub fn merge_file(args: &[String]) -> Result<ExitCode> {
    // args[0] is the subcommand name itself.
    let argv = &args[1..];

    let mut to_stdout = false;
    let mut quiet = false;
    let mut object_id = false;
    // `--ours`/`--theirs`/`--union` all write the same slot in git, so the
    // last one on the command line wins; `--no-<x>` clears it.
    let mut favor: Option<Conflict> = None;
    // Same for the two style flags.
    let mut style: Option<ConflictStyle> = None;
    let mut marker_size: u8 = 7;
    let mut algorithm = Algorithm::Myers;
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

        if let Some(long) = arg.strip_prefix("--") {
            let (name, value) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            // Boolean options reject `--opt=v` the way parse-options does.
            if value.is_some() && !matches!(name, "marker-size" | "diff-algorithm") {
                return Ok(usage_error(&format!("option `{name}' takes no value")));
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
                "ours" => favor = Some(Conflict::Ours),
                "theirs" => favor = Some(Conflict::Theirs),
                "union" => favor = Some(Conflict::Union),
                "no-ours" | "no-theirs" | "no-union" => favor = None,
                "marker-size" => {
                    let Some(v) = value.or_else(|| next_value(argv, &mut i)) else {
                        return Ok(usage_error("option `marker-size' requires a value"));
                    };
                    match v.parse::<u8>() {
                        Ok(n) => marker_size = n,
                        Err(_) => {
                            return Ok(usage_error("option `marker-size' expects a numerical value"))
                        }
                    }
                }
                "no-marker-size" => marker_size = 7,
                "diff-algorithm" => {
                    let Some(v) = value.or_else(|| next_value(argv, &mut i)) else {
                        return Ok(usage_error("option `diff-algorithm' requires a value"));
                    };
                    algorithm = match v {
                        "myers" | "default" => Algorithm::Myers,
                        "minimal" => Algorithm::MyersMinimal,
                        "histogram" => Algorithm::Histogram,
                        "patience" => anyhow::bail!(
                            "--diff-algorithm=patience is unsupported (ported: myers, minimal, histogram)"
                        ),
                        other => {
                            return Ok(usage_error(&format!("option diff-algorithm accepts \"myers\", \"minimal\", \"patience\" and \"histogram\", got \"{other}\"")))
                        }
                    };
                }
                other => return Ok(usage_error(&format!("unknown option `{other}'"))),
            }
            continue;
        }

        // Short options, grouped left to right. `-L` consumes the rest of the
        // token as its value, or the next argument if the token ends there.
        let mut chars = arg[1..].char_indices();
        while let Some((at, c)) = chars.next() {
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
                            None => return Ok(usage_error("switch `L' requires a value")),
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

    // The repository is mandatory for `--object-id`, and otherwise consulted
    // only for `merge.conflictStyle`.
    let repo = gix::discover(".").ok();
    if object_id && repo.is_none() {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        return Ok(ExitCode::from(128));
    }

    let style = match style {
        Some(style) => style,
        None => repo.as_ref().map_or(ConflictStyle::Merge, config_style),
    };
    let conflict = favor.unwrap_or(Conflict::Keep { style, marker_size });

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
        current: Some(label_args.first().map_or(operands[0], String::as_str).as_bytes()),
        ancestor: Some(label_args.get(1).map_or(operands[1], String::as_str).as_bytes()),
        other: Some(label_args.get(2).map_or(operands[2], String::as_str).as_bytes()),
    };

    let (merged, conflicts) = three_way_merge(
        &contents[0],
        &contents[1],
        &contents[2],
        &labels,
        conflict,
        algorithm,
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

/// Report a bad command line the way parse-options does: reason then usage on
/// stderr, exit 129.
fn usage_error(reason: &str) -> ExitCode {
    eprintln!("error: {reason}");
    eprint!("{USAGE}");
    ExitCode::from(129)
}

/// The `merge.conflictStyle` default; unknown values fall back to `merge`.
fn config_style(repo: &gix::Repository) -> ConflictStyle {
    let snapshot = repo.config_snapshot();
    let Some(value) = snapshot.string("merge.conflictStyle") else {
        return ConflictStyle::Merge;
    };
    match value.as_slice() {
        b"diff3" => ConflictStyle::Diff3,
        b"zdiff3" => ConflictStyle::ZealousDiff3,
        _ => ConflictStyle::Merge,
    }
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

// ---------------------------------------------------------------------------
// Three-way line merge — port of `gix-merge`'s built-in text driver.
// ---------------------------------------------------------------------------

/// Which buffer a hunk's lines come from.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum Side {
    Current,
    Other,
    /// Marker for filler hunks that only point into the ancestor.
    Ancestor,
}

/// A change region: `before` indexes ancestor tokens, `after` indexes `side`'s.
#[derive(Debug, Clone)]
struct Hunk {
    before: Range<u32>,
    after: Range<u32>,
    side: Side,
}

/// Merge `current` and `other` over the common `ancestor`.
///
/// Returns the merged bytes and the number of conflict regions left in them
/// (always zero when `conflict` resolves conflicts automatically).
fn three_way_merge<'a>(
    current: &'a [u8],
    ancestor: &'a [u8],
    other: &'a [u8],
    labels: &Labels<'_>,
    conflict: Conflict,
    algorithm: Algorithm,
) -> (Vec<u8>, usize) {
    let mut input: InternedInput<&'a [u8]> = InternedInput::default();
    input.update_before(byte_lines(ancestor));
    input.update_after(byte_lines(current));
    let hunks = collect_hunks(algorithm, &input, Side::Current, Vec::new());

    // Interning is shared, so the current-side tokens stay valid once `after`
    // is re-filled with the other side.
    let current_tokens = std::mem::take(&mut input.after);
    input.update_after(byte_lines(other));
    let mut hunks = collect_hunks(algorithm, &input, Side::Other, hunks);
    hunks.sort_by_key(|h| h.before.start);

    let input = &input;
    let current_tokens = &current_tokens[..];
    let mut out = Vec::new();
    let mut conflicts = 0usize;

    if hunks.is_empty() {
        write_ancestor(input, 0, input.before.len(), &mut out);
        return (out, 0);
    }

    let mut hunks = hunks.into_iter().peekable();
    let mut intersecting = Vec::new();
    let mut ancestor_integrated_until = 0;
    let mut current_hunks = Vec::with_capacity(2);

    while take_intersecting(&mut hunks, &mut current_hunks, &mut intersecting).is_some() {
        if intersecting.is_empty() {
            // A change only one side made: take it verbatim.
            let hunk = current_hunks.pop().expect("pushed during intersection check");
            write_ancestor(input, ancestor_integrated_until, hunk.before.start as usize, &mut out);
            ancestor_integrated_until = hunk.before.end;
            write_hunks(std::slice::from_ref(&hunk), input, current_tokens, &mut out);
            continue;
        }

        let filled_hunks_side = current_hunks.first().expect("at least one hunk").side;
        {
            let filled = before_range_from_hunks(&current_hunks);
            let other_range = before_range_from_hunks(&intersecting);
            let extended = filled.start..other_range.end.max(filled.end);
            fill_ancestor(&extended, &mut current_hunks);
            fill_ancestor(&extended, &mut intersecting);
        }

        match conflict {
            Conflict::Keep { style, marker_size } => {
                let (front_and_back, num_front) = match style {
                    ConflictStyle::Merge | ConflictStyle::ZealousDiff3 => {
                        zealously_contract_hunks(&mut current_hunks, &mut intersecting, input, current_tokens)
                    }
                    ConflictStyle::Diff3 => (Vec::new(), 0),
                };
                let (ours, theirs) = ours_and_theirs(filled_hunks_side, &current_hunks, &intersecting);
                let (front, back) = front_and_back.split_at(num_front);
                let first = first_hunk(front, ours, theirs, back);
                let last = last_hunk(front, ours, theirs, back);

                write_ancestor(input, ancestor_integrated_until, first.before.start as usize, &mut out);
                write_hunks(front, input, current_tokens, &mut out);

                // With nothing kept in front, the ancestor lines just written
                // are what the marker's line ending is taken from.
                let filler = Hunk {
                    before: ancestor_integrated_until..first.before.start,
                    after: 0..0,
                    side: Side::Ancestor,
                };
                let before_markers: &[Hunk] = if front.is_empty() {
                    std::slice::from_ref(&filler)
                } else {
                    front
                };
                let nl = detect_line_ending(before_markers, input, current_tokens)
                    .or_else(|| detect_line_ending(ours, input, current_tokens))
                    .unwrap_or(LF);

                if contains_lines(ours) || contains_lines(theirs) {
                    match style {
                        ConflictStyle::Merge => {
                            conflicts += 1;
                            write_conflict_marker(&mut out, b'<', labels.current, marker_size, nl);
                            write_hunks(ours, input, current_tokens, &mut out);
                            write_conflict_marker(&mut out, b'=', None, marker_size, nl);
                            write_hunks(theirs, input, current_tokens, &mut out);
                            write_conflict_marker(&mut out, b'>', labels.other, marker_size, nl);
                        }
                        ConflictStyle::Diff3 | ConflictStyle::ZealousDiff3 => {
                            if hunks_differ_in_diff3(style, ours, theirs, input, current_tokens) {
                                conflicts += 1;
                                write_conflict_marker(&mut out, b'<', labels.current, marker_size, nl);
                                write_hunks(ours, input, current_tokens, &mut out);
                                let base = Hunk {
                                    before: first.before.start..last.before.end,
                                    after: 0..0,
                                    side: Side::Ancestor,
                                };
                                let base = std::slice::from_ref(&base);
                                let base_nl = detect_line_ending(base, input, current_tokens).unwrap_or(LF);
                                write_conflict_marker(&mut out, b'|', labels.ancestor, marker_size, base_nl);
                                write_hunks(base, input, current_tokens, &mut out);
                                write_conflict_marker(&mut out, b'=', None, marker_size, nl);
                                write_hunks(theirs, input, current_tokens, &mut out);
                                write_conflict_marker(&mut out, b'>', labels.other, marker_size, nl);
                            } else {
                                write_hunks(ours, input, current_tokens, &mut out);
                            }
                        }
                    }
                }

                write_hunks(back, input, current_tokens, &mut out);
                ancestor_integrated_until = last.before.end;
            }
            Conflict::Ours | Conflict::Theirs => {
                let (ours, theirs) = ours_and_theirs(filled_hunks_side, &current_hunks, &intersecting);
                let chosen = if conflict == Conflict::Ours { ours } else { theirs };
                if let Some(first) = chosen.first() {
                    write_ancestor(input, ancestor_integrated_until, first.before.start as usize, &mut out);
                }
                write_hunks(chosen, input, current_tokens, &mut out);
                if let Some(last) = chosen.last() {
                    ancestor_integrated_until = last.before.end;
                }
            }
            Conflict::Union => {
                let (front_and_back, num_front) =
                    zealously_contract_hunks(&mut current_hunks, &mut intersecting, input, current_tokens);
                let (ours, theirs) = ours_and_theirs(filled_hunks_side, &current_hunks, &intersecting);
                let (front, back) = front_and_back.split_at(num_front);
                let first = first_hunk(front, ours, theirs, back);

                write_ancestor(input, ancestor_integrated_until, first.before.start as usize, &mut out);
                write_hunks(front, input, current_tokens, &mut out);
                assure_ends_with_nl(&mut out, detect_line_ending(front, input, current_tokens).unwrap_or(LF));
                write_hunks(ours, input, current_tokens, &mut out);
                assure_ends_with_nl(&mut out, detect_line_ending(ours, input, current_tokens).unwrap_or(LF));
                write_hunks(theirs, input, current_tokens, &mut out);
                if !back.is_empty() {
                    assure_ends_with_nl(&mut out, detect_line_ending(theirs, input, current_tokens).unwrap_or(LF));
                }
                write_hunks(back, input, current_tokens, &mut out);
                ancestor_integrated_until = last_hunk(front, ours, theirs, back).before.end;
            }
        }
    }

    write_ancestor(input, ancestor_integrated_until, input.before.len(), &mut out);
    (out, conflicts)
}

/// Assign the two hunk lists to our/their side.
fn ours_and_theirs<'h>(
    filled_side: Side,
    current_hunks: &'h [Hunk],
    intersecting: &'h [Hunk],
) -> (&'h [Hunk], &'h [Hunk]) {
    match filled_side {
        Side::Current => (current_hunks, intersecting),
        Side::Other => (intersecting, current_hunks),
        Side::Ancestor => unreachable!("initial hunks are never ancestors"),
    }
}

/// Diff `input` and record the resulting hunks as belonging to `side`.
fn collect_hunks(
    algorithm: Algorithm,
    input: &InternedInput<&[u8]>,
    side: Side,
    mut hunks: Vec<Hunk>,
) -> Vec<Hunk> {
    let mut diff = Diff::compute(algorithm, input);
    diff.postprocess_lines(input);
    hunks.extend(diff.hunks().map(|h| Hunk {
        before: h.before,
        after: h.after,
        side,
    }));
    hunks
}

/// Pull the next hunk plus every following hunk of the *other* side that
/// overlaps it, splitting chains of mutual overlap until they close.
fn take_intersecting(
    iter: &mut Peekable<impl Iterator<Item = Hunk>>,
    input: &mut Vec<Hunk>,
    intersecting: &mut Vec<Hunk>,
) -> Option<()> {
    input.clear();
    input.push(iter.next()?);
    intersecting.clear();

    fn left_overlaps_right(left: &Hunk, right: &Hunk) -> bool {
        left.side != right.side
            && (right.before.contains(&left.before.start)
                || (right.before.is_empty() && right.before.start == left.before.start))
    }

    loop {
        let hunk = input.last().expect("just pushed");
        while iter.peek().filter(|b| left_overlaps_right(b, hunk)).is_some() {
            intersecting.extend(iter.next());
        }
        let mut found_more = false;
        while intersecting
            .last_mut()
            .zip(iter.peek_mut())
            .filter(|(last, candidate)| left_overlaps_right(candidate, last))
            .is_some()
        {
            input.extend(iter.next());
            found_more = true;
        }
        if !found_more {
            break;
        }
    }
    Some(())
}

/// Insert ancestor-only filler hunks so `in_out` covers `range` contiguously.
fn fill_ancestor(Range { start, end }: &Range<u32>, in_out: &mut Vec<Hunk>) {
    fn is_nonzero(n: &u32) -> bool {
        *n > 0
    }
    if in_out.is_empty() {
        return;
    }
    let mut first_idx = 0;
    if let Some(lines) = in_out[0].before.start.checked_sub(*start).filter(is_nonzero) {
        in_out.insert(0, ancestor_hunk(*start, lines));
        first_idx += 1;
    }

    let mut added = false;
    for (idx, next_idx) in (first_idx..in_out.len()).map(|idx| (idx, idx + 1)) {
        let Some(next) = in_out.get(next_idx) else { break };
        let hunk = &in_out[idx];
        if let Some(lines) = next.before.start.checked_sub(hunk.before.end).filter(is_nonzero) {
            in_out.push(ancestor_hunk(hunk.before.end, lines));
            added = true;
        }
    }
    let len = in_out.len();
    if added {
        in_out[first_idx..len].sort_by_key(|h| h.before.start);
    }

    let last_end = in_out[len - 1].before.end;
    if let Some(lines) = end.checked_sub(last_end).filter(is_nonzero) {
        in_out.push(ancestor_hunk(last_end, lines));
    }
}

fn ancestor_hunk(start: u32, num_lines: u32) -> Hunk {
    let range = start..start + num_lines;
    Hunk {
        before: range.clone(),
        after: range,
        side: Side::Ancestor,
    }
}

/// Shrink `a_hunks`/`b_hunks` to the lines that actually differ, moving the
/// common leading and trailing lines into the returned list. The second
/// element is how many of those go in front; the rest go behind.
#[must_use]
fn zealously_contract_hunks(
    a_hunks: &mut Vec<Hunk>,
    b_hunks: &mut Vec<Hunk>,
    input: &InternedInput<&[u8]>,
    current_tokens: &[Token],
) -> (Vec<Hunk>, usize) {
    let line = |token_idx: u32, side: Side| {
        let tokens = tokens_for_side(side, input, current_tokens);
        input.interner[tokens[token_idx as usize]]
    };
    let (mut last_a, mut last_b) = (0, 0);

    let (mut out, hunks_in_front) = {
        let (mut remove_a_from, mut remove_b_from) = (None, None);
        let (mut a_equal_till, mut b_equal_till) = (None, None);
        for ((a_tok, a_idx, a_side), (b_tok, b_idx, b_side)) in
            iterate_hunks(a_hunks).zip(iterate_hunks(b_hunks))
        {
            if last_a != a_idx {
                a_equal_till = None;
                last_a = a_idx;
            }
            if last_b != b_idx {
                b_equal_till = None;
                last_b = b_idx;
            }
            if line(a_tok, a_side) == line(b_tok, b_side) {
                (remove_a_from, remove_b_from) = (Some(a_idx), Some(b_idx));
                (a_equal_till, b_equal_till) = (Some(a_tok), Some(b_tok));
            } else {
                break;
            }
        }

        let mut out = Vec::with_capacity(
            remove_a_from.unwrap_or(usize::from(a_equal_till.is_some())),
        );
        truncate_from_front(a_hunks, remove_a_from, a_equal_till, Some(&mut out));
        truncate_from_front(b_hunks, remove_b_from, b_equal_till, None);
        let hunks_in_front = out.len();
        (out, hunks_in_front)
    };

    (last_a, last_b) = (0, 0);
    {
        let (mut remove_a_from, mut remove_b_from) = (None, None);
        let (mut a_equal_from, mut b_equal_from) = (None, None);
        for ((a_tok, a_idx, a_side), (b_tok, b_idx, b_side)) in
            iterate_hunks_rev(a_hunks).zip(iterate_hunks_rev(b_hunks))
        {
            if last_a != a_idx {
                a_equal_from = None;
                last_a = a_idx;
            }
            if last_b != b_idx {
                b_equal_from = None;
                last_b = b_idx;
            }
            if line(a_tok, a_side) == line(b_tok, b_side) {
                (remove_a_from, remove_b_from) = (Some(a_idx), Some(b_idx));
                (a_equal_from, b_equal_from) = (Some(a_tok), Some(b_tok));
            } else {
                break;
            }
        }

        truncate_from_back(a_hunks, remove_a_from, a_equal_from, Some(&mut out));
        truncate_from_back(b_hunks, remove_b_from, b_equal_from, None);
    }

    (out, hunks_in_front)
}

/// The range a hunk contributes lines from: `after` for real sides, `before`
/// for ancestor filler.
fn range_by_side(hunk: &mut Hunk) -> &mut Range<u32> {
    match hunk.side {
        Side::Current | Side::Other => &mut hunk.after,
        Side::Ancestor => &mut hunk.before,
    }
}

fn truncate_from_front(
    hunks: &mut Vec<Hunk>,
    remove_until_idx: Option<usize>,
    equal_till: Option<u32>,
    mut out_hunks: Option<&mut Vec<Hunk>>,
) {
    let Some(remove_until_idx) = remove_until_idx else {
        return;
    };
    let mut last_index_to_remove = Some(remove_until_idx);
    let hunk = &mut hunks[remove_until_idx];
    let range = range_by_side(hunk);
    if let Some(equal_till) = equal_till {
        let orig_start = range.start;
        let new_start = equal_till + 1;
        range.start = new_start;
        if Range::<u32>::is_empty(range) {
            range.start = orig_start;
        } else {
            last_index_to_remove = remove_until_idx.checked_sub(1);
            if let Some(out) = out_hunks.as_deref_mut() {
                let mut removed = hunk.clone();
                let new_range = range_by_side(&mut removed);
                new_range.start = orig_start;
                new_range.end = new_start;
                out.push(removed);
            }
        }
    }
    if let Some(last_index_to_remove) = last_index_to_remove {
        let mut idx = 0;
        hunks.retain(|hunk| {
            if idx > last_index_to_remove {
                true
            } else {
                idx += 1;
                if let Some(out) = out_hunks.as_deref_mut() {
                    out.push(hunk.clone());
                }
                false
            }
        });
    }
}

fn truncate_from_back(
    hunks: &mut Vec<Hunk>,
    remove_from_idx: Option<usize>,
    equal_from: Option<u32>,
    mut out_hunks: Option<&mut Vec<Hunk>>,
) {
    let Some(mut remove_from_idx) = remove_from_idx else {
        return;
    };
    let hunk = &mut hunks[remove_from_idx];
    let range = range_by_side(hunk);
    if let Some(equal_from) = equal_from {
        let orig_end = range.end;
        let new_end = equal_from;
        range.end = new_end;
        if Range::<u32>::is_empty(range) {
            range.end = orig_end;
        } else {
            remove_from_idx += 1;
            if let Some(out) = out_hunks.as_deref_mut() {
                let mut removed = hunk.clone();
                let new_range = range_by_side(&mut removed);
                new_range.start = new_end;
                new_range.end = orig_end;
                out.push(removed);
            }
        }
    }
    if let Some(out) = out_hunks {
        out.extend_from_slice(&hunks[remove_from_idx..]);
    }
    hunks.truncate(remove_from_idx);
}

/// `(token index, hunk index, side)` over every line the hunks contribute.
fn iterate_hunks(hunks: &[Hunk]) -> impl Iterator<Item = (u32, usize, Side)> + '_ {
    hunks.iter().enumerate().flat_map(|(hunk_idx, hunk)| {
        contributed_range(hunk)
            .clone()
            .map(move |idx| (idx, hunk_idx, hunk.side))
    })
}

/// Same as [`iterate_hunks`], from the end backwards.
fn iterate_hunks_rev(hunks: &[Hunk]) -> impl Iterator<Item = (u32, usize, Side)> + '_ {
    hunks.iter().enumerate().rev().flat_map(|(hunk_idx, hunk)| {
        contributed_range(hunk)
            .clone()
            .rev()
            .map(move |idx| (idx, hunk_idx, hunk.side))
    })
}

fn contributed_range(hunk: &Hunk) -> &Range<u32> {
    match hunk.side {
        Side::Current | Side::Other => &hunk.after,
        Side::Ancestor => &hunk.before,
    }
}

fn tokens_for_side<'a>(
    side: Side,
    input: &'a InternedInput<&[u8]>,
    current_tokens: &'a [Token],
) -> &'a [Token] {
    match side {
        Side::Current => current_tokens,
        Side::Other => &input.after,
        Side::Ancestor => &input.before,
    }
}

/// Whether any hunk actually contributes a line.
fn contains_lines(hunks: &[Hunk]) -> bool {
    hunks.iter().any(|h| !h.after.is_empty())
}

/// In plain `diff3`, identical changes on both sides are not a conflict.
/// Every other style always treats the region as differing.
fn hunks_differ_in_diff3(
    style: ConflictStyle,
    a: &[Hunk],
    b: &[Hunk],
    input: &InternedInput<&[u8]>,
    current_tokens: &[Token],
) -> bool {
    if style != ConflictStyle::Diff3 {
        return true;
    }
    fn tokens_of<'a>(
        hunk: &Hunk,
        input: &'a InternedInput<&[u8]>,
        current_tokens: &'a [Token],
    ) -> &'a [Token] {
        let range = &hunk.after;
        &tokens_for_side(hunk.side, input, current_tokens)[range.start as usize..range.end as usize]
    }
    a.iter()
        .flat_map(|h| tokens_of(h, input, current_tokens))
        .ne(b.iter().flat_map(|h| tokens_of(h, input, current_tokens)))
}

/// The line terminator to use for markers next to `hunks`, if it can be told.
///
/// This is the deviation from git's `xdl_merge` noted in the module docs.
fn detect_line_ending(
    hunks: &[Hunk],
    input: &InternedInput<&[u8]>,
    current_tokens: &[Token],
) -> Option<&'static [u8]> {
    fn is_crlf(
        hunks: &[Hunk],
        input: &InternedInput<&[u8]>,
        current_tokens: &[Token],
    ) -> Option<bool> {
        let (range, side) = hunks.iter().rev().find_map(|h| {
            (!h.after.is_empty())
                .then_some((&h.after, h.side))
                .or((!h.before.is_empty()).then_some((&h.before, Side::Ancestor)))
        })?;
        let tokens = tokens_for_side(side, input, current_tokens);
        {
            let last = tokens.get(range.end as usize - 1).map(|t| input.interner[*t])?;
            if last.last() == Some(&b'\n') {
                return last.get(last.len().checked_sub(2)?).map(|c| *c == b'\r');
            }
        }
        let second_to_last = tokens
            .get(range.end.checked_sub(2)? as usize)
            .map(|t| input.interner[*t])?;
        second_to_last
            .get(second_to_last.len().checked_sub(2)?)
            .map(|c| *c == b'\r')
    }
    is_crlf(hunks, input, current_tokens).map(|crlf| if crlf { &b"\r\n"[..] } else { &b"\n"[..] })
}

fn assure_ends_with_nl(out: &mut Vec<u8>, nl: &[u8]) {
    if !out.is_empty() && !out.ends_with(b"\n") {
        out.extend_from_slice(nl);
    }
}

fn write_conflict_marker(
    out: &mut Vec<u8>,
    marker: u8,
    label: Option<&[u8]>,
    marker_size: u8,
    nl: &[u8],
) {
    assure_ends_with_nl(out, nl);
    out.extend(std::iter::repeat(marker).take(marker_size as usize));
    if let Some(label) = label {
        out.push(b' ');
        out.extend_from_slice(label);
    }
    out.extend_from_slice(nl);
}

/// Copy untouched ancestor lines `[from, to)` through to the output.
fn write_ancestor(input: &InternedInput<&[u8]>, from: u32, to: usize, out: &mut Vec<u8>) {
    if to < from as usize {
        return;
    }
    if let Some(tokens) = input.before.get(from as usize..to) {
        write_tokens(&input.interner, tokens, out);
    }
}

fn write_hunks(
    hunks: &[Hunk],
    input: &InternedInput<&[u8]>,
    current_tokens: &[Token],
    out: &mut Vec<u8>,
) {
    for hunk in hunks {
        let (tokens, range) = match hunk.side {
            Side::Current => (current_tokens, &hunk.after),
            Side::Other => (input.after.as_slice(), &hunk.after),
            Side::Ancestor => (input.before.as_slice(), &hunk.before),
        };
        write_tokens(
            &input.interner,
            &tokens[range.start as usize..range.end as usize],
            out,
        );
    }
}

fn write_tokens(interner: &Interner<&[u8]>, tokens: &[Token], out: &mut Vec<u8>) {
    for token in tokens {
        out.extend_from_slice(interner[*token]);
    }
}

fn first_hunk<'a>(front: &'a [Hunk], ours: &'a [Hunk], theirs: &'a [Hunk], back: &'a [Hunk]) -> &'a Hunk {
    front
        .first()
        .or(ours.first())
        .or(theirs.first())
        .or(back.first())
        .expect("at least one hunk anywhere")
}

fn last_hunk<'a>(front: &'a [Hunk], ours: &'a [Hunk], theirs: &'a [Hunk], back: &'a [Hunk]) -> &'a Hunk {
    back.last()
        .or(theirs.last())
        .or(ours.last())
        .or(front.last())
        .expect("at least one hunk anywhere")
}

fn before_range_from_hunks(hunks: &[Hunk]) -> Range<u32> {
    hunks
        .first()
        .zip(hunks.last())
        .map(|(f, l)| f.before.start..l.before.end)
        .expect("at least one entry")
}
