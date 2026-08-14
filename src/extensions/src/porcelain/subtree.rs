//! `git subtree` — split a subdirectory out to, and merge it back from, its own
//! history.
//!
//! A port of `contrib/subtree/git-subtree.sh` (git 2.55.0), which is not a C
//! builtin but a POSIX shell driver over git plumbing. The script is the spec:
//! every message, exit code, commit-message trailer and traversal order below is
//! taken from it, and the structure mirrors it function-for-function
//! (`main`/`cmd_add`/`cmd_split`/`copy_or_skip`/`process_split_commit`/…).
//!
//! Ported and behaving as stock git:
//!
//! * The whole option surface, as `git rev-parse --parseopt --stuck-long`
//!   presents it to the script: `-h`/`--help`, `-q`/`--quiet`, `-d`/`--debug`,
//!   `-P`/`--prefix`, `--annotate`, `-b`/`--branch`, `--ignore-joins`,
//!   `--onto`, `--rejoin`, `--squash`, `-m`/`--message`, `--no-gpg-sign`, their
//!   `--no-…` forms where the spec allows one, attached and separate values,
//!   short clusters, unambiguous long-option abbreviation, and the
//!   `die_incompatible_opt` matrix that rejects a split-only flag on `add` (and
//!   vice versa). Usage errors print git's `parse_options` wording and exit 129;
//!   the script's own `die` paths exit 1.
//! * `subtree add --prefix=<p> <commit>` and `--prefix=<p> <repository> <ref>`,
//!   with `--squash` and `-m`. The synthesized commits — `Add '<dir>/' from
//!   commit '<sha>'` plus the `git-subtree-dir:`/`git-subtree-mainline:`/
//!   `git-subtree-split:` trailers, and the `Squashed '<dir>/' …` bodies — are
//!   byte-identical to stock, so the resulting object ids match.
//! * `subtree split --prefix=<p> [<commit>]` with `--annotate`, `-b`/`--branch`,
//!   `--ignore-joins`, `--onto` and `--rejoin`: the full history rewrite
//!   (`process_split_commit`/`copy_or_skip`/`copy_commit`), the join-commit cache
//!   seeded from prior `git-subtree-*` trailers, the `notree` bookkeeping, the
//!   `revcount/revmax (createcount) [extracount]` progress line, and the branch
//!   ancestry check.
//! * `subtree push <repository> <refspec>`, which is `split` followed by
//!   `git push <repository> <split>:refs/heads/<remoteref>`.
//! * `subtree merge <rev> [<repository>]` and `subtree pull <repository> <ref>`,
//!   with `--squash` and `-m`: `git merge --no-ff -Xsubtree=<prefix>` over the
//!   revision (or, under `--squash`, over a fresh squash commit chained onto the
//!   previous one). Both rest on `git merge`'s `-X`/`--strategy-option`, which
//!   this build now accepts.
//! * Consequently `split --rejoin` / `push --rejoin` complete the rejoin through
//!   `cmd_merge` when a prior `git-subtree-dir:` join commit is already reachable
//!   from `HEAD`, as well as through `cmd_add` when none is.
//!
//! Deliberate floors, refused rather than approximated:
//!
//! * `-S`/`--gpg-sign[=<key-id>]`. The script passes it through to
//!   `git commit-tree`, which this build refuses for want of a signing driver
//!   (`porcelain/commit_tree.rs`). `--no-gpg-sign` is accepted because it names
//!   exactly what this port already does — write unsigned commits.
//!
//! Two implementation substitutions, both invisible in the result:
//!
//! * The script keeps its rewrite cache in `$GIT_DIR/subtree-cache/$$` because
//!   its `while read` loops run in pipeline subshells. Nothing here forks, so the
//!   cache is an in-process map; `cache_set`'s "already exists" fatal is kept,
//!   and `cache_setup`'s `debug "Using cachedir: …"` line is dropped rather than
//!   made up, since no such directory is created.
//! * `add <repository> <ref>` names the fetched tip `FETCH_HEAD` as the script
//!   does, but falls back to `git ls-remote <repository> <ref>` if `git fetch`
//!   wrote no `FETCH_HEAD`. The objects arrive from the same fetch either way, so
//!   the added commit is the same; the fallback costs one extra connection.
//!
//! Known text divergences, stated rather than hidden: `require_work_tree`
//! interpolates `$0`, which here is this binary rather than
//! `…/git-core/git-subtree`; and the progress line's `revmax` is a plain
//! decimal, while the script interpolates `wc -l`, which on BSD `wc` (macOS)
//! space-pads it to width 8.

use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use gix::bstr::ByteSlice;
use gix::hash::ObjectId;
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;

/// The usage block `git rev-parse --parseopt` renders from the script's
/// `OPTS_SPEC`, byte-for-byte. Printed on stdout for `-h`/`--help` and on stderr
/// after the `error:` line for any other usage error; both exit 129.
const USAGE: &str = "\
usage: git subtree add   --prefix=<prefix> [-S[=<key-id>]] <commit>
   or: git subtree add   --prefix=<prefix> [-S[=<key-id>]] <repository> <ref>
   or: git subtree merge --prefix=<prefix> [-S[=<key-id>]] <commit>
   or: git subtree split --prefix=<prefix> [-S[=<key-id>]] [<commit>]
   or: git subtree pull  --prefix=<prefix> [-S[=<key-id>]] <repository> <ref>
   or: git subtree push  --prefix=<prefix> [-S[=<key-id>]] <repository> <refspec>

    -h, --help            show the help
    -q, --quiet           quiet
    -d, --debug           show debug messages
    -P, --[no-]prefix ... the name of the subdir to split out

options for 'split' (also: 'push')
    --[no-]annotate ...   add a prefix to commit message of new commits
    -b, --branch ...      create a new branch from the split subtree
    --[no-]ignore-joins   ignore prior --rejoin commits
    --[no-]onto ...       try connecting new tree to an existing one
    --[no-]rejoin         merge the new branch back into HEAD

options for 'add' and 'merge' (also: 'pull', 'split --rejoin', and 'push --rejoin')
    --[no-]squash         merge subtree changes as a single commit
    -m, --message ...     use the given message as the commit message for the merge commit
    -S, --[no-]gpg-sign[=<key-id>]
                          GPG-sign commits. The keyid argument is optional and defaults to the committer identity

";

/// The script's control-flow exits — `die` (status 1), `exit $?` after a failed
/// child, and `parseopt`'s 129 — carried as an `anyhow` error so `?` unwinds to
/// [`subtree`] the way `exit` unwinds a shell. The message, if any, is already on
/// stderr by the time this is constructed, exactly as `die` prints then exits.
#[derive(Debug)]
struct Exit(u8);

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exit {}", self.0)
    }
}

impl std::error::Error for Exit {}

/// `git-sh-setup`'s `die`: the message verbatim on stderr, then exit 1. Callers
/// supply their own `fatal: ` prefix, because the script does — two of its `die`
/// calls deliberately have none.
fn die<T>(msg: &str) -> Result<T> {
    eprintln!("{msg}");
    Err(Exit(1).into())
}

/// Unwind with `status` and no message — the script's bare `exit $?` after a
/// child command has already reported its own failure.
fn exit_with<T>(status: u8) -> Result<T> {
    Err(Exit(status).into())
}

/// git's `unknown` label in `parse_options`: `error: <what>` and the usage block,
/// both on stderr, exit 129. Reached when no option in the spec matches.
fn unknown_option<T>(what: &str) -> Result<T> {
    eprintln!("error: {what}");
    eprint!("{USAGE}");
    Err(Exit(129).into())
}

/// git's `show_usage` label: `error: <what>` on stderr but the usage block on
/// *stdout* (`usage_with_options_internal(…, err = 0)`), exit 129. Reached for an
/// ambiguous abbreviation.
fn ambiguous_option<T>(what: &str) -> Result<T> {
    eprintln!("error: {what}");
    print!("{USAGE}");
    Err(Exit(129).into())
}

/// git's `opterror`: the message alone on stderr, with no usage block, because
/// `parse_long_opt` returns `PARSE_OPT_ERROR` and `parse_options` exits 129
/// straight away.
fn opt_error<T>(what: &str) -> Result<T> {
    eprintln!("error: {what}");
    Err(Exit(129).into())
}

// ---------------------------------------------------------------------------
// option spec
// ---------------------------------------------------------------------------

/// Whether an option takes a value, mirroring the `OPTS_SPEC` suffixes: none,
/// `=` (required) or `?` (optional, so `-S` and `-Skey` are both legal).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Value {
    None,
    Required,
    Optional,
}

/// One `OPTS_SPEC` line. `noneg` is the spec's `!`, git's `PARSE_OPT_NONEG`:
/// the option has no `--no-` spelling and never answers an abbreviation of `no-`.
struct OptSpec {
    short: Option<char>,
    long: &'static str,
    value: Value,
    noneg: bool,
}

