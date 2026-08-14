//! The differential runner: one case, run twice, compared four ways.
//!
//! A case is judged on stdout bytes, exit code, and the *resulting repository
//! state* — that last one is what makes this more than an output diff. A
//! command can print the right thing and still corrupt the index; probing the
//! post-state with stock git in both repos catches that.
//!
//! stderr is deliberately not byte-compared. Error prose is not a compatibility
//! surface and zvcs is specified to be terser than git. It is still recorded so
//! a human can read it, and whether the command *errored at all* is compared
//! via the exit code.

use crate::env;
use crate::fixture::{Shape, Templates};
use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// One invocation to compare.
#[derive(Clone, Debug)]
pub struct Case {
    /// Subcommand, e.g. `rev-parse`. Used for per-command scoring.
    pub cmd: &'static str,
    /// Full argv after the binary name, including the subcommand.
    pub args: Vec<String>,
    /// Repository shape the case runs against.
    pub shape: Shape,
    /// Bytes fed to the child on stdin, byte-identically to both sides.
    ///
    /// `None` means stdin is closed (`/dev/null`), which is what every case did
    /// before this field existed. A whole class of git is *only* reachable
    /// through stdin — `mktree`, `mktag`, `stripspace`, `patch-id`, `mailinfo`,
    /// `column`, `unpack-objects`, and the `--stdin` mode of a dozen more take
    /// their entire payload there. With stdin nailed shut those commands could
    /// only ever be measured on the empty-input path, so a score of 100% for
    /// them meant "agrees on nothing", not "agrees".
    ///
    /// Deliberately `&'static [u8]`: the payload is a literal compiled into the
    /// corpus, never a file read at run time. A case that reads the filesystem
    /// for its input is not reproducible, and an unreproducible case cannot be
    /// the premise of a differential comparison.
    pub stdin: Option<&'static [u8]>,
    /// Compare stderr byte for byte as well.
    ///
    /// Off by default, and deliberately so: the harness's standing policy is that
    /// error *prose* is not a compatibility surface (see the module header). But
    /// for the commands whose whole contract is a refusal — a merge that will not
    /// overwrite, a pull that will not run, a stash that has nothing to pop — the
    /// message *is* the behaviour, and every one of those shipped wrong at least
    /// once while stdout, exit code and state all agreed. Cases that opt in here
    /// are measured on it; the rest of the corpus is unaffected, so no existing
    /// score moves.
    pub compare_stderr: bool,
    /// Directory the command runs in, **relative to the fixture root**.
    ///
    /// `None` means the fixture root, which is what every case did before this
    /// field existed — and that was the blind spot. Git decides *which
    /// repository it is in* before it does anything else
    /// (`setup.c:setup_git_directory_gently_1`), and that decision is a function
    /// of the working directory: whether it is inside `.git`, inside a linked
    /// worktree, inside a bare repository, or inside a submodule. With every
    /// case pinned to the worktree root, the whole of discovery was
    /// structurally unmeasurable, and it shipped broken more than once —
    /// commands run from inside `.git` failed outright, and a command run in a
    /// bare repository's subdirectory aborted the process.
    ///
    /// Created on both sides if the fixture does not already contain it, by the
    /// same code, so "the directory exists" is never itself a difference
    /// between the two runs.
    ///
    /// Deliberately relative and `&'static str`: an absolute path would name one
    /// side's copy, and the two copies live at different roots.
    pub cwd: Option<&'static str>,
    /// Environment applied **on top of** [`crate::env::harden`], identically to
    /// both sides.
    ///
    /// Additive only. [`crate::env::is_pinned`] rejects any key `harden`
    /// already sets, because a case that re-points `HOME` or `GIT_COMMITTER_DATE`
    /// puts the machine back into a comparison whose premise is that nothing but
    /// the binary differs. What it *is* for is the variables `harden` leaves
    /// unset precisely because it clears the environment — `GIT_DIR`,
    /// `GIT_WORK_TREE`, `GIT_CEILING_DIRECTORIES` — each of which redirects
    /// discovery and none of which any case could reach before.
    ///
    /// Values may not contain a literal absolute path, for the same reason `cwd`
    /// may not: the two sides run in different directories. Write
    /// [`REPO_PLACEHOLDER`] instead and it is replaced with that side's own
    /// fixture root.
    pub env: &'static [(&'static str, &'static str)],
}

/// Stands in for the running side's fixture root inside a case's [`Case::env`]
/// values, so one literal can name both copies.
pub const REPO_PLACEHOLDER: &str = "{repo}";

impl Case {
    pub fn new(cmd: &'static str, args: &[&str], shape: Shape) -> Self {
        Self {
            cmd,
            args: args.iter().map(|s| s.to_string()).collect(),
            shape,
            stdin: None,
            compare_stderr: false,
            cwd: None,
            env: &[],
        }
    }

    /// Same as [`Case::new`], with stderr compared byte for byte too.
    pub fn strict(cmd: &'static str, args: &[&str], shape: Shape) -> Self {
        Self { compare_stderr: true, ..Self::new(cmd, args, shape) }
    }

    /// Same as [`Case::new`], with `stdin` delivered to both sides.
    pub fn with_stdin(
        cmd: &'static str,
        args: &[&str],
        shape: Shape,
        stdin: &'static [u8],
    ) -> Self {
        Self { stdin: Some(stdin), ..Self::new(cmd, args, shape) }
    }

    /// Run this case from `cwd`, a path relative to the fixture root.
    ///
    /// A builder rather than another constructor: cwd and extra environment
    /// combine with each other and with every existing constructor, and four
    /// more `Case::new`-shaped functions to spell the combinations would be
    /// worse than two methods that compose.
    pub fn in_dir(self, cwd: &'static str) -> Self {
        Self { cwd: Some(cwd), ..self }
    }

    /// Run this case with `env` added on top of [`crate::env::harden`].
    pub fn with_env(self, env: &'static [(&'static str, &'static str)]) -> Self {
        Self { env, ..self }
    }

