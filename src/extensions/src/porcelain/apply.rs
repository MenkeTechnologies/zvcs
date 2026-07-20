//! `git apply` — read a unified diff and apply it to the working tree.
//!
//! Unlike most modules here, `apply` has no gitoxide substrate to lean on: the
//! vendored crates ship a diff *producer* (`gix-diff`, `gix-imara-diff`) but no
//! patch *parser* or *applier*. The unified-diff parse and the hunk placement
//! search below are therefore a direct port of git's `apply.c` — specifically
//! `parse_fragment`, `find_pos` (the alternating backwards/forwards scan) and
//! `match_fragment`'s `match_beginning` / `match_end` constraints — so hunk
//! placement, offset tolerance and failure points land where stock git puts
//! them.
//!
//! Supported (output, exit code and resulting worktree match stock git):
//!   * `git apply <patch>...` / stdin (no operand, or `-`)
//!   * `-p<n>`, `-R`/`--reverse`, `--check`, `--numstat`, `-z`, `--apply`,
//!     `--allow-empty`, `--unidiff-zero`, `--binary`/`--allow-binary-replacement`
//!     (accepted, no-op as in modern git), `-q`/`--quiet`,
//!     `--whitespace=warn|nowarn`, `--recount`, `--directory=<root>`, `--`,
//!     and the `--no-` form of each of git's negatable options
//!   * usage errors: unknown option/switch (git's own usage block on stderr,
//!     exit 129), a missing or non-integer option value, an unrecognised
//!     `--whitespace` action, and `--ours`/`--theirs`/`--union` without `--3way`
//!     (`fatal:`, exit 128)
//!   * patch kinds: modification, creation, deletion, rename, mode change, and
//!     symlink blobs; git-style (`diff --git`) and traditional `---`/`+++` diffs
//!
//! Faithful to git on the write side: the whole patch is validated before any
//! file is touched (atomicity), targets are removed and re-created rather than
//! rewritten in place (so the resulting mode is the patch's mode under the
//! process umask, not the old file's), leading directories are created for new
//! paths, and directories emptied by a deletion or rename are pruned.
//!
//! Argument parsing covers git's whole `apply` option table, because git's own
//! ordering makes that observable: it finishes parsing, runs its usage-level
//! validations, *then* opens the patch files, *then* parses them. A flag this
//! port cannot honour is therefore recorded during parsing and only reported
//! once the input is known to contain at least one patch — the first moment
//! ignoring it could change a result. Until that moment git has not consulted it
//! either, so `git apply --stat missing-file` and `git apply --3way not-a-patch`
//! report what git reports (`can't open patch` / `No valid patches in input`,
//! exit 128) rather than a premature unsupported-flag error.
//!
//! Not implemented — these `bail!` rather than produce plausible-looking wrong
//! results: `--index`/`--cached`/`-N`/`--intent-to-add` (index mutation),
//! `-3`/`--3way` and `--ours`/`--theirs`/`--union` (3-way merge), `--reject`,
//! `--stat`/`--summary` (git's scaled diffstat renderer), `--exclude`/`--include`
//! (path filtering), `--build-fake-ancestor`, `-C<n>` (context reduction),
//! `--no-add`, `--allow-overlap`, `--inaccurate-eof`, `--unsafe-paths`,
//! `-v`/`--verbose`, the whitespace-fixing `--whitespace` actions
//! (`fix`/`strip`/`error`/`error-all`), `--ignore-whitespace`/
//! `--ignore-space-change`, copy patches, binary patches, non-UTF-8 paths, and
//! running from a subdirectory of the worktree (git reinterprets patch paths
//! against the repo prefix there).
//!
//! Whitespace-error warnings (git's default `--whitespace=warn`) are not
//! emitted; they go to stderr only and never alter the applied content.
//!
//! `-q`/`--quiet` silences every `error:` diagnostic, matching git, where they
//! all go through `error()`; exit codes are unaffected, and `fatal:` messages and
//! usage errors are not silenced.

use anyhow::{bail, Result};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The flag set this port honours, quoted verbatim in the unsupported-flag error.
const PORTED: &str = "-p<n>, -R/--reverse, --check, --numstat, -z, --apply, \
                      --allow-empty, --unidiff-zero, --binary, -q/--quiet, \
                      --whitespace=warn|nowarn, --recount, --directory=<root>";

/// git's `apply` usage block, printed after `unknown option`/`unknown switch` on
/// stderr with exit 129 (`parse-options`' `PARSE_OPT_ERROR`).
const USAGE: &str = r"usage: git apply [<options>] [<patch>...]

    --exclude <path>      don't apply changes matching the given path
    --include <path>      apply changes matching the given path
    -p <num>              remove <num> leading slashes from traditional diff paths
    --no-add              ignore additions made by the patch
    --add                 opposite of --no-add
    --[no-]stat           instead of applying the patch, output diffstat for the input
    --[no-]numstat        show number of added and deleted lines in decimal notation
    --[no-]summary        instead of applying the patch, output a summary for the input
    --[no-]check          instead of applying the patch, see if the patch is applicable
    --[no-]index          make sure the patch is applicable to the current index
    -N, --[no-]intent-to-add
                          mark new files with `git add --intent-to-add`
    --[no-]cached         apply a patch without touching the working tree
    --[no-]unsafe-paths   accept a patch that touches outside the working area
    --[no-]apply          also apply the patch (use with --stat/--summary/--check)
    -3, --[no-]3way       attempt three-way merge, fall back on normal patch if that fails
    --ours                for conflicts, use our version
    --theirs              for conflicts, use their version
    --union               for conflicts, use a union version
    --[no-]build-fake-ancestor <file>
                          build a temporary index based on embedded index information
    -z                    paths are separated with NUL character
    -C <n>                ensure at least <n> lines of context match
    --[no-]whitespace <action>
                          detect new or modified lines that have whitespace errors
    --[no-]ignore-space-change
                          ignore changes in whitespace when finding context
    --[no-]ignore-whitespace
                          ignore changes in whitespace when finding context
    -R, --[no-]reverse    apply the patch in reverse
    --[no-]unidiff-zero   don't expect at least one line of context
    --[no-]reject         leave the rejected hunks in corresponding *.rej files
    --[no-]allow-overlap  allow overlapping hunks
    -v, --[no-]verbose    be more verbose
    -q, --[no-]quiet      be more quiet
    --[no-]inaccurate-eof tolerate incorrectly detected missing new-line at the end of file
    --[no-]recount        do not trust the line counts in the hunk headers
    --[no-]directory <root>
                          prepend <root> to all filenames
    --[no-]allow-empty    don't return error for empty patches

