//! `git filter-branch` — rewrite branches.
//!
//! A port of `$(git --exec-path)/git-filter-branch` (git 2.55.0), a 665-line
//! POSIX shell script rather than a C builtin. The script is the spec: every
//! message, exit code, file under the `.git-rewrite` scratch directory and the
//! order in which the filters run is taken from it, and the code below follows
//! it top to bottom (`$functions`, `set_ident`, the option loop, the backup-ref
//! scan, the rewrite loop, `remap_to_ancestor`, the ref update, the tag pass,
//! the `--state-branch` save).
//!
//! The filters are user shell code, so they must run in a shell. This port
//! keeps **one** `/bin/sh` for the whole run — the script's own shell — started
//! in the temporary work tree with `GIT_DIR`, `GIT_WORK_TREE=.` and
//! `GIT_INDEX_FILE` exported, and drives it over its stdin with an exit-status
//! channel on file descriptor 3. That is what makes `--setup` work (it defines
//! shell functions and variables the later filters call) and what makes an
//! `--env-filter` assignment to `GIT_AUTHOR_NAME` survive into the
//! `git commit-tree` that follows it, exactly as in the script. Only the
//! script's `eval`s are sent to that shell; all of its control flow, its
//! old->new commit map and its ref bookkeeping are Rust.
//!
//! Ported and behaving as stock git:
//!
//!   * The startup warning block and `Proceeding with filter-branch...` on
//!     stdout, the ten-second `sleep`, and the `FILTER_BRANCH_SQUELCH_WARNING` /
//!     `GIT_TEST_DISALLOW_ABBREVIATED_OPTIONS` squelch (script lines 86-98).
//!   * `-h` as the first argument, handled by `git-sh-setup` because
//!     `OPTIONS_SPEC` is empty: `usage: git filter-branch <USAGE>` on stdout,
//!     exit 0, ahead of every other check.
//!   * `git-sh-setup`'s `git_dir_init` with `SUBDIRECTORY_OK` unset — the
//!     `You need to run this command from the toplevel of the working tree.`
//!     refusal — then `require_clean_work_tree 'rewrite branches'` for a
//!     non-bare repository (line 111): the unborn-`HEAD` `fatal: Needed a single
//!     revision`, and the `Cannot rewrite branches: You have unstaged changes.`
//!     / `... Your index contains uncommitted changes.` / `Additionally, your
//!     index contains uncommitted changes.` triple, on stderr, exit 1.
//!   * The hand-rolled option loop (lines 130-208). It is a `case` chain, not
//!     `parse_options`: no `=value` form, no abbreviation, no clustering, every
//!     non-boolean switch takes its value as the next argument, and a switch as
//!     the last argument is a usage error. `--original`'s value goes through the
//!     script's `expr` normalisation, so `--original refs/x//` is `refs/x/`.
//!   * `Cannot set --prune-empty and --commit-filter at the same time`,
//!     `-f`'s `rm -rf "$tempdir"`, `<tempdir> already exists, please remove it`
//!     (which reports the `-d` value as written, not resolved), and the
//!     `Cannot create a new backup.` refusal with its two follow-on lines.
//!   * The rewrite: `--setup`, `--env-filter`, `--tree-filter`, `--index-filter`,
//!     `--parent-filter`, `--msg-filter`, `--commit-filter`, `--prune-empty`,
//!     `--subdirectory-filter`, `--tag-name-filter`, `--original`, `-d`,
//!     `-f`/`--force`, `--state-branch` and the `--` separator, in the script's
//!     order, with `GIT_COMMIT` and the six `GIT_AUTHOR_*`/`GIT_COMMITTER_*`
//!     variables exported from each commit's own header the way `set_ident` and
//!     `finish_ident` build them (`@<timestamp> <tz>` dates, and the
//!     `${EMAIL%%@*}` fallback for an empty name).
//!   * The `$functions` text — `EMPTY_TREE`, `warn`, `map`, `skip_commit`,
//!     `git_commit_non_empty_tree`, the `die` with the extra line break — is
//!     embedded verbatim and prepended to `--commit-filter` exactly as the
//!     script does, so a commit filter calling `skip_commit "$@"` or
//!     `git_commit_non_empty_tree "$@"` behaves identically. The map lives in
//!     `$tempdir/map/<sha>` with the same one-file-per-commit layout, because
//!     `map()` reads those files directly.
//!   * The `\rRewrite <sha> (<n>/<m>)<progress>    ` line on stdout, including
//!     the script's quirks: the `printf` runs on every commit but `$count` is
//!     only refreshed when the sample condition fires, and `$progress` starts as
//!     the literal `dummy to ensure this is not empty`.
//!   * `Found nothing to rewrite` with exit status 2, the
//!     `WARNING: not rewriting '<ref>' (not a committish)` skip,
//!     `You must specify a ref to rewrite.`, `Ref '<ref>' was rewritten`,
//!     `Ref '<ref>' was deleted`, `WARNING: Ref '<ref>' is unchanged`, the
//!     `refs/original/` backups written with `filter-branch: backup`, the
//!     annotated-tag `WARNING: You said to rewrite tagged commits, ...` pair,
//!     the `<tag> -> <new> (<sha> -> <new sha>)` tag lines, the
//!     `gpg signature stripped from tag object <sha>` warning, and the
//!     `git read-tree -u -m HEAD` that refreshes the work tree at the end.
//!
//! Deliberate floors, refused rather than approximated:
//!
//!   * **The rev-list option surface.** The script hands `<rev-list options>`
//!     to `git rev-parse` and `git rev-list --simplify-merges`. This port
//!     understands the selection arguments — nothing, `<rev>`, `^<rev>`,
//!     `<a>..<b>`, `--all`, `--branches`, `--tags`, `--remotes`, `--` and plain
//!     pathspecs — and rejects everything else (`--since`, `--author`,
//!     `--max-count`, `<a>...<b>`, magic or wildcard pathspecs, …) with
//!     `unsupported rev-list argument`. Accepting one and ignoring it would
//!     silently rewrite a different set of commits than the user asked for.
//!   * **`git commit-tree`'s ident handling is probed before anything is
//!     rewritten.** The script's whole ident mechanism is `GIT_AUTHOR_DATE` in
//!     git's raw `@<timestamp> <tz>` form, and it is the `git` on `PATH` that
//!     has to parse it. The probe stays because that `git` is whichever one the
//!     user's `PATH` names — unlike stock, this port does not prepend its own
//!     directory — so a `PATH` pointing at a build without the form would
//!     otherwise re-date every commit in the range silently. Both stock git and
//!     this build pass it: `gix_date`'s parser learned the form as a port of
//!     git's `match_object_header_date` (`date.c:804-825`, mirrored in
//!     `src/ported/gix-date/src/parse/function.rs`), which is what
//!     `git var GIT_AUTHOR_IDENT` and `git commit-tree` read.
//!
//! Implementation substitutions, all stated rather than hidden:
//!
//!   * **Without an index filter the tree is built directly, not through the
//!     scratch index.** For `--tree-filter` and `--subdirectory-filter` the
//!     script runs `git read-tree -i -m <commit>` into `$GIT_INDEX_FILE`,
//!     `git checkout-index -f -u -a` out of it, `git clean -d -q -f -x`,
//!     `git update-index --add --replace --remove --stdin` back into it and
//!     `git write-tree` off it, purely to get from one tree to the next; nothing
//!     the user wrote ever looks at that index. Here those five steps are done
//!     natively: the commit's tree (or its `--subdirectory-filter` subtree, or
//!     the empty tree when the commit has no such directory) is written into the
//!     temporary work tree, everything left from the previous commit is removed
//!     first, and after the tree filter the work tree is hashed straight back
//!     into a tree object. `--index-filter` is the case where the index *is* the
//!     user's interface, so it gets a real one: the tree the filters have
//!     produced so far is loaded into `$GIT_INDEX_FILE` with `git read-tree -i
//!     -m <tree>`, the filter runs against it, and `git write-tree` reads the
//!     result back at the script's own point in the loop. That works because
//!     `GIT_INDEX_FILE` is now honoured — `gix::Repository::index_path()` reads
//!     it, a port of what `setup_git_env()` passes to `repo_set_gitdir()` — so
//!     the filter's `git rm --cached` and `git update-index` edit the scratch
//!     index and leave the caller's alone.
//!     The result is the same tree the script's index round trip produces, and
//!     the same commits come out, with three differences worth naming:
//!       - Checkout and re-hash apply no clean/smudge filters, where
//!         `checkout-index`/`update-index` do. For a repository with no filters
//!         (no `.gitattributes`, no `core.autocrlf`) that is identical; with
//!         filters, this port keeps the byte-exact blob where the script would
//!         round trip it.
//!       - Submodule entries survive because they are carried over from the
//!         commit's own tree, which is what the script's `--ignore-submodules`
//!         diff amounts to; a tree filter cannot change them either way.
//!       - Clearing the work tree removes a nested repository a tree filter may
//!         have created, where `git clean -d -f -x` would need `-ff`.
//!   * The scratch directory holds what the filters can see — `map/<sha>`,
//!     `commit` and `message` — plus a `message-in` holding the message the msg
//!     filter reads, where the script pipes it in from its own header-stripping
//!     loop. Its purely internal files (`backup-refs`, `raw-refs`, `heads`,
//!     `parse`, `revs`) have no on-disk equivalent here; that state is in memory.
//!   * `GIT_DIR`, `GIT_WORK_TREE=.` and `GIT_INDEX_FILE` are still exported to
//!     the filters, exactly as the script exports them, so a filter that runs
//!     `git` sees the environment it is written against — but with this build's
//!     plumbing ignoring the latter two, such a filter would work on the
//!     caller's real index and work tree. That is a plumbing gap, not something
//!     filter-branch can paper over; `--index-filter`, whose entire contract is
//!     that variable, is refused above rather than left to misfire.
//!
//! Two stock quirks are reproduced on purpose, and are bugs in the script:
//!
//!   * `test -f "$orig_namespace$ref"` (line 508) tests a path relative to the
//!     temporary work tree, not under `$GIT_DIR`, so it is false for a real
//!     backup ref and true only if a tree filter happened to create such a file.
//!   * `--state-branch`'s loader writes `${line%:*}` (the *old* id) into
//!     `map/${line#*:}` (named by the *new* id), inverting the map its own saver
//!     wrote (lines 301-309 against 638-645).
//!
//! Also note that submodule changes are compared in `require_clean_work_tree`,
//! whereas git passes `--ignore-submodules`; `gix::status` has no equivalent
//! knob, the same gap `rebase.rs` carries.