    /// Stable identity for reporting and for reproducing a single failure.
    ///
    /// The stdin payload is part of the identity: two cases can share a shape
    /// and an argv and still be different invocations, and a report that
    /// collapsed them would name the wrong one.
    ///
    /// Working directory and extra environment are part of it for exactly the
    /// same reason, and they are the *whole* difference between the discovery
    /// cases: `rev-parse --git-dir` is one argv against one shape and means
    /// something different in each of a dozen directories. They are appended as
    /// their own segments, so a case that sets neither keeps the identity it
    /// already had — the report and `scripts/split_failures.pl` key on these
    /// strings, and the environment is rendered unsubstituted so the id is the
    /// same on every machine.
    pub fn id(&self) -> String {
        let strict = if self.compare_stderr { "!" } else { "" };
        let mut id =
            format!("{}{}::{}::{}", strict, self.shape.name(), self.cmd, self.args.join(" "));
        if let Some(cwd) = self.cwd {
            id.push_str(&format!("::cwd[{cwd}]"));
        }
        if !self.env.is_empty() {
            let rendered: Vec<String> =
                self.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
            id.push_str(&format!("::env[{}]", rendered.join(" ")));
        }
        if let Some(bytes) = self.stdin {
            id.push_str(&format!("::stdin[{}B/{:016x}]", bytes.len(), fnv1a64(bytes)));
        }
        id
    }
}

/// FNV-1a, used only to give a stdin payload a short stable name in case ids.
/// Not security-relevant; chosen because it is four lines and has no dependency.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Why a case did not match. Ordered roughly by how damning it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// stdout, exit code, and post-state all agree.
    Match,
    /// zvcs refused the subcommand or a flag it has not ported yet.
    ///
    /// Counted as a **failure** for parity scoring. An unported command is
    /// exactly the gap being measured; scoring it as a skip would inflate the
    /// number, which is the one thing this harness must never do.
    Unsupported,
    /// Same exit code and state, different bytes on stdout.
    StdoutDiff,
    /// Different exit codes.
    ExitDiff,
    /// Same output, but the repository was left in a different state.
    StateDiff,
    /// stdout, exit code and state all agree, but the message on stderr does not.
    /// Only reachable for a case that opted into stderr comparison.
    StderrDiff,
    /// zvcs crashed (signal, or a panic surfacing as a Rust backtrace).
    Crash,
    /// zvcs did not exit within the case timeout while stock git did. Tracked
    /// apart from Crash: a hang is usually a wait on input git does not want.
    Hang,
    /// Stock git does not agree with *itself* on this invocation, so byte
    /// comparison cannot measure anything. Established by re-running the stock
    /// side in a second pristine repo and diffing the two stock outputs — never
    /// asserted from a pattern.
    ///
    /// Only reachable when stock disagrees with stock, so it can never mask a
    /// real zvcs difference. Reported in its own bucket and excluded from the
    /// parity denominator: counting an unmeasurable case as a failure is as
    /// wrong as counting it as a pass.
    Nondeterministic,
    /// Stock git did not finish inside [`CASE_TIMEOUT`], so there is no oracle to
    /// compare against.
    ///
    /// Kept apart from [`Verdict::Nondeterministic`] because the cause is
    /// different and the reader should be able to tell them apart: stock did not
    /// disagree with itself, it never answered. Excluded from the denominator for
    /// the same reason — a case the harness could not measure is not a case the
    /// port failed. It cannot mask a zvcs defect, because a zvcs side that hangs
    /// or crashes is judged before this is reached.
    ///
    /// Seeing many of these means the machine is too loaded for the ceiling, not
    /// that anything regressed.
    StockTimeout,
}

impl Verdict {
    pub fn is_match(self) -> bool {
        self == Verdict::Match
    }

    pub fn label(self) -> &'static str {
        match self {
            Verdict::Match => "MATCH",
            Verdict::Unsupported => "UNSUPPORTED",
            Verdict::StdoutDiff => "STDOUT-DIFF",
            Verdict::ExitDiff => "EXIT-DIFF",
            Verdict::StateDiff => "STATE-DIFF",
            Verdict::StderrDiff => "STDERR-DIFF",
            Verdict::Crash => "CRASH",
            Verdict::Hang => "HANG",
            Verdict::Nondeterministic => "NONDETERMINISTIC",
            Verdict::StockTimeout => "STOCK-TIMEOUT",
        }
    }
}

/// Raw result of running one side.
struct Side {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    code: Option<i32>,
    timed_out: bool,
}

/// Full record of a compared case, retained so failures can be printed with
/// enough detail to act on without re-running.
pub struct Outcome {
    pub case: Case,
    pub verdict: Verdict,
    pub stock_stdout: String,
    pub zvcs_stdout: String,
    pub stock_stderr: String,
    pub zvcs_stderr: String,
    pub stock_code: Option<i32>,
    pub zvcs_code: Option<i32>,
    pub stock_state: String,
    pub zvcs_state: String,
}

/// Ceiling on a single invocation. Fuzzing reaches commands that wait on input
/// or spin; without a bound, one such case stalls the whole run rather than
/// being reported as the defect it is.
const CASE_TIMEOUT: Duration = Duration::from_secs(20);

/// The directory a case runs in, created if the fixture does not contain it.
///
/// `create_dir_all` is a no-op on a directory that already exists, so the same
/// call covers both "the fixture has this path" (`.git/refs/heads`) and "the
/// case needs a directory that is not tracked and so cannot be in the fixture"
/// (an empty non-repository directory to run from). Both sides go through here,
/// so the directory's existence is never itself an asymmetry.
fn case_dir(repo: &Path, cwd: Option<&str>) -> Result<PathBuf> {
    let Some(rel) = cwd else { return Ok(repo.to_path_buf()) };
    let dir = repo.join(rel);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating case working directory {}", dir.display()))?;
    Ok(dir)
}

/// Apply a case's extra environment on top of the hardened one.
///
/// Two invariants, both asserted rather than documented-and-hoped: the key is
/// not one of `harden`'s pins (see [`env::is_pinned`]), and the value names this
/// side's fixture root through [`REPO_PLACEHOLDER`] rather than as a literal
/// path. A corpus-wide test checks both statically; the asserts catch a case
/// added without running it.
fn apply_case_env(cmd: &mut Command, repo: &Path, extra: &[(&'static str, &'static str)]) {
    for (key, value) in extra {
        assert!(
            !env::is_pinned(key),
            "case environment may not override the hardened pin {key}"
        );
        assert!(
            !value.starts_with('/'),
            "case environment {key} must use {REPO_PLACEHOLDER}, not the absolute path {value}"
        );
        cmd.env(key, value.replace(REPO_PLACEHOLDER, &repo.to_string_lossy()));
    }
}