/// The script's `OPTS_SPEC`, in order. The order is load-bearing: git's
/// `parse_long_opt` reports the *last two* candidates of an ambiguous
/// abbreviation, so a reordering would change the `ambiguous option:` text.
const SPECS: &[OptSpec] = &[
    OptSpec { short: Some('h'), long: "help", value: Value::None, noneg: true },
    OptSpec { short: Some('q'), long: "quiet", value: Value::None, noneg: true },
    OptSpec { short: Some('d'), long: "debug", value: Value::None, noneg: true },
    OptSpec { short: Some('P'), long: "prefix", value: Value::Required, noneg: false },
    OptSpec { short: None, long: "annotate", value: Value::Required, noneg: false },
    OptSpec { short: Some('b'), long: "branch", value: Value::Required, noneg: true },
    OptSpec { short: None, long: "ignore-joins", value: Value::None, noneg: false },
    OptSpec { short: None, long: "onto", value: Value::Required, noneg: false },
    OptSpec { short: None, long: "rejoin", value: Value::None, noneg: false },
    OptSpec { short: None, long: "squash", value: Value::None, noneg: false },
    OptSpec { short: Some('m'), long: "message", value: Value::Required, noneg: true },
    OptSpec { short: Some('S'), long: "gpg-sign", value: Value::Optional, noneg: false },
];

/// `git rev-parse --parseopt --stuck-long -- "$@"`: normalize the command line
/// into the long, value-attached option tokens the script's own `case` arms
/// match, plus the positionals in encounter order.
///
/// Option and non-option arguments may interleave (git's `parse_options`
/// permutes), `--` ends option parsing, and `-h`/`--help` prints the usage on
/// stdout and exits 129 before anything else happens — including before the
/// work-tree check, which is why `git subtree` outside a repository still
/// answers with usage.
fn parseopt(argv: &[String]) -> Result<(Vec<String>, Vec<String>)> {
    let mut opts: Vec<String> = Vec::new();
    let mut positionals: Vec<String> = Vec::new();
    let mut only_positionals = false;

    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        i += 1;

        if only_positionals {
            positionals.push(arg.to_string());
            continue;
        }
        if arg == "--" {
            only_positionals = true;
            continue;
        }
        // A lone `-` is a positional to `parse_options`, not an option.
        if !arg.starts_with('-') || arg == "-" {
            positionals.push(arg.to_string());
            continue;
        }

        // parse_options_step() tests `--help-all` with a `strcmp()` of its own,
        // ahead of parse_long_opt() and after the `--` break above: the name
        // never abbreviates and never takes an `=<value>`, which is why it is
        // not an [`SPECS`] line. The `OPTS_SPEC` declares no hidden option, so
        // `USAGE_FULL` renders the same block `-h` prints.
        if arg == "--help-all" {
            print!("{USAGE}");
            return exit_with(129);
        }

        if let Some(body) = arg.strip_prefix("--") {
            parse_long(body, argv, &mut i, &mut opts)?;
            continue;
        }

        // A short cluster: every letter is an option, and the first one that
        // takes a value swallows the rest of the cluster (or the next argument).
        let letters: Vec<char> = arg[1..].chars().collect();
        let mut c = 0;
        while c < letters.len() {
            let letter = letters[c];
            c += 1;
            let Some(spec) = SPECS.iter().find(|s| s.short == Some(letter)) else {
                return unknown_option(&format!("unknown switch `{letter}'"));
            };
            if spec.long == "help" {
                print!("{USAGE}");
                return exit_with(129);
            }
            let rest: String = letters[c..].iter().collect();
            match spec.value {
                Value::None => opts.push(format!("--{}", spec.long)),
                Value::Optional => {
                    // `-Skey` carries a value, bare `-S` does not — an optional
                    // argument never reaches for the next command-line word.
                    if rest.is_empty() {
                        opts.push(format!("--{}", spec.long));
                    } else {
                        opts.push(format!("--{}={rest}", spec.long));
                        c = letters.len();
                    }
                }
                Value::Required => {
                    let value = if rest.is_empty() {
                        let Some(next) = argv.get(i) else {
                            return opt_error(&format!("switch `{letter}' requires a value"));
                        };
                        i += 1;
                        next.clone()
                    } else {
                        rest
                    };
                    opts.push(format!("--{}={value}", spec.long));
                    c = letters.len();
                }
            }
        }
    }

    Ok((opts, positionals))
}