";

// Reasons quoted back in the deferred unsupported-flag error.
const R_INDEX: &str = "index mutation is not implemented";
const R_3WAY: &str = "3-way merge is not implemented";
const R_REJECT: &str = "reject files are not implemented";
const R_STAT: &str = "the diffstat renderer is not implemented";
const R_PATHSPEC: &str = "path filtering is not implemented";
const R_CONTEXT: &str = "context reduction is not implemented";
const R_WS: &str = "whitespace fixing is not implemented";
const R_IGNORE_WS: &str = "whitespace-insensitive matching is not implemented";
const R_EOF: &str = "EOF-newline fudging is not implemented";
const R_NOADD: &str = "dropping additions is not implemented";
const R_OVERLAP: &str = "overlapping hunks are not implemented";
const R_VERBOSE: &str = "verbose progress output is not implemented";
const R_ANCESTOR: &str = "building a fake ancestor index is not implemented";
const R_UNSAFE: &str = "paths outside the working area are not implemented";

/// A flag git accepts that this port parses but cannot honour: the spelling as
/// the user wrote it, plus why. `key` exists so a later `--no-<flag>` cancels the
/// right entry; the vector keeps argv order, so the flag reported is the first
/// unhonoured one on the command line.
struct Unhonoured {
    key: &'static str,
    spelling: String,
    why: &'static str,
}

fn mark(v: &mut Vec<Unhonoured>, key: &'static str, spelling: &str, why: &'static str) {
    v.retain(|u| u.key != key);
    v.push(Unhonoured {
        key,
        spelling: spelling.to_owned(),
        why,
    });
}

fn unmark(v: &mut Vec<Unhonoured>, key: &'static str) {
    v.retain(|u| u.key != key);
}

/// Parsed command-line options for a single `apply` invocation. Only the flags
/// this port honours get a field; the rest live in the `Unhonoured` list.
struct Opts {
    strip: usize,               // -p<n>: leading path components to drop (default 1)
    reverse: bool,              // -R/--reverse: swap pre- and post-image
    check: bool,                // --check: validate only, never write
    numstat: bool,              // --numstat: machine-readable added/deleted counts
    nul: bool,                  // -z: NUL-terminate --numstat records
    unidiff_zero: bool,         // --unidiff-zero: relax the begin/end anchoring
    allow_empty: bool,          // --allow-empty: an input with no patches is not an error
    quiet: bool,                // -q/--quiet: silence `error:` diagnostics
    recount: bool,              // --recount: derive hunk sizes from the body, not the header
    directory: Option<String>,  // --directory=<root>: prepend <root> to every path
    apply_override: Option<bool>, // --apply / --no-apply
    apply: bool,                // whether the patch is actually applied
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            strip: 1,
            reverse: false,
            check: false,
            numstat: false,
            nul: false,
            unidiff_zero: false,
            allow_empty: false,
            quiet: false,
            recount: false,
            directory: None,
            apply_override: None,
            apply: true,
        }
    }
}

/// `error:` diagnostics, which `-q` silences in git.
fn err(quiet: bool, msg: &str) {
    if !quiet {
        eprintln!("{msg}");
    }
}

/// Fetch the value of a long option, from `--name=value` or the following argv
/// entry.
fn long_value(
    args: &[String],
    i: &mut usize,
    name: &str,
    inline: Option<&str>,
) -> Result<String, ExitCode> {
    if let Some(v) = inline {
        return Ok(v.to_owned());
    }
    match args.get(*i) {
        Some(v) => {
            *i += 1;
            Ok(v.clone())
        }
        None => {
            eprintln!("error: option `{name}' requires a value");
            Err(ExitCode::from(129))
        }
    }
}