fn run_side(
    bin: &Path,
    repo: &Path,
    home: &Path,
    args: &[String],
    stdin: Option<&'static [u8]>,
    cwd: Option<&str>,
    extra_env: &[(&'static str, &'static str)],
) -> Result<Side> {
    let dir = case_dir(repo, cwd)?;
    let mut cmd = Command::new(bin);
    env::harden(&mut cmd, home);
    apply_case_env(&mut cmd, repo, extra_env);
    cmd.current_dir(&dir)
        .args(args)
        // Closed stdin stays the default. A command that reads input it was not
        // given must still hit EOF rather than block, or the `Hang` verdict
        // stops meaning anything.
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn {} {:?}", bin.display(), args))?;

    // Written from a helper thread, not inline: a command that both consumes a
    // payload and prints while consuming it (`stripspace`, `column`, `apply
    // --stat`) would otherwise deadlock — the child blocking on a full stdout
    // pipe while this thread blocks writing stdin. The handle is moved into the
    // thread so dropping it there closes the pipe and delivers EOF.
    let writer = stdin.map(|bytes| {
        let mut h = child.stdin.take().expect("stdin piped when a payload is set");
        std::thread::spawn(move || {
            let _ = h.write_all(bytes);
            let _ = h.flush();
        })
    });

    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if start.elapsed() >= CASE_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    // Safe to join only now: the child has exited (or been killed), so a writer
    // still holding unwritten bytes gets EPIPE and returns instead of blocking.
    if let Some(w) = writer {
        let _ = w.join();
    }

    // Pipes are drained after exit; every fixture case produces bounded output,
    // so this cannot deadlock on a full pipe buffer in practice.
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut h) = child.stdout.take() {
        let _ = h.read_to_end(&mut stdout);
    }
    if let Some(mut h) = child.stderr.take() {
        let _ = h.read_to_end(&mut stderr);
    }

    match status {
        Some(s) => Ok(Side { stdout, stderr, code: s.code(), timed_out: false }),
        None => Ok(Side { stdout, stderr, code: None, timed_out: true }),
    }
}

/// Probe repository state with **stock** git, so the probe itself is never the
/// thing under test. Any single probe failing is folded into the digest as an
/// `<err>` marker rather than aborting: a command under test is allowed to
/// leave a repo in a state some probes reject, and that difference is signal.
fn probe_state(repo: &Path, home: &Path) -> String {
    const PROBES: &[&[&str]] = &[
        &["status", "--porcelain=v1", "--untracked-files=all"],
        &["for-each-ref", "--format=%(refname) %(objecttype) %(objectname)"],
        &["rev-parse", "--abbrev-ref", "HEAD"],
        &["rev-parse", "HEAD"],
        &["ls-files", "--stage"],
        &["stash", "list"],
        &["cat-file", "--batch-check", "--batch-all-objects"],
        // Repository-local config. A command that reports success while failing
        // to persist the setting it promised — `clone --set-upstream` writing no
        // `branch.<name>.remote`, `remote add` writing no fetch refspec — is
        // otherwise only caught if it also happens to print something.
        //
        // Safe to compare byte-for-byte because `env::harden` pins every
        // machine-derived input git consults and both sides run on the same
        // filesystem, so the values git auto-detects at `init` time
        // (`core.filemode`, `core.ignorecase`, `core.precomposeunicode`) are
        // equal by construction. `--local` is explicit so a stray global or
        // system file could not contribute even if the /dev/null pins were lost.
        //
        // Order is compared as well as content: `--list` prints in file order,
        // and writing the right keys into the wrong section or sequence is a
        // real difference in `.git/config` bytes.
        &["config", "--list", "--local"],
    ];

    let mut digest = String::new();
    // Resolved once: with no stock git the probes cannot run at all, and every
    // probe folds into the digest as a marker rather than aborting the case.
    let Ok(stock) = crate::stock::git() else {
        return "<no-stock-git>\n".to_string();
    };
    for probe in PROBES {
        let mut cmd = Command::new(stock);
        env::harden(&mut cmd, home);
        cmd.current_dir(repo).args(*probe);
        let rendered = match cmd.output() {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
            Ok(_) => "<err>\n".to_string(),
            Err(_) => "<spawn-failed>\n".to_string(),
        };
        digest.push_str(&format!("# {}\n{}", probe.join(" "), rendered));
    }
    digest.push_str(&probe_storage(repo));
    digest.push_str(&probe_reflogs(repo));
    digest.push_str(&probe_rr_cache(repo));
    digest.push_str(&probe_op_state(repo));
    digest
}

/// Root-level files and refs that record an **in-progress operation**.
///
/// Enumerated from git 2.55.0 rather than globbed over `.git`, because a glob
/// would sweep in `index`, `COMMIT_EDITMSG`, `FETCH_HEAD`, `shallow` and the
/// hook samples — machine-local scratch and already-measured facts — and would
/// make the probe's meaning depend on whatever else happens to sit in the
/// directory. Each name below is cited to the code that writes or deletes it:
///
///  * `wt-status.c:1823` `wt_status_get_state` reads `MERGE_HEAD`,
///    `CHERRY_PICK_HEAD` and `REVERT_HEAD` to decide which operation is live;
///  * `wt-status.c:1783` `wt_status_check_bisect` keys on `BISECT_LOG` and
///    reads `BISECT_START`;
///  * `bisect.c:1191` `bisect_clean_state` is the authoritative list of what a
///    finished bisect must remove — `BISECT_ANCESTORS_OK`, `BISECT_LOG`,
///    `BISECT_NAMES`, `BISECT_RUN`, `BISECT_TERMS`, `BISECT_FIRST_PARENT`,
///    `BISECT_START`, plus the `BISECT_HEAD` and `BISECT_EXPECTED_REV` refs;
///  * `path.c:1582` names `SQUASH_MSG`, `MERGE_MSG`, `MERGE_RR`, `MERGE_MODE`
///    and `MERGE_HEAD`;
///  * `merge-ort.c:4950` writes `AUTO_MERGE`, `branch.c:835` deletes it;
///  * `sequencer.c:1713` writes `REBASE_HEAD`, `sequencer.c:5047` clears it;
///  * `reset.c:53`, `builtin/merge.c:1635` and `builtin/am.c:1092` write
///    `ORIG_HEAD`;
///  * `refs.c:917` lists `MERGE_AUTOSTASH`, `NOTES_MERGE_REF` and
///    `NOTES_MERGE_PARTIAL` as root refs.
///
/// `COMMIT_EDITMSG` is deliberately *not* here. It is the editor scratch buffer
/// every commit leaves behind, not state any `--continue`/`--abort` consults,
/// and `wt_status_get_state` never looks at it.
const OP_STATE_FILES: &[&str] = &[
    "AUTO_MERGE",
    "BISECT_ANCESTORS_OK",
    "BISECT_EXPECTED_REV",
    "BISECT_FIRST_PARENT",
    "BISECT_HEAD",
    "BISECT_LOG",
    "BISECT_NAMES",
    "BISECT_RUN",
    "BISECT_START",
    "BISECT_TERMS",
    "CHERRY_PICK_HEAD",
    "MERGE_AUTOSTASH",
    "MERGE_HEAD",
    "MERGE_MODE",
    "MERGE_MSG",
    "MERGE_RR",
    "NOTES_MERGE_PARTIAL",
    "NOTES_MERGE_REF",
    "ORIG_HEAD",
    "REBASE_HEAD",
    "REVERT_HEAD",
    "SQUASH_MSG",
];