/// Resolve one `--…` argument against [`SPECS`] and push its normalized form.
///
/// A port of git's `parse_long_opt`: an exact name wins outright, otherwise an
/// unambiguous abbreviation of either the name or its `no-` form is accepted,
/// and `--n`/`--no`/`--no-` are abbreviations of *every* negatable option (so
/// they are ambiguous). A value-taking option written without `=` reaches for the
/// next command-line word, which is why the cursor is passed in.
fn parse_long(
    body: &str,
    argv: &[String],
    cursor: &mut usize,
    opts: &mut Vec<String>,
) -> Result<()> {
    let (name, attached) = match body.split_once('=') {
        Some((n, v)) => (n, Some(v)),
        None => (body, None),
    };

    // Exact match first: git returns from the loop the moment one is found, so a
    // later exact name beats an earlier abbreviation.
    let exact = SPECS.iter().find_map(|s| {
        if name == s.long {
            Some((s, false))
        } else if !s.noneg && name.strip_prefix("no-") == Some(s.long) {
            Some((s, true))
        } else {
            None
        }
    });

    let (spec, negated) = match exact {
        Some(hit) => hit,
        None => {
            // Abbreviations. `candidates` keeps encounter order so the ambiguity
            // message names the last two, as git's rolling
            // `ambiguous_option`/`abbrev_option` pair does.
            let mut candidates: Vec<(&OptSpec, bool)> = Vec::new();
            for spec in SPECS {
                if spec.long.starts_with(name) {
                    candidates.push((spec, false));
                    continue;
                }
                if spec.noneg {
                    continue;
                }
                // `starts_with("no-", arg)`: the argument is itself a prefix of
                // `no-`, which git treats as an abbreviated negation of every
                // negatable option.
                if !name.is_empty() && "no-".starts_with(name) {
                    candidates.push((spec, true));
                    continue;
                }
                if let Some(rest) = name.strip_prefix("no-") {
                    if spec.long.starts_with(rest) {
                        candidates.push((spec, true));
                    }
                }
            }
            match candidates.len() {
                0 => return unknown_option(&format!("unknown option `{body}'")),
                1 => candidates[0],
                n => {
                    let (a, a_neg) = candidates[n - 2];
                    let (b, b_neg) = candidates[n - 1];
                    let a_no = if a_neg { "no-" } else { "" };
                    let b_no = if b_neg { "no-" } else { "" };
                    return ambiguous_option(&format!(
                        "ambiguous option: {body} (could be --{a_no}{} or --{b_no}{})",
                        a.long, b.long
                    ));
                }
            }
        }
    };

    if spec.long == "help" && !negated {
        print!("{USAGE}");
        return exit_with(129);
    }

    // A negated option never carries a value, whatever the positive form takes.
    if negated {
        if attached.is_some() {
            return opt_error(&format!("option `no-{}' takes no value", spec.long));
        }
        opts.push(format!("--no-{}", spec.long));
        return Ok(());
    }

    match (spec.value, attached) {
        (Value::None, Some(_)) => opt_error(&format!("option `{}' takes no value", spec.long)),
        (Value::None, None) | (Value::Optional, None) => {
            opts.push(format!("--{}", spec.long));
            Ok(())
        }
        (_, Some(v)) => {
            opts.push(format!("--{}={v}", spec.long));
            Ok(())
        }
        (Value::Required, None) => {
            // `--prefix lib`: git's parse-options still accepts a separated value
            // for a required argument, and `--stuck-long` reattaches it.
            let Some(value) = argv.get(*cursor) else {
                return opt_error(&format!("option `{}' requires a value", spec.long));
            };
            *cursor += 1;
            opts.push(format!("--{}={value}", spec.long));
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// command state
// ---------------------------------------------------------------------------

/// The subcommands `git-subtree.sh`'s `case "$arg_command"` accepts, and which
/// of the two option families each one allows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cmd {
    Add,
    Merge,
    Split,
    Pull,
    Push,
}

impl Cmd {
    /// The script's spelling, for the `die_incompatible_opt` message.
    fn name(self) -> &'static str {
        match self {
            Cmd::Add => "add",
            Cmd::Merge => "merge",
            Cmd::Split => "split",
            Cmd::Pull => "pull",
            Cmd::Push => "push",
        }
    }
}

/// Everything `main` gathers plus the rewrite cache `cache_setup` would have put
/// under `$GIT_DIR/subtree-cache/$$`.
struct Ctx {
    repo: gix::Repository,
    /// This binary, re-executed for the index/worktree/network steps the script
    /// shells out for (`read-tree`, `checkout`, `write-tree`, `reset`, `fetch`,
    /// `push`, `ls-remote`, `check-ref-format`, `update-ref`, `diff-index`).
    exe: PathBuf,
    command: Cmd,
    quiet: bool,
    debug: bool,
    /// `debug`'s two-spaces-per-level indent.
    indent: usize,
    /// `$arg_prefix` exactly as given — what `cmd_merge` hands to
    /// `git merge -Xsubtree=`, trailing slashes and all.
    prefix: String,
    /// `$arg_prefix` with any trailing slashes removed — the spelling every
    /// message, trailer and `git ls-tree` lookup uses.
    dir: String,
    split_branch: Option<String>,
    split_onto: Option<String>,
    split_ignore_joins: bool,
    split_annotate: String,
    split_rejoin: bool,
    addmerge_squash: bool,
    addmerge_message: Option<String>,
    /// `$cachedir/$oldrev` → the rewritten id, plus the `latest_old`/`latest_new`
    /// pseudo-keys the script stores alongside them.
    cache: HashMap<String, String>,
    /// `$cachedir/notree/$rev`: commits whose tree has no `$dir` entry.
    notree: HashSet<String>,
    revcount: usize,
    revmax: usize,
    createcount: usize,
    extracount: usize,
}

impl Ctx {
    /// `say`, always at the script's `say >&2` call sites: silenced by `--quiet`.
    fn say_err(&self, msg: &str) {
        if !self.quiet {
            eprintln!("{msg}");
        }
    }

    /// `debug`: indented by the current nesting depth, on stderr, only under `-d`.
    fn debug(&self, msg: &str) {
        if self.debug {
            eprintln!("{:width$}{msg}", "", width = self.indent * 2);
        }
    }

    /// `progress`: one rewritten line under `\r`, or a `progress: ` prefixed line
    /// when `debug` is also writing to stderr and would otherwise overwrite it.
    fn progress(&self, msg: &str) {
        if self.quiet {
            return;
        }
        if self.debug {
            eprintln!("progress: {msg}");
        } else {
            eprint!("{msg}\r");
        }
    }

    /// `cache_get REV` for a single revision.
    fn cache_get(&self, rev: &str) -> Option<&String> {
        self.cache.get(rev)
    }

    /// `cache_set OLDREV NEWREV`, including its refusal to overwrite a real
    /// revision's entry (the `latest_*` pseudo-keys are exempt and are rewritten
    /// on every commit).
    fn cache_set(&mut self, oldrev: &str, newrev: &str) -> Result<()> {
        if oldrev != "latest_old" && oldrev != "latest_new" && self.cache.contains_key(oldrev) {
            return die(&format!("fatal: cache for {oldrev} already exists!"));
        }
        self.cache.insert(oldrev.to_string(), newrev.to_string());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// child processes
// ---------------------------------------------------------------------------

/// Flush this process's own stdout before a child writes to the same descriptor,
/// so `echo … ; git …` keeps its order when stdout is a pipe rather than a tty.
fn flush_stdout() {
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
}

/// Run `git <args>` as a child of this binary with all three streams inherited,
/// and unwind with the child's status when it fails — the script's `|| exit $?`.
fn git_run(ctx: &Ctx, args: &[&str]) -> Result<()> {
    flush_stdout();
    let status = Command::new(&ctx.exe)
        .args(args)
        .status()
        .map_err(|e| anyhow::anyhow!("could not run git {}: {e}", args.join(" ")))?;
    if status.success() {
        return Ok(());
    }
    exit_with(status.code().unwrap_or(1) as u8)
}

/// Run `git <args>` for its exit status only, with both output streams
/// inherited. `Ok(false)` is a non-zero exit, which several call sites test
/// rather than propagate (`ensure_clean`, `git rev-parse` probes).
fn git_ok(ctx: &Ctx, args: &[&str]) -> Result<bool> {
    flush_stdout();
    Ok(Command::new(&ctx.exe)
        .args(args)
        .status()
        .map_err(|e| anyhow::anyhow!("could not run git {}: {e}", args.join(" ")))?
        .success())
}

/// `$(git <args>)`: capture stdout with stderr inherited, strip the trailing
/// newlines a command substitution strips, and unwind on failure.
fn git_capture(ctx: &Ctx, args: &[&str]) -> Result<String> {
    flush_stdout();
    let out = Command::new(&ctx.exe)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| anyhow::anyhow!("could not run git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return exit_with(out.status.code().unwrap_or(1) as u8);
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .trim_end_matches('\n')
        .to_string())
}

// ---------------------------------------------------------------------------
// revision helpers
// ---------------------------------------------------------------------------

/// `git rev-parse --verify --quiet "<spec>"`, peeled to a commit: the id, or
/// `None` when the spec names nothing (or nothing that is a commit).
fn rev_parse_commit(repo: &gix::Repository, spec: &str) -> Option<ObjectId> {
    let object = repo.rev_parse_single(spec).ok()?.object().ok()?;
    object.peel_to_commit().ok().map(|c| c.id)
}

/// `rev_exists REV`: whether `git rev-parse <rev>` would succeed at all — used
/// for `refs/heads/<branch>` and for the `<rev>^` probe in `try_remove_previous`,
/// neither of which requires a commit.
fn rev_exists(repo: &gix::Repository, spec: &str) -> bool {
    repo.rev_parse_single(spec).is_ok()
}

/// `toptree_for_commit COMMIT`: the id of the commit's own tree.
fn toptree_for_commit(repo: &gix::Repository, commit: ObjectId) -> Result<ObjectId> {
    Ok(repo.find_commit(commit)?.tree_id()?.detach())
}

/// `subtree_for_commit COMMIT DIR`: the id of the tree at `dir` in the commit,
/// `None` when the commit has no such entry.
///
/// A gitlink there is a submodule and is skipped (`continue`), and anything that
/// is neither tree nor commit is the script's fatal — a blob at `<dir>` means the
/// prefix does not name a directory in that revision.
fn subtree_for_commit(
    repo: &gix::Repository,
    commit: ObjectId,
    dir: &str,
) -> Result<Option<ObjectId>> {
    let tree = repo.find_commit(commit)?.tree()?;
    let Some(entry) = tree.lookup_entry(dir.split('/').map(str::as_bytes))? else {
        return Ok(None);
    };
    let mode = entry.mode();
    if mode.is_tree() {
        return Ok(Some(entry.object_id()));
    }
    if mode.is_commit() {
        return Ok(None);
    }
    // `git ls-tree` calls both a regular file and a symlink a `blob`.
    die("fatal: tree entry is of type blob, expected tree or commit")
}

/// `git rev-list <tip> [^<hidden>…]`, newest first — git's default ordering,
/// which is what the script's un-sorted `git log`/`git rev-list` pipelines see.
///
/// Each entry is `(commit, parents)`, which is `--parents`' extra column; the
/// walk borrows the repository, so the ids are collected before returning.
fn rev_list(
    repo: &gix::Repository,
    tips: Vec<ObjectId>,
    hidden: Vec<ObjectId>,
) -> Result<Vec<(ObjectId, Vec<ObjectId>)>> {
    if tips.is_empty() {
        return Ok(Vec::new());
    }
    let mut platform = repo
        .rev_walk(tips)
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst));
    if !hidden.is_empty() {
        platform = platform.with_hidden(hidden);
    }
    let mut out = Vec::new();
    for info in platform.all()? {
        let info = info?;
        out.push((info.id, info.parent_ids.to_vec()));
    }
    Ok(out)
}

/// git's `sort_in_topological_order` with LIFO tie-breaking, so `--topo-order`
/// keeps a branch contiguous. The same algorithm as `porcelain/rev_list.rs`'s,
/// duplicated rather than shared because it is private there.
fn topo_sort(commits: &[ObjectId], parents_of: &HashMap<ObjectId, Vec<ObjectId>>) -> Vec<ObjectId> {
    let mut indegree: HashMap<ObjectId, usize> = commits.iter().map(|id| (*id, 1usize)).collect();
    for id in commits {
        for parent in parents_of.get(id).into_iter().flatten() {
            if let Some(n) = indegree.get_mut(parent) {
                if *n != 0 {
                    *n += 1;
                }
            }
        }
    }

    let mut queue: Vec<ObjectId> = commits
        .iter()
        .filter(|id| indegree.get(*id) == Some(&1))
        .copied()
        .collect();
    queue.reverse();

    let mut out = Vec::with_capacity(commits.len());
    while let Some(id) = queue.pop() {
        for parent in parents_of.get(&id).into_iter().flatten() {
            if let Some(n) = indegree.get_mut(parent) {
                if *n == 0 {
                    continue;
                }
                *n -= 1;
                if *n == 1 {
                    queue.push(*parent);
                }
            }
        }
        indegree.insert(id, 0);
        out.push(id);
    }
    out
}

// ---------------------------------------------------------------------------
// join-commit discovery
// ---------------------------------------------------------------------------

/// The two `--grep` patterns the script builds for `git log`.
enum Grep {
    /// `^git-subtree-dir: <dir>/*$` — the `/*` is "zero or more slashes", so a
    /// trailer written with a trailing slash still matches.
    Dir(String),
    /// `^Add '<dir>/' from commit '` — used under `--ignore-joins`, which wants
    /// the original `add` and not the later `--rejoin` commits.
    AddSubject(String),
}

impl Grep {
    /// git's `--grep` matches with `REG_NEWLINE`, so the anchors bind to line
    /// boundaries inside the commit message rather than to the whole message.
    fn matches(&self, message: &[u8]) -> bool {
        match self {
            Grep::Dir(dir) => {
                let head = format!("git-subtree-dir: {dir}");
                message.split(|&b| b == b'\n').any(|line| {
                    line.strip_prefix(head.as_bytes())
                        .is_some_and(|rest| rest.iter().all(|&b| b == b'/'))
                })
            }
            Grep::AddSubject(dir) => {
                let head = format!("Add '{dir}/' from commit '");
                message
                    .split(|&b| b == b'\n')
                    .any(|line| line.starts_with(head.as_bytes()))
            }
        }
    }
}

/// The `git-subtree-mainline:` and `git-subtree-split:` values of a commit
/// message, last occurrence winning.
///
/// The script reads the `START %H%n%s%n%n%b%nEND%n` stream with `while read a b
/// junk`, so a trailer is recognised by its first whitespace-delimited field,
/// leading whitespace is insignificant, and anything past the value is dropped.
fn subtree_trailers(message: &[u8]) -> (Option<String>, Option<String>) {
    let mut mainline = None;
    let mut split = None;
    for line in message.split(|&b| b == b'\n') {
        let mut fields = line.to_str_lossy();
        let text = fields.to_mut();
        let mut words = text.split_whitespace();
        let (Some(key), Some(value)) = (words.next(), words.next()) else {
            continue;
        };
        match key {
            "git-subtree-mainline:" => mainline = Some(value.to_string()),
            "git-subtree-split:" => split = Some(value.to_string()),
            _ => {}
        }
    }
    (mainline, split)
}

/// `process_subtree_split_trailer SPLIT_HASH MAIN_HASH [REPOSITORY]`: resolve a
/// `git-subtree-split:` value to a commit, fetching it from `repository` first
/// when it is not present locally (it may be a tag that was never fetched).
fn process_subtree_split_trailer(
    ctx: &mut Ctx,
    split: &str,
    sq: &str,
    repository: Option<&str>,
) -> Result<ObjectId> {
    if let Some(id) = rev_parse_commit(&ctx.repo, &format!("{split}^{{commit}}")) {
        return Ok(id);
    }
    let fail = format!("fatal: could not rev-parse split hash {split} from commit {sq}");
    match repository {
        Some(repository) => {
            git_run(ctx, &["fetch", repository, split])?;
            // Re-open so the pack the child just wrote is visible to this process.
            ctx.repo = gix::discover(".")?;
            match rev_parse_commit(&ctx.repo, &format!("{split}^{{commit}}")) {
                Some(id) => Ok(id),
                None => die(&fail),
            }
        }
        None => die(&format!(
            "{fail}\n\
             hint: hash might be a tag, try fetching it from the subtree repository:\n\
             hint:    git fetch <subtree-repository> {split}"
        )),
    }
}

/// `find_latest_squash DIR [REPOSITORY]`: the newest commit reachable from
/// `HEAD` that records a subtree state for `dir`, as `(squash-commit, subtree-commit)`.
///
/// A commit carrying `git-subtree-mainline:` is a `--rejoin`/`add` merge rather
/// than a squash, and the script substitutes its second parent so the caller can
/// treat both shapes alike.
fn find_latest_squash(ctx: &mut Ctx, repository: Option<&str>) -> Result<Option<(String, String)>> {
    let dir = ctx.dir.clone();
    ctx.debug(&format!(
        "Looking for latest squash (dir={dir}, repository={})...",
        repository.unwrap_or("")
    ));

    let Some(head) = rev_parse_commit(&ctx.repo, "HEAD") else {
        return Ok(None);
    };
    let grep = Grep::Dir(dir);
    for (id, _) in rev_list(&ctx.repo, vec![head], Vec::new())? {
        let message = ctx.repo.find_commit(id)?.decode()?.message.to_vec();
        if !grep.matches(&message) {
            continue;
        }
        let mut sq = id.to_string();
        let (mainline, split) = subtree_trailers(&message);
        let Some(split) = split else { continue };
        let sub = process_subtree_split_trailer(ctx, &split, &sq, repository)?;
        if mainline.is_some() {
            let spec = format!("{sq}^2");
            let Some(second) = ctx.repo.rev_parse_single(spec.as_str()).ok() else {
                return die("");
            };
            sq = second.detach().to_string();
        }
        ctx.debug(&format!("Squash found: {sq} {sub}"));
        return Ok(Some((sq, sub.to_string())));
    }
    Ok(None)
}

/// `find_existing_splits DIR REV [REPOSITORY]`: seed the rewrite cache from every
/// join commit already in `rev`'s history, and return the `^<rev>^` exclusions
/// that keep the rewrite from walking behind them.
fn find_existing_splits(
    ctx: &mut Ctx,
    rev: ObjectId,
    repository: Option<&str>,
) -> Result<Vec<ObjectId>> {
    ctx.debug("Looking for prior splits...");
    ctx.indent += 1;

    let grep = if ctx.split_ignore_joins {
        Grep::AddSubject(ctx.dir.clone())
    } else {
        Grep::Dir(ctx.dir.clone())
    };

    let mut unrevs: Vec<ObjectId> = Vec::new();
    for (id, _) in rev_list(&ctx.repo, vec![rev], Vec::new())? {
        let message = ctx.repo.find_commit(id)?.decode()?.message.to_vec();
        if !grep.matches(&message) {
            continue;
        }
        let sq = id.to_string();
        let (mainline, split) = subtree_trailers(&message);
        let sub = match split {
            Some(split) => Some(process_subtree_split_trailer(ctx, &split, &sq, repository)?),
            None => None,
        };

        ctx.debug(&format!("Main is: '{}'", mainline.clone().unwrap_or_default()));
        let Some(sub) = sub else { continue };
        let sub = sub.to_string();
        match mainline {
            // A squash commit stands in for the subtree it squashed.
            None => {
                ctx.debug(&format!("  Squash: {sq} from {sub}"));
                ctx.cache_set(&sq, &sub)?;
            }
            Some(main) => {
                ctx.debug(&format!("  Prior: {main} -> {sub}"));
                ctx.cache_set(&main, &sub)?;
                ctx.cache_set(&sub, &sub)?;
                // `try_remove_previous`: exclude the join point's parent, when it
                // has one. A root commit contributes no exclusion.
                for point in [&main, &sub] {
                    let spec = format!("{point}^");
                    if rev_exists(&ctx.repo, &spec) {
                        if let Some(id) = rev_parse_commit(&ctx.repo, &spec) {
                            unrevs.push(id);
                        }
                    }
                }
            }
        }
    }

    ctx.indent -= 1;
    Ok(unrevs)
}

// ---------------------------------------------------------------------------
// commit synthesis
// ---------------------------------------------------------------------------

/// Write a commit object, serialized against other zvcs writers the way every
/// other object-writing verb here is.
///
/// The lock is taken and released around this one write rather than held across
/// the whole rewrite, because the `add`/`--rejoin` paths re-execute this binary
/// for `read-tree`/`checkout`/`reset` and a held lock would block those children
/// in the daemon's queue.
fn write_commit(repo: &gix::Repository, commit: gix::objs::Commit) -> Result<ObjectId> {
    let _lock = crate::lock::RepoLock::acquire(repo.git_dir());
    Ok(repo.write_object(&commit)?.detach())
}

/// The `encoding` header `git commit-tree` would write: present only when
/// `i18n.commitEncoding` names something other than UTF-8.
fn commit_encoding(repo: &gix::Repository) -> Option<gix::bstr::BString> {
    repo.config_snapshot().string("i18n.commitEncoding").and_then(|v| {
        let utf8 = {
            let name = v.to_str_lossy();
            name.eq_ignore_ascii_case("utf-8") || name.eq_ignore_ascii_case("utf8")
        };
        (!utf8).then_some(v)
    })
}

/// Turn a configured identity into an owned signature, as
/// `porcelain/commit_tree.rs` does: git's gecos-derived fallback is not ported,
/// so an unconfigured identity is an error rather than a commit whose author
/// line would not match git's.
fn identity(
    configured: Option<Result<gix::actor::SignatureRef<'_>, gix::config::time::Error>>,
    role: &str,
) -> Result<gix::actor::Signature> {
    let Some(signature) = configured else {
        let upper = role.to_uppercase();
        bail!(
            "no {role} identity configured (set user.name/user.email or \
             GIT_{upper}_NAME/GIT_{upper}_EMAIL); git's gecos fallback is not ported"
        );
    };
    Ok(signature?.to_owned()?)
}

/// `copy_commit REV TREE FLAGS_STR`: the source commit's message and identities
/// on a new root-level tree and parent set.
///
/// The script exports `%an %ae %aD %cn %ce %cD` into the environment and lets
/// `git commit-tree` parse them back; copying the decoded signatures is the same
/// commit, without the RFC-2822 round trip. The message is `--annotate`'s prefix
/// followed by `%B` verbatim, and `git commit-tree` performs no cleanup on it.
fn copy_commit(ctx: &Ctx, rev: ObjectId, tree: ObjectId, parents: &[ObjectId]) -> Result<ObjectId> {
    // `$p` is accumulated as `"$p -p $parent"` from an empty string, so it
    // carries a leading space that the debug line shows.
    let flags: String = parents.iter().map(|p| format!(" -p {p}")).collect();
    ctx.debug(&format!("copy_commit {{{rev}}} {{{tree}}} {{{flags}}}"));
    let source = ctx.repo.find_commit(rev)?;
    let decoded = source.decode()?;
    let mut message = ctx.split_annotate.clone().into_bytes();
    message.extend_from_slice(decoded.message);

    write_commit(
        &ctx.repo,
        gix::objs::Commit {
            tree,
            parents: parents.iter().copied().collect(),
            author: decoded.author()?.to_owned()?,
            committer: decoded.committer()?.to_owned()?,
            encoding: commit_encoding(&ctx.repo),
            message: message.into(),
            extra_headers: Vec::new(),
        },
    )
}

/// `git commit-tree <tree> [-p <parent>…]` reading `message` from stdin: a fresh
/// commit under the *current* identity and time, which is what the script's
/// squash, `add` and rejoin commits are.
fn commit_tree(ctx: &Ctx, tree: ObjectId, parents: &[ObjectId], message: &str) -> Result<ObjectId> {
    let author = identity(ctx.repo.author(), "author")?;
    let committer = identity(ctx.repo.committer(), "committer")?;
    write_commit(
        &ctx.repo,
        gix::objs::Commit {
            tree,
            parents: parents.iter().copied().collect(),
            author,
            committer,
            encoding: commit_encoding(&ctx.repo),
            message: message.into(),
            extra_headers: Vec::new(),
        },
    )
}

/// `add_msg DIR LATEST_OLD LATEST_NEW`: the `add` commit's message.
///
/// Under `--rejoin` the caller has already built the message with `rejoin_msg`
/// (trailers included), so it is passed through untouched.
fn add_msg(ctx: &Ctx, latest_old: Option<ObjectId>, latest_new: ObjectId) -> String {
    let body = ctx
        .addmerge_message
        .clone()
        .unwrap_or_else(|| format!("Add '{}/' from commit '{latest_new}'", ctx.dir));
    if ctx.split_rejoin {
        return format!("{body}\n");
    }
    format!(
        "{body}\n\ngit-subtree-dir: {}\ngit-subtree-mainline: {}\ngit-subtree-split: {latest_new}\n",
        ctx.dir,
        latest_old.map(|id| id.to_string()).unwrap_or_default()
    )
}

/// `add_squashed_msg REV DIR`: the merge message of a `--squash`ed `add`.
fn add_squashed_msg(ctx: &Ctx, rev: ObjectId) -> String {
    match &ctx.addmerge_message {
        Some(m) => format!("{m}\n"),
        None => format!("Merge commit '{rev}' as '{}'\n", ctx.dir),
    }
}

/// `rejoin_msg DIR LATEST_OLD LATEST_NEW`: the message of the commit that merges
/// a fresh split back into the mainline.
fn rejoin_msg(ctx: &Ctx, latest_old: &str, latest_new: &str) -> String {
    let body = ctx
        .addmerge_message
        .clone()
        .unwrap_or_else(|| format!("Split '{}/' into commit '{latest_new}'", ctx.dir));
    format!(
        "{body}\n\ngit-subtree-dir: {}\ngit-subtree-mainline: {latest_old}\ngit-subtree-split: {latest_new}\n",
        ctx.dir
    )
}

/// `squash_msg DIR OLD_SUBTREE_COMMIT NEW_SUBTREE_COMMIT`: the body of a squash
/// commit — a one-line summary, the `%h %s` log of what came in, the
/// `REVERT: %h %s` log of what went out, and the two locating trailers.
fn squash_msg(ctx: &Ctx, oldsub: Option<ObjectId>, newsub: ObjectId) -> Result<String> {
    // `git rev-parse --short` / `%h`: `core.abbrev`, floored at git's
    // `MINIMUM_ABBREV`. git additionally lengthens until the prefix is unique in
    // the object database; that refinement is not reachable from here.
    let hexsz = ctx.repo.object_hash().len_in_hex();
    let abbrev = crate::abbrev::configured_abbrev(&ctx.repo, hexsz).clamp(4, hexsz);
    let short = |id: ObjectId| id.to_string()[..abbrev].to_string();

    let mut out = String::new();
    match oldsub {
        Some(oldsub) => {
            out.push_str(&format!(
                "Squashed '{}/' changes from {}..{}\n\n",
                ctx.dir,
                short(oldsub),
                short(newsub)
            ));
            for (id, _) in rev_list(&ctx.repo, vec![newsub], vec![oldsub])? {
                let commit = ctx.repo.find_commit(id)?;
                let subject = commit.message()?.summary().to_str_lossy().into_owned();
                out.push_str(&format!("{} {subject}\n", short(id)));
            }
            for (id, _) in rev_list(&ctx.repo, vec![oldsub], vec![newsub])? {
                let commit = ctx.repo.find_commit(id)?;
                let subject = commit.message()?.summary().to_str_lossy().into_owned();
                out.push_str(&format!("REVERT: {} {subject}\n", short(id)));
            }
        }
        None => out.push_str(&format!(
            "Squashed '{}/' content from commit {}\n",
            ctx.dir,
            short(newsub)
        )),
    }
    out.push_str(&format!(
        "\ngit-subtree-dir: {}\ngit-subtree-split: {newsub}\n",
        ctx.dir
    ));
    Ok(out)
}

/// `new_squash_commit OLD OLDSUB NEWSUB`: a commit carrying the subtree's tree
/// and nothing but the squash message, chained onto the previous squash when
/// there is one.
fn new_squash_commit(
    ctx: &Ctx,
    old: Option<ObjectId>,
    oldsub: Option<ObjectId>,
    newsub: ObjectId,
) -> Result<ObjectId> {
    let tree = toptree_for_commit(&ctx.repo, newsub)?;
    let message = squash_msg(ctx, oldsub, newsub)?;
    let parents: Vec<ObjectId> = old.into_iter().collect();
    commit_tree(ctx, tree, &parents, &message)
}

/// `copy_or_skip REV TREE NEWPARENTS`: reuse an already-rewritten parent whose
/// tree is identical to this one, or copy the commit.
///
/// A parent is only reusable when no other parent carries history that would be
/// lost by collapsing onto it — the script's `identical`/`nonidentical`/`extras`
/// test — and duplicate parents are folded, since two mainline parents often map
/// to the same rewritten commit.
fn copy_or_skip(
    ctx: &Ctx,
    rev: ObjectId,
    tree: ObjectId,
    newparents: &[ObjectId],
) -> Result<ObjectId> {
    let mut identical: Option<ObjectId> = None;
    let mut nonidentical: Option<ObjectId> = None;
    let mut gotparents: Vec<ObjectId> = Vec::new();
    let mut copycommit = false;

    for parent in newparents {
        let ptree = toptree_for_commit(&ctx.repo, *parent)?;
        if ptree == tree {
            match identical {
                Some(previous) => {
                    let mergebase = ctx
                        .repo
                        .merge_bases_many(previous, &[*parent])?
                        .into_iter()
                        .next()
                        .map(|id| id.detach());
                    if mergebase == Some(previous) {
                        identical = Some(*parent);
                    } else if mergebase != Some(*parent) {
                        // No common history: neither candidate subsumes the
                        // other, so the commit has to be copied.
                        copycommit = true;
                    }
                }
                None => identical = Some(*parent),
            }
        } else {
            nonidentical = Some(*parent);
        }

        if !gotparents.contains(parent) {
            gotparents.push(*parent);
        }
    }

    if let (Some(identical), Some(nonidentical)) = (identical, nonidentical) {
        let extras = rev_list(&ctx.repo, vec![nonidentical], vec![identical])?.len();
        if extras != 0 {
            copycommit = true;
        }
    }

    match identical {
        Some(identical) if !copycommit => Ok(identical),
        _ => copy_commit(ctx, rev, tree, &gotparents),
    }
}

// ---------------------------------------------------------------------------
// the split walk
// ---------------------------------------------------------------------------

/// `check_parents [REVS...]`: rewrite any parent the walk has not reached yet.
///
/// `--topo-order --reverse` normally delivers parents first; a parent that is
/// missing anyway came in through an `--onto` or join-commit exclusion, and is
/// processed out of band with its parents fetched from the object database.
fn check_parents(ctx: &mut Ctx, parents: &[ObjectId], indent: usize) -> Result<()> {
    let missed: Vec<ObjectId> = parents
        .iter()
        .filter(|p| ctx.cache_get(&p.to_string()).is_none())
        .copied()
        .collect();
    for miss in missed {
        if ctx.notree.contains(&miss.to_string()) {
            continue;
        }
        ctx.indent = indent + 1;
        ctx.debug(&format!("incorrect order: {miss}"));
        process_split_commit(ctx, miss, None, indent + 1)?;
    }
    ctx.indent = indent;
    Ok(())
}

/// `process_split_commit REV PARENTS`: map one mainline commit to its subtree
/// counterpart, recording either the rewritten commit or the fact that this
/// revision has no subtree at all.
///
/// `parents` is `None` for the out-of-order recursion from [`check_parents`],
/// which is the script's `test $indent -eq 0` branch: it re-reads the parents
/// from the object database and counts against `extracount` rather than
/// `revcount`.
fn process_split_commit(
    ctx: &mut Ctx,
    rev: ObjectId,
    parents: Option<Vec<ObjectId>>,
    indent: usize,
) -> Result<()> {
    let parents = match parents {
        Some(parents) if indent == 0 => {
            ctx.revcount += 1;
            parents
        }
        _ => {
            ctx.extracount += 1;
            ctx.repo
                .find_commit(rev)?
                .parent_ids()
                .map(|id| id.detach())
                .collect()
        }
    };

    ctx.indent = indent;
    ctx.progress(&format!(
        "{}/{} ({}) [{}]",
        ctx.revcount, ctx.revmax, ctx.createcount, ctx.extracount
    ));
    ctx.debug(&format!("Processing commit: {rev}"));

    ctx.indent = indent + 1;
    if let Some(existing) = ctx.cache_get(&rev.to_string()) {
        let existing = existing.clone();
        ctx.debug(&format!("prior: {existing}"));
        ctx.indent = indent;
        return Ok(());
    }
    ctx.createcount += 1;
    ctx.debug(&format!(
        "parents: {}",
        parents
            .iter()
            .map(ObjectId::to_string)
            .collect::<Vec<_>>()
            .join(" ")
    ));
    check_parents(ctx, &parents, indent + 1)?;
    ctx.indent = indent + 1;

    let newparents: Vec<ObjectId> = parents
        .iter()
        .filter_map(|p| ctx.cache_get(&p.to_string()).cloned())
        .filter_map(|s| ObjectId::from_hex(s.as_bytes()).ok())
        .collect();
    // `newparents=$(cache_get $parents)` is one *line* per parent, and the
    // command substitution keeps the newlines, so the debug line wraps.
    ctx.debug(&format!(
        "newparents: {}",
        newparents
            .iter()
            .map(ObjectId::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    ));

    let tree = subtree_for_commit(&ctx.repo, rev, &ctx.dir)?;
    ctx.debug(&format!(
        "tree is: {}",
        tree.map(|t| t.to_string()).unwrap_or_default()
    ));

    // No `<dir>` in this revision: it is a mainline commit from before the
    // subtree existed (or a subtree commit already at the root). Remember that so
    // `check_parents` does not chase it again, and let a commit that already has
    // rewritten parents stand in for itself.
    let Some(tree) = tree else {
        let hex = rev.to_string();
        ctx.notree.insert(hex.clone());
        if !newparents.is_empty() {
            ctx.cache_set(&hex, &hex)?;
        }
        ctx.indent = indent;
        return Ok(());
    };

    let newrev = copy_or_skip(ctx, rev, tree, &newparents)?;
    ctx.debug(&format!("newrev is: {newrev}"));
    let rev = rev.to_string();
    let newrev = newrev.to_string();
    ctx.cache_set(&rev, &newrev)?;
    ctx.cache_set("latest_new", &newrev)?;
    ctx.cache_set("latest_old", &rev)?;
    ctx.indent = indent;
    Ok(())
}

// ---------------------------------------------------------------------------
// preconditions
// ---------------------------------------------------------------------------

/// `ensure_clean`: neither the worktree nor the index may differ from `HEAD`.
fn ensure_clean(ctx: &Ctx) -> Result<()> {
    if !git_ok(ctx, &["diff-index", "HEAD", "--exit-code", "--quiet"])? {
        return die("fatal: working tree has modifications.  Cannot add.");
    }
    if !git_ok(ctx, &["diff-index", "--cached", "HEAD", "--exit-code", "--quiet"])? {
        return die("fatal: index has modifications.  Cannot add.");
    }
    Ok(())
}

/// `ensure_valid_ref_format REF`: the name must be usable as a branch.
fn ensure_valid_ref_format(ctx: &Ctx, name: &str) -> Result<()> {
    if git_ok(ctx, &["check-ref-format", &format!("refs/heads/{name}")])? {
        return Ok(());
    }
    die(&format!("fatal: '{name}' does not look like a ref"))
}

// ---------------------------------------------------------------------------
// subcommands
// ---------------------------------------------------------------------------

/// `cmd_add REV` / `cmd_add REPOSITORY REF`.
fn cmd_add(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    ensure_clean(ctx)?;

    match args.len() {
        1 => {
            if rev_parse_commit(&ctx.repo, &format!("{}^{{commit}}", args[0])).is_none() {
                return die(&format!("fatal: '{}' does not refer to a commit", args[0]));
            }
            cmd_add_commit(ctx, &args[0])
        }
        2 => {
            // A refspec would be misleading: only the one fetched tip is added.
            ensure_valid_ref_format(ctx, &args[1])?;
            cmd_add_repository(ctx, &args[0], &args[1])
        }
        _ => {
            ctx.say_err(&format!("fatal: parameters were '{}'", args.join(" ")));
            die("Provide either a commit or a repository and commit.")
        }
    }
}

/// `cmd_add_repository REPOSITORY REFSPEC`: fetch the ref, then add its tip.
///
/// The script adds `FETCH_HEAD`, and so does this when `git fetch` wrote one.
/// When it did not, the tip is named by `git ls-remote <repository> <ref>`
/// instead — the objects have already arrived either way, so the resulting
/// commit is the same; the cost is one extra connection to the remote.
fn cmd_add_repository(ctx: &mut Ctx, repository: &str, refspec: &str) -> Result<()> {
    println!("git fetch {repository} {refspec}");
    git_run(ctx, &["fetch", repository, refspec])?;
    // Re-open so the pack and `FETCH_HEAD` the child just wrote are visible.
    ctx.repo = gix::discover(".")?;
    if rev_parse_commit(&ctx.repo, "FETCH_HEAD^{commit}").is_some() {
        return cmd_add_commit(ctx, "FETCH_HEAD");
    }

    let listing = git_capture(ctx, &["ls-remote", repository, refspec])?;
    let Some(rev) = dwim_remote_ref(&listing, refspec) else {
        return die(&format!("fatal: couldn't find remote ref {refspec}"));
    };
    if rev_parse_commit(&ctx.repo, &format!("{rev}^{{commit}}")).is_none() {
        return die(&format!(
            "fatal: '{rev}' was not fetched; the remote moved while '{refspec}' was being fetched"
        ));
    }
    cmd_add_commit(ctx, &rev)
}

/// Pick the object id `git fetch <repository> <ref>` would have recorded in
/// `FETCH_HEAD`, applying git's `ref_rev_parse_rules` precedence to the
/// `git ls-remote` listing (exact name first, then `refs/`, tags, heads, remotes).
fn dwim_remote_ref(listing: &str, wanted: &str) -> Option<String> {
    let rows: Vec<(&str, &str)> = listing
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .collect();
    let candidates = [
        wanted.to_string(),
        format!("refs/{wanted}"),
        format!("refs/tags/{wanted}"),
        format!("refs/heads/{wanted}"),
        format!("refs/remotes/{wanted}"),
        format!("refs/remotes/{wanted}/HEAD"),
    ];
    for candidate in candidates {
        if let Some((id, _)) = rows.iter().find(|(_, name)| *name == candidate) {
            return Some((*id).to_string());
        }
    }
    None
}

/// `cmd_add_commit REV`: graft `rev`'s tree in at `<dir>` and record the join.
fn cmd_add_commit(ctx: &mut Ctx, spec: &str) -> Result<()> {
    let Some(rev) = rev_parse_commit(&ctx.repo, &format!("{spec}^{{commit}}")) else {
        return exit_with(128);
    };

    ctx.debug(&format!("Adding {} as '{rev}'...", ctx.dir));
    // A `--rejoin` reaches here with `<dir>` already in the index; only a genuine
    // `add` has to read the subtree in.
    if !ctx.split_rejoin {
        let prefix = format!("--prefix={}", ctx.dir);
        git_run(ctx, &["read-tree", &prefix, &rev.to_string()])?;
    }
    let dir = ctx.dir.clone();
    git_run(ctx, &["checkout", "--", &dir])?;
    let tree = git_capture(ctx, &["write-tree"])?;
    let Ok(tree) = ObjectId::from_hex(tree.as_bytes()) else {
        return exit_with(128);
    };

    let Some(headrev) = rev_parse_commit(&ctx.repo, "HEAD") else {
        return exit_with(128);
    };
    let headp: Vec<ObjectId> = if headrev != rev { vec![headrev] } else { Vec::new() };

    let commit = if ctx.addmerge_squash {
        let squashed = new_squash_commit(ctx, None, None, rev)?;
        let message = add_squashed_msg(ctx, squashed);
        let mut parents = headp;
        parents.push(squashed);
        commit_tree(ctx, tree, &parents, &message)?
    } else {
        let message = add_msg(ctx, Some(headrev), rev);
        let mut parents = headp;
        parents.push(rev);
        commit_tree(ctx, tree, &parents, &message)?
    };
    git_run(ctx, &["reset", &commit.to_string()])?;

    ctx.say_err(&format!("Added dir '{}'", ctx.dir));
    Ok(())
}

/// `cmd_split [REV] [REPOSITORY]`: rewrite `<dir>`'s history into a standalone
/// commit chain and return its tip.
///
/// The caller prints the tip: `git subtree split` writes it to stdout, while
/// `cmd_push` captures it as the thing to push.
fn cmd_split(ctx: &mut Ctx, args: &[String]) -> Result<String> {
    let rev = match args.len() {
        0 => match rev_parse_commit(&ctx.repo, "HEAD") {
            Some(id) => id,
            None => return exit_with(128),
        },
        1 | 2 => match rev_parse_commit(&ctx.repo, &format!("{}^{{commit}}", args[0])) {
            Some(id) => id,
            None => return die(&format!("fatal: '{}' does not refer to a commit", args[0])),
        },
        _ => {
            return die(&format!(
                "fatal: you must provide exactly one revision, and optionally a repository.  Got: '{}'",
                args.join(" ")
            ))
        }
    };

    // The prefix is checked against the commit here, not against the worktree as
    // it is for every other subcommand.
    if ctx.repo.rev_parse_single(format!("{rev}:{}", ctx.dir).as_str()).is_err() {
        return die(&format!(
            "fatal: '{}' does not exist; use 'git subtree add'",
            ctx.dir
        ));
    }
    let repository = args.get(1).cloned();

    if ctx.split_rejoin {
        ensure_clean(ctx)?;
    }

    ctx.debug(&format!("Splitting {}...", ctx.dir));

    if let Some(onto) = ctx.split_onto.clone() {
        ctx.debug(&format!("Reading history for --onto={onto}..."));
        let Some(onto) = rev_parse_commit(&ctx.repo, &onto) else {
            return exit_with(128);
        };
        // The `onto` history is already just the subdir, so any commit found
        // there can be used as a rewritten parent verbatim.
        for (id, _) in rev_list(&ctx.repo, vec![onto], Vec::new())? {
            let id = id.to_string();
            ctx.debug(&format!("cache: {id}"));
            ctx.cache_set(&id, &id)?;
        }
    }

    let unrevs = find_existing_splits(ctx, rev, repository.as_deref())?;

    // The walk cannot be path-limited to `<dir>`: some of the commits it must
    // visit have the subtree's contents at the root instead.
    let infos = rev_list(&ctx.repo, vec![rev], unrevs)?;
    let mut parents_of: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    let mut ids: Vec<ObjectId> = Vec::with_capacity(infos.len());
    for (id, parents) in infos {
        parents_of.insert(id, parents);
        ids.push(id);
    }
    let mut ordered = topo_sort(&ids, &parents_of);
    ordered.reverse();

    ctx.revmax = ordered.len();
    ctx.revcount = 0;
    ctx.createcount = 0;
    ctx.extracount = 0;
    for id in ordered {
        let parents = parents_of.get(&id).cloned().unwrap_or_default();
        process_split_commit(ctx, id, Some(parents), 0)?;
    }

    let Some(latest_new) = ctx.cache_get("latest_new").cloned() else {
        return die("fatal: no new revisions were found");
    };

    if ctx.split_rejoin {
        ctx.debug("Merging split branch into HEAD...");
        let latest_old = ctx.cache_get("latest_old").cloned().unwrap_or_default();
        // `arg_addmerge_message="$(rejoin_msg …)"`: the command substitution
        // strips the heredoc's trailing newline, and `add_msg` re-adds it.
        let message = rejoin_msg(ctx, &latest_old, &latest_new)
            .trim_end_matches('\n')
            .to_string();
        ctx.addmerge_message = Some(message);
        if find_latest_squash(ctx, None)?.is_none() {
            cmd_add(ctx, std::slice::from_ref(&latest_new))?;
        } else {
            cmd_merge(ctx, std::slice::from_ref(&latest_new))?;
        }
    }

    if let Some(branch) = ctx.split_branch.clone() {
        let action = if rev_exists(&ctx.repo, &format!("refs/heads/{branch}")) {
            let Some(existing) = rev_parse_commit(&ctx.repo, &format!("refs/heads/{branch}")) else {
                return exit_with(128);
            };
            let Ok(tip) = ObjectId::from_hex(latest_new.as_bytes()) else {
                return exit_with(128);
            };
            let is_ancestor = ctx
                .repo
                .merge_bases_many(existing, &[tip])?
                .into_iter()
                .any(|id| id.detach() == existing);
            if !is_ancestor {
                return die(&format!(
                    "fatal: branch '{branch}' is not an ancestor of commit '{latest_new}'."
                ));
            }
            "Updated"
        } else {
            "Created"
        };
        git_run(
            ctx,
            &[
                "update-ref",
                "-m",
                "subtree split",
                &format!("refs/heads/{branch}"),
                &latest_new,
            ],
        )?;
        ctx.say_err(&format!("{action} branch '{branch}'"));
    }

    Ok(latest_new)
}

/// `cmd_push REPOSITORY [+][LOCALREV:]REMOTEREF`: split, then push the split tip
/// to the remote branch.
fn cmd_push(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    if args.len() != 2 {
        return die("fatal: you must provide <repository> <refspec>");
    }
    if !Path::new(&ctx.dir).exists() {
        return die(&format!(
            "fatal: '{}' must already exist. Try 'git subtree add'.",
            ctx.dir
        ));
    }

    let repository = args[0].clone();
    let refspec = args[1].strip_prefix('+').unwrap_or(&args[1]).to_string();
    let (localrevname, remoteref) = match refspec.split_once(':') {
        Some((local, remote)) => (local.to_string(), remote.to_string()),
        None => ("HEAD".to_string(), refspec.clone()),
    };
    ensure_valid_ref_format(ctx, &remoteref)?;
    let Some(localrev) = rev_parse_commit(&ctx.repo, &format!("{localrevname}^{{commit}}")) else {
        return die(&format!(
            "fatal: '{localrevname}' does not refer to a commit"
        ));
    };

    println!("git push using:  {repository} {refspec}");
    let split = cmd_split(ctx, &[localrev.to_string(), repository.clone()])?;
    git_run(
        ctx,
        &[
            "push",
            &repository,
            &format!("{split}:refs/heads/{remoteref}"),
        ],
    )
}

/// `cmd_merge REV [REPOSITORY]`: merge a subtree revision into `$dir`.
///
/// Under `--squash` the revision is first collapsed to a single squash commit
/// chained onto the previous one, so the mainline never gains the subtree's own
/// history. Either way the merge itself is `git merge --no-ff -Xsubtree=<prefix>`,
/// which reshapes the incoming tree to sit under the prefix.
fn cmd_merge(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    if args.is_empty() || args.len() > 2 {
        return die(&format!(
            "fatal: you must provide exactly one revision, and optionally a repository. Got: '{}'",
            args.join(" ")
        ));
    }
    let Some(mut rev) = rev_parse_commit(&ctx.repo, &format!("{}^{{commit}}", args[0])) else {
        return die(&format!("fatal: '{}' does not refer to a commit", args[0]));
    };
    let repository = args.get(1).cloned();
    ensure_clean(ctx)?;

    if ctx.addmerge_squash {
        let Some((old, sub)) = find_latest_squash(ctx, repository.as_deref())? else {
            return die(&format!(
                "fatal: can't squash-merge: '{}' was never added.",
                ctx.dir
            ));
        };
        if sub == rev.to_string() {
            ctx.say_err(&format!("Subtree is already at commit {rev}."));
            return exit_with(0);
        }
        let old = ObjectId::from_hex(old.as_bytes()).ok();
        let oldsub = ObjectId::from_hex(sub.as_bytes()).ok();
        let new = new_squash_commit(ctx, old, oldsub, rev)?;
        ctx.debug(&format!("New squash commit: {new}"));
        rev = new;
    }

    // `$arg_gpg_sign` is only ever empty or `--no-gpg-sign` here (the parser
    // refuses `-S`), and `--no-gpg-sign` is what an unsigned merge already does,
    // so the merge is spelled without it.
    let subtree = format!("-Xsubtree={}", ctx.prefix);
    let rev = rev.to_string();
    match ctx.addmerge_message.clone() {
        Some(m) => git_run(
            ctx,
            &["merge", "--no-ff", &subtree, &format!("--message={m}"), &rev],
        ),
        None => git_run(ctx, &["merge", "--no-ff", &subtree, &rev]),
    }
}

/// `cmd_pull REPOSITORY REF`: fetch the ref, then merge the fetched tip.
fn cmd_pull(ctx: &mut Ctx, args: &[String]) -> Result<()> {
    if args.len() != 2 {
        return die("fatal: you must provide <repository> <ref>");
    }
    let (repository, refname) = (args[0].clone(), args[1].clone());
    ensure_clean(ctx)?;
    ensure_valid_ref_format(ctx, &refname)?;
    git_run(ctx, &["fetch", &repository, &refname])?;
    // Re-open so the pack and `FETCH_HEAD` the child just wrote are visible.
    ctx.repo = gix::discover(".")?;
    cmd_merge(ctx, &["FETCH_HEAD".to_string(), repository])
}

// ---------------------------------------------------------------------------
// entry point
// ---------------------------------------------------------------------------

/// `git subtree` — dispatch to `add`, `merge`, `split`, `pull` or `push`.
///
/// The stages follow `main` in the script: normalize the command line, require a
/// work tree, pre-scan for `--rejoin` (which decides whether the `add`/`merge`
/// options are legal on `split`/`push`), read the options for real, then validate
/// the prefix against the filesystem.
pub fn subtree(args: &[String]) -> Result<ExitCode> {
    match run(args) {
        Ok(code) => Ok(code),
        Err(e) => match e.downcast_ref::<Exit>() {
            Some(exit) => Ok(ExitCode::from(exit.0)),
            None => Err(e),
        },
    }
}

/// The body of [`subtree`], with the script's `exit`/`die` paths as errors.
fn run(args: &[String]) -> Result<ExitCode> {
    // `if test $# -eq 0; then set -- -h; fi`
    let argv: Vec<String> = if args.is_empty() {
        vec!["-h".to_string()]
    } else {
        args.to_vec()
    };
    let (opts, positionals) = parseopt(&argv)?;

    let repo = gix::discover(".")?;
    // `. git-sh-setup` with `SUBDIRECTORY_OK` unset: refuse a bare repository,
    // then run from the top of the work tree, since `<prefix>` and every
    // index/worktree child command are relative to it.
    let Some(workdir) = repo.workdir().map(Path::to_path_buf) else {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("git"));
        return die(&format!(
            "fatal: {} cannot be used without a working tree.",
            exe.display()
        ));
    };
    std::env::set_current_dir(&workdir)?;
    let repo = gix::discover(".")?;

    // First pass: only `--rejoin`, so the real pass can tell a caller that
    // `--squash` on a plain `split` is a mistake but on `split --rejoin` is not.
    let mut split_rejoin = false;
    for opt in &opts {
        match opt.as_str() {
            "--rejoin" => split_rejoin = true,
            "--no-rejoin" => split_rejoin = false,
            _ => {}
        }
    }

    let command = match positionals.first().map(String::as_str) {
        Some("add") => Cmd::Add,
        Some("merge") => Cmd::Merge,
        Some("split") => Cmd::Split,
        Some("pull") => Cmd::Pull,
        Some("push") => Cmd::Push,
        other => {
            return die(&format!(
                "fatal: unknown command '{}'",
                other.unwrap_or_default()
            ))
        }
    };
    let allow_split = matches!(command, Cmd::Split | Cmd::Push);
    let allow_addmerge = match command {
        Cmd::Add | Cmd::Merge | Cmd::Pull => true,
        Cmd::Split | Cmd::Push => split_rejoin,
    };

    // `die_incompatible_opt OPTION COMMAND`, quoting the normalized long form.
    let incompatible = |opt: &str| -> Result<()> {
        die(&format!(
            "fatal: the '{opt}' flag does not make sense with 'git subtree {}'.",
            command.name()
        ))
    };

    let mut quiet = false;
    let mut debug = false;
    let mut prefix = String::new();
    let mut split_branch: Option<String> = None;
    let mut split_onto: Option<String> = None;
    let mut split_ignore_joins = false;
    let mut split_annotate = String::new();
    let mut addmerge_squash = false;
    let mut addmerge_message: Option<String> = None;
    // `$arg_gpg_sign` verbatim, for the `debug "gpg-sign: {…}"` line.
    let mut gpg_sign = String::new();

    for opt in &opts {
        let (name, value) = match opt.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (opt.as_str(), None),
        };
        match name {
            "--quiet" => quiet = true,
            "--debug" => debug = true,
            "--annotate" => {
                if !allow_split {
                    incompatible(opt)?;
                }
                split_annotate = value.unwrap_or_default().to_string();
            }
            "--no-annotate" => {
                if !allow_split {
                    incompatible(opt)?;
                }
                split_annotate.clear();
            }
            "--branch" => {
                if !allow_split {
                    incompatible(opt)?;
                }
                split_branch = Some(value.unwrap_or_default().to_string());
            }
            "--prefix" => prefix = value.unwrap_or_default().to_string(),
            "--no-prefix" => prefix.clear(),
            "--message" => {
                if !allow_addmerge {
                    incompatible(opt)?;
                }
                addmerge_message = Some(value.unwrap_or_default().to_string());
            }
            "--onto" => {
                if !allow_split {
                    incompatible(opt)?;
                }
                split_onto = Some(value.unwrap_or_default().to_string());
            }
            "--no-onto" => {
                if !allow_split {
                    incompatible(opt)?;
                }
                split_onto = None;
            }
            "--rejoin" | "--no-rejoin" => {
                if !allow_split {
                    incompatible(opt)?;
                }
            }
            "--ignore-joins" => {
                if !allow_split {
                    incompatible(opt)?;
                }
                split_ignore_joins = true;
            }
            "--no-ignore-joins" => {
                if !allow_split {
                    incompatible(opt)?;
                }
                split_ignore_joins = false;
            }
            "--squash" => {
                if !allow_addmerge {
                    incompatible(opt)?;
                }
                addmerge_squash = true;
            }
            "--no-squash" => {
                if !allow_addmerge {
                    incompatible(opt)?;
                }
                addmerge_squash = false;
            }
            // The script forwards `$arg_gpg_sign` to `git commit-tree`, which
            // this build refuses for want of a signing driver. `--no-gpg-sign`
            // names what already happens, so it is honoured.
            "--no-gpg-sign" => gpg_sign = opt.clone(),
            "--gpg-sign" => {
                bail!(
                    "`-S`/`--gpg-sign` is not supported (no signing driver in the vendored \
                     crates); `--no-gpg-sign` is accepted, since this build writes unsigned \
                     commits"
                )
            }
            _ => return die(&format!("fatal: unexpected option: {opt}")),
        }
    }

    if prefix.is_empty() {
        return die("fatal: you must provide the --prefix option.");
    }

    match command {
        Cmd::Add => {
            if Path::new(&prefix).exists() {
                return die(&format!("fatal: prefix '{prefix}' already exists."));
            }
        }
        // Checked later against the commit, not the working tree.
        Cmd::Split => {}
        _ => {
            if !Path::new(&prefix).exists() {
                return die(&format!(
                    "fatal: '{prefix}' does not exist; use 'git subtree add'"
                ));
            }
        }
    }

    // `dir="$(dirname "$arg_prefix/.")"`: the prefix with any trailing slashes
    // removed, which is the spelling every message and trailer uses.
    let trimmed = prefix.trim_end_matches('/');
    let dir = if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    };

    let exe = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("cannot resolve the git binary: {e}"))?;
    let mut ctx = Ctx {
        repo,
        exe,
        command,
        quiet,
        debug,
        indent: 0,
        prefix: prefix.clone(),
        dir,
        split_branch,
        split_onto,
        split_ignore_joins,
        split_annotate,
        split_rejoin,
        addmerge_squash,
        addmerge_message,
        cache: HashMap::new(),
        notree: HashSet::new(),
        revcount: 0,
        revmax: 0,
        createcount: 0,
        extracount: 0,
    };

    ctx.debug(&format!("command: {{{}}}", ctx.command.name()));
    ctx.debug(&format!(
        "quiet: {{{}}}",
        if ctx.quiet { "1" } else { "" }
    ));
    ctx.debug(&format!("dir: {{{}}}", ctx.dir));
    ctx.debug(&format!("opts: {{{}}}", positionals[1..].join(" ")));
    ctx.debug(&format!("gpg-sign: {{{gpg_sign}}}"));
    ctx.debug("");

    let rest = positionals[1..].to_vec();
    match ctx.command {
        Cmd::Add => cmd_add(&mut ctx, &rest)?,
        Cmd::Split => {
            let latest = cmd_split(&mut ctx, &rest)?;
            println!("{latest}");
        }
        Cmd::Push => cmd_push(&mut ctx, &rest)?,
        Cmd::Merge => cmd_merge(&mut ctx, &rest)?,
        Cmd::Pull => cmd_pull(&mut ctx, &rest)?,
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `git rev-parse --parseopt --stuck-long` folds every spelling of an option
    /// into one long, value-attached token.
    #[test]
    fn parseopt_normalizes_every_spelling() {
        let argv = |s: &str| -> Vec<String> { s.split(' ').map(str::to_string).collect() };
        let opts = |s: &str| parseopt(&argv(s)).unwrap().0;

        assert_eq!(opts("-P lib add"), vec!["--prefix=lib"]);
        assert_eq!(opts("-Plib add"), vec!["--prefix=lib"]);
        assert_eq!(opts("--prefix lib add"), vec!["--prefix=lib"]);
        assert_eq!(opts("--pref=lib add"), vec!["--prefix=lib"]);
        assert_eq!(
            opts("-qP lib add"),
            vec!["--quiet".to_string(), "--prefix=lib".to_string()]
        );
        assert_eq!(opts("--no-squash add"), vec!["--no-squash"]);
        assert_eq!(opts("-S add"), vec!["--gpg-sign"]);
        assert_eq!(opts("-Skey add"), vec!["--gpg-sign=key"]);
        assert_eq!(opts("--squa add"), vec!["--squash"]);
    }

    /// Positionals keep their order across `--` and interleaved options, because
    /// `parse_options` permutes rather than stopping at the first non-option.
    #[test]
    fn parseopt_permutes_positionals() {
        let argv: Vec<String> = "-P lib add -- HEAD"
            .split(' ')
            .map(str::to_string)
            .collect();
        let (opts, positionals) = parseopt(&argv).unwrap();
        assert_eq!(opts, vec!["--prefix=lib"]);
        assert_eq!(positionals, vec!["add", "HEAD"]);
    }

    /// `--n`/`--no`/`--no-` abbreviate every negatable option, so git reports the
    /// last two in spec order as the ambiguity.
    #[test]
    fn parseopt_reports_ambiguity_like_parse_options() {
        let err = parseopt(&[String::from("--no")]).unwrap_err();
        assert_eq!(err.downcast_ref::<Exit>().map(|e| e.0), Some(129));
    }

    /// A blob at `<dir>` is a fatal, and the `/*` in the `git-subtree-dir`
    /// pattern means "zero or more slashes" rather than "any characters".
    #[test]
    fn grep_dir_tolerates_trailing_slashes_only() {
        let grep = Grep::Dir("lib".to_string());
        assert!(grep.matches(b"x\n\ngit-subtree-dir: lib\n"));
        assert!(grep.matches(b"git-subtree-dir: lib//\n"));
        assert!(!grep.matches(b"git-subtree-dir: library\n"));
        assert!(!grep.matches(b"prefixed git-subtree-dir: lib\n"));
    }

    /// Trailers are read field-wise, last occurrence winning, exactly as the
    /// script's `while read a b junk` loop reads them.
    #[test]
    fn trailers_take_the_last_value() {
        let (main, split) = subtree_trailers(
            b"Split 'lib/' into commit 'aaa'\n\
              \n\
              git-subtree-dir: lib\n\
              git-subtree-mainline: 1111111111111111111111111111111111111111\n\
              git-subtree-split: 2222222222222222222222222222222222222222\n",
        );
        assert_eq!(
            main.as_deref(),
            Some("1111111111111111111111111111111111111111")
        );
        assert_eq!(
            split.as_deref(),
            Some("2222222222222222222222222222222222222222")
        );
    }

    /// `git fetch <repo> <ref>` records the tip git's DWIM rules pick, and a
    /// branch outranks a same-named remote-tracking ref.
    #[test]
    fn remote_ref_dwim_follows_rev_parse_rules() {
        let listing = "aaa\trefs/remotes/main\nbbb\trefs/heads/main\n";
        assert_eq!(dwim_remote_ref(listing, "main").as_deref(), Some("bbb"));
        assert_eq!(
            dwim_remote_ref("ccc\trefs/tags/v1\n", "v1").as_deref(),
            Some("ccc")
        );
        assert_eq!(dwim_remote_ref("ccc\trefs/tags/v1\n", "v2"), None);
    }
}