use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write as _};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;
use gix::objs::Write as _;

/// `$USAGE` from `git-filter-branch` lines 100-106, verbatim. The continuation
/// lines are indented with a single tab, as in the script.
const USAGE: &str = "\
[--setup <command>] [--subdirectory-filter <directory>] [--env-filter <command>]
\t[--tree-filter <command>] [--index-filter <command>]
\t[--parent-filter <command>] [--msg-filter <command>]
\t[--commit-filter <command>] [--tag-name-filter <command>]
\t[--original <namespace>]
\t[-d <directory>] [-f | --force] [--state-branch <branch>]
\t[--] [<rev-list options>...]";

/// The startup warning, lines 89-94 of the script. Emitted on stdout unless
/// squelched, followed by a ten-second sleep and `Proceeding with ...`.
const WARNING: &str = "\
WARNING: git-filter-branch has a glut of gotchas generating mangled history
\t rewrites.  Hit Ctrl-C before proceeding to abort, then use an
\t alternative filtering tool such as 'git filter-repo'
\t (https://github.com/newren/git-filter-repo/) instead.  See the
\t filter-branch manual page for more details; to squelch this warning,
\t set FILTER_BRANCH_SQUELCH_WARNING=1.
";

/// `$functions`, script lines 14-65, verbatim — the helpers a `--commit-filter`
/// is documented to call. The script prepends this to the user's fragment and
/// runs the result in a fresh `/bin/sh`, where `$workdir` comes in from the
/// command's environment, so `map()` can find `$workdir/../map/<sha>`.
const FUNCTIONS: &str = r#"EMPTY_TREE=$(git hash-object -t tree /dev/null)

warn () {
	echo "$*" >&2
}

map()
{
	# if it was not rewritten, take the original
	if test -r "$workdir/../map/$1"
	then
		cat "$workdir/../map/$1"
	else
		echo "$1"
	fi
}

# if you run 'skip_commit "$@"' in a commit filter, it will print
# the (mapped) parents, effectively skipping the commit.

skip_commit()
{
	shift;
	while [ -n "$1" ];
	do
		shift;
		map "$1";
		shift;
	done;
}

# if you run 'git_commit_non_empty_tree "$@"' in a commit filter,
# it will skip commits that leave the tree untouched, commit the other.
git_commit_non_empty_tree()
{
	if test $# = 3 && test "$1" = $(git rev-parse "$3^{tree}"); then
		map "$3"
	elif test $# = 1 && test "$1" = $EMPTY_TREE; then
		:
	else
		git commit-tree "$@"
	fi
}
# override die(): this version puts in an extra line break, so that
# the progress is still visible

die()
{
	echo >&2
	echo "$*" >&2
	exit 1
}"#;

/// Every switch in the inner `case "$ARG"` (lines 166-207). Each takes exactly
/// one argument, supplied as the following `argv` element — the script offers no
/// `--opt=value` form.
const VALUED: &[&str] = &[
    "-d",
    "--setup",
    "--subdirectory-filter",
    "--env-filter",
    "--tree-filter",
    "--index-filter",
    "--parent-filter",
    "--msg-filter",
    "--commit-filter",
    "--tag-name-filter",
    "--original",
    "--state-branch",
];

/// The script's control-flow exits — `die` (status 1), `die_with_status 2`, and
/// `exit $?` after a failed child — carried as an `anyhow` error so `?` unwinds
/// the way `exit` unwinds a shell. Any message is already on stderr by the time
/// this is constructed, exactly as `die` prints and then exits.
#[derive(Debug)]
struct Exit(u8);

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exit {}", self.0)
    }
}

impl std::error::Error for Exit {}

/// `git-sh-setup`'s `die`, which overrides the `$functions` one for the script's
/// own use: `die_with_status 1`, i.e. the message verbatim on stderr, exit 1.
fn die<T>(msg: &str) -> Result<T> {
    eprintln!("{msg}");
    Err(Exit(1).into())
}

/// `die_with_status <status> "$@"`: message on stderr, exit with that status.
fn die_with_status<T>(status: u8, msg: &str) -> Result<T> {
    eprintln!("{msg}");
    Err(Exit(status).into())
}

/// `usage()` as `git-sh-setup` defines it when `OPTIONS_SPEC` is empty
/// (git-sh-setup line 80): `die "usage: $dashless $USAGE"` — stderr, exit 1.
fn usage<T>() -> Result<T> {
    die(&format!("usage: git filter-branch {USAGE}"))
}