/// Directories whose whole contents are operation state.
///
/// Walked rather than whitelisted, on the same reasoning as `probe_storage`'s
/// listing: git writes twenty-odd files under `rebase-merge` alone
/// (`sequencer.c:75`-`212`) and `builtin/am.c` another twenty under
/// `rebase-apply`, the set differs per invocation, and a file nobody thought of
/// is exactly the one a port forgets to write.
///
///  * `sequencer/` — `sequencer.c:68`-`73`: `todo`, `opts`, `head`,
///    `abort-safety`.
///  * `rebase-merge/` — `sequencer.c:75`, the interactive/merge rebase state.
///  * `rebase-apply/` — `wt-status.c:1753` and `builtin/am.c:161`, the `am` and
///    `rebase --apply` state.
///  * `NOTES_MERGE_WORKTREE/` — `notes-merge.c:282`, where a conflicted notes
///    merge parks its per-note files.
const OP_STATE_DIRS: &[&str] = &["NOTES_MERGE_WORKTREE", "rebase-apply", "rebase-merge", "sequencer"];

/// In-progress operation state: `.git/sequencer`, `.git/rebase-merge`,
/// `.git/rebase-apply`, and the root state files, as one `key: value` line each.
///
/// Nothing above this reads any of it. That is the whole state that makes
/// `--continue`, `--abort` and `--skip` work, and it was invisible: a
/// `cherry-pick A B C` that stopped on a conflict without writing
/// `.git/sequencer` at all scored the same as one that wrote it correctly, and
/// only tripped a probe later, incidentally, when the follow-up `--abort` left
/// an extra commit that `for-each-ref` happened to see.
///
/// **Contents, not presence.** Presence alone would pass a `sequencer/todo`
/// that lists the wrong commits or the wrong verbs, which is the same class of
/// silent-but-wrong that `probe_reflogs` was added to close. Nothing is elided:
/// unlike pack filenames, every value in these files — object ids, branch
/// names, todo verbs, `am` patch text — is a function of repository content
/// that two correct implementations must agree on, and both sides run the same
/// fixture. Verified by building seven interrupted operations (cherry-pick,
/// revert, merge, `rebase`, `rebase -i`, `am`, `bisect`) twice with stock 2.55.0
/// under `env::harden` and diffing the two `.git` trees: no differences, so
/// nothing here can push a case into `Nondeterministic`. Absolute paths, if a
/// future git writes one, are already covered by `normalize`'s `<REPO>`/`<HOME>`
/// substitution, which is applied to the whole digest.
///
/// **One line per fact**, with content newlines escaped, because the report
/// pairs the two digests line by line (`report.rs:259`) to name the fact that
/// moved. A multi-line value spliced in raw would shift every following line and
/// report a dozen phantom differences instead of the one real one.
fn probe_op_state(repo: &Path) -> String {
    let git = repo.join(".git");
    let mut out = String::from("# op-state\n");

    for name in OP_STATE_FILES {
        out.push_str(&format!("{name}: {}\n", read_as_value(&git.join(name))));
    }

    for dir in OP_STATE_DIRS {
        let path = git.join(dir);
        if !path.is_dir() {
            out.push_str(&format!("{dir}/: <absent>\n"));
            continue;
        }
        // Recorded separately from the file lines so that an operation that
        // creates the directory and writes nothing into it is still visible.
        out.push_str(&format!("{dir}/: <dir>\n"));
        for (rel, file) in walk_files(&path) {
            out.push_str(&format!("{dir}/{rel}: {}\n", read_as_value(&file)));
        }
    }
    out
}

/// One state file as a single `value` field: `<absent>` when it is not there,
/// otherwise its bytes with backslash, newline and carriage return escaped so
/// the fact occupies exactly one line.
fn read_as_value(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes)
            .replace('\\', "\\\\")
            .replace('\n', "\\n")
            .replace('\r', "\\r"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "<absent>".to_string(),
        Err(_) => "<unreadable>".to_string(),
    }
}