/// Parse the whole option table. Diagnostics are printed here; the returned
/// `ExitCode` is git's for that failure (129 for usage errors, 128 for the two
/// `fatal:` paths).
fn parse_opts(
    args: &[String],
    o: &mut Opts,
    sources: &mut Vec<String>,
    unhonoured: &mut Vec<Unhonoured>,
) -> Result<(), ExitCode> {
    let mut three_way = false;
    let mut conflict_given = false;
    let mut no_more_opts = false;
    let mut i = 0;

    while i < args.len() {
        let a = args[i].clone();
        i += 1;

        if no_more_opts || a == "-" || !a.starts_with('-') {
            sources.push(a);
            continue;
        }
        if a == "--" {
            no_more_opts = true;
            continue;
        }

        if let Some(long) = a.strip_prefix("--") {
            let (given, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            // `--no-add` is an option in its own right, not the negation of
            // `--add`, so it must not be split here.
            let (name, neg) = match given.strip_prefix("no-") {
                Some(rest) if given != "no-add" => (rest, true),
                _ => (given, false),
            };

            match name {
                // ---- honoured ----
                "numstat" => o.numstat = !neg,
                "check" => o.check = !neg,
                "reverse" => o.reverse = !neg,
                "unidiff-zero" => o.unidiff_zero = !neg,
                "allow-empty" => o.allow_empty = !neg,
                "quiet" => o.quiet = !neg,
                "recount" => o.recount = !neg,
                "apply" => o.apply_override = Some(!neg),
                "directory" => {
                    o.directory = if neg {
                        None
                    } else {
                        Some(long_value(args, &mut i, name, inline)?)
                    }
                }
                "whitespace" => {
                    if neg {
                        unmark(unhonoured, "whitespace");
                    } else {
                        let v = long_value(args, &mut i, name, inline)?;
                        match v.as_str() {
                            // Neither warns nor alters the applied bytes, and we
                            // emit no whitespace warnings.
                            "warn" | "nowarn" => unmark(unhonoured, "whitespace"),
                            "fix" | "strip" | "error" | "error-all" => {
                                mark(unhonoured, "whitespace", &a, R_WS)
                            }
                            _ => {
                                eprintln!("error: unrecognized whitespace option '{v}'");
                                return Err(ExitCode::from(129));
                            }
                        }
                    }
                }
                // Hidden legacy spellings: binary application needs no opt-in in
                // modern git, so both are genuine no-ops.
                "binary" | "allow-binary-replacement" if !neg => {}
                // `--add` is the default, so it really is a no-op.
                "add" if !neg => unmark(unhonoured, "no-add"),

                // ---- parsed, validated, reported before they could matter ----
                "no-add" if !neg => mark(unhonoured, "no-add", &a, R_NOADD),
                "exclude" | "include" if !neg => {
                    long_value(args, &mut i, name, inline)?;
                    mark(unhonoured, "pathspec", &a, R_PATHSPEC);
                }
                "ours" | "theirs" | "union" if !neg => conflict_given = true,
                "3way" => {
                    three_way = !neg;
                    if neg {
                        unmark(unhonoured, "3way");
                    } else {
                        mark(unhonoured, "3way", &a, R_3WAY);
                    }
                }
                "stat" | "summary" => {
                    let key = if name == "stat" { "stat" } else { "summary" };
                    if neg {
                        unmark(unhonoured, key);
                    } else {
                        mark(unhonoured, key, &a, R_STAT);
                    }
                }
                "index" | "cached" => {
                    let key = if name == "index" { "index" } else { "cached" };
                    if neg {
                        unmark(unhonoured, key);
                    } else {
                        mark(unhonoured, key, &a, R_INDEX);
                    }
                }
                "intent-to-add" => {
                    if neg {
                        unmark(unhonoured, "intent-to-add");
                    } else {
                        mark(unhonoured, "intent-to-add", &a, R_INDEX)
                    }
                }
                "unsafe-paths" => {
                    if neg {
                        unmark(unhonoured, "unsafe-paths");
                    } else {
                        mark(unhonoured, "unsafe-paths", &a, R_UNSAFE)
                    }
                }
                "reject" => {
                    if neg {
                        unmark(unhonoured, "reject");
                    } else {
                        mark(unhonoured, "reject", &a, R_REJECT)
                    }
                }
                "allow-overlap" => {
                    if neg {
                        unmark(unhonoured, "allow-overlap");
                    } else {
                        mark(unhonoured, "allow-overlap", &a, R_OVERLAP)
                    }
                }
                "verbose" => {
                    if neg {
                        unmark(unhonoured, "verbose");
                    } else {
                        mark(unhonoured, "verbose", &a, R_VERBOSE)
                    }
                }
                "inaccurate-eof" => {
                    if neg {
                        unmark(unhonoured, "inaccurate-eof");
                    } else {
                        mark(unhonoured, "inaccurate-eof", &a, R_EOF)
                    }
                }
                "ignore-space-change" | "ignore-whitespace" => {
                    if neg {
                        unmark(unhonoured, "ignore-whitespace");
                    } else {
                        mark(unhonoured, "ignore-whitespace", &a, R_IGNORE_WS)
                    }
                }
                "build-fake-ancestor" => {
                    if neg {
                        unmark(unhonoured, "build-fake-ancestor");
                    } else {
                        long_value(args, &mut i, name, inline)?;
                        mark(unhonoured, "build-fake-ancestor", &a, R_ANCESTOR);
                    }
                }

                // `given`, not `name`: git names the option as it was written.
                _ => {
                    eprintln!("error: unknown option `{given}'");
                    eprint!("{USAGE}");
                    return Err(ExitCode::from(129));
                }
            }
            continue;
        }

        // Short options, which cluster (`-qR`) and may carry their value glued on
        // (`-p2`) or as the next argv entry (`-p 2`).
        let chars: Vec<char> = a[1..].chars().collect();
        let mut k = 0;
        while k < chars.len() {
            let c = chars[k];
            k += 1;
            match c {
                'p' | 'C' => {
                    let glued: String = chars[k..].iter().collect();
                    k = chars.len();
                    let v = if !glued.is_empty() {
                        glued
                    } else {
                        match args.get(i) {
                            Some(v) => {
                                i += 1;
                                v.clone()
                            }
                            None => {
                                eprintln!("error: switch `{c}' requires a value");
                                return Err(ExitCode::from(129));
                            }
                        }
                    };
                    if c == 'p' {
                        // git parses -p itself, so its rejection is `fatal:`/128,
                        // not parse-options' `error:`/129.
                        match v.parse::<usize>() {
                            Ok(n) => o.strip = n,
                            Err(_) => {
                                eprintln!(
                                    "fatal: option -p expects a non-negative integer, got '{v}'"
                                );
                                return Err(ExitCode::from(128));
                            }
                        }
                    } else if v.parse::<usize>().is_err() {
                        eprintln!(
                            "error: switch `C' expects a non-negative integer value with an optional k/m/g suffix"
                        );
                        return Err(ExitCode::from(129));
                    } else {
                        mark(unhonoured, "context", &format!("-C{v}"), R_CONTEXT);
                    }
                }
                'z' => o.nul = true,
                'R' => o.reverse = true,
                'q' => o.quiet = true,
                'N' => mark(unhonoured, "intent-to-add", "-N", R_INDEX),
                'v' => mark(unhonoured, "verbose", "-v", R_VERBOSE),
                '3' => {
                    three_way = true;
                    mark(unhonoured, "3way", "-3", R_3WAY);
                }
                _ => {
                    eprintln!("error: unknown switch `{c}'");
                    eprint!("{USAGE}");
                    return Err(ExitCode::from(129));
                }
            }
        }
    }

    // git's one post-parse usage check, run before it opens any patch file.
    if conflict_given && !three_way {
        eprintln!("fatal: --ours, --theirs, and --union require --3way");
        return Err(ExitCode::from(128));
    }

    // --check and --numstat turn applying off; --apply turns it back on.
    o.apply = o.apply_override.unwrap_or(!(o.check || o.numstat));
    Ok(())
}

pub fn apply(args: &[String]) -> Result<ExitCode> {
    let mut o = Opts::default();
    let mut sources: Vec<String> = Vec::new();
    let mut unhonoured: Vec<Unhonoured> = Vec::new();

    if let Err(code) = parse_opts(args, &mut o, &mut sources, &mut unhonoured) {
        return Ok(code);
    }

    // ---- read the patch text ------------------------------------------------
    let mut buf: Vec<u8> = Vec::new();
    if sources.is_empty() {
        std::io::stdin().read_to_end(&mut buf)?;
    } else {
        for src in &sources {
            if src == "-" {
                std::io::stdin().read_to_end(&mut buf)?;
                continue;
            }
            match std::fs::read(src) {
                Ok(b) => buf.extend_from_slice(&b),
                Err(e) => {
                    err(
                        o.quiet,
                        &format!("error: can't open patch '{src}': {}", io_msg(&e)),
                    );
                    return Ok(ExitCode::from(128));
                }
            }
        }
    }

    let mut patches = parse_patches(&split_lines(&buf), o.strip, o.recount)?;
    if patches.is_empty() {
        if o.allow_empty {
            return Ok(ExitCode::SUCCESS);
        }
        err(
            o.quiet,
            "error: No valid patches in input (allow with \"--allow-empty\")",
        );
        return Ok(ExitCode::from(128));
    }

    // Past here a flag we cannot honour would change the result, so report it.
    if let Some(u) = unhonoured.first() {
        let (flag, why) = (&u.spelling, u.why);
        bail!("unsupported flag {flag:?}: {why} (ported: {PORTED})");
    }

    if let Some(root) = &o.directory {
        for p in &mut patches {
            prefix_names(p, root)?;
        }
    }
    if o.reverse {
        for p in &mut patches {
            p.reverse();
        }
    }

    if o.numstat {
        print!("{}", render_numstat(&patches, o.nul));
    }
    if !o.apply && !o.check {
        return Ok(ExitCode::SUCCESS);
    }

    // git reinterprets patch paths against the repo prefix when invoked below the
    // worktree root; rather than silently applying to the wrong paths, refuse.
    if let Ok(repo) = gix::discover(".") {
        if let Some(workdir) = repo.workdir() {
            let here = std::env::current_dir()?.canonicalize()?;
            if workdir.canonicalize()? != here {
                bail!("running apply from a subdirectory of the worktree is not supported");
            }
        }
    }

    // ---- check phase: build every result in memory, touching nothing --------
    let mut staged: HashMap<String, Option<Vec<u8>>> = HashMap::new();
    let mut ops: Vec<Op> = Vec::new();
    let mut failed = false;

    for p in &patches {
        if p.binary {
            bail!("binary patch application is not implemented (ported: {PORTED})");
        }
        // The name git reports errors against: the pre-image path when there is
        // one (`apply_fragments`), else the post-image path.
        let label = p.old_name.clone().or_else(|| p.new_name.clone()).unwrap_or_default();

        // A path that must not already exist: a creation target, or a rename
        // destination.
        if let Some(new) = &p.new_name {
            if (p.is_new || p.is_rename) && exists(&staged, new) {
                err(o.quiet, &format!("error: {new}: already exists in working directory"));
                failed = true;
                continue;
            }
        }

        let mut image: Vec<Vec<u8>> = if p.is_new {
            Vec::new()
        } else {
            let old = p.old_name.as_deref().unwrap_or_default();
            match read_current(&staged, old) {
                Some(bytes) => split_lines(&bytes).into_iter().map(|l| l.to_vec()).collect(),
                None => {
                    err(o.quiet, &format!("error: {old}: No such file or directory"));
                    failed = true;
                    continue;
                }
            }
        };

        if let Err(old_pos) = apply_hunks(&mut image, p, o.unidiff_zero) {
            err(o.quiet, &format!("error: patch failed: {label}:{old_pos}"));
            err(o.quiet, &format!("error: {label}: patch does not apply"));
            failed = true;
            continue;
        }

        if p.is_delete {
            if !image.is_empty() {
                err(o.quiet, "error: removal patch leaves file contents");
                failed = true;
                continue;
            }
            let old = p.old_name.clone().unwrap_or_default();
            staged.insert(old.clone(), None);
            ops.push(Op {
                remove: Some(old),
                prune_dirs: true,
                create: None,
            });
            continue;
        }

        let new = p.new_name.clone().unwrap_or_default();
        let data: Vec<u8> = image.concat();
        let mode = p.new_mode.unwrap_or(0o100644);
        if let Some(old) = &p.old_name {
            if old != &new {
                staged.insert(old.clone(), None);
            }
        }
        staged.insert(new.clone(), Some(data.clone()));
        ops.push(Op {
            remove: p.old_name.clone(),
            prune_dirs: p.is_rename,
            create: Some((new, mode, data)),
        });
    }

    if failed {
        return Ok(ExitCode::from(1));
    }
    if !o.apply {
        return Ok(ExitCode::SUCCESS);
    }

    // ---- write phase: nothing here may fail on a well-formed patch ----------
    for op in ops {
        if let Some(old) = &op.remove {
            let _ = std::fs::remove_file(old);
            if op.prune_dirs {
                prune_empty_parents(Path::new(old));
            }
        }
        if let Some((path, mode, data)) = op.create {
            create_leading_dirs(Path::new(&path))?;
            write_created(Path::new(&path), mode, &data)?;
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// One file's worth of work, resolved during the check phase and replayed
/// verbatim during the write phase (git's `write_out_one_result`: remove the
/// pre-image path, then create the post-image path).
struct Op {
    remove: Option<String>,
    prune_dirs: bool,
    create: Option<(String, u32, Vec<u8>)>,
}

/// A single file's patch: the extended header facts plus its hunks.
struct Patch {
    old_name: Option<String>, // None once normalised => creation
    new_name: Option<String>, // None once normalised => deletion
    new_mode: Option<u32>,
    is_new: bool,
    is_delete: bool,
    is_rename: bool,
    binary: bool,
    hunks: Vec<Hunk>,
    added: usize,
    deleted: usize,
}

impl Patch {
    /// `-R`: swap the two images, so the patch undoes itself.
    fn reverse(&mut self) {
        std::mem::swap(&mut self.old_name, &mut self.new_name);
        std::mem::swap(&mut self.is_new, &mut self.is_delete);
        std::mem::swap(&mut self.added, &mut self.deleted);
        // The mode we create with is now the mode the pre-image had. We only
        // track the post-image mode, and for a reversal the two sides' modes are
        // the same except for an explicit mode change, which we cannot invert
        // without the old mode; leaving it lets the create fall back to 0644.
        for h in &mut self.hunks {
            std::mem::swap(&mut h.pre, &mut h.post);
            std::mem::swap(&mut h.old_pos, &mut h.new_pos);
        }
    }
}

/// One `@@` fragment. `pre`/`post` hold whole lines *including* their trailing
/// newline (absent on a line marked `\ No newline at end of file`), matching how
/// git's `struct image` stores them so the EOF-newline distinction falls out of
/// plain byte comparison.
struct Hunk {
    old_pos: usize,
    new_pos: usize,
    pre: Vec<Vec<u8>>,
    post: Vec<Vec<u8>>,
    trailing: usize, // trailing context lines; 0 means the hunk must match at EOF
}

// ---------------------------------------------------------------------------
// hunk placement — port of apply.c:find_pos / match_fragment
// ---------------------------------------------------------------------------

/// Apply every hunk of `p` to `image` in order. On failure returns the failing
/// hunk's pre-image start line, which is the number git prints in
/// `patch failed: <path>:<n>`.
fn apply_hunks(image: &mut Vec<Vec<u8>>, p: &Patch, unidiff_zero: bool) -> Result<(), usize> {
    for h in &p.hunks {
        // "a hunk that is (oldpos <= 1) with or without leading context must
        // match at the beginning"; "a hunk without trailing lines must match at
        // the end" — both defeated by --unidiff-zero, which makes the absence of
        // context uninformative.
        let match_beginning = h.old_pos == 0 || (h.old_pos == 1 && !unidiff_zero);
        let match_end = !unidiff_zero && h.trailing == 0;
        let start = h.new_pos.saturating_sub(1);

        let at = find_pos(image, &h.pre, start, match_beginning, match_end).ok_or(h.old_pos)?;
        image.splice(at..at + h.pre.len(), h.post.iter().cloned());
    }
    Ok(())
}

/// Locate `pre` in `image`, starting at `line` and walking outward one line at a
/// time, alternating backwards then forwards exactly as git does (so a patch
/// that could land in two places lands where git lands it).
fn find_pos(
    image: &[Vec<u8>],
    pre: &[Vec<u8>],
    mut line: usize,
    match_beginning: bool,
    match_end: bool,
) -> Option<usize> {
    if match_beginning {
        line = 0;
    } else if match_end {
        line = image.len().saturating_sub(pre.len());
    }
    if line > image.len() {
        line = image.len();
    }

    let (mut backwards, mut forwards, mut current) = (line, line, line);
    let mut i: usize = 0;
    loop {
        if matches_at(image, pre, current, match_beginning, match_end) {
            return Some(current);
        }
        // Pick the next candidate: odd steps go backwards, even steps forwards,
        // skipping (and burning a step on) a direction that has run out.
        loop {
            if backwards == 0 && forwards == image.len() {
                return None;
            }
            if i % 2 == 1 {
                if backwards == 0 {
                    i += 1;
                    continue;
                }
                backwards -= 1;
                current = backwards;
            } else {
                if forwards == image.len() {
                    i += 1;
                    continue;
                }
                forwards += 1;
                current = forwards;
            }
            break;
        }
        i += 1;
    }
}

/// Whether `pre` sits in `image` at line `at`, honouring the anchoring flags.
fn matches_at(
    image: &[Vec<u8>],
    pre: &[Vec<u8>],
    at: usize,
    match_beginning: bool,
    match_end: bool,
) -> bool {
    if at + pre.len() > image.len() {
        return false;
    }
    if match_end && at + pre.len() != image.len() {
        return false;
    }
    if match_beginning && at != 0 {
        return false;
    }
    image[at..at + pre.len()] == *pre
}

// ---------------------------------------------------------------------------
// patch parsing
// ---------------------------------------------------------------------------

/// Split `buf` into lines that keep their trailing newline; a final line without
/// one is kept as-is.
fn split_lines(buf: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, &b) in buf.iter().enumerate() {
        if b == b'\n' {
            out.push(&buf[start..=i]);
            start = i + 1;
        }
    }
    if start < buf.len() {
        out.push(&buf[start..]);
    }
    out
}

/// A line as text with its terminator removed, for header matching.
fn txt(line: &[u8]) -> String {
    let end = line.len() - usize::from(line.last() == Some(&b'\n'));
    String::from_utf8_lossy(&line[..end]).into_owned()
}

/// Scan the whole input for patch headers, skipping any surrounding prose
/// (commit messages, mail headers) as git does.
fn parse_patches(lines: &[&[u8]], strip: usize, recount: bool) -> Result<Vec<Patch>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = txt(lines[i]);
        if l.starts_with("diff --git ") {
            let (p, next) = parse_one(lines, i, strip, true, recount)?;
            i = next;
            out.push(p);
        } else if l.starts_with("--- ")
            && lines.get(i + 1).map(|n| txt(n).starts_with("+++ ")) == Some(true)
        {
            let (p, next) = parse_one(lines, i, strip, false, recount)?;
            i = next;
            out.push(p);
        } else {
            i += 1;
        }
    }
    Ok(out)
}

/// Parse one file's patch beginning at `start`, returning it and the index of
/// the first line after it.
fn parse_one(
    lines: &[&[u8]],
    start: usize,
    strip: usize,
    git_style: bool,
    recount: bool,
) -> Result<(Patch, usize)> {
    let mut p = Patch {
        old_name: None,
        new_name: None,
        new_mode: None,
        is_new: false,
        is_delete: false,
        is_rename: false,
        binary: false,
        hunks: Vec::new(),
        added: 0,
        deleted: 0,
    };
    let mut i = start;

    if git_style {
        let header = txt(lines[i]);
        if let Some((a, b)) = git_header_names(&header["diff --git ".len()..], strip)? {
            p.old_name = Some(a);
            p.new_name = Some(b);
        }
        i += 1;
    }

    // Extended headers, then the `---`/`+++` pair, in whatever order they appear.
    while i < lines.len() {
        let l = txt(lines[i]);
        if let Some(rest) = l.strip_prefix("new file mode ") {
            p.is_new = true;
            p.new_mode = Some(octal(rest)?);
        } else if l.starts_with("deleted file mode ") {
            p.is_delete = true;
        } else if let Some(rest) = l.strip_prefix("new mode ") {
            p.new_mode = Some(octal(rest)?);
        } else if l.starts_with("old mode ") {
            // The pre-image mode is only needed to invert a mode change, which we
            // do not support; ignore it.
        } else if let Some(rest) = l.strip_prefix("rename from ") {
            p.is_rename = true;
            p.old_name = Some(strip_path(&unquote(rest)?, strip.saturating_sub(1))?);
        } else if let Some(rest) = l.strip_prefix("rename to ") {
            p.is_rename = true;
            p.new_name = Some(strip_path(&unquote(rest)?, strip.saturating_sub(1))?);
        } else if l.starts_with("copy from ") || l.starts_with("copy to ") {
            bail!("copy patches are not implemented (ported: {PORTED})");
        } else if l.starts_with("similarity index ") || l.starts_with("dissimilarity index ") {
            // Rename/copy scoring; irrelevant to application.
        } else if let Some(rest) = l.strip_prefix("index ") {
            // `index <old>..<new> <mode>` carries the mode when it did not change;
            // git creates the result with it, so an executable file stays one.
            if let Some((_, mode)) = rest.split_once(' ') {
                if p.new_mode.is_none() {
                    p.new_mode = Some(octal(mode)?);
                }
            }
        } else if let Some(rest) = l.strip_prefix("--- ") {
            p.old_name = header_path(rest, strip)?;
        } else if let Some(rest) = l.strip_prefix("+++ ") {
            p.new_name = header_path(rest, strip)?;
        } else if l.starts_with("GIT binary patch") || l.starts_with("Binary files ") {
            p.binary = true;
            i += 1;
            // Consume the encoded payload up to the next patch header.
            while i < lines.len() {
                let n = txt(lines[i]);
                if n.starts_with("diff --git ") || n.starts_with("--- ") {
                    break;
                }
                i += 1;
            }
            return Ok((normalise(p)?, i));
        } else {
            break;
        }
        i += 1;
    }

    while i < lines.len() && txt(lines[i]).starts_with("@@ ") {
        let (h, added, deleted, next) = parse_hunk(lines, i, recount)?;
        p.added += added;
        p.deleted += deleted;
        p.hunks.push(h);
        i = next;
    }

    Ok((normalise(p)?, i))
}

/// Reconcile the creation/deletion flags with the two names, so that exactly one
/// side is `None` for a creation or deletion.
fn normalise(mut p: Patch) -> Result<Patch> {
    if p.old_name.is_none() && p.new_name.is_none() {
        bail!("corrupt patch: no file name in the header");
    }
    if p.old_name.is_none() {
        p.is_new = true;
    }
    if p.new_name.is_none() {
        p.is_delete = true;
    }
    if p.is_new {
        p.old_name = None;
    }
    if p.is_delete {
        p.new_name = None;
    }
    Ok(p)
}

/// Parse an `@@ -a,b +c,d @@` fragment and its body.
///
/// `recount` is `--recount`: the counts in the header are not trusted, so the
/// body runs until the first line that is not a body line instead of until the
/// header's counts are exhausted, and a mismatch is not an error.
fn parse_hunk(
    lines: &[&[u8]],
    start: usize,
    recount: bool,
) -> Result<(Hunk, usize, usize, usize)> {
    let header = txt(lines[start]);
    let (old_pos, mut old_rem, new_pos, mut new_rem) =
        hunk_range(&header).ok_or_else(|| anyhow::anyhow!("corrupt hunk header {header:?}"))?;

    let mut h = Hunk {
        old_pos,
        new_pos,
        pre: Vec::new(),
        post: Vec::new(),
        trailing: 0,
    };
    let (mut added, mut deleted) = (0usize, 0usize);
    let mut last = Side::None;
    let mut i = start + 1;

    while i < lines.len() {
        let raw = lines[i];
        // `\ No newline at end of file` retracts the newline from the line just
        // read, on whichever image(s) that line joined.
        if raw.first() == Some(&b'\\') {
            match last {
                Side::Context => {
                    drop_newline(h.pre.last_mut());
                    drop_newline(h.post.last_mut());
                }
                Side::Pre => drop_newline(h.pre.last_mut()),
                Side::Post => drop_newline(h.post.last_mut()),
                Side::None => {}
            }
            i += 1;
            continue;
        }
        if !recount && old_rem == 0 && new_rem == 0 {
            break;
        }
        // A context line whose single leading space was stripped in transit is
        // still a context line; git accepts the bare newline.
        let (marker, body): (u8, &[u8]) = match raw.first() {
            Some(&b'\n') | None => (b' ', &b"\n"[..]),
            Some(&c) if c == b' ' || c == b'+' || c == b'-' => (c, &raw[1..]),
            _ => break,
        };
        match marker {
            b' ' => {
                h.pre.push(body.to_vec());
                h.post.push(body.to_vec());
                h.trailing += 1;
                last = Side::Context;
                old_rem = old_rem.saturating_sub(1);
                new_rem = new_rem.saturating_sub(1);
            }
            b'-' => {
                h.pre.push(body.to_vec());
                h.trailing = 0;
                deleted += 1;
                last = Side::Pre;
                old_rem = old_rem.saturating_sub(1);
            }
            _ => {
                h.post.push(body.to_vec());
                h.trailing = 0;
                added += 1;
                last = Side::Post;
                new_rem = new_rem.saturating_sub(1);
            }
        }
        i += 1;
    }

    if !recount && (old_rem != 0 || new_rem != 0) {
        bail!("corrupt patch: truncated hunk {header:?}");
    }
    Ok((h, added, deleted, i))
}

/// Which image(s) the most recent body line joined, for the `\ No newline` rule.
enum Side {
    None,
    Context,
    Pre,
    Post,
}

fn drop_newline(line: Option<&mut Vec<u8>>) {
    if let Some(l) = line {
        if l.last() == Some(&b'\n') {
            l.pop();
        }
    }
}

/// `@@ -a[,b] +c[,d] @@ [section]` → `(a, b, c, d)`.
fn hunk_range(header: &str) -> Option<(usize, usize, usize, usize)> {
    let rest = header.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let new = rest.split_once(" @@")?.0;
    let (os, oc) = one_range(old)?;
    let (ns, nc) = one_range(new)?;
    Some((os, oc, ns, nc))
}

fn one_range(s: &str) -> Option<(usize, usize)> {
    match s.split_once(',') {
        Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

// ---------------------------------------------------------------------------
// path handling
// ---------------------------------------------------------------------------

/// A `---`/`+++` header path: text up to the first tab (traditional diffs append
/// a timestamp there), `/dev/null` meaning "this side does not exist".
fn header_path(rest: &str, strip: usize) -> Result<Option<String>> {
    let name = rest.split('\t').next().unwrap_or("");
    if name == "/dev/null" {
        return Ok(None);
    }
    Ok(Some(strip_path(&unquote(name)?, strip)?))
}

/// Both names off a `diff --git a/x b/y` line.
///
/// Quoted forms are unquoted; otherwise we take git's rule of accepting a split
/// only when the two halves are the same path after stripping, which is the case
/// that matters here — a header with no `---`/`+++` pair is a pure mode change,
/// where both sides name the same file.
fn git_header_names(rest: &str, strip: usize) -> Result<Option<(String, String)>> {
    if rest.starts_with('"') {
        if let Some(end) = rest[1..].find('"').map(|i| i + 1) {
            let a = strip_path(&unquote(&rest[..=end])?, strip)?;
            let b = strip_path(&unquote(rest[end + 2..].trim())?, strip)?;
            return Ok(Some((a, b)));
        }
        return Ok(None);
    }
    for (idx, _) in rest.match_indices(' ') {
        let (Ok(a), Ok(b)) = (
            strip_path(rest[..idx].as_bytes(), strip),
            strip_path(rest[idx + 1..].as_bytes(), strip),
        ) else {
            continue;
        };
        if a == b {
            return Ok(Some((a, b)));
        }
    }
    Ok(None)
}

/// Drop `n` leading slash-separated components, as `-p<n>` asks.
fn strip_path(name: &[u8], n: usize) -> Result<String> {
    let mut s: &[u8] = name;
    for _ in 0..n {
        match s.iter().position(|&b| b == b'/') {
            Some(i) => s = &s[i + 1..],
            None => bail!(
                "removing {n} leading path components from {:?} would leave nothing",
                String::from_utf8_lossy(name)
            ),
        }
    }
    let out = String::from_utf8(s.to_vec())
        .map_err(|_| anyhow::anyhow!("non-UTF-8 paths in patches are not supported"))?;
    check_path(out)
}

/// Reject anything that would escape the working tree. `--unsafe-paths`, which
/// is what lets git through this gate, is not honoured, so this is unconditional.
fn check_path(out: String) -> Result<String> {
    if out.is_empty() || out.starts_with('/') || out.split('/').any(|c| c == "..") {
        bail!("refusing to apply to path {out:?} outside the working tree");
    }
    Ok(out)
}

/// `--directory=<root>`: git's `prefix_one()` — prepend `root` to every patch
/// path, after `-p<n>` has done its stripping. A `/dev/null` side is `None` here
/// (a creation's pre-image, a deletion's post-image) and stays that way.
fn prefix_names(p: &mut Patch, root: &str) -> Result<()> {
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return Ok(());
    }
    for name in [&mut p.old_name, &mut p.new_name] {
        if let Some(n) = name {
            let joined = format!("{root}/{n}");
            *n = check_path(joined)?;
        }
    }
    Ok(())
}

/// Undo git's C-style quoting when a header path is wrapped in double quotes.
fn unquote(s: &str) -> Result<Vec<u8>> {
    let b = s.as_bytes();
    if b.len() < 2 || b[0] != b'"' || b[b.len() - 1] != b'"' {
        return Ok(b.to_vec());
    }
    let inner = &b[1..b.len() - 1];
    let mut out = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        if inner[i] != b'\\' {
            out.push(inner[i]);
            i += 1;
            continue;
        }
        i += 1;
        let Some(&c) = inner.get(i) else {
            bail!("corrupt quoted path {s:?}");
        };
        i += 1;
        match c {
            b'a' => out.push(0x07),
            b'b' => out.push(0x08),
            b't' => out.push(b'\t'),
            b'n' => out.push(b'\n'),
            b'v' => out.push(0x0b),
            b'f' => out.push(0x0c),
            b'r' => out.push(b'\r'),
            b'"' | b'\\' => out.push(c),
            b'0'..=b'7' => {
                let mut v = u32::from(c - b'0');
                for _ in 0..2 {
                    match inner.get(i) {
                        Some(&d) if (b'0'..=b'7').contains(&d) => {
                            v = v * 8 + u32::from(d - b'0');
                            i += 1;
                        }
                        _ => break,
                    }
                }
                out.push(v as u8);
            }
            _ => bail!("corrupt quoted path {s:?}"),
        }
    }
    Ok(out)
}

/// C-style path quoting for `--numstat`, matching git's default `core.quotePath`.
fn quote_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let needs = bytes
        .iter()
        .any(|&b| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || b >= 0x80);
    if !needs {
        return path.to_owned();
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

fn octal(s: &str) -> Result<u32> {
    u32::from_str_radix(s.trim(), 8).map_err(|_| anyhow::anyhow!("corrupt file mode {s:?}"))
}

// ---------------------------------------------------------------------------
// output and filesystem
// ---------------------------------------------------------------------------

/// `--numstat`: `<added>\t<deleted>\t<path>`, `-\t-\t` for binary patches, the
/// post-image path (pre-image for a deletion), quoted unless `-z`.
fn render_numstat(patches: &[Patch], nul: bool) -> String {
    let mut out = String::new();
    for p in patches {
        if p.binary {
            out.push_str("-\t-\t");
        } else {
            out.push_str(&format!("{}\t{}\t", p.added, p.deleted));
        }
        let name = p
            .new_name
            .as_deref()
            .or(p.old_name.as_deref())
            .unwrap_or_default();
        if nul {
            out.push_str(name);
            out.push('\0');
        } else {
            out.push_str(&quote_path(name));
            out.push('\n');
        }
    }
    out
}

/// The current bytes of `path`, preferring the result an earlier patch in this
/// same run produced. `None` means the path does not exist.
fn read_current(staged: &HashMap<String, Option<Vec<u8>>>, path: &str) -> Option<Vec<u8>> {
    if let Some(entry) = staged.get(path) {
        return entry.clone();
    }
    let meta = std::fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() {
        // A symlink's blob content is its target, with no trailing newline.
        return Some(
            std::fs::read_link(path)
                .ok()?
                .into_os_string()
                .into_string()
                .ok()?
                .into_bytes(),
        );
    }
    std::fs::read(path).ok()
}

fn exists(staged: &HashMap<String, Option<Vec<u8>>>, path: &str) -> bool {
    match staged.get(path) {
        Some(entry) => entry.is_some(),
        None => std::fs::symlink_metadata(path).is_ok(),
    }
}

fn create_leading_dirs(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

/// Create `path` fresh with `mode`, as git's `try_create_file` does: symlinks via
/// `symlink(2)`, everything else opened `O_CREAT|O_EXCL` with 0777 or 0666 so the
/// process umask decides the final permissions.
#[cfg(unix)]
fn write_created(path: &Path, mode: u32, data: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    if mode & 0o170000 == 0o120000 {
        let target = String::from_utf8_lossy(data).into_owned();
        std::os::unix::fs::symlink(&target, path)?;
        return Ok(());
    }
    let perm = if mode & 0o100 != 0 { 0o777 } else { 0o666 };
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(perm)
        .open(path)?;
    f.write_all(data)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_created(path: &Path, _mode: u32, data: &[u8]) -> Result<()> {
    std::fs::write(path, data)?;
    Ok(())
}

/// After removing a file, drop the directories it emptied, exactly as git's
/// `remove_path` does. Stops at the first non-empty (or non-removable) parent.
fn prune_empty_parents(path: &Path) {
    let mut dir: PathBuf = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => return,
    };
    while std::fs::remove_dir(&dir).is_ok() {
        match dir.parent() {
            Some(p) if !p.as_os_str().is_empty() => dir = p.to_path_buf(),
            _ => break,
        }
    }
}

/// An io error's message without Rust's ` (os error N)` suffix, so our stderr
/// reads like git's `strerror`-based output.
fn io_msg(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error ") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}