/// Quote a value for `/bin/sh`, so the driver shell receives it byte for byte.
fn sq(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

// ---------------------------------------------------------------------------
// the driver shell
// ---------------------------------------------------------------------------

/// The script's own `/bin/sh`, kept alive for the whole run so that `--setup`
/// definitions and `--env-filter` assignments persist across commits exactly as
/// they do in the script. Commands go in over stdin; each one is followed by a
/// `printf … >&3` that reports `$?` on a private pipe, which is how [`run`]
/// knows a fragment finished and whether it failed.
///
/// [`run`]: Shell::run
struct Shell {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    status: BufReader<fs::File>,
}

impl Shell {
    /// Start `/bin/sh` in `cwd` with `env` exported and a status pipe on fd 3.
    fn spawn(cwd: &Path, env: &[(&str, String)]) -> Result<Self> {
        let mut fds = [0 as libc::c_int; 2];
        // A plain pipe(2): neither end is close-on-exec, which is what lets the
        // write end survive into the shell as fd 3.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let (read_fd, write_fd) = (fds[0], fds[1]);

        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-s").current_dir(cwd).stdin(Stdio::piped());
        for (key, value) in env {
            cmd.env(key, value);
        }
        unsafe {
            cmd.pre_exec(move || {
                if libc::dup2(write_fd, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().map_err(|e| {
            unsafe { libc::close(read_fd) };
            unsafe { libc::close(write_fd) };
            anyhow::anyhow!("cannot run /bin/sh: {e}")
        })?;
        // Only the shell may hold the write end open, or the status pipe never
        // reports end-of-file when it dies.
        unsafe { libc::close(write_fd) };
        let stdin = child.stdin.take().expect("stdin was piped");
        let status = unsafe { <fs::File as std::os::fd::FromRawFd>::from_raw_fd(read_fd) };
        Ok(Self {
            child,
            stdin,
            status: BufReader::new(status),
        })
    }

    /// Run `code` in the shell and return its exit status.
    fn run(&mut self, code: &str) -> Result<i32> {
        write!(self.stdin, "{code}\nprintf '%s\\n' \"$?\" >&3\n")?;
        self.stdin.flush()?;
        let mut line = String::new();
        if self.status.read_line(&mut line)? == 0 {
            bail!("the filter shell exited while running: {code}");
        }
        Ok(line.trim().parse::<i32>().unwrap_or(1))
    }

    /// `name=<value>` in the shell, with `value` quoted so it arrives verbatim.
    fn set(&mut self, name: &str, value: &str) -> Result<()> {
        let code = format!("{name}={}", sq(value));
        match self.run(&code)? {
            0 => Ok(()),
            _ => bail!("could not set {name} in the filter shell"),
        }
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        // Closing stdin is the shell's end-of-input; kill covers a fragment
        // that left a child of its own attached to the terminal.
        let _ = self.stdin.write_all(b"exit 0\n");
        let _ = self.stdin.flush();
        if self.child.wait().is_err() {
            let _ = self.child.kill();
        }
    }
}

// ---------------------------------------------------------------------------
// options
// ---------------------------------------------------------------------------

/// The script's filter variables, lines 115-129, after the option loop.
#[derive(Default)]
struct Opts {
    tempdir: String,
    filter_setup: String,
    filter_env: String,
    filter_tree: String,
    filter_index: String,
    filter_parent: String,
    filter_msg: String,
    filter_commit: String,
    filter_tag_name: String,
    filter_subdir: String,
    state_branch: String,
    orig_namespace: String,
    force: bool,
    prune_empty: bool,
    remap_to_ancestor: bool,
    /// Whether `--commit-filter` was given, for the `--prune-empty` conflict.
    saw_commit_filter: bool,
}

/// The option loop, script lines 130-208. Returns the options and the tail that
/// becomes `"$@"` — the rev-list arguments.
fn parse_options(args: &[String]) -> Result<(Opts, Vec<String>)> {
    let mut opts = Opts {
        tempdir: ".git-rewrite".to_string(),
        filter_msg: "cat".to_string(),
        orig_namespace: "refs/original/".to_string(),
        ..Opts::default()
    };

    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();

        // The outer `case "$1"`: `--` ends options, three booleans consume
        // themselves, any other `-*` falls through to the valued handling, and
        // anything else (including an empty string) ends the loop.
        match arg {
            "--" => {
                i += 1;
                break;
            }
            "--force" | "-f" => {
                opts.force = true;
                i += 1;
                continue;
            }
            // Deprecated and inert: `$remap_to_ancestor` is set automatically.
            "--remap-to-ancestor" => {
                opts.remap_to_ancestor = true;
                i += 1;
                continue;
            }
            "--prune-empty" => {
                opts.prune_empty = true;
                i += 1;
                continue;
            }
            // `-*)` with an empty body: fall through. A bare `-` matches this
            // glob too, so it reaches the valued handling as an unknown option.
            _ if arg.starts_with('-') => {}
            _ => break,
        }

        // `case "$#" in 1) usage ;; esac` — the value must be a separate,
        // present argument. This fires for `--tree-filter` as the last word and
        // for a bare `-`, which reaches here as an unknown option with no value.
        if i + 1 >= args.len() {
            return usage();
        }
        let value = args[i + 1].clone();
        i += 2;

        if !VALUED.contains(&arg) {
            // The inner `case "$ARG"`'s `*)` arm. Note this also catches every
            // `--opt=value` spelling, which the script does not understand.
            return usage();
        }
        match arg {
            "-d" => opts.tempdir = value,
            "--setup" => opts.filter_setup = value,
            "--subdirectory-filter" => {
                opts.filter_subdir = value;
                opts.remap_to_ancestor = true;
            }
            "--env-filter" => opts.filter_env = value,
            "--tree-filter" => opts.filter_tree = value,
            "--index-filter" => opts.filter_index = value,
            "--parent-filter" => opts.filter_parent = value,
            "--msg-filter" => opts.filter_msg = value,
            "--commit-filter" => {
                opts.saw_commit_filter = true;
                opts.filter_commit = format!("{FUNCTIONS}; {value}");
            }
            "--tag-name-filter" => opts.filter_tag_name = value,
            // `orig_namespace=$(expr "$OPTARG/" : '\(.*[^/]\)/*$')/`: trailing
            // slashes collapse to exactly one.
            "--original" => {
                let trimmed = value.trim_end_matches('/');
                opts.orig_namespace = format!("{trimmed}/");
            }
            "--state-branch" => opts.state_branch = value,
            _ => return usage(),
        }
    }

    Ok((opts, args[i.min(args.len())..].to_vec()))
}

// ---------------------------------------------------------------------------
// entry point
// ---------------------------------------------------------------------------

/// `git filter-branch` — see the module documentation for the ported surface.
pub fn filter_branch(args: &[String]) -> Result<ExitCode> {
    match run(args) {
        Ok(code) => Ok(code),
        Err(e) => match e.downcast_ref::<Exit>() {
            Some(exit) => Ok(ExitCode::from(exit.0)),
            None => Err(e),
        },
    }
}

/// The body of [`filter_branch`], with the script's `exit`/`die` paths as errors.
fn run(args: &[String]) -> Result<ExitCode> {
    // The dispatcher passes the argument tail; tolerate the subcommand at
    // index 0 so both calling conventions behave identically.
    let args = match args.first() {
        Some(a) if a == "filter-branch" => &args[1..],
        _ => args,
    };

    // Script lines 86-98. The guard is `test -z "$A$B"`, i.e. both variables
    // empty or unset. It runs before anything else, `-h` included.
    let squelched = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    if !squelched("FILTER_BRANCH_SQUELCH_WARNING")
        && !squelched("GIT_TEST_DISALLOW_ABBREVIATED_OPTIONS")
    {
        print!("{WARNING}");
        let _ = std::io::stdout().flush();
        std::thread::sleep(std::time::Duration::from_secs(10));
        print!("Proceeding with filter-branch...\n\n");
    }

    // `git-sh-setup` line 88: `case "$1" in -h) echo "$LONG_USAGE"; exit`.
    // First argument only, stdout, exit 0, ahead of the worktree check.
    if args.first().is_some_and(|a| a == "-h") {
        println!("usage: git filter-branch {USAGE}");
        return Ok(ExitCode::SUCCESS);
    }

    let repo = gix::discover(".")?;
    let git_dir = repo.path().canonicalize()?;

    // `git_dir_init` with `SUBDIRECTORY_OK` unset: `git rev-parse --show-cdup`
    // must be empty. It fails outright in a bare repository, which the `test -z`
    // then accepts.
    if let Some(workdir) = repo.workdir() {
        let top = workdir.canonicalize()?;
        let here = std::env::current_dir()?.canonicalize()?;
        if top != here {
            return die("You need to run this command from the toplevel of the working tree.");
        }
    }

    // Script line 111: `if [ "$(is_bare_repository)" = false ]`.
    if !repo.is_bare() {
        require_clean_work_tree(&repo)?;
    }

    let (mut opts, rev_args) = parse_options(args)?;

    // Script lines 210-219: only `t,<non-empty>` is an error.
    match (opts.prune_empty, opts.saw_commit_filter) {
        (false, false) => opts.filter_commit = "git commit-tree \"$@\"".to_string(),
        (true, false) => {
            opts.filter_commit = format!("{FUNCTIONS}; git_commit_non_empty_tree \"$@\"");
        }
        (false, true) => {}
        (true, true) => {
            return die("Cannot set --prune-empty and --commit-filter at the same time");
        }
    }

    // Script lines 221-228.
    if opts.force {
        let _ = fs::remove_dir_all(&opts.tempdir);
    } else if Path::new(&opts.tempdir).is_dir() {
        return die(&format!("{} already exists, please remove it", opts.tempdir));
    }

    // The ident round-trip every rewritten commit depends on. See the module
    // documentation: this build's date parser rejects git's raw `@<ts> <tz>`
    // form, and running anyway would re-date the whole range.
    check_ident_roundtrip()?;

    let orig_dir = std::env::current_dir()?;
    fs::create_dir_all(Path::new(&opts.tempdir).join("t"))
        .map_err(|_| anyhow::anyhow!("could not create {}", opts.tempdir))
        .or_else(|_| die(""))?;
    let tempdir = Path::new(&opts.tempdir).canonicalize()?;
    // `trap 'cd "$orig_dir"; rm -rf "$tempdir"' 0` (line 237).
    let _cleanup = TempDir(tempdir.clone());

    let mut ctx = Ctx {
        exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("git")),
        git_dir,
        workdir: tempdir.join("t"),
        index_file: tempdir.join("index"),
        map_dir: tempdir.join("map"),
        tempdir,
        orig_dir,
        ident: Vec::new(),
    };

    rewrite(&repo, &mut ctx, &opts, &rev_args)
}

/// `git-sh-setup`'s `require_clean_work_tree 'rewrite branches'`.
///
/// filter-branch passes no `$2`, so there is no `Please commit or stash them.`
/// line. The `git update-index --refresh` it runs first is why stat-only
/// staleness does not count as an unstaged change here.
fn require_clean_work_tree(repo: &gix::Repository) -> Result<()> {
    // `git rev-parse --verify HEAD >/dev/null || exit 1`.
    if repo.head_id().is_err() {
        eprintln!("fatal: Needed a single revision");
        return Err(Exit(1).into());
    }
    let (unstaged, staged) = dirty_state(repo)?;
    if unstaged {
        eprintln!("Cannot rewrite branches: You have unstaged changes.");
        if staged {
            eprintln!("Additionally, your index contains uncommitted changes.");
        }
        return Err(Exit(1).into());
    }
    if staged {
        eprintln!("Cannot rewrite branches: Your index contains uncommitted changes.");
        return Err(Exit(1).into());
    }
    Ok(())
}

/// `(unstaged, staged)` for `require_clean_work_tree`, matching git's
/// `diff-files` / `diff-index --cached HEAD` pair.
fn dirty_state(repo: &gix::Repository) -> Result<(bool, bool)> {
    let mut unstaged = false;
    let mut staged = false;
    let patterns: Vec<BString> = Vec::new();
    for item in repo.status(gix::progress::Discard)?.into_iter(patterns)? {
        match item? {
            gix::status::Item::TreeIndex(_) => staged = true,
            gix::status::Item::IndexWorktree(iw) => {
                use gix::status::index_worktree::Item;
                use gix::status::plumbing::index_as_worktree::EntryStatus;
                match iw {
                    // Untracked files and stat-only staleness do not make the
                    // tree dirty, exactly as `diff-files` sees it after the
                    // `git update-index --refresh` the script runs first.
                    Item::Modification { status, .. } => match status {
                        EntryStatus::NeedsUpdate(_) => {}
                        _ => unstaged = true,
                    },
                    Item::Rewrite { .. } => unstaged = true,
                    Item::DirectoryContents { .. } => {}
                }
            }
        }
    }
    Ok((unstaged, staged))
}

/// Refuse the run unless the `git` on `PATH` reproduces git's ident handling.
///
/// `set_ident` exports `GIT_AUTHOR_DATE='@<timestamp> <tz>'` and a name that
/// still carries the trailing space `parse_ident_from_commit`'s `sed` leaves in
/// it; `git var GIT_AUTHOR_IDENT` answers with the normalised ident, which is
/// what `git commit-tree` will write into every rewritten commit. If the answer
/// differs, the rewrite would silently re-date or re-name the whole range.
fn check_ident_roundtrip() -> Result<()> {
    const WANT: &str = "A <a@example.com> 1112911993 +0000";
    let out = Command::new("git")
        .arg("var")
        .arg("GIT_AUTHOR_IDENT")
        .env("GIT_AUTHOR_NAME", "A ")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "@1112911993 +0000")
        .output();
    let got = match &out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim_end().to_string(),
        Ok(o) => String::from_utf8_lossy(&o.stderr).trim_end().to_string(),
        Err(e) => e.to_string(),
    };
    if got == WANT {
        return Ok(());
    }
    die(&format!(
        "refusing to rewrite: the 'git' on PATH does not honour the ident environment \
         filter-branch exports.\n\
         `git var GIT_AUTHOR_IDENT` with GIT_AUTHOR_NAME='A ' and \
         GIT_AUTHOR_DATE='@1112911993 +0000' answered\n  {got}\nexpected\n  {WANT}\n\
         Every rewritten commit would take the current time instead of its own date."
    ))
}

/// `trap 'cd "$orig_dir"; rm -rf "$tempdir"' 0` — the scratch directory goes
/// away on every exit from [`run`], including an error unwinding through it.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------
// plumbing re-execution
// ---------------------------------------------------------------------------

/// Paths and the environment the script exports at lines 249-250 and 286-287:
/// every child runs in `$tempdir/t` against the real `$GIT_DIR`, with the
/// temporary index and the temporary work tree.
struct Ctx {
    exe: PathBuf,
    git_dir: PathBuf,
    tempdir: PathBuf,
    workdir: PathBuf,
    index_file: PathBuf,
    map_dir: PathBuf,
    orig_dir: PathBuf,
    /// The six ident variables as the last rewritten commit left them in the
    /// shell. The script does not unset them until line 596, so the `git
    /// update-ref` calls that move the refs and rewrite the tags inherit them —
    /// which is whose name and date end up in the reflog. Cleared before the
    /// `--state-branch` save, exactly where the script unsets them.
    ident: Vec<(String, String)>,
}

impl Ctx {
    /// The environment the driver shell and every re-executed child receives.
    fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("GIT_DIR", self.git_dir.display().to_string()),
            ("GIT_WORK_TREE", ".".to_string()),
            ("GIT_INDEX_FILE", self.index_file.display().to_string()),
        ]
    }

    /// This binary, re-executed for the ref and object steps the script runs as
    /// `git <verb>` — the same substitution `subtree` makes.
    fn git(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.exe);
        cmd.args(args).current_dir(&self.workdir);
        for (key, value) in self.env() {
            cmd.env(key, value);
        }
        for (key, value) in &self.ident {
            cmd.env(key, value);
        }
        cmd
    }

    /// Run a plumbing command and return `(status, stdout)`, with its stderr going
    /// where the script's would — straight through to the caller's.
    fn git_output(&self, args: &[&str]) -> Result<(i32, String)> {
        let out = self
            .git(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| anyhow::anyhow!("could not run git {}: {e}", args.join(" ")))?;
        Ok((
            out.status.code().unwrap_or(1),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ))
    }

    /// Run a plumbing command, letting its output through, and return its status.
    fn git_status(&self, args: &[&str]) -> Result<i32> {
        Ok(self
            .git(args)
            .status()
            .map_err(|e| anyhow::anyhow!("could not run git {}: {e}", args.join(" ")))?
            .code()
            .unwrap_or(1))
    }

    /// `$tempdir/map/<sha>`, the one file per rewritten commit that `map()` reads.
    fn map_file(&self, id: &str) -> PathBuf {
        self.map_dir.join(id)
    }

    /// `map()` from `$functions`: the file's contents, or the id itself.
    fn map(&self, id: &str) -> String {
        match fs::read_to_string(self.map_file(id)) {
            Ok(text) => text,
            Err(_) => format!("{id}\n"),
        }
    }

    /// `$(map $id)` — the shell's command substitution, whose word splitting is
    /// what lets a `skip_commit` filter map one commit onto several parents.
    fn map_words(&self, id: &str) -> Vec<String> {
        self.map(id).split_whitespace().map(String::from).collect()
    }
}