/// Every regular file under `dir`, as `(path relative to dir, absolute path)`,
/// sorted by the relative path so the listing does not depend on readdir order.
///
/// Symlinks are reported by name only — following them could walk outside the
/// repo, and no git metadata directory uses them for content.
fn walk_files(dir: &Path) -> Vec<(String, PathBuf)> {
    fn rec(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for entry in rd.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
            let path = entry.path();
            match std::fs::symlink_metadata(&path) {
                Ok(m) if m.is_dir() => rec(&path, &rel, out),
                Ok(_) => out.push((rel, path)),
                Err(_) => {}
            }
        }
    }
    let mut out = Vec::new();
    rec(dir, "", &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Reduce one entry of the object store to a name two correct implementations
/// must agree on, eliding only values neither side can reproduce.
///
/// Two elisions, each for a value that is *not* a function of repository
/// content:
///
///  * **Checksums.** Pack, multi-pack-index-bitmap and split-commit-graph
///    filenames embed a hash of their own bytes, and the vendored gitoxide
///    cannot reproduce git's pack bytes (see `probe_storage`). Handled by
///    [`elide_hashes`].
///  * **Temp-file randomness.** Git names in-progress files from `mkstemp`
///    (`tmp_pack_XXXXXX`) or from its own pid (`.tmp-<pid>-pack-<hash>.pack`,
///    the `.tmp-%d-pack` format in `pack-objects`). Neither is reproducible
///    even by stock git against itself: two runs of `index-pack --stdin` on
///    empty input leave `tmp_pack_juzecI` and `tmp_pack_OWu7xG`. Left raw,
///    those cases stopped being *measured* at all — they turned into
///    `Nondeterministic` exclusions, which is a worse outcome than the blind
///    spot this listing closes. The elision keeps the fact that a temp file was
///    left behind, and how many; only the random field is masked.
fn stable_entry_name(rel: &str) -> String {
    let component = |c: &str| -> String {
        // `.tmp-<pid>-pack-<hash>.pack`: mask the pid between the first two
        // dashes, leaving the rest (including the `<hash>`) to `elide_hashes`.
        if let Some(rest) = c.strip_prefix(".tmp-") {
            if let Some((pid, tail)) = rest.split_once('-') {
                if !pid.is_empty() && pid.chars().all(|ch| ch.is_ascii_digit()) {
                    return format!(".tmp-<pid>-{}", elide_hashes(tail));
                }
            }
        }
        // `tmp_<kind>_<mkstemp suffix>`: mask the final field.
        if c.starts_with("tmp_") {
            if let Some(cut) = c.rfind('_') {
                return format!("{}<tmp>", &c[..=cut]);
            }
        }
        elide_hashes(c)
    };
    rel.split('/').map(component).collect::<Vec<_>>().join("/")
}

/// Replace every run of 32 or more hex digits with `<hash>`.
fn elide_hashes(name: &str) -> String {
    let bytes: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len());
    let mut i = 0;
    while i < bytes.len() {
        let mut j = i;
        while j < bytes.len() && bytes[j].is_ascii_hexdigit() {
            j += 1;
        }
        if j - i >= 32 {
            out.push_str("<hash>");
        } else {
            out.extend(&bytes[i..j.max(i + 1)]);
        }
        i = j.max(i + 1);
    }
    out
}

/// Reflogs: `.git/logs/**`, compared line for line.
///
/// Nothing above reads them, so a command that lands the right ref value while
/// writing no reflog entry — or the wrong message, or an entry on the wrong log
/// — scored `Match`. `update-ref refs/heads/main HEAD~1` was the live example:
/// identical stdout, identical refs, identical objects, and one missing line in
/// `.git/logs/HEAD`.
///
/// Compared verbatim, including the committer identity and timestamp, because
/// `env::harden` pins `GIT_COMMITTER_{NAME,EMAIL,DATE}` and git stamps reflog
/// entries from exactly those. Verified by building the Branched shape twice
/// with stock and diffing `.git/logs` — identical. Since the timestamp is a
/// constant rather than a clock read, normalising it would only hide an
/// implementation that ignores the pinned date and stamps wall-clock time.
fn probe_reflogs(repo: &Path) -> String {
    let logs = repo.join(".git").join("logs");
    let mut out = String::from("# reflogs\n");
    for (rel, path) in walk_files(&logs) {
        let body = std::fs::read(&path)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_else(|_| "<unreadable>\n".to_string());
        out.push_str(&format!("## {rel}\n{body}"));
    }
    out
}

/// Recorded conflict resolutions: `.git/rr-cache/**`, compared byte for byte.
///
/// The preimage/postimage bytes *are* rerere — a run that creates the cache
/// directory but records the wrong hunks, or records nothing at all, is the
/// failure the feature exists to prevent. Only the exit code and stdout were
/// checked before, and both are silent on the record path.
///
/// Directory names are the hash of the conflict hunks, so they are stable for a
/// given fixture and are kept as-is rather than elided; verified by recording
/// the same conflict twice with stock and diffing the trees.
fn probe_rr_cache(repo: &Path) -> String {
    let rr = repo.join(".git").join("rr-cache");
    let mut out = String::from("# rr-cache\n");
    for (rel, path) in walk_files(&rr) {
        let body = std::fs::read(&path)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_else(|_| "<unreadable>\n".to_string());
        out.push_str(&format!("## {rel}\n{body}"));
    }
    out
}

/// Object *storage layout*, which the command probes above cannot see.
///
/// Every probe above reports the logical object and ref set. `repack` without
/// `-d` deletes nothing, so it leaves that set invariant — meaning a `repack`
/// that does nothing at all was indistinguishable from one that works, and
/// scored full marks. The same held for `gc` and `pack-objects`. This closes
/// that hole.
///
/// Deliberately compares **counts and presence, not bytes**. A pack's filename
/// embeds its checksum, and the vendored gitoxide cannot reproduce git's exact
/// pack bytes: `gix-pack` offers a single output mode,
/// `Mode::PackCopyAndBaseObjects`, with no delta compression
/// (`gix-pack/src/data/output/entry/iter_from_counts.rs:362`). Comparing names
/// or bytes would fail every valid-but-different pack, which measures the
/// wrong thing. Counting detects the no-op — the failure that was actually
/// hiding — without demanding byte-identical packs.
///
/// This is a known, bounded relaxation: a pack that is well-formed but differs
/// from git's grouping still passes. It is recorded here rather than left for a
/// reader to infer from a number.
///
/// What it does *not* relax is which files exist. An earlier version counted a
/// fixed list of extensions, so anything outside that list was invisible:
/// `objects/pack/multi-pack-index` has no extension at all and `.bitmap` was
/// simply not on the list, which is why `repack --write-midx -d -a` writing a
/// midx under stock and nothing under zvcs still scored `Match`. The listing
/// below is enumerated from the directory instead of from a whitelist, so a
/// file type nobody thought of is compared the day git starts writing it.
fn probe_storage(repo: &Path) -> String {
    let objects = repo.join(".git").join("objects");

    // Loose objects live in the 256 fan-out directories; everything else under
    // `objects/` (pack/, info/) is not a loose object.
    let loose = std::fs::read_dir(&objects)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter(|e| {
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    name.len() == 2 && name.chars().all(|c| c.is_ascii_hexdigit())
                })
                .map(|e| {
                    std::fs::read_dir(e.path())
                        .map(|inner| inner.filter_map(Result::ok).count())
                        .unwrap_or(0)
                })
                .sum::<usize>()
        })
        .unwrap_or(0);

    // Every entry under `objects/pack` and `objects/info`, with checksum runs
    // elided and the result sorted. Duplicates are kept, so splitting one pack
    // into two still shows up as two `pack-<hash>.pack` lines.
    let mut entries: Vec<String> = Vec::new();
    for sub in ["pack", "info"] {
        for (rel, _) in walk_files(&objects.join(sub)) {
            entries.push(format!("{sub}/{}", stable_entry_name(&rel)));
        }
    }
    // Sort *after* eliding: two names that differ only inside the checksum
    // collapse to the same string, and their pre-elision order is arbitrary.
    entries.sort();
    let listing: String = entries.iter().map(|e| format!("{e}\n")).collect();

    format!("# storage-layout\nloose {loose}\n{listing}")
}