// ---------------------------------------------------------------------------
// the rewrite
// ---------------------------------------------------------------------------

/// Script lines 239-665: everything from the first exported variable to the
/// closing `exit 0`.
fn rewrite(
    repo: &gix::Repository,
    ctx: &mut Ctx,
    opts: &Opts,
    rev_args: &[String],
) -> Result<ExitCode> {
    // Lines 252-266: refs/original must be empty, or `-f` clears it.
    let mut backup_refs: Vec<(String, ObjectId)> = Vec::new();
    for reference in repo.references()?.all()?.flatten() {
        let name = reference.name().as_bstr().to_string();
        if let Some(id) = reference.try_id().map(|id| id.detach()) {
            backup_refs.push((name, id));
        }
    }
    backup_refs.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, id) in &backup_refs {
        if !name.starts_with(&opts.orig_namespace) {
            continue;
        }
        if !opts.force {
            return die(&format!(
                "Cannot create a new backup.\nA previous backup already exists in {}\n\
                 Force overwriting the backup with -f",
                opts.orig_namespace
            ));
        }
        ctx.git_status(&["update-ref", "-d", name, &id.to_string()])?;
    }

    // Lines 269-284: the refs to update, and the revisions to walk.
    let selection = select(repo, rev_args)?;
    let mut heads: Vec<String> = Vec::new();
    for name in &selection.head_refs {
        match repo
            .rev_parse_single(format!("{name}^0").as_str())
            .ok()
            .and_then(|id| id.object().ok())
            .filter(|o| o.kind == gix::object::Kind::Commit)
        {
            Some(_) => heads.push(name.clone()),
            None => eprintln!("WARNING: not rewriting '{name}' (not a committish)"),
        }
    }
    if heads.is_empty() {
        return die("You must specify a ref to rewrite.");
    }

    fs::create_dir(&ctx.map_dir).or_else(|_| die("Could not create map/ directory"))?;

    // Lines 292-313: `--state-branch` seeds the map. The loader's inversion is
    // the script's, kept deliberately; see the module documentation.
    let mut state_commit: Option<ObjectId> = None;
    if !opts.state_branch.is_empty() {
        state_commit = repo
            .rev_parse_single(opts.state_branch.as_str())
            .ok()
            .and_then(|id| id.object().ok())
            .and_then(|o| o.peel_to_commit().ok())
            .map(|c| c.id);
        match state_commit {
            Some(id) => {
                eprintln!("Populating map from {} ({id})", opts.state_branch);
                let blob = repo
                    .rev_parse_single(format!("{id}:filter.map").as_str())
                    .ok()
                    .and_then(|b| b.object().ok());
                let Some(blob) = blob else {
                    return die(&format!(
                        "Unable to load state from {}:filter.map",
                        opts.state_branch
                    ));
                };
                for line in ByteSlice::lines(&blob.data[..]) {
                    let line = line.to_str_lossy();
                    let Some(colon) = line.rfind(':') else {
                        return die(&format!(
                            "Unable to load state from {}:filter.map",
                            opts.state_branch
                        ));
                    };
                    let (old, new) = (&line[..colon], &line[colon + 1..]);
                    let (name, body) = (new, old);
                    fs::write(ctx.map_file(name.trim()), format!("{body}\n"))?;
                }
            }
            None => eprintln!("Branch {} does not exist. Will create", opts.state_branch),
        }
    }

    // Lines 315-343: the pathspecs, then the walk itself.
    let mut pathspecs = selection.pathspecs.clone();
    let mut remap_to_ancestor = opts.remap_to_ancestor;
    if selection.saw_nonrev {
        remap_to_ancestor = true;
    }
    if !opts.filter_subdir.is_empty() {
        pathspecs.push(opts.filter_subdir.clone());
    }
    let revs = walk(repo, &selection.tips, &selection.hidden, &pathspecs)?;
    let commits = revs.len() as i64;
    if commits == 0 {
        return die_with_status(2, "Found nothing to rewrite");
    }

    // The script's shell, with the filters in the same variables it uses.
    let mut sh = Shell::spawn(&ctx.workdir, &ctx.env())?;
    sh.set("workdir", &ctx.workdir.display().to_string())?;
    sh.set("filter_setup", &opts.filter_setup)?;
    sh.set("filter_env", &opts.filter_env)?;
    sh.set("filter_tree", &opts.filter_tree)?;
    sh.set("filter_index", &opts.filter_index)?;
    sh.set("filter_parent", &opts.filter_parent)?;
    sh.set("filter_msg", &opts.filter_msg)?;
    sh.set("filter_commit", &opts.filter_commit)?;
    sh.set("filter_tag_name", &opts.filter_tag_name)?;

    // Line 385: `eval "$filter_setup" < /dev/null`.
    if sh.run("eval \"$filter_setup\" < /dev/null")? != 0 {
        return die(&format!("filter setup failed: {}", opts.filter_setup));
    }

    let mut progress = Progress::new(commits);
    for (commit, parents) in &revs {
        let commit_hex = commit.to_string();
        progress.report(&commit_hex);

        // Line 392: a commit already in the map (from `--state-branch`) is done.
        if ctx.map_file(&commit_hex).is_file() {
            continue;
        }

        // Lines 394-413: what the script reads into the scratch index — the
        // commit's tree, or the subtree `--subdirectory-filter` names, or the
        // empty tree when the commit has no such directory (the script removes
        // the index there, and `git write-tree` then reports the empty tree).
        let commit_tree = tree_of(repo, *commit)?;
        let base_tree = if opts.filter_subdir.is_empty() {
            commit_tree
        } else {
            match entry_at(repo, Some(commit_tree), &opts.filter_subdir)? {
                Some((mode, id)) if mode.is_tree() => id,
                // `git read-tree` refuses a blob while `git rev-parse -q
                // --verify` prints it, which is the script's `else` arm.
                Some((_, id)) => {
                    println!("{id}");
                    eprintln!("fatal: not a tree object");
                    return die("Could not initialize the index");
                }
                None => ObjectId::empty_tree(repo.object_hash()),
            }
        };

        // Lines 415-423: `$GIT_COMMIT`, `../commit`, `set_ident`, `--env-filter`.
        sh.run(&format!("GIT_COMMIT={commit_hex}\nexport GIT_COMMIT"))?;
        let object = repo
            .find_object(*commit)
            .map_err(|_| anyhow::anyhow!("cannot read commit {commit_hex}"))?;
        let raw = object.data.clone();
        fs::write(ctx.tempdir.join("commit"), &raw)?;
        // `eval "$(set_ident <../commit)"` is one eval, so one round trip.
        if sh.run(&set_ident(&raw).join("\n"))? != 0 {
            return die(&format!(
                "setting author/committer failed for commit {commit_hex}"
            ));
        }
        if sh.run("eval \"$filter_env\" < /dev/null")? != 0 {
            return die(&format!("env filter failed: {}", opts.filter_env));
        }

        // Lines 425-440: the tree filter, in the temporary work tree. The
        // script checks the index out, cleans what the previous commit left and
        // hashes the result back; here the tree goes out and comes back
        // directly. Clearing first rather than after the checkout reaches the
        // same state, since every entry is written out again anyway.
        let mut tree = base_tree;
        if !opts.filter_tree.is_empty() {
            clear_worktree(&ctx.workdir)?;
            if checkout_tree(repo, base_tree, &ctx.workdir).is_err() {
                return die("Could not checkout the index");
            }
            if sh.run("eval \"$filter_tree\" < /dev/null")? != 0 {
                return die(&format!("tree filter failed: {}", opts.filter_tree));
            }
            tree = hash_worktree(repo, &ctx.workdir, base_tree)?;
        }

        // Line 442, the index filter. The script keeps one scratch index live for
        // the whole iteration — `git read-tree -i -m $commit` at the top of the
        // loop, `git update-index` after the tree filter — and this is the point
        // at which the two agree on its contents, so the tree the filters have
        // produced so far is loaded into `$GIT_INDEX_FILE` right here. The filter
        // then sees exactly the entries the script would show it, and its own
        // `git rm --cached` / `git update-index` write back to the same file.
        if !opts.filter_index.is_empty() {
            let code = ctx.git_status(&["read-tree", "-i", "-m", &tree.to_string()])?;
            if code != 0 {
                return die("Could not initialize the index");
            }
            if sh.run("eval \"$filter_index\" < /dev/null")? != 0 {
                return die(&format!("index filter failed: {}", opts.filter_index));
            }
        }

        // Lines 445-460: the mapped parents, then the parent filter.
        let mut parentstr = String::new();
        let mut seen: Vec<String> = Vec::new();
        for parent in parents {
            for reparent in ctx.map_words(&parent.to_string()) {
                if seen.iter().any(|s| *s == reparent) {
                    continue;
                }
                parentstr.push_str(" -p ");
                parentstr.push_str(&reparent);
                seen.push(reparent);
            }
        }
        sh.set("parentstr", &parentstr)?;
        if !opts.filter_parent.is_empty()
            && sh.run("parentstr=\"$(echo \"$parentstr\" | eval \"$filter_parent\")\"")? != 0
        {
            return die(&format!("parent filter failed: {}", opts.filter_parent));
        }

        // Lines 462-472: the message, header lines stripped, through the msg
        // filter. The script strips them with a `while read` loop over
        // `../commit`; the body handed to the filter is the same bytes.
        let body = match raw.windows(2).position(|w| w == b"\n\n") {
            Some(at) => &raw[at + 2..],
            None => &[][..],
        };
        fs::write(ctx.tempdir.join("message-in"), body)?;
        if sh.run("eval \"$filter_msg\" < ../message-in > ../message")? != 0 {
            return die(&format!("msg filter failed: {}", opts.filter_msg));
        }

        // Lines 474-479: `tree=$(git write-tree)` under `$need_index`. Only the
        // index filter needs it here — the other two `need_index` cases have
        // already produced their tree without one — and it is read back at the
        // script's point rather than at the filter, so a msg or parent filter
        // that touches the index is still seen.
        if !opts.filter_index.is_empty() {
            let (code, out) = ctx.git_output(&["write-tree"])?;
            if code != 0 {
                return Err(Exit(code as u8).into());
            }
            tree = out.parse::<ObjectId>().map_err(|_| {
                anyhow::anyhow!("git write-tree did not print a tree id: {out}")
            })?;
        }

        // Lines 480-482: the commit filter, in its own shell, with `$workdir`
        // in its environment so `map()` resolves.
        let map_path = format!("../map/{commit_hex}");
        let code = sh.run(&format!(
            "workdir=\"$workdir\" /bin/sh -c \"$filter_commit\" \"git commit-tree\" \
             {tree} $parentstr < ../message > {}",
            sq(&map_path)
        ))?;
        if code != 0 {
            return die("could not write rewritten commit");
        }
    }
    drop(progress);

    // The ident the last commit left exported. The script's `git update-ref`
    // calls below run with it still in the environment, so it is what lands in
    // the reflog; take it from the shell rather than from the last `set_ident`,
    // because an `--env-filter` may have changed it.
    let idents = [
        "GIT_AUTHOR_NAME",
        "GIT_AUTHOR_EMAIL",
        "GIT_AUTHOR_DATE",
        "GIT_COMMITTER_NAME",
        "GIT_COMMITTER_EMAIL",
        "GIT_COMMITTER_DATE",
    ];
    let ident_file = ctx.tempdir.join("ident");
    let printf = idents
        .iter()
        .map(|name| format!("\"${name}\""))
        .collect::<Vec<_>>()
        .join(" ");
    sh.run(&format!(
        "printf '%s\\0' {printf} > {}",
        sq(&ident_file.display().to_string())
    ))?;
    if let Ok(raw) = fs::read(&ident_file) {
        ctx.ident = idents
            .iter()
            .zip(raw.split(|b| *b == 0))
            .map(|(name, value)| ((*name).to_string(), String::from_utf8_lossy(value).into_owned()))
            .filter(|(_, value)| !value.is_empty())
            .collect();
    }

    // Lines 491-500: heads pruned out of the rewrite map onto their nearest
    // surviving ancestor.
    if remap_to_ancestor {
        for name in &heads {
            let sha1 = rev_parse_commit(repo, name)?;
            if ctx.map_file(&sha1.to_string()).is_file() {
                continue;
            }
            // `git rev-list --simplify-merges -1 "$ref" "$@"`.
            let ancestor = walk(repo, &[sha1], &[], &pathspecs)?
                .last()
                .map(|(id, _)| *id);
            if let Some(ancestor) = ancestor {
                let mapped = ctx.map_words(&ancestor.to_string()).join(" ");
                fs::write(ctx.map_file(&sha1.to_string()), format!("{mapped}\n"))?;
            }
        }
    }

    // Lines 504-540: update the refs, keeping a backup of each.
    println!();
    for name in &heads {
        // The script's own bug: this path is relative to the temporary work
        // tree, so it is only ever true if a tree filter created such a file.
        if ctx
            .workdir
            .join(format!("{}{name}", opts.orig_namespace))
            .is_file()
        {
            continue;
        }
        let sha1 = rev_parse_commit(repo, name)?.to_string();
        let rewritten = ctx.map(&sha1).trim_end_matches('\n').to_string();
        if sha1 == rewritten {
            eprintln!("WARNING: Ref '{name}' is unchanged");
            continue;
        }
        if rewritten.is_empty() {
            println!("Ref '{name}' was deleted");
            if ctx.git_status(&["update-ref", "-m", "filter-branch: delete", "-d", name, &sha1])? != 0
            {
                return die(&format!("Could not delete {name}"));
            }
        } else {
            println!("Ref '{name}' was rewritten");
            let ok = ctx
                .git(&["update-ref", "-m", "filter-branch: rewrite", name, &rewritten, &sha1])
                .stderr(Stdio::null())
                .status()?
                .success();
            if !ok {
                let is_tag = repo
                    .find_reference(name.as_str())
                    .ok()
                    .and_then(|r| r.try_id().map(|id| id.detach()))
                    .and_then(|id| repo.find_object(id).ok())
                    .is_some_and(|o| o.kind == gix::object::Kind::Tag);
                if is_tag {
                    if opts.filter_tag_name.is_empty() {
                        eprintln!("WARNING: You said to rewrite tagged commits, but not the corresponding tag.");
                        eprintln!("WARNING: Perhaps use '--tag-name-filter cat' to rewrite the tag.");
                    }
                } else {
                    return die(&format!("Could not rewrite {name}"));
                }
            }
        }
        let backup = format!("{}{name}", opts.orig_namespace);
        let code = ctx.git_status(&["update-ref", "-m", "filter-branch: backup", &backup, &sha1])?;
        if code != 0 {
            return Err(Exit(code as u8).into());
        }
    }

    // Lines 546-594: the tag pass.
    if !opts.filter_tag_name.is_empty() {
        filter_tags(repo, ctx, &mut sh)?;
    }

    // Lines 596-654: the ident environment is dropped, then the state branch is
    // saved with the map as it now stands.
    ctx.ident.clear();
    if !opts.state_branch.is_empty() {
        save_state(ctx, &opts.state_branch, state_commit)?;
    }

    drop(sh);

    // Lines 661-663: back to the original work tree.
    if !repo.is_bare() {
        let mut cmd = Command::new(&ctx.exe);
        cmd.args(["read-tree", "-u", "-m", "HEAD"])
            .current_dir(&ctx.orig_dir);
        let code = cmd.status()?.code().unwrap_or(1);
        if code != 0 {
            return Err(Exit(code as u8).into());
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `git rev-parse "$ref"^0`, the commit a head names.
fn rev_parse_commit(repo: &gix::Repository, name: &str) -> Result<ObjectId> {
    Ok(repo
        .rev_parse_single(format!("{name}^0").as_str())?
        .object()?
        .id)
}

// ---------------------------------------------------------------------------
// per-commit helpers
// ---------------------------------------------------------------------------

/// `report_progress`, script lines 345-364.
///
/// The `printf` runs on every commit but `count` is only refreshed when the
/// sample condition fires, and `progress` starts as a placeholder that is
/// replaced the first time it does — both are the script's, and both are
/// visible in its output.
struct Progress {
    commits: i64,
    count: i64,
    seen: i64,
    next_sample_at: i64,
    progress: String,
    start: i64,
}

impl Progress {
    fn new(commits: i64) -> Self {
        Self {
            commits,
            count: 0,
            seen: 0,
            next_sample_at: 0,
            progress: "dummy to ensure this is not empty".to_string(),
            start: now(),
        }
    }

    fn report(&mut self, commit: &str) {
        self.seen += 1;
        if self.seen > self.next_sample_at {
            self.count = self.seen;
            let elapsed = now() - self.start;
            let remaining = (self.commits - self.count) * elapsed / self.count;
            if elapsed > 0 {
                self.next_sample_at = (elapsed + 1) * self.count / elapsed;
            } else {
                self.next_sample_at += 1;
            }
            self.progress = format!(" ({elapsed} seconds passed, remaining {remaining} predicted)");
        }
        print!(
            "\rRewrite {commit} ({}/{}){}    ",
            self.count, self.commits, self.progress
        );
        let _ = std::io::stdout().flush();
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        let _ = std::io::stdout().flush();
    }
}

/// `date '+%s'`.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `set_ident` (lines 71-84): the shell assignments `parse_ident_from_commit`
/// and `finish_ident` produce for one raw commit object.
///
/// The `sed` captures everything up to ` <` as the name, so a name keeps the
/// trailing space; the date is the raw `<timestamp> <tz>` with an `@` in front.
/// `finish_ident`'s `case` is emitted verbatim because it reads the shell's
/// current value: a commit with no such header leaves the previous one in place.
fn set_ident(raw: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for (header, var) in [("author", "AUTHOR"), ("committer", "COMMITTER")] {
        if let Some(line) = ident_line(raw, header) {
            if let Some((name, rest)) = line.split_once(" <") {
                if let Some((email, date)) = rest.split_once("> ") {
                    out.push(format!("GIT_{var}_NAME={}", sq(name)));
                    out.push(format!("GIT_{var}_EMAIL={}", sq(email)));
                    out.push(format!("GIT_{var}_DATE={}", sq(&format!("@{date}"))));
                }
            }
        }
        out.push(format!(
            "case \"$GIT_{var}_NAME\" in \"\") GIT_{var}_NAME=\"${{GIT_{var}_EMAIL%%@*}}\" && \
             export GIT_{var}_NAME;; esac"
        ));
        out.push(format!("export GIT_{var}_NAME"));
        out.push(format!("export GIT_{var}_EMAIL"));
        out.push(format!("export GIT_{var}_DATE"));
    }
    out
}

/// The value of a header line in the commit's header block; `sed`'s `/^$/q`
/// stops it from reading anything in the message.
fn ident_line(raw: &[u8], header: &str) -> Option<String> {
    let prefix = format!("{header} ");
    for line in ByteSlice::lines(raw) {
        if line.is_empty() {
            return None;
        }
        let line = line.to_str_lossy();
        if let Some(rest) = line.strip_prefix(&prefix) {
            return Some(rest.to_string());
        }
    }
    None
}

/// `git clean -d -q -f -x` in the temporary work tree, run before the checkout
/// rather than after it: nothing the previous commit or its tree filter left
/// behind survives into this one.
fn clear_worktree(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        // `file_type` does not follow symlinks, so a symlink to a directory is
        // removed as the one entry it is.
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// `git checkout-index -f -u -a`: write a tree into the temporary work tree.
///
/// Submodule entries are skipped, as `checkout-index` skips them; no clean or
/// smudge filter is applied, so the bytes in the work tree are the blob's own.
fn checkout_tree(repo: &gix::Repository, tree: ObjectId, dir: &Path) -> Result<()> {
    use gix::object::tree::EntryKind;
    let tree = repo.find_object(tree)?.peel_to_tree()?;
    for entry in tree.iter() {
        let entry = entry?;
        let name = gix::path::from_bstr(entry.filename());
        let path = dir.join(name.as_ref());
        let id = entry.oid().to_owned();
        match entry.mode().kind() {
            EntryKind::Tree => {
                fs::create_dir_all(&path)?;
                checkout_tree(repo, id, &path)?;
            }
            EntryKind::Commit => {}
            EntryKind::Link => {
                let target = repo.find_object(id)?.data.clone();
                let target = gix::path::from_bstr(target.as_bstr()).into_owned();
                std::os::unix::fs::symlink(target, &path)?;
            }
            kind => {
                let data = repo.find_object(id)?.data.clone();
                fs::write(&path, data)?;
                if kind == EntryKind::BlobExecutable {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
                }
            }
        }
    }
    Ok(())
}

/// `git update-index --add --replace --remove --stdin` followed by
/// `git write-tree`: the tree the work tree now holds.
///
/// `base` supplies the submodule entries, which never appear in the work tree
/// and which the script's `--ignore-submodules` diff leaves in the index.
fn hash_worktree(repo: &gix::Repository, dir: &Path, base: ObjectId) -> Result<ObjectId> {
    /// `(mode, id)` per path, the model both sources feed.
    type Items = Vec<(Vec<u8>, u32, ObjectId)>;

    fn collect(repo: &gix::Repository, dir: &Path, prefix: &[u8], out: &mut Items) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let mut path = prefix.to_vec();
            if !path.is_empty() {
                path.push(b'/');
            }
            path.extend_from_slice(entry.file_name().as_encoded_bytes());
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                collect(repo, &entry.path(), &path, out)?;
            } else if file_type.is_symlink() {
                let target = fs::read_link(entry.path())?;
                let id = repo.write_blob(gix::path::into_bstr(target).as_ref())?;
                out.push((path, 0o120000, id.detach()));
            } else {
                use std::os::unix::fs::PermissionsExt;
                let mode = if entry.metadata()?.permissions().mode() & 0o111 != 0 {
                    0o100755
                } else {
                    0o100644
                };
                let id = repo.write_blob(fs::read(entry.path())?)?;
                out.push((path, mode, id.detach()));
            }
        }
        Ok(())
    }

    fn gitlinks(repo: &gix::Repository, tree: ObjectId, prefix: &[u8], out: &mut Items) -> Result<()> {
        for entry in repo.find_object(tree)?.peel_to_tree()?.iter() {
            let entry = entry?;
            let mut path = prefix.to_vec();
            if !path.is_empty() {
                path.push(b'/');
            }
            path.extend_from_slice(entry.filename());
            match entry.mode().kind() {
                gix::object::tree::EntryKind::Tree => {
                    gitlinks(repo, entry.oid().to_owned(), &path, out)?;
                }
                gix::object::tree::EntryKind::Commit => {
                    out.push((path, 0o160000, entry.oid().to_owned()));
                }
                _ => {}
            }
        }
        Ok(())
    }

    let mut items: Items = Vec::new();
    collect(repo, dir, b"", &mut items)?;
    gitlinks(repo, base, b"", &mut items)?;
    write_tree(repo, &items, b"")
}