/// Strip the three things that legitimately differ between two copies of the same
/// repo: their filesystem paths, and where each binary is installed.
///
/// This is the only masking applied, and it is intentionally narrow. Every
/// widening of this function weakens the parity number, so it stays auditable
/// in one place.
///
/// `exec_dir` is the side's *own* exec-path — where git looks for `git-<verb>`
/// helpers, as that side reports it. A few commands print it: `git p4`'s usage is
/// built from `sys.argv[0]`, and `git help --all` heads its listing with
/// `available git commands in '<exec-path>'`. Masking it is not a favour to the
/// port, it is the same fact the `<REPO>` and `<HOME>` tokens already encode —
/// established by running the *same stock git 2.55.0* from two prefixes:
///
/// ```text
/// A: usage: …/stockgit/git/2.55.0/libexec/git-core/git-p4 <command> [options]
/// B: usage: …/stock2/git/2.55.0/libexec/git-core/git-p4   <command> [options]
/// ```
///
/// Identical exit codes, identical text, one differing path — stock fails the
/// case against itself. A comparison that a binary cannot pass against its own
/// twin is measuring the filesystem, not the implementation.
///
/// It is a substitution of one known, computed path per side, never a pattern
/// over arbitrary paths: nothing about *what* the command printed is hidden, only
/// where this particular copy happens to live. Every other byte still has to
/// agree, which is why `version --build-options` still fails — its values
/// describe a C toolchain, not a location.
fn normalize(raw: &[u8], repo: &Path, home: &Path, exec_dir: &Path) -> String {
    let mut s = String::from_utf8_lossy(raw).into_owned();
    // Exec-path first: on the zvcs side it lives under `home`, so masking `home`
    // ahead of it would rewrite the prefix and leave the two sides unequal.
    for (path, token) in [(exec_dir, "<EXEC-PATH>"), (repo, "<REPO>"), (home, "<HOME>")] {
        let p = path.to_string_lossy().into_owned();
        if p.is_empty() {
            continue;
        }
        s = s.replace(&p, token);
        // Both the symlinked and resolved forms show up on macOS (/tmp vs /private/tmp).
        if let Ok(canon) = path.canonicalize() {
            s = s.replace(&canon.to_string_lossy().into_owned(), token);
        }
    }
    s
}

/// What `bin` reports as its exec-path, under the same hardened environment the
/// cases run in.
///
/// Asked of the binary rather than derived from its location: git computes it
/// from its own installation layout, and zvcs answers `$GIT_EXEC_PATH` else
/// `$HOME/.zvcs/bin`. Guessing either would mask the wrong string, and masking a
/// string neither side prints is worse than masking nothing.
/// The stock side's exec-path, resolved once for the whole run.
fn stock_exec_dir(home: &Path) -> &'static Path {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| match crate::stock::git() {
        Ok(bin) => exec_path_of(bin, home),
        Err(_) => PathBuf::new(),
    })
}

/// The binary-under-test's exec-path, resolved once for the whole run.
fn zvcs_exec_dir(bin: &Path, home: &Path) -> &'static Path {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| exec_path_of(bin, home))
}

fn exec_path_of(bin: &Path, home: &Path) -> PathBuf {
    let mut cmd = Command::new(bin);
    cmd.arg("--exec-path").current_dir(std::env::temp_dir());
    env::harden(&mut cmd, home);
    cmd.output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim().to_string()))
        .unwrap_or_default()
}

/// Every phrase the port uses to say "I have not implemented this".
///
/// Enumerated rather than pattern-guessed, and kept in one place so the list can
/// be re-derived from the port's source with a single grep. `unsupported`,
/// `not ported`, and `not supported` are matched as bare fragments because the
/// port inflects them a dozen ways ("unsupported flag", "unsupported option",
/// "unsupported mode", "unsupported revision range", "--patch is not ported",
/// "recognised but not ported", "magic pathspecs are not supported"); matching
/// each inflection separately is how three of these were missed to begin with.
const GAP_MARKERS: &[&str] = &[
    "not ported",
    "not yet ported",
    "is ported so far",
    "unsupported",
    "not supported",
    "not implemented",
];

/// True when zvcs is reporting a gap rather than disagreeing about behavior.
///
/// **This widens the failure bucket, it never narrows it.** `Unsupported` is
/// counted as a failure; recognising more of them moves cases *out* of
/// `exit-diff` and, where zvcs happened to fail with git's exit code and no
/// stdout, *out of `Match`* — a case that was passing only by coincidence.
/// Nothing here can turn a failure into a pass.
///
/// A marker only counts on a line spoken in *this port's own voice*. `fatal.rs`
/// makes that a type-level distinction and states the invariant the scan relies
/// on: a message git itself would `die()` with is rendered exactly as git renders
/// it, while a gap keeps the `zvcs: <verb>: …` prefix, because "a port that has
/// not implemented something and says so in git's voice is lying about its own
/// coverage". Git never writes `zvcs: `, so the prefix is the machine-readable
/// channel the note below asked for — no new protocol, just the one the port
/// already guarantees.
///
/// Scanning the whole of stderr instead scored four cases as gaps where zvcs was
/// byte-identical to stock — stdout, stderr *and* exit code — because the marker
/// sat inside git's own text that zvcs correctly reproduces: `error: unsupported
/// option 'bogus'` (column), `usage: working without -z is not supported`
/// (diff-pairs), and `fatal: replaying merge commits is not supported yet!`
/// (replay, twice). Reproducing git exactly is the thing being measured, so the
/// old scan penalised the port for succeeding, and any rewording to escape it
/// would have been a real parity regression.
///
/// Narrowing the scan cannot inflate the score. A case that leaves this bucket is
/// not thereby a pass — it is compared on stdout, exit code and repository state
/// like every other case, and matches only if all of them agree with stock.
fn is_unsupported(stderr: &str) -> bool {
    stderr
        .lines()
        .filter(|l| l.trim_start().starts_with("zvcs: "))
        .any(|l| GAP_MARKERS.iter().any(|m| l.contains(m)))
}

fn looks_like_panic(stderr: &str) -> bool {
    stderr.contains("panicked at") || stderr.contains("RUST_BACKTRACE")
}

/// Run one case against both implementations and judge it.
pub fn run_case(
    case: &Case,
    zvcs_bin: &Path,
    templates: &Templates,
    workdir: &Path,
) -> Result<Outcome> {
    let stock_repo = workdir.join("stock");
    let zvcs_repo = workdir.join("zvcs");
    let _ = std::fs::remove_dir_all(&stock_repo);
    let _ = std::fs::remove_dir_all(&zvcs_repo);
    templates.instantiate(case.shape, &stock_repo)?;
    templates.instantiate(case.shape, &zvcs_repo)?;

    let home = &templates.home;
    let stock = run_side(crate::stock::git()?, &stock_repo, home, &case.args, case.stdin, case.cwd, case.env)?;
    let zvcs = run_side(zvcs_bin, &zvcs_repo, home, &case.args, case.stdin, case.cwd, case.env)?;

    let stock_state = probe_state(&stock_repo, home);
    let zvcs_state = probe_state(&zvcs_repo, home);

    // Asked of each binary once per run, not per case: 4000+ cases would
    // otherwise pay two extra child processes each for an answer that cannot
    // change while the run is in flight.
    let stock_exec = stock_exec_dir(home);
    let zvcs_exec = zvcs_exec_dir(zvcs_bin, home);

    let stock_stdout = normalize(&stock.stdout, &stock_repo, home, stock_exec);
    let zvcs_stdout = normalize(&zvcs.stdout, &zvcs_repo, home, zvcs_exec);
    let stock_stderr = normalize(&stock.stderr, &stock_repo, home, stock_exec);
    let zvcs_stderr = normalize(&zvcs.stderr, &zvcs_repo, home, zvcs_exec);
    let stock_state_n = normalize(stock_state.as_bytes(), &stock_repo, home, stock_exec);
    let zvcs_state_n = normalize(zvcs_state.as_bytes(), &zvcs_repo, home, zvcs_exec);

    // Ordering matters: a crash outranks a gap, and a gap outranks the ordinary
    // diffs it would otherwise masquerade as.
    //
    // The stock timeout is checked first because it is not a verdict about zvcs at
    // all. `timed_out` was recorded for both sides and only ever read for one, so a
    // stock side the harness had killed fell through to the exit-code comparison
    // and was scored against the port: `stock=None` against a perfectly good exit
    // code reads as `exit-diff`. That is the same error the `Nondeterministic`
    // bucket exists to avoid — "counting an unmeasurable case as a failure is as
    // wrong as counting it as a pass".
    //
    // It is not hypothetical. `difftool --tool-help` and `mergetool --tool-help`
    // shell out to probe every tool on `PATH`; stock takes 1.6s for the first on an
    // idle machine and was measured at 29.7s under sixteen concurrent agents, past
    // the 20s ceiling, while this port answers in 88ms. Given the time, the two
    // agree byte for byte on stdout and stderr — verified — so every one of those
    // 60-odd failures in a loaded sweep was the harness timing out its own oracle.
    let verdict = if stock.timed_out {
        Verdict::StockTimeout
    } else if zvcs.timed_out {
        Verdict::Hang
    } else if looks_like_panic(&zvcs_stderr) || zvcs.code.is_none() {
        Verdict::Crash
    } else if is_unsupported(&zvcs_stderr) {
        Verdict::Unsupported
    } else if stock.code != zvcs.code {
        Verdict::ExitDiff
    } else if stock_stdout != zvcs_stdout {
        Verdict::StdoutDiff
    } else if stock_state_n != zvcs_state_n {
        Verdict::StateDiff
    } else if case.compare_stderr && stock_stderr != zvcs_stderr {
        Verdict::StderrDiff
    } else {
        Verdict::Match
    };

    // A failing case might be one stock git cannot reproduce itself. Re-run the
    // stock side in a fresh copy and compare stock against stock; only a
    // disagreement there reclassifies. Done lazily, on failure only, so the
    // common path still costs one stock run.
    let verdict = if verdict != Verdict::Match
        && stock_disagrees_with_itself(case, templates, workdir, &stock_stdout, &stock_state_n)?
    {
        Verdict::Nondeterministic
    } else {
        verdict
    };

    Ok(Outcome {
        case: case.clone(),
        verdict,
        stock_stdout,
        zvcs_stdout,
        stock_stderr,
        zvcs_stderr,
        stock_code: stock.code,
        zvcs_code: zvcs.code,
        stock_state: stock_state_n,
        zvcs_state: zvcs_state_n,
    })
}

/// Re-run the stock side in a second pristine repo and report whether stock
/// disagrees with itself — on **either** stdout or resulting repository state.
///
/// This is the only evidence accepted for calling a case unmeasurable. Git's
/// output and state carry values that are re-rolled every run, and no
/// implementation can match a value stock does not reproduce:
///   * `unpack-file` prints a randomly named temp file (stdout);
///   * `blame` stamps uncommitted lines with the current wall clock (stdout);
///   * `mergetool`, on the no-tool/EOF path, leaves `*_{BASE,LOCAL,REMOTE,
///     BACKUP}_<pid>.txt` temp files whose names embed the process id (state).
///
/// State non-determinism is checked as well as stdout precisely because of that
/// last class: an earlier version compared stdout only and mis-scored the
/// mergetool case as a failure though stock could not reproduce its own state.
///
/// The alternative would be hand-written masks per pattern, which have to be
/// maintained and quietly widen. Asking the oracle to reproduce itself needs no
/// pattern and cannot be aimed at a real difference: if stock agrees with stock
/// on both surfaces, this returns false and the original verdict stands.
fn stock_disagrees_with_itself(
    case: &Case,
    templates: &Templates,
    workdir: &Path,
    first_stdout: &str,
    first_state: &str,
) -> Result<bool> {
    let repo = workdir.join("stock-repeat");
    let _ = std::fs::remove_dir_all(&repo);
    templates.instantiate(case.shape, &repo)?;
    let home = &templates.home;
    let again = run_side(crate::stock::git()?, &repo, home, &case.args, case.stdin, case.cwd, case.env)?;
    if normalize(&again.stdout, &repo, home, stock_exec_dir(home)) != *first_stdout {
        return Ok(true);
    }
    let again_state = normalize(probe_state(&repo, home).as_bytes(), &repo, home, stock_exec_dir(home));
    Ok(again_state != *first_state)
}