/// Write the tree holding every item under `prefix`, recursing into the
/// directories it names. Empty directories are dropped, as git never records
/// a tree with no entries as an entry of its own.
fn write_tree(repo: &gix::Repository, items: &[(Vec<u8>, u32, ObjectId)], prefix: &[u8]) -> Result<ObjectId> {
    let mut here: Vec<(Vec<u8>, u32, ObjectId)> = Vec::new();
    let mut dirs: Vec<Vec<u8>> = Vec::new();
    for (path, mode, id) in items {
        let Some(rest) = path.strip_prefix(prefix) else {
            continue;
        };
        let rest = if prefix.is_empty() {
            rest
        } else {
            match rest.strip_prefix(b"/") {
                Some(rest) => rest,
                None => continue,
            }
        };
        match rest.iter().position(|b| *b == b'/') {
            None => here.push((rest.to_vec(), *mode, *id)),
            Some(at) => {
                let dir = rest[..at].to_vec();
                if !dirs.contains(&dir) {
                    dirs.push(dir);
                }
            }
        }
    }
    for dir in dirs {
        let mut sub_prefix = prefix.to_vec();
        if !sub_prefix.is_empty() {
            sub_prefix.push(b'/');
        }
        sub_prefix.extend_from_slice(&dir);
        let id = write_tree(repo, items, &sub_prefix)?;
        if id != ObjectId::empty_tree(repo.object_hash()) {
            here.push((dir, 0o40000, id));
        }
    }
    // git's tree order: byte-wise by name, with a directory sorting as though
    // its name ended in a slash.
    here.sort_by(|a, b| name_cmp(&a.0, a.1, &b.0, b.1));

    let mut buf = Vec::new();
    for (name, mode, id) in here {
        buf.extend_from_slice(format!("{mode:o} ").as_bytes());
        buf.extend_from_slice(&name);
        buf.push(0);
        buf.extend_from_slice(id.as_slice());
    }
    repo.write_buf(gix::object::Kind::Tree, &buf)
        .map_err(|e| anyhow::anyhow!("could not write tree: {e}"))
}

/// git's `base_name_compare`, as `gix_object::tree::Entry`'s own `Ord` spells it.
fn name_cmp(a: &[u8], a_mode: u32, b: &[u8], b_mode: u32) -> std::cmp::Ordering {
    let common = a.len().min(b.len());
    a[..common].cmp(&b[..common]).then_with(|| {
        let slash = |name: &[u8], mode: u32| -> Option<u8> {
            name.get(common).copied().or((mode == 0o40000).then_some(b'/'))
        };
        slash(a, a_mode).cmp(&slash(b, b_mode))
    })
}

// ---------------------------------------------------------------------------
// tags and state
// ---------------------------------------------------------------------------

/// `--tag-name-filter`, script lines 546-594.
fn filter_tags(repo: &gix::Repository, ctx: &Ctx, sh: &mut Shell) -> Result<()> {
    let mut tags: Vec<(ObjectId, gix::object::Kind, String)> = Vec::new();
    for reference in repo.references()?.prefixed("refs/tags/")?.flatten() {
        let Some(id) = reference.try_id().map(|id| id.detach()) else {
            continue;
        };
        let Ok(object) = repo.find_object(id) else {
            continue;
        };
        tags.push((id, object.kind, reference.name().as_bstr().to_string()));
    }
    tags.sort_by(|a, b| a.2.cmp(&b.2));

    for (id, kind, refname) in tags {
        let name = refname.trim_start_matches("refs/tags/").to_string();
        // `if [ "$type" != "commit" -a "$type" != "tag" ]`.
        if kind != gix::object::Kind::Commit && kind != gix::object::Kind::Tag {
            continue;
        }
        let sha1t = id;
        let sha1 = if kind == gix::object::Kind::Tag {
            match repo
                .find_object(id)
                .ok()
                .and_then(|o| o.peel_to_commit().ok())
            {
                Some(commit) => commit.id,
                None => continue,
            }
        } else {
            id
        };
        let sha1_hex = sha1.to_string();
        if !ctx.map_file(&sha1_hex).is_file() {
            continue;
        }
        let new_sha1 = ctx.map(&sha1_hex).trim_end_matches('\n').to_string();

        sh.run(&format!("GIT_COMMIT={sha1_hex}\nexport GIT_COMMIT"))?;
        sh.set("tagref", &name)?;
        let out_path = ctx.tempdir.join("tag-name");
        if sh.run(&format!(
            "echo \"$tagref\" | eval \"$filter_tag_name\" > {}",
            sq(&out_path.display().to_string())
        ))? != 0
        {
            return die("tag name filter failed");
        }
        let new_ref = fs::read_to_string(&out_path)?
            .trim_end_matches('\n')
            .to_string();

        println!("{name} -> {new_ref} ({sha1_hex} -> {new_sha1})");

        let target = if kind == gix::object::Kind::Tag {
            let raw = repo.find_object(sha1t)?.data.clone();
            let mut body = format!("object {new_sha1}\ntype commit\ntag {new_ref}\n").into_bytes();
            body.extend_from_slice(&strip_tag_headers(&raw));
            let id = repo
                .write_buf(gix::object::Kind::Tag, &body)
                .map_err(|e| anyhow::anyhow!("Could not create new tag object for {name}: {e}"))?;
            if ByteSlice::lines(&raw[..])
                .any(|l| l.starts_with(b"-----BEGIN PGP SIGNATURE-----"))
            {
                eprintln!("gpg signature stripped from tag object {sha1t}");
            }
            id.to_string()
        } else {
            new_sha1
        };

        if ctx.git_status(&["update-ref", &format!("refs/tags/{new_ref}"), &target])? != 0 {
            return die(&format!("Could not write tag {new_ref}"));
        }
    }
    Ok(())
}