/// Locate the zvcs `git` binary. Explicit override wins; otherwise the usual
/// cargo output paths, debug first to match the project's local-dev rule.
pub fn locate_zvcs_bin(explicit: Option<&str>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        let p = PathBuf::from(p);
        anyhow::ensure!(p.exists(), "zvcs binary not found at {}", p.display());
        return Ok(p);
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("locating zvcs repo root")?
        .to_path_buf();
    for candidate in ["target/debug/git", "target/release/git"] {
        let p = root.join(candidate);
        if p.exists() {
            return Ok(p);
        }
    }
    anyhow::bail!("no zvcs `git` binary found; run `cargo build` first")
}

#[cfg(test)]
mod tests {
    use super::{is_unsupported, probe_op_state, OP_STATE_DIRS, OP_STATE_FILES};
    use std::path::PathBuf;

    /// A scratch `.git` tree. The probe only reads the filesystem, so the test
    /// needs no git binary, no network and no fixture — just a temp directory.
    fn scratch(tag: &str) -> PathBuf {
        let repo: PathBuf =
            std::env::temp_dir().join(format!("zvcs-parity-op-state-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        repo
    }

    /// Every enumerated fact is reported even when nothing is in progress, so
    /// the two digests being compared line up positionally.
    #[test]
    fn op_state_reports_absent_facts() {
        let repo = scratch("op-state-empty");
        let probe = probe_op_state(&repo);
        assert_eq!(probe.lines().next(), Some("# op-state"));
        for name in OP_STATE_FILES {
            assert!(
                probe.lines().any(|l| l == format!("{name}: <absent>")),
                "missing absent line for {name} in:\n{probe}"
            );
        }
        for dir in OP_STATE_DIRS {
            assert!(probe.lines().any(|l| l == format!("{dir}/: <absent>")));
        }
    }

    /// Contents are compared, not just presence, and a multi-line value stays on
    /// one line — `report.rs` pairs the two digests by line position, so a raw
    /// newline here would misalign every following fact.
    #[test]
    fn op_state_flattens_contents_to_one_line_per_fact() {
        let repo = scratch("op-state-inprogress");
        let git = repo.join(".git");
        std::fs::write(git.join("CHERRY_PICK_HEAD"), b"0123456789abcdef\n").unwrap();
        std::fs::create_dir_all(git.join("sequencer")).unwrap();
        std::fs::write(git.join("sequencer/todo"), b"pick aaa one\npick bbb two\n").unwrap();
        std::fs::create_dir_all(git.join("rebase-merge")).unwrap();

        let probe = probe_op_state(&repo);
        let lines: Vec<&str> = probe.lines().collect();
        assert!(lines.contains(&"CHERRY_PICK_HEAD: 0123456789abcdef\\n"));
        assert!(lines.contains(&"sequencer/: <dir>"));
        assert!(lines.contains(&"sequencer/todo: pick aaa one\\npick bbb two\\n"));
        // An operation that creates its directory but writes nothing into it is
        // still distinguishable from one that never started.
        assert!(lines.contains(&"rebase-merge/: <dir>"));
        assert!(lines.contains(&"rebase-apply/: <absent>"));
        // Every line carries exactly one fact.
        assert!(lines.iter().skip(1).all(|l| l.contains(": ")));
    }

    /// A todo list that names different commits must not compare equal to one
    /// that names the right ones. This is the `cherry-pick A B C` blind spot.
    #[test]
    fn op_state_distinguishes_differing_sequencer_todos() {
        let a = scratch("op-state-todo-a");
        let b = scratch("op-state-todo-b");
        for (repo, todo) in [(&a, "pick aaa one\npick bbb two\n"), (&b, "pick aaa one\n")] {
            std::fs::create_dir_all(repo.join(".git/sequencer")).unwrap();
            std::fs::write(repo.join(".git/sequencer/todo"), todo).unwrap();
        }
        assert_ne!(probe_op_state(&a), probe_op_state(&b));
        // …and a missing sequencer differs from a present one.
        let c = scratch("op-state-todo-c");
        assert_ne!(probe_op_state(&a), probe_op_state(&c));
    }

    /// A gap is only a gap when the port says so in its own voice.
    ///
    /// Every string here is a real stderr captured from one of the two binaries;
    /// the `git_*` cases are stock git 2.55.0's own wording, reproduced
    /// byte-for-byte by zvcs, and scoring them as gaps marked the port down for
    /// being correct.
    #[test]
    fn only_the_ports_own_voice_counts_as_a_gap() {
        // zvcs speaking for itself: `zvcs: <verb>: …`, exit 1 (see `fatal.rs`).
        assert!(is_unsupported("zvcs: history: `history fixup` is not ported: requires a commit-replay engine\n"));
        assert!(is_unsupported("zvcs: jump: unsupported mode \"diff\"\n"));
        assert!(is_unsupported("zvcs: diff: unsupported option \"--no-such-flag\"\n"));

        // git's own wording, which zvcs reproduces exactly. Not a gap.
        assert!(!is_unsupported("error: unsupported option 'bogus'\n"));
        assert!(!is_unsupported("usage: working without -z is not supported\n"));
        assert!(!is_unsupported("fatal: replaying merge commits is not supported yet!\n"));
        assert!(!is_unsupported("fatal: Argument not supported for format 'tar': -9\n"));
        assert!(!is_unsupported("warning: --no-curl not supported in this build\n"));

        // A gap reported alongside git-voiced output is still a gap.
        assert!(is_unsupported("error: unsupported option 'bogus'\nzvcs: column: --mode is not ported\n"));

        // The prefix alone is not enough; the line must actually claim a gap.
        assert!(!is_unsupported("zvcs: commit: nothing to commit\n"));
    }
}