/// The `sed` of lines 574-581: drop `object`/`type`/`tag` from the header block,
/// and stop at a PGP signature without printing it.
fn strip_tag_headers(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut in_header = true;
    for line in raw.split(|b| *b == b'\n') {
        if line.starts_with(b"-----BEGIN PGP SIGNATURE-----") {
            break;
        }
        if in_header {
            if line.is_empty() {
                in_header = false;
            } else if line.starts_with(b"object ")
                || line.starts_with(b"type ")
                || line.starts_with(b"tag ")
            {
                continue;
            }
        }
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    // `split` yields a trailing empty element for a trailing newline, which the
    // loop turns into one newline too many.
    if raw.last() == Some(&b'\n') {
        out.pop();
    }
    out
}

/// `--state-branch`'s save, script lines 635-654: the whole map as one
/// `filter.map` blob under a commit on that branch.
fn save_state(ctx: &Ctx, state_branch: &str, state_commit: Option<ObjectId>) -> Result<()> {
    eprintln!("Saving rewrite state to {state_branch}");
    let mut names: Vec<String> = Vec::new();
    for entry in fs::read_dir(&ctx.map_dir)? {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    // `for file in ../map/*` — the shell's glob is sorted.
    names.sort();
    let mut blob = String::new();
    for from_commit in names {
        let to_commit = fs::read_to_string(ctx.map_file(&from_commit))?;
        blob.push_str(&format!("{from_commit}:{}\n", to_commit.trim_end()));
    }

    let (code, out) = {
        let mut child = ctx
            .git(&["hash-object", "-w", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .expect("stdin was piped")
            .write_all(blob.as_bytes())?;
        let out = child.wait_with_output()?;
        (out.status.code().unwrap_or(1), out.stdout)
    };
    if code != 0 {
        return die("Unable to save state");
    }
    let state_blob = String::from_utf8_lossy(&out).trim().to_string();

    let (code, out) = {
        let mut child = ctx
            .git(&["mktree"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .expect("stdin was piped")
            .write_all(format!("100644 blob {state_blob}\tfilter.map\n").as_bytes())?;
        let out = child.wait_with_output()?;
        (out.status.code().unwrap_or(1), out.stdout)
    };
    if code != 0 {
        return die("Unable to save state");
    }
    let state_tree = String::from_utf8_lossy(&out).trim().to_string();

    // The ident environment the rewrite exported is gone by now (lines 596-598),
    // so this commit takes the caller's own identity and the current time.
    let mut args: Vec<String> = vec!["commit-tree".into(), state_tree];
    if let Some(parent) = state_commit {
        args.push("-p".into());
        args.push(parent.to_string());
    }
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut cmd = Command::new(&ctx.exe);
    cmd.args(&argv)
        .current_dir(&ctx.orig_dir)
        .env("GIT_DIR", &ctx.git_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child = cmd.spawn()?;
    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(b"Sync\n")?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return die("Unable to save state");
    }
    let new_state = String::from_utf8_lossy(&out.stdout).trim().to_string();
    ctx.git_status(&["update-ref", state_branch, &new_state])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// revision selection and the walk
// ---------------------------------------------------------------------------

/// What `git rev-parse` pulls out of `<rev-list options>` for the script: the
/// refs to update, the tips to walk, and the pathspecs.
#[derive(Default)]
struct Selection {
    tips: Vec<ObjectId>,
    hidden: Vec<ObjectId>,
    head_refs: Vec<String>,
    pathspecs: Vec<String>,
    /// `test -z "$nonrevs"` (line 317): any non-revision argument, `--` included.
    saw_nonrev: bool,
}

/// The script's three `git rev-parse` calls over `"$@"` (lines 269, 316, 325),
/// as one pass. Arguments outside the ported set are refused rather than
/// silently dropped, since dropping one rewrites a different set of commits.
fn select(repo: &gix::Repository, args: &[String]) -> Result<Selection> {
    let mut sel = Selection::default();
    let mut in_paths = false;
    let mut saw_rev = false;

    for arg in args {
        if in_paths {
            sel.pathspecs.push(arg.clone());
            continue;
        }
        match arg.as_str() {
            "--" => {
                in_paths = true;
                sel.saw_nonrev = true;
                continue;
            }
            "--all" | "--branches" | "--tags" | "--remotes" => {
                let prefix = match arg.as_str() {
                    "--branches" => "refs/heads/",
                    "--tags" => "refs/tags/",
                    "--remotes" => "refs/remotes/",
                    _ => "refs/",
                };
                let mut refs: Vec<(String, ObjectId)> = Vec::new();
                for reference in repo.references()?.prefixed(prefix)?.flatten() {
                    let name = reference.name().as_bstr().to_string();
                    if let Ok(id) = rev_parse_commit(repo, &name) {
                        refs.push((name, id));
                    }
                }
                refs.sort_by(|a, b| a.0.cmp(&b.0));
                for (name, id) in refs {
                    sel.head_refs.push(name);
                    sel.tips.push(id);
                }
                saw_rev = true;
                continue;
            }
            _ => {}
        }
        if let Some(rev) = arg.strip_prefix('^') {
            let id = resolve(repo, rev)?;
            sel.hidden.push(id);
            saw_rev = true;
            continue;
        }
        if let Some((base, tip)) = arg.split_once("..") {
            if base.contains('.') || tip.starts_with('.') {
                bail!("unsupported rev-list argument: {arg}");
            }
            let base = if base.is_empty() { "HEAD" } else { base };
            let tip = if tip.is_empty() { "HEAD" } else { tip };
            sel.hidden.push(resolve(repo, base)?);
            sel.tips.push(resolve(repo, tip)?);
            if let Some(name) = symbolic_full_name(repo, tip) {
                sel.head_refs.push(name);
            }
            saw_rev = true;
            continue;
        }
        if arg.starts_with('-') {
            bail!("unsupported rev-list argument: {arg}");
        }
        match repo.rev_parse_single(arg.as_str()) {
            Ok(_) => {
                sel.tips.push(resolve(repo, arg)?);
                if let Some(name) = symbolic_full_name(repo, arg) {
                    sel.head_refs.push(name);
                }
                saw_rev = true;
            }
            Err(_) if Path::new(arg).exists() => {
                sel.pathspecs.push(arg.clone());
                sel.saw_nonrev = true;
            }
            Err(_) => {
                return die(&format!(
                    "fatal: ambiguous argument '{arg}': unknown revision or path not in the \
                     working tree."
                ));
            }
        }
    }
    if !sel.pathspecs.is_empty() {
        sel.saw_nonrev = true;
    }
    for spec in &sel.pathspecs {
        if spec.starts_with(':') || spec.contains(['*', '?', '[']) {
            bail!("unsupported pathspec: {spec}");
        }
    }

    // `--default HEAD` on both the ref list and the walk.
    if !saw_rev {
        sel.tips.push(rev_parse_commit(repo, "HEAD")?);
        if let Some(name) = symbolic_full_name(repo, "HEAD") {
            sel.head_refs.push(name);
        }
    }
    Ok(sel)
}

/// The commit a revision names, peeling tags the way `rev-list` does.
fn resolve(repo: &gix::Repository, rev: &str) -> Result<ObjectId> {
    Ok(repo
        .rev_parse_single(rev)?
        .object()?
        .peel_to_commit()?
        .id)
}

/// `git rev-parse --symbolic-full-name <rev>`: the ref a revision names, or
/// nothing at all when it names a raw object.
///
/// Symbolic refs are followed, which is why the default `HEAD` names
/// `refs/heads/<branch>` and it is the branch that gets rewritten and backed up.
fn symbolic_full_name(repo: &gix::Repository, rev: &str) -> Option<String> {
    let mut reference = repo.try_find_reference(rev).ok().flatten()?;
    while let Some(Ok(next)) = reference.follow() {
        reference = next;
    }
    Some(reference.name().as_bstr().to_string())
}

/// One commit in the pathspec-limited walk.
struct Node {
    parents: Vec<ObjectId>,
    /// TREESAME against each parent, or against the empty tree for a root.
    treesame: Vec<bool>,
    /// `--full-history` with parent rewriting: ordinary commits are included
    /// only if they are !TREESAME; merges always are.
    included: bool,
    /// The parents that survive that rewriting, each with the TREESAME bit of
    /// the slot it came from.
    rewritten: Vec<(ObjectId, bool)>,
    /// `--simplify-merges`: the commit this one collapses to, itself if it stays.
    simplified: ObjectId,
    /// Its parents once it does stay.
    final_parents: Vec<ObjectId>,
}

/// `git rev-list --reverse --topo-order --parents --simplify-merges`, oldest
/// first, as `(commit, parents)`.
///
/// Without pathspecs no commit is TREESAME to anything, so simplification is a
/// no-op and the parents are the real ones — that is the shape every filter but
/// `--subdirectory-filter` sees. With pathspecs the two documented phases run:
/// `--full-history` with parent rewriting, then the `--simplify-merges` rules.
fn walk(
    repo: &gix::Repository,
    tips: &[ObjectId],
    hidden: &[ObjectId],
    pathspecs: &[String],
) -> Result<Vec<(ObjectId, Vec<ObjectId>)>> {
    // Two refs can name the same commit (a tag on a branch tip), and the
    // traversal would then hand it out twice; git seeds each commit once. A tip
    // that is itself excluded is not a tip at all — `git rev-list HEAD..HEAD`
    // is empty — and the traversal would otherwise still hand it out.
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let seed: Vec<ObjectId> = tips
        .iter()
        .rev()
        .copied()
        .filter(|id| !hidden.contains(id) && seen.insert(*id))
        .collect();
    let topo = gix::traverse::commit::topo::Builder::from_iters(
        &repo.objects,
        seed,
        Some(hidden.iter().copied()),
    )
    .sorting(gix::traverse::commit::topo::Sorting::TopoOrder)
    .parents(gix::traverse::commit::Parents::All)
    .build()?;

    let mut order: Vec<ObjectId> = Vec::new();
    let mut parents: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    for info in topo {
        let info = info?;
        order.push(info.id);
        parents.insert(info.id, info.parent_ids.to_vec());
    }

    if pathspecs.is_empty() {
        let mut out: Vec<(ObjectId, Vec<ObjectId>)> = order
            .into_iter()
            .map(|id| {
                let p = parents.get(&id).cloned().unwrap_or_default();
                (id, p)
            })
            .collect();
        out.reverse();
        return Ok(out);
    }

    let mut nodes: HashMap<ObjectId, Node> = HashMap::new();
    for id in &order {
        let real = parents.get(id).cloned().unwrap_or_default();
        let tree = tree_of(repo, *id)?;
        let treesame: Vec<bool> = if real.is_empty() {
            vec![treesame(repo, None, tree, pathspecs)?]
        } else {
            real.iter()
                .map(|p| {
                    let parent_tree = tree_of(repo, *p)?;
                    treesame(repo, Some(parent_tree), tree, pathspecs)
                })
                .collect::<Result<_>>()?
        };
        // `--dense`, the default: included unless TREESAME to a parent; a merge
        // is always included.
        let included = real.len() > 1 || !treesame.iter().all(|t| *t);
        nodes.insert(
            *id,
            Node {
                parents: real,
                treesame,
                included,
                rewritten: Vec::new(),
                simplified: *id,
                final_parents: Vec::new(),
            },
        );
    }

    // Phase one: prune the commits that are not included from every parent list.
    for id in &order {
        let (real, treesame_bits) = {
            let node = &nodes[id];
            (node.parents.clone(), node.treesame.clone())
        };
        let mut rewritten: Vec<(ObjectId, bool)> = Vec::new();
        for (slot, parent) in real.iter().enumerate() {
            let bit = treesame_bits.get(slot).copied().unwrap_or(false);
            if let Some(p) = nearest_included(*parent, &nodes) {
                match rewritten.iter_mut().find(|(q, _)| *q == p) {
                    Some(entry) => entry.1 |= bit,
                    None => rewritten.push((p, bit)),
                }
            }
        }
        nodes.get_mut(id).expect("just inserted").rewritten = rewritten;
    }

    // Phase two: the `--simplify-merges` rules, oldest first so every parent is
    // already simplified when its child is reached.
    let mut ancestry = Ancestry::default();
    for id in order.iter().rev() {
        let rewritten = nodes[id].rewritten.clone();
        let mut ps: Vec<(ObjectId, bool)> = Vec::new();
        for (parent, bit) in rewritten {
            let p = nodes.get(&parent).map_or(parent, |n| n.simplified);
            // Drop a parent that is a root TREESAME to the empty tree.
            if nodes
                .get(&p)
                .is_some_and(|n| n.parents.is_empty() && n.treesame.first().copied() == Some(true))
            {
                continue;
            }
            match ps.iter_mut().find(|(q, _)| *q == p) {
                Some(entry) => entry.1 |= bit,
                None => ps.push((p, bit)),
            }
        }
        // Drop parents that are ancestors of other parents, but never drop
        // every parent this commit is TREESAME to.
        let mut keep = vec![true; ps.len()];
        for i in 0..ps.len() {
            for j in 0..ps.len() {
                if i != j && keep[i] && keep[j] && ancestry.is_ancestor(repo, ps[i].0, ps[j].0)? {
                    keep[i] = false;
                    break;
                }
            }
        }
        let treesame_survives = ps
            .iter()
            .enumerate()
            .any(|(i, (_, bit))| *bit && keep[i]);
        if !treesame_survives {
            if let Some(i) = ps.iter().position(|(_, bit)| *bit) {
                keep[i] = true;
            }
        }
        let ps: Vec<(ObjectId, bool)> = ps
            .into_iter()
            .zip(keep)
            .filter_map(|(p, k)| k.then_some(p))
            .collect();

        let node = nodes.get_mut(id).expect("just inserted");
        // A root, a merge or a !TREESAME commit stays; anything else becomes its
        // only parent.
        if ps.len() == 1 && ps[0].1 {
            node.simplified = ps[0].0;
        } else {
            node.simplified = *id;
            node.final_parents = ps.into_iter().map(|(p, _)| p).collect();
        }
    }

    let mut out: Vec<(ObjectId, Vec<ObjectId>)> = order
        .iter()
        .filter(|id| nodes[*id].included && nodes[*id].simplified == **id)
        .map(|id| (*id, nodes[id].final_parents.clone()))
        .collect();
    out.reverse();
    Ok(out)
}

/// The nearest ancestor of `id` (itself included) that survives phase one, or
/// nothing when the chain runs into a pruned root.
fn nearest_included(mut id: ObjectId, nodes: &HashMap<ObjectId, Node>) -> Option<ObjectId> {
    loop {
        match nodes.get(&id) {
            // Outside the walk — a boundary parent below a `^rev` — is kept as
            // it is, which is what leaves the untouched history below a range
            // attached to the rewritten commits above it.
            None => return Some(id),
            Some(node) if node.included => return Some(id),
            Some(node) => id = *node.parents.first()?,
        }
    }
}

/// Memoised reachability over real parents, for the ancestor test the
/// `--simplify-merges` rules make on a merge's parent list.
#[derive(Default)]
struct Ancestry {
    parents: HashMap<ObjectId, Vec<ObjectId>>,
}

impl Ancestry {
    fn parents_of(&mut self, repo: &gix::Repository, id: ObjectId) -> Result<Vec<ObjectId>> {
        if let Some(p) = self.parents.get(&id) {
            return Ok(p.clone());
        }
        let parents: Vec<ObjectId> = repo
            .find_object(id)?
            .try_into_commit()?
            .parent_ids()
            .map(|p| p.detach())
            .collect();
        self.parents.insert(id, parents.clone());
        Ok(parents)
    }

    fn is_ancestor(&mut self, repo: &gix::Repository, a: ObjectId, b: ObjectId) -> Result<bool> {
        if a == b {
            return Ok(false);
        }
        let mut seen: HashSet<ObjectId> = HashSet::new();
        let mut queue = vec![b];
        while let Some(id) = queue.pop() {
            if !seen.insert(id) {
                continue;
            }
            for parent in self.parents_of(repo, id)? {
                if parent == a {
                    return Ok(true);
                }
                queue.push(parent);
            }
        }
        Ok(false)
    }
}

/// A commit's tree.
fn tree_of(repo: &gix::Repository, id: ObjectId) -> Result<ObjectId> {
    Ok(repo.find_object(id)?.peel_to_tree()?.id)
}

/// TREESAME: does every pathspec name the same entry in both trees? For the
/// plain pathspecs this port accepts — a file or a directory, no magic and no
/// wildcards — that is exactly a pathspec-restricted empty diff.
fn treesame(
    repo: &gix::Repository,
    old: Option<ObjectId>,
    new: ObjectId,
    pathspecs: &[String],
) -> Result<bool> {
    for spec in pathspecs {
        let spec = spec.trim_end_matches('/');
        if spec.is_empty() {
            if old != Some(new) {
                return Ok(false);
            }
            continue;
        }
        if entry_at(repo, old, spec)? != entry_at(repo, Some(new), spec)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The `(mode, id)` a path names in a tree, or nothing when it is absent.
fn entry_at(
    repo: &gix::Repository,
    tree: Option<ObjectId>,
    path: &str,
) -> Result<Option<(gix::object::tree::EntryMode, ObjectId)>> {
    let Some(tree) = tree else { return Ok(None) };
    let tree = repo.find_object(tree)?.peel_to_tree()?;
    Ok(tree
        .lookup_entry_by_path(Path::new(path))?
        .map(|e| (e.mode(), e.id().detach())))
}
