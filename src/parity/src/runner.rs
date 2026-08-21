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
//!
//! # Both sides are asked to reproduce themselves
//!
//! A byte comparison is only meaningful between two values that each side can
//! produce twice. The stock side has been re-run on failure since the beginning
//! ([`Verdict::Nondeterministic`]); the zvcs side was not, so a zvcs-side flake
//! was reported as a hard parity failure with a specific diff attached and was
//! indistinguishable from a real bug. Two failures from one loaded fuzz run —
//! a `merge` state-diff and a `filter-branch` stdout-diff — were handed on as
//! defects and turned out to be irreproducible by hand and through the harness.
//! Engineering time spent chasing a non-bug is exactly the cost this crate
//! exists to prevent, so the repeat is now symmetric: both sides are asked, and
//! a side that disagrees with *itself* is reported as that
//! ([`Verdict::ZvcsNondeterministic`]) rather than as a difference between the
//! two.
//!
//! **The repeat is failure-triggered, not unconditional.** An unconditional
//! second zvcs run would double the wall clock of every sweep — 4000+ cases,
//! each already paying two child processes and two full state probes — for
//! evidence that is only ever consulted on the <1% of cases that fail. Runtime
//! is not a neutral cost here: a harness people run less often measures less.
//! The blind spot the cheap version accepts is a case where zvcs is
//! nondeterministic *and* the sampled run happened to land on stock's answer;
//! that case scores `Match` and no repeat is taken. That is the same blind spot
//! the stock repeat has always had, it errs toward the port being asked to be
//! right rather than toward an exclusion, and the flake surfaces the first time
//! the coin lands the other way.
//!
//! **A flake is counted, not excluded.** It gets its own verdict, its own
//! counter and its own column, and it stays inside the parity denominator as a
//! failure — see [`Verdict::ZvcsNondeterministic`] for why excluding it would be
//! the one kind of exclusion this harness must never have.

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
    /// `-c key=value` overrides, rendered ahead of the subcommand and applied
    /// identically to both sides.
    ///
    /// The largest surface the harness could not reach. Git's behaviour is a
    /// function of its configuration at least as much as of its argv —
    /// `core.abbrev` decides how wide every id prints, `diff.renames` decides
    /// whether a rename is a rename, `status.showUntrackedFiles` decides what
    /// `status` even lists — and with no case able to set a key, every one of
    /// those defaults was the only value ever measured. A port that ignores a
    /// setting entirely scored exactly the same as one that honours it.
    ///
    /// `-c` rather than writing `.git/config`: a case is one invocation against
    /// a pristine copy and cannot write a file first, and the command line is
    /// the one config source that needs no prior state. It also keeps the
    /// setting visible in [`Case::id`], so a failure names the key.
    ///
    /// A key that makes stock git **die** is kept, not filtered. Agreeing on the
    /// refusal is parity; excluding it would be measuring only the values git
    /// likes, which is the same error as scoring an unported command as a skip.
    ///
    /// Values are literal, like every other argv token — nothing here is
    /// substituted, so a value must not name an absolute path for the same
    /// reason [`Case::env`] values may not.
    pub config: Vec<(String, String)>,
    /// Global options that precede the subcommand — `--no-pager`, `-C <dir>`,
    /// `--namespace=<n>`, `--literal-pathspecs`, and the rest of the set git
    /// parses in `git.c:handle_options` before it dispatches a verb.
    ///
    /// Entirely unmeasured before this field existed: every case's argv started
    /// at the subcommand, so the whole of `handle_options` was reachable only by
    /// whatever a subcommand happened to re-implement. `git --list-cmds=main`,
    /// for one, is not supported by the port and no case could have caught it.
    ///
    /// One element is one *whole* option including its argument, so `-C src` is
    /// `["-C", "src"]` and not two independent tokens. That is what lets
    /// [`crate::fuzz::shrink`] drop an option without leaving its operand behind
    /// as a stray positional.
    pub globals: Vec<Vec<String>>,
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
    ///
    /// Owned rather than `&'static [(&'static str, &'static str)]`, which is what
    /// it was while every environment in the harness was a curated literal. The
    /// fuzzer draws a *variable* and a *value* independently from two pools, so
    /// the pair it produces exists only at run time and cannot be a `'static`
    /// reference to anything. Curated call sites are unaffected: [`Case::with_env`]
    /// still takes a borrowed slice of string pairs and copies it in.
    pub env: Vec<(String, String)>,
}

/// Stands in for the running side's fixture root inside a case's [`Case::env`]
/// values, so one literal can name both copies.
pub const REPO_PLACEHOLDER: &str = "{repo}";

impl Case {
    pub fn new(cmd: &'static str, args: &[&str], shape: Shape) -> Self {
        Self {
            cmd,
            args: args.iter().map(|s| s.to_string()).collect(),
            config: Vec::new(),
            globals: Vec::new(),
            shape,
            stdin: None,
            compare_stderr: false,
            cwd: None,
            env: Vec::new(),
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
    pub fn with_env(self, env: &[(&str, &str)]) -> Self {
        Self { env: env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), ..self }
    }

    /// Run this case with `-c key=value` in front of the subcommand.
    pub fn with_config(self, config: &[(&str, &str)]) -> Self {
        Self {
            config: config.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            ..self
        }
    }

    /// Run this case with global options in front of the subcommand. Each inner
    /// slice is one whole option including its argument: `&[&["-C", "src"]]`.
    pub fn with_globals(self, globals: &[&[&str]]) -> Self {
        Self {
            globals: globals
                .iter()
                .map(|g| g.iter().map(|t| t.to_string()).collect())
                .collect(),
            ..self
        }
    }

    /// The full argv handed to the binary: config overrides, then global
    /// options, then the subcommand and its own arguments.
    ///
    /// Config comes first because `-c` commutes with every other global option —
    /// it only appends to the parameter config list — while `-C` and
    /// `--git-dir` do not commute with *each other* (`--git-dir` is resolved
    /// relative to the directory `-C` has already moved to). Keeping the
    /// sampled globals adjacent and in the order they were drawn preserves that
    /// relationship; hoisting `-c` out of the way keeps it from splitting a
    /// pair whose order is load-bearing.
    ///
    /// Assembled here rather than stored flattened so [`crate::fuzz::shrink`]
    /// can drop one config pair or one whole global option, which it could not
    /// do if they were already spliced into `args` as loose tokens.
    pub fn argv(&self) -> Vec<String> {
        let mut argv = Vec::with_capacity(self.config.len() * 2 + self.globals.len() + self.args.len());
        for (key, value) in &self.config {
            argv.push("-c".to_string());
            argv.push(format!("{key}={value}"));
        }
        for global in &self.globals {
            argv.extend(global.iter().cloned());
        }
        argv.extend(self.args.iter().cloned());
        argv
    }

    /// How many independently droppable pieces this case has, across every
    /// dimension the shrinker minimizes. Used to report whether shrinking
    /// achieved anything; the subcommand at `args[0]` is never droppable.
    pub fn size(&self) -> usize {
        self.args.len().saturating_sub(1)
            + self.config.len()
            + self.globals.len()
            + self.env.len()
            + usize::from(self.cwd.is_some())
            + usize::from(self.stdin.is_some())
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
    ///
    /// Config overrides and global options are rendered *inside* the argv
    /// segment rather than as segments of their own, because that is what they
    /// are: `-c core.abbrev=4 --no-pager status` is one command line, and a
    /// reader who copies the segment after `::<cmd>::` onto a `git` prefix gets
    /// the invocation back. A case that sets neither keeps byte-for-byte the id
    /// it had before those fields existed, so no existing failure is renamed.
    ///
    /// The grammar `[!]<shape>::<cmd>::<argv>[::cwd[…]][::env[…]][::stdin[…]]`
    /// is load-bearing: `scripts/split_failures.pl` matches `<shape>::<cmd>::`
    /// off the front of a failure header to file the block under a subcommand,
    /// so the two leading segments must stay first and must stay free of spaces.
    pub fn id(&self) -> String {
        format!("{}{}", self.id_head(), self.id_tail())
    }

    /// `[!]<shape>::<cmd>::` — the two segments a failure is *filed* under.
    ///
    /// Split out of [`Case::id`] so [`Sequence::step_id`] renders the identical
    /// head rather than a second copy of the format string. A sequence whose head
    /// drifted from this one would still print, and would then be filed under no
    /// subcommand at all by `scripts/split_failures.pl` — a failure that silently
    /// disappears from the per-command briefs is worse than one that shouts.
    fn id_head(&self) -> String {
        let strict = if self.compare_stderr { "!" } else { "" };
        format!("{}{}::{}::", strict, self.shape.name(), self.cmd)
    }

    /// The rest of a case's identity: its argv, then the working directory,
    /// environment and stdin segments it actually carries.
    ///
    /// Everything here describes *one invocation*, which is why a sequence reuses
    /// it verbatim for the step it is reporting: the step's own argv and payload
    /// are what a reader needs to see beside "step 3 of 7".
    fn id_tail(&self) -> String {
        let mut id = self.argv().join(" ");
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

// ---------------------------------------------------------------------------
// Sequences: several invocations against one repository
// ---------------------------------------------------------------------------

/// One invocation inside a [`Sequence`]: an argv and the bytes fed to it.
///
/// Exactly two dimensions, and the choice of *which* two is the whole design of
/// the sequence corpus — see [`Sequence`] for the argument. `stdin` is here
/// rather than on the sequence because the operations sequences exist to reach
/// are precisely the ones that take a payload on one step and nothing on the
/// next: `am` reads a mailbox and then `am --skip` reads nothing, `rebase -i`
/// reads a todo through its editor and then `--continue` reads nothing. A
/// per-sequence payload would be delivered to every step, and a step that is
/// handed input it did not ask for is a different invocation from the one the
/// corpus meant to write.
#[derive(Clone, Debug)]
pub struct Step {
    pub args: Vec<String>,
    pub stdin: Option<&'static [u8]>,
}

impl Step {
    /// `<argv>` for the script listing, with the payload named when there is one.
    /// Rendered from the same [`fnv1a64`] digest [`Case::id`] uses, so the same
    /// bytes read as the same token wherever a reader meets them.
    fn render(&self) -> String {
        match self.stdin {
            None => self.args.join(" "),
            Some(b) => format!("{} <stdin[{}B/{:016x}]", self.args.join(" "), b.len(), fnv1a64(b)),
        }
    }
}

/// An ordered list of invocations run against **one** repository per side, with
/// the full comparison taken after every step.
///
/// # Why this dimension exists
///
/// Every other case in this harness is a single invocation against a pristine
/// fixture, and git's stateful operations are multi-step by construction. The
/// interesting divergences live *between* the steps: a conflicted `cherry-pick`
/// and then `--continue`; a `rebase -i` that writes a todo and then `--skip`; an
/// `am` that stops mid-mailbox and then `--abort`; `bisect start`/`good`/`bad`
/// walking to a verdict. Reaching those states from one argv means the fixture
/// has to *be* the interrupted operation, which pins one moment of it and leaves
/// the transitions — the part that actually breaks — unmeasured.
///
/// # What is per step and what is per case
///
/// A step carries **argv and stdin**. Everything else — shape, working
/// directory, extra environment, `-c` overrides, global options, whether stderr
/// is compared, and the subcommand the whole thing is scored under — lives on
/// the [`Case`] envelope and is shared by every step.
///
/// The split is not arbitrary. Those shared fields are what decide *which
/// repository is being talked to and how its inputs are interpreted*: the shape
/// is the repository, `cwd` and the `GIT_DIR`-family variables are how discovery
/// finds it, and `-c`/globals are the configuration the invocation is read
/// under. Holding them constant is what makes a sequence one workflow rather
/// than a bag of unrelated invocations that happen to share a directory. argv
/// and stdin are the only two things that genuinely differ step to step in the
/// workflows this corpus is written to reach.
///
/// The alternative — every dimension per step — was rejected for two reasons.
/// It would put six dimensions per step into [`Sequence::step_id`], and an id
/// nobody can read is an id nobody reproduces by hand. And the shape *cannot* be
/// per step in any case: a second shape means a second repository, which throws
/// away the state the previous steps built, which is the entire point. The cost
/// is a real expressiveness limit — a workflow needing `-c` on one step only, or
/// needing to run one step from inside a linked worktree, cannot be written
/// today. It is recorded here rather than left to be discovered: the fix is to
/// move that one field onto [`Step`] as an override with the envelope's value as
/// the default, and no comparison changes when it happens.
///
/// The envelope is a real [`Case`] rather than a copy of its fields so the two
/// units cannot drift: a dimension added to `Case` tomorrow is available to a
/// sequence the same day, delivered to both sides by the same [`run_side`] that
/// delivers it for a single case. Its `args` and `stdin` are always overwritten
/// by the step being run and are never themselves executed.
///
/// # Cost
///
/// A sequence of N steps costs N invocations and N state probes **per side**,
/// plus the two fixture instantiations every case already pays. It is the cheap
/// shape of this for three reasons:
///
///  * **One repository per side for the whole sequence.** The obvious
///    alternative — replay steps 1..i from a pristine copy for each i — is
///    O(N²) invocations and buys nothing, because the state a step needs is
///    exactly the state the previous step left.
///  * **It stops at the first divergence.** A sequence that breaks at step 2 of
///    7 pays for 2 steps, not 7. Continuing would be worse than useless: past
///    the first difference the two repositories are no longer the same premise,
///    so every later step would compare two different questions and report
///    differences that are consequences of the first one.
///  * **The repeat stays failure-triggered**, exactly as it is for a single
///    case. Only a failing sequence replays its prefix
///    ([`repeat_sequence_side`]), and only up to the step that failed.
///
/// The price is reported rather than estimated: `main` prints how many
/// sequences are in the run and the total invocation count per side beside the
/// case count, so the difference between "cases" and "invocations" is on the
/// first screen of every sweep instead of being inferred from a wall clock.
/// Deliberately not written as a percentage here — a number in a comment goes
/// stale the first time a sequence is added, and the run states the real one.
///
/// # Why the comparison is taken at every step
///
/// Reporting only the end state would make a sequence barely better than a
/// single case: "these two repositories differ after seven commands" names no
/// command. Comparing at each step means a divergence is attributed to the argv
/// that caused it, and — the stronger property — every step after the first runs
/// on a premise that has been *proven* equal on both sides rather than assumed.
/// A `cherry-pick --continue` that fails because the preceding `add` staged the
/// wrong thing is reported as the `add`, which is where the bug is.
pub struct Sequence {
    /// Short stable slug naming the workflow, rendered into every step id. For a
    /// curated sequence it is how a reader finds it in `corpus/sequences.rs`
    /// again; for a generated one it names the family and the draw that produced
    /// it, so a reader can tell the two apart at a glance in a failure header.
    ///
    /// Owned rather than `&'static str`, which is what it was while every
    /// sequence in the harness was a literal. [`crate::fuzz::generate_sequences`]
    /// composes a name from a family slug and a draw index at run time, and there
    /// is nothing `'static` for it to borrow. Curated call sites are unaffected:
    /// [`Sequence::new`] takes anything that converts, so `"conflict-abort"`
    /// still works and no existing step id changes by a byte.
    pub name: String,
    /// Everything a step does not carry. See the type docs for the split.
    pub envelope: Case,
    pub steps: Vec<Step>,
}

impl Sequence {
    /// A sequence scored under `cmd` and named `name`, over `shape`.
    ///
    /// `cmd` is the workflow's headline verb, not necessarily the verb of every
    /// step: the stash-conflict workflow runs `checkout --theirs` and `add` in
    /// the middle and is still a statement about `stash`. Scoring the whole
    /// sequence under one command is what puts it in that command's brief in
    /// `scripts/split_failures.pl`, which is where somebody fixing `stash` will
    /// look for it.
    pub fn new(cmd: &'static str, name: impl Into<String>, shape: Shape) -> Self {
        Self { name: name.into(), envelope: Case::new(cmd, &[], shape), steps: Vec::new() }
    }

    /// Append a step.
    pub fn step(self, args: &[&str]) -> Self {
        self.step_argv(args.iter().map(|s| s.to_string()).collect(), None)
    }

    /// Append a step fed `stdin`, byte-identically to both sides.
    pub fn step_stdin(self, args: &[&str], stdin: &'static [u8]) -> Self {
        self.step_argv(args.iter().map(|s| s.to_string()).collect(), Some(stdin))
    }

    /// Append a step whose argv was built at run time.
    ///
    /// The one push site the other two delegate to, and the only one a generated
    /// sequence can use: [`crate::fuzz::generate_sequences`] draws a step's
    /// tokens out of a [`crate::fuzz::Grammar`] and owns the `String`s it
    /// produced, so there is no `&[&str]` for it to hand over. Kept as a single
    /// push rather than three so a field added to [`Step`] cannot be filled in
    /// two places and forgotten in the third.
    pub fn step_argv(mut self, args: Vec<String>, stdin: Option<&'static [u8]>) -> Self {
        self.steps.push(Step { args, stdin });
        self
    }

    /// Compare stderr byte for byte on every step, as [`Case::strict`] does for a
    /// single invocation.
    pub fn strict(self) -> Self {
        self.map_envelope(|c| Case { compare_stderr: true, ..c })
    }

    /// Run every step from `cwd`, a path relative to the fixture root.
    pub fn in_dir(self, cwd: &'static str) -> Self {
        self.map_envelope(|c| c.in_dir(cwd))
    }

    /// Run every step with `env` added on top of [`crate::env::harden`].
    pub fn with_env(self, env: &[(&str, &str)]) -> Self {
        self.map_envelope(|c| c.with_env(env))
    }

    /// Run every step with `-c key=value` in front of its subcommand.
    pub fn with_config(self, config: &[(&str, &str)]) -> Self {
        self.map_envelope(|c| c.with_config(config))
    }

    /// Run every step with these global options in front of its subcommand.
    pub fn with_globals(self, globals: &[&[&str]]) -> Self {
        self.map_envelope(|c| c.with_globals(globals))
    }

    /// Apply one of [`Case`]'s own builders to the envelope.
    ///
    /// The builders are reused rather than reimplemented so a sequence's
    /// envelope can never be constructed in a way a single case could not be —
    /// including the invariants `Case::with_env`'s callers rely on.
    fn map_envelope(self, f: impl FnOnce(Case) -> Case) -> Self {
        Self { envelope: f(self.envelope), ..self }
    }

    pub fn cmd(&self) -> &'static str {
        self.envelope.cmd
    }

    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// The [`Case`] for step `index`: the envelope, with that step's argv and
    /// payload substituted in.
    ///
    /// This is the whole mechanism by which a sequence reaches both sides
    /// identically — there is no second delivery path to keep in sync with
    /// [`run_side`], and a dimension the envelope carries is applied to a step
    /// exactly as it would be to a single case.
    pub fn step_case(&self, index: usize) -> Case {
        let step = &self.steps[index];
        assert!(!step.args.is_empty(), "sequence {} step {index} has no argv", self.name);
        Case { args: step.args.clone(), stdin: step.stdin, ..self.envelope.clone() }
    }

    /// Every step rendered as `<n>  <argv>`, for the failure block. The failing
    /// step is marked, because a script that does not say where it stopped makes
    /// a reader count lines.
    pub fn script(&self, failing: usize) -> Vec<String> {
        self.steps
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mark = if i == failing { "->" } else { "  " };
                format!("{mark} {}  {}", i + 1, s.render())
            })
            .collect()
    }

    /// Identity of one step, in a grammar that extends [`Case::id`] rather than
    /// competing with it:
    ///
    /// ```text
    /// [!]<shape>::<cmd>::seq[<name>]::step<i>/<n>::<argv>[::cwd[…]][::env[…]][::stdin[…]]::script[<argv1> | <argv2> | …]
    /// ```
    ///
    /// Three properties are deliberate.
    ///
    /// **The head is unchanged**, so `scripts/split_failures.pl` — which matches
    /// `<shape>::<cmd>::` off the front of a failure header — files a sequence
    /// failure under its subcommand exactly as it files everything else, and a
    /// case that is *not* a sequence keeps byte for byte the id it has today.
    ///
    /// **The step is named before its argv**, because "step 2 of 5, running
    /// `cherry-pick --continue`" is the finding. A sequence that reported only
    /// its last step, or reported a state difference with no argv attached,
    /// would be barely better than the single-invocation case it replaces.
    ///
    /// **The whole script is in the id**, because a step is not reproducible
    /// without the steps that built its premise. The alternative — naming the
    /// sequence and making a reader find it in the corpus — makes the id
    /// dependent on a source file that changes, which is exactly what an id is
    /// for avoiding. It is long; it is also sufficient, and the report prints the
    /// script line by line beneath it for reading rather than copying.
    pub fn step_id(&self, index: usize) -> String {
        let case = self.step_case(index);
        let script: Vec<String> = self.steps.iter().map(Step::render).collect();
        format!(
            "{}seq[{}]::step{}/{}::{}::script[{}]",
            case.id_head(),
            self.name,
            index + 1,
            self.steps.len(),
            case.id_tail(),
            script.join(" | ")
        )
    }
}

/// Where in a sequence an [`Outcome`] came from, and what the whole sequence was.
#[derive(Clone, Debug)]
pub struct StepRef {
    /// 1-based, as it is printed.
    pub index: usize,
    pub total: usize,
    /// The sequence-aware id, rendered by [`Sequence::step_id`].
    pub id: String,
    /// Every step, with the reported one marked. Printed under the failure so a
    /// reader sees the premise without parsing it back out of the id.
    pub script: Vec<String>,
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
    /// zvcs does not agree with *itself* on this invocation: a second run in a
    /// second pristine repo produced different stdout or left a different
    /// post-state. Established the same way the stock side's non-determinism is
    /// — by re-running and diffing — never asserted from a pattern.
    ///
    /// **Counted as a failure, inside the parity denominator, never excluded.**
    /// That is the whole difference between this verdict and
    /// [`Verdict::Nondeterministic`], and it is deliberate. Stock's exclusion is
    /// safe because it is triggered by a property of the *oracle*: no zvcs
    /// behaviour can reach it, so it cannot be aimed at a zvcs bug. This one is
    /// triggered by the behaviour of the binary under test, so excluding it
    /// would mean a port that is *randomly* wrong quietly removes its own cases
    /// from the denominator and outscores a port that is deterministically
    /// wrong. A self-serving exclusion is the one thing measurement
    /// infrastructure must never have, so the error is taken in the other
    /// direction.
    ///
    /// The honest counter-argument, recorded rather than hidden: some cases in
    /// here are not zvcs's fault at all — a `filter-branch` progress line prints
    /// `(N seconds passed, remaining M predicted)`, and on a loaded machine one
    /// side reads a different clock than the other while stock's two samples
    /// happen to agree. No implementation can match a value stock does not
    /// stably produce either, so counting it marks the port down for a case
    /// nothing could pass. Two facts make that the better error: it is bounded
    /// and named (`--verbose` lists every one with its id), and it moves the
    /// number *down*, which is the safe direction for a number nobody may tune
    /// upward. Widening this into an exclusion, if it is ever worth doing, needs
    /// evidence that the case is unmeasurable for both binaries — which is what
    /// the stock repeat already tests and this one deliberately does not
    /// duplicate.
    ZvcsNondeterministic,
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

    /// True for the verdicts that mean *nothing could be measured*, as opposed
    /// to *something was measured and it differed*. These are exactly the cases
    /// `report.rs` leaves out of the parity denominator (`Tally::scored`).
    ///
    /// [`Verdict::ZvcsNondeterministic`] is deliberately **not** here: it is
    /// measured (both zvcs runs completed and disagreed) and it is counted.
    pub fn is_unmeasurable(self) -> bool {
        matches!(self, Verdict::Nondeterministic | Verdict::StockTimeout)
    }

    /// True for a failure the harness can expect to reproduce on demand.
    ///
    /// The shrinker needs this: minimizing a case is a search whose predicate is
    /// "does this still fail", and a predicate that answers from a coin flip —
    /// an unmeasurable case, or one zvcs does not reproduce — minimizes toward
    /// whichever argv happened to flake next and prints a "minimal" case that
    /// never reproduced anything.
    pub fn is_measured_failure(self) -> bool {
        !self.is_match() && !self.is_unmeasurable() && self != Verdict::ZvcsNondeterministic
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
            Verdict::ZvcsNondeterministic => "ZVCS-NONDETERMINISTIC",
            Verdict::Nondeterministic => "NONDETERMINISTIC",
            Verdict::StockTimeout => "STOCK-TIMEOUT",
        }
    }

    /// One line saying why a case is in an unscored or unreproducible bucket, for
    /// the listing `--verbose` prints. A count with no names and no reasons reads
    /// as "nothing to see here", which is precisely what a silent exclusion must
    /// never be allowed to look like.
    pub fn exclusion_reason(self) -> Option<&'static str> {
        match self {
            Verdict::Nondeterministic => Some(
                "stock git did not reproduce its own stdout or post-state; \
                 excluded from the parity denominator",
            ),
            Verdict::StockTimeout => Some(
                "stock git did not answer inside the case timeout, so there was no oracle; \
                 excluded from the parity denominator",
            ),
            Verdict::ZvcsNondeterministic => Some(
                "zvcs did not reproduce its own stdout or post-state while stock did; \
                 counted as a failure, not excluded",
            ),
            _ => None,
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
    /// Present exactly when this outcome came from a [`Sequence`]: which step was
    /// compared, and the script it sits in.
    ///
    /// `case` still holds the *step's* invocation — the same argv, shape,
    /// environment and payload a single case would carry — so everything that
    /// reads an outcome (the per-command tally, the state diff, the repeat
    /// report) works on a sequence step without knowing it is one. This field is
    /// what the two things that must know consult: [`Outcome::id`], and the
    /// failure block that prints the script.
    pub step: Option<StepRef>,
    pub verdict: Verdict,
    pub stock_stdout: String,
    pub zvcs_stdout: String,
    pub stock_stderr: String,
    pub zvcs_stderr: String,
    pub stock_code: Option<i32>,
    pub zvcs_code: Option<i32>,
    pub stock_state: String,
    pub zvcs_state: String,
    /// The second zvcs run, present exactly when one was taken — that is, on a
    /// case that failed and whose stock side reproduced itself.
    ///
    /// Kept even when it *agreed* with the first run, because "this failure was
    /// reproduced" is the fact a reader most wants attached to a failure they
    /// are about to spend an afternoon on, and it has already been paid for.
    pub zvcs_repeat: Option<Repeat>,
}

impl Outcome {
    /// What this outcome is called in the report and in `split_failures.pl`'s
    /// briefs: the step id for a sequence, the plain case id for everything else.
    ///
    /// One accessor rather than two call sites choosing between the fields,
    /// because the choice is not the reader's business and getting it wrong is
    /// silent — a sequence printed under its bare step argv reads as an ordinary
    /// failure against a pristine repository, which is the one thing it is not.
    pub fn id(&self) -> String {
        match &self.step {
            Some(s) => s.id.clone(),
            None => self.case.id(),
        }
    }
}

/// One repeat run of a side, reduced to the surfaces the repeat compares.
#[derive(Clone, Debug, Default)]
pub struct Repeat {
    /// The repeat hit the case timeout. Such a run proves nothing in either
    /// direction — see [`repeat_disagreement`].
    pub timed_out: bool,
    pub code: Option<i32>,
    /// Normalized exactly like the first run's, against the repeat's own repo.
    pub stdout: String,
    pub state: String,
    /// Which surface this repeat disagreed with the first run on, filled in by
    /// [`judge`] — the one place that holds both runs.
    ///
    /// Recorded rather than left for the report to recompute: the verdict and the
    /// printed explanation must come from the same comparison, or a report can
    /// say "did not reproduce its post-state" about a case classified on stdout.
    pub disagreement: Option<Surface>,
}

/// The surface a repeat run disagreed with the first run on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    Stdout,
    State,
}

impl Surface {
    pub fn name(self) -> &'static str {
        match self {
            Surface::Stdout => "stdout",
            Surface::State => "post-state",
        }
    }
}

/// Ceiling on a single invocation. Fuzzing reaches commands that wait on input
/// or spin; without a bound, one such case stalls the whole run rather than
/// being reported as the defect it is.
///
/// This is the *base* budget; see [`case_timeout`] for the one command that
/// needs more and why raising this number instead would be wrong.
const CASE_TIMEOUT: Duration = Duration::from_secs(20);

/// Extra seconds for the commands whose **correct** behaviour includes sleeping.
///
/// A ceiling exists to catch a hang. A command that is *specified* to sit still
/// for ten seconds spends half of the 20s budget doing the right thing, so on a
/// loaded machine — the fuzz runs use every core but one — it flips between
/// finishing and being killed, and the harness reports its own kill as a defect.
/// That is how `branched::filter-branch::filter-branch --force HEAD` came to be
/// filed as a stdout-diff.
///
/// Both alternatives are worse. Raising [`CASE_TIMEOUT`] to cover it buys the
/// slowest command in the corpus a budget that every other command then also
/// gets, so a real hang in `status` would have to burn 30s before it is
/// reported — the ceiling stops meaning anything. Excluding `filter-branch` from
/// the corpus deletes the measurement outright, which is the failure mode this
/// crate is built to refuse.
///
/// So the allowance is per command, additive on top of the base ceiling, and
/// each entry is cited to the sleep it pays for:
///
///  * `filter-branch` — 10s. Git's `git-filter-branch.sh` prints its
///    "this is not the recommended way" warning and then `sleep 10` before doing
///    anything, so a user can interrupt; the port reproduces that faithfully at
///    `src/extensions/src/porcelain/filter_branch.rs:585-588`. It is skipped only
///    when `FILTER_BRANCH_SQUELCH_WARNING` is set, and [`env::harden`] clears the
///    environment, so under this harness the sleep always happens.
///
/// An entry here can only ever let a *correct* run finish. It cannot hide a hang
/// in any other command, and it cannot turn a difference into a match: a case
/// that completes inside the budget is compared exactly as before.
const SLEEP_ALLOWANCE: &[(&str, u64)] = &[("filter-branch", 10)];

/// The wall-clock ceiling for one invocation of `case`: the base ceiling plus
/// whatever [`SLEEP_ALLOWANCE`] documents for its subcommand.
fn case_timeout(case: &Case) -> Duration {
    let extra = SLEEP_ALLOWANCE
        .iter()
        .find(|(cmd, _)| *cmd == case.cmd)
        .map_or(0, |(_, secs)| *secs);
    CASE_TIMEOUT + Duration::from_secs(extra)
}

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
fn apply_case_env(cmd: &mut Command, repo: &Path, extra: &[(String, String)]) {
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

/// Run one side of a case.
///
/// Takes the whole [`Case`] rather than a parameter per dimension: every
/// dimension a case can carry — argv, config, globals, stdin, cwd, environment —
/// has to reach both sides identically, and a six-argument call is a standing
/// invitation to add a seventh dimension and forget one of its two call sites.
fn run_side(bin: &Path, repo: &Path, home: &Path, case: &Case) -> Result<Side> {
    let stdin = case.stdin;
    let dir = case_dir(repo, case.cwd)?;
    let mut cmd = Command::new(bin);
    env::harden(&mut cmd, home);
    apply_case_env(&mut cmd, repo, &case.env);
    cmd.current_dir(&dir)
        .args(case.argv())
        // Closed stdin stays the default. A command that reads input it was not
        // given must still hit EOF rather than block, or the `Hang` verdict
        // stops meaning anything.
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn {} {:?}", bin.display(), case.argv()))?;

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
    let ceiling = case_timeout(case);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if start.elapsed() >= ceiling {
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

    // A killed child's pipes still hold whatever it managed to write. Those bytes
    // are kept for a human to read, and `timed_out` is the flag that keeps them
    // out of every comparison: [`classify`] answers `StockTimeout`/`Hang` before
    // any content is looked at, and [`repeat_disagreement`] refuses to draw a
    // conclusion from a repeat that was killed. Half an answer compared against a
    // whole one is a difference the case did not have.
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

/// One side reduced to exactly what the comparison reads: whether it answered at
/// all, its exit code, and its three normalized surfaces.
///
/// Exists so [`classify`] is a pure function of the things being compared and can
/// be exercised without a git, a fixture or a clock — the classification rules
/// are the part of this crate a bug would be most expensive in, and they were
/// previously reachable only by running a real sweep.
pub struct Compared<'a> {
    pub timed_out: bool,
    pub code: Option<i32>,
    pub stdout: &'a str,
    pub stderr: &'a str,
    pub state: &'a str,
}

/// Judge one case from the two sides' comparable projections.
///
/// Ordering matters: a crash outranks a gap, and a gap outranks the ordinary
/// diffs it would otherwise masquerade as.
///
/// The stock timeout is checked first because it is not a verdict about zvcs at
/// all. `timed_out` was recorded for both sides and only ever read for one, so a
/// stock side the harness had killed fell through to the exit-code comparison
/// and was scored against the port: `stock=None` against a perfectly good exit
/// code reads as `exit-diff`. That is the same error the `Nondeterministic`
/// bucket exists to avoid — "counting an unmeasurable case as a failure is as
/// wrong as counting it as a pass".
///
/// It is not hypothetical. `difftool --tool-help` and `mergetool --tool-help`
/// shell out to probe every tool on `PATH`; stock takes 1.6s for the first on an
/// idle machine and was measured at 29.7s under sixteen concurrent agents, past
/// the 20s ceiling, while this port answers in 88ms. Given the time, the two
/// agree byte for byte on stdout and stderr — verified — so every one of those
/// 60-odd failures in a loaded sweep was the harness timing out its own oracle.
///
/// Both timeout arms sit above every content comparison, which is what keeps a
/// killed child's partial output out of the diff: whatever bytes it wrote before
/// the kill are recorded for a human but can never *be* the verdict.
fn classify(stock: &Compared<'_>, zvcs: &Compared<'_>, compare_stderr: bool) -> Verdict {
    if stock.timed_out {
        Verdict::StockTimeout
    } else if zvcs.timed_out {
        Verdict::Hang
    } else if looks_like_panic(zvcs.stderr) || zvcs.code.is_none() {
        Verdict::Crash
    } else if is_unsupported(zvcs.stderr) {
        Verdict::Unsupported
    } else if stock.code != zvcs.code {
        Verdict::ExitDiff
    } else if stock.stdout != zvcs.stdout {
        Verdict::StdoutDiff
    } else if stock.state != zvcs.state {
        Verdict::StateDiff
    } else if compare_stderr && stock.stderr != zvcs.stderr {
        Verdict::StderrDiff
    } else {
        Verdict::Match
    }
}

/// Whether a repeat run disagrees with the first run, and on which surface.
///
/// The only evidence accepted for calling a side non-reproducible, and it is
/// shared by both sides so the two can never drift into asking different
/// questions.
///
/// **A repeat that timed out proves nothing.** It is not evidence of
/// non-determinism — a killed child's partial stdout differs from a complete
/// one every time, which would let a busy machine manufacture "stock does not
/// reproduce itself" out of a real, measured zvcs failure and drop it from the
/// denominator. That is a flattering exclusion driven by machine load, and it
/// was live: the stock repeat compared `again.stdout` without ever looking at
/// `again.timed_out`. Nor is it evidence of determinism, so the caller keeps
/// whatever verdict it already had — a difference two completed runs produced
/// stands on its own.
///
/// **Exit code is deliberately not a surface here.** Adding it would widen the
/// stock side's exclusion — cases that are failures today would become
/// unmeasurable and leave the denominator — and this harness does not widen
/// exclusions to make numbers move. The consequence on the zvcs side is that a
/// port whose exit code alone is unstable is reported as `exit-diff` rather than
/// as a flake, which errs toward the port being asked to be right.
fn repeat_disagreement(first_stdout: &str, first_state: &str, again: &Repeat) -> Option<Surface> {
    if again.timed_out {
        return None;
    }
    if again.stdout != first_stdout {
        return Some(Surface::Stdout);
    }
    if again.state != first_state {
        return Some(Surface::State);
    }
    None
}

/// Classify a case, then — only if it failed — ask each side to reproduce itself.
///
/// The repeats are closures rather than calls so the whole decision procedure is
/// testable against deterministic fakes: "a side that disagrees with itself lands
/// in the flake bucket" is a rule, and a rule nothing tests is a rule that drifts.
///
/// Precedence is stock first, and that is load-bearing rather than incidental.
/// When both sides are non-reproducible, the stock finding is the stronger
/// statement — *no* implementation could match a value stock does not stably
/// produce — so it wins, and every case classified `Nondeterministic` before this
/// function existed is still classified `Nondeterministic`. The zvcs repeat is
/// not even taken in that case: its answer could not change anything.
fn judge(
    compare_stderr: bool,
    stock: &Compared<'_>,
    zvcs: &Compared<'_>,
    stock_repeat: &mut dyn FnMut() -> Result<Repeat>,
    zvcs_repeat: &mut dyn FnMut() -> Result<Repeat>,
) -> Result<(Verdict, Option<Repeat>)> {
    let verdict = classify(stock, zvcs, compare_stderr);
    // Done lazily, on failure only, so the common path still costs one run per
    // side. See the module header for why the repeat is not unconditional.
    if verdict.is_match() {
        return Ok((verdict, None));
    }

    // A side that was killed has no first answer to reproduce — only the bytes it
    // got out before the kill. Comparing a complete repeat against that partial
    // capture reports a disagreement every time, which would quietly relabel a
    // `Hang` (a serious, specific defect) as a flake and a `StockTimeout` as
    // stock non-determinism. Neither reclassification is measured; both are the
    // timeout being laundered into a different word for it. So the timeout
    // verdicts stand as they are, and neither repeat is paid for.
    if stock.timed_out || zvcs.timed_out {
        return Ok((verdict, None));
    }

    let stock_again = stock_repeat()?;
    if repeat_disagreement(stock.stdout, stock.state, &stock_again).is_some() {
        return Ok((Verdict::Nondeterministic, None));
    }

    let mut zvcs_again = zvcs_repeat()?;
    zvcs_again.disagreement = repeat_disagreement(zvcs.stdout, zvcs.state, &zvcs_again);
    let verdict = if zvcs_again.disagreement.is_some() {
        Verdict::ZvcsNondeterministic
    } else {
        verdict
    };
    Ok((verdict, Some(zvcs_again)))
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
    let stock_exec = stock_exec_dir(home);
    let zvcs_exec = zvcs_exec_dir(zvcs_bin, home);
    compare_in(
        case,
        zvcs_bin,
        &stock_repo,
        &zvcs_repo,
        home,
        &mut || {
            repeat_side(crate::stock::git()?, case, templates, workdir, "stock-repeat", stock_exec)
        },
        &mut || repeat_side(zvcs_bin, case, templates, workdir, "zvcs-repeat", zvcs_exec),
    )
}

/// Run one invocation in two repositories that are **already prepared**, and
/// judge it.
///
/// Extracted from [`run_case`] when sequences arrived, because a sequence step
/// and a standalone case are the same act of measurement performed against
/// different premises: the case runs in a pristine copy, the step runs in
/// whatever the steps before it left. Everything after "the repositories exist" —
/// which binary runs where, which surfaces are normalized against which root,
/// which repeat is taken and when — is identical, and a second copy of it would
/// be a second place for the two to drift into comparing different things.
///
/// The repeats stay closures rather than becoming arguments of their own,
/// because *what it means to reproduce this invocation* is precisely what
/// differs: a case re-runs one argv in a fresh copy, a step has to replay its
/// whole prefix (see [`repeat_sequence_side`]). [`judge`] calls them only on a
/// failure, so neither is paid for on the common path.
#[allow(clippy::too_many_arguments)]
fn compare_in(
    case: &Case,
    zvcs_bin: &Path,
    stock_repo: &Path,
    zvcs_repo: &Path,
    home: &Path,
    stock_repeat: &mut dyn FnMut() -> Result<Repeat>,
    zvcs_repeat: &mut dyn FnMut() -> Result<Repeat>,
) -> Result<Outcome> {
    let (stock_repo, zvcs_repo) = (stock_repo.to_path_buf(), zvcs_repo.to_path_buf());
    let stock = run_side(crate::stock::git()?, &stock_repo, home, case)?;
    let zvcs = run_side(zvcs_bin, &zvcs_repo, home, case)?;

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

    // A failing case might be one neither binary reproduces itself. Each side is
    // re-run in a fresh copy of the same shape and compared against its own first
    // answer; only a disagreement there reclassifies. Both repeats are lazy — the
    // closures are called by `judge` only when the case failed.
    let (verdict, zvcs_repeat) = judge(
        case.compare_stderr,
        &Compared {
            timed_out: stock.timed_out,
            code: stock.code,
            stdout: &stock_stdout,
            stderr: &stock_stderr,
            state: &stock_state_n,
        },
        &Compared {
            timed_out: zvcs.timed_out,
            code: zvcs.code,
            stdout: &zvcs_stdout,
            stderr: &zvcs_stderr,
            state: &zvcs_state_n,
        },
        stock_repeat,
        zvcs_repeat,
    )?;

    Ok(Outcome {
        case: case.clone(),
        step: None,
        verdict,
        stock_stdout,
        zvcs_stdout,
        stock_stderr,
        zvcs_stderr,
        stock_code: stock.code,
        zvcs_code: zvcs.code,
        stock_state: stock_state_n,
        zvcs_state: zvcs_state_n,
        zvcs_repeat,
    })
}

/// Whether the sequence stops here: because this step diverged, or because it
/// was the last one.
///
/// A rule rather than an `if` buried in the loop, because it is the whole
/// semantics of a sequence and both halves of it are load-bearing.
///
/// **Stop on the first divergence.** Past it the two repositories are no longer
/// the same premise, so every later step would be comparing two different
/// questions: a `cherry-pick --continue` run against a repo whose preceding
/// `add` staged nothing is not the same invocation as the one stock ran, and
/// reporting its difference as a second finding would file one bug twice and
/// point the second copy at innocent code. The steps that were not run are named
/// in the failure block, so nothing is hidden — they are *unmeasured*, which is a
/// different claim from *passing*, and the report never makes the second one.
///
/// **Stop at the end.** The last step's outcome is the sequence's outcome when
/// nothing diverged, so a clean sequence scores exactly one `Match` — the same
/// weight in the parity denominator as any other case. Counting one per step
/// instead would let a seven-step sequence outvote seven single-invocation cases
/// on nothing but its length, which is a way of tuning a number by writing
/// longer corpus entries.
fn step_is_final(verdict: Verdict, index: usize, total: usize) -> bool {
    !verdict.is_match() || index + 1 == total
}

/// Run a whole sequence against both implementations, comparing after every step.
///
/// One repository per side for the entire sequence — that is what makes step 4's
/// premise be step 3's result rather than a fixture that was hand-built to
/// resemble it. The comparison is taken after each step and the first divergence
/// ends the run; see [`step_is_final`] for why, and [`Sequence`] for the cost.
pub fn run_sequence(
    seq: &Sequence,
    zvcs_bin: &Path,
    templates: &Templates,
    workdir: &Path,
) -> Result<Outcome> {
    anyhow::ensure!(!seq.steps.is_empty(), "sequence {} has no steps", seq.name);

    let stock_repo = workdir.join("stock");
    let zvcs_repo = workdir.join("zvcs");
    let _ = std::fs::remove_dir_all(&stock_repo);
    let _ = std::fs::remove_dir_all(&zvcs_repo);
    templates.instantiate(seq.envelope.shape, &stock_repo)?;
    templates.instantiate(seq.envelope.shape, &zvcs_repo)?;

    let home = &templates.home;
    let stock_exec = stock_exec_dir(home);
    let zvcs_exec = zvcs_exec_dir(zvcs_bin, home);

    for index in 0..seq.steps.len() {
        let case = seq.step_case(index);
        let mut outcome = compare_in(
            &case,
            zvcs_bin,
            &stock_repo,
            &zvcs_repo,
            home,
            &mut || {
                repeat_sequence_side(
                    crate::stock::git()?,
                    seq,
                    index,
                    templates,
                    workdir,
                    "stock-repeat",
                    stock_exec,
                )
            },
            &mut || {
                repeat_sequence_side(
                    zvcs_bin, seq, index, templates, workdir, "zvcs-repeat", zvcs_exec,
                )
            },
        )?;
        if step_is_final(outcome.verdict, index, seq.steps.len()) {
            outcome.step = Some(StepRef {
                index: index + 1,
                total: seq.steps.len(),
                id: seq.step_id(index),
                script: seq.script(index),
            });
            return Ok(outcome);
        }
    }
    unreachable!("the last step is always final")
}

/// Replay a sequence's first `index + 1` steps in a fresh repository and report
/// what the last of them produced, so the caller can ask whether that side
/// reproduces *itself*.
///
/// The prefix has to be replayed, not just the failing step: the step's answer is
/// a function of the state the steps before it built, and re-running
/// `cherry-pick --continue` alone in a pristine copy would produce "no cherry-pick
/// in progress" on both sides and prove nothing about the case that failed. That
/// is the one difference from [`repeat_side`], and it is why the repeat is a
/// closure the caller supplies rather than something [`compare_in`] decides.
///
/// Cost is bounded by the same rule everything else here follows: [`judge`] calls
/// this only on a failure, and only up to the step that failed.
fn repeat_sequence_side(
    bin: &Path,
    seq: &Sequence,
    index: usize,
    templates: &Templates,
    workdir: &Path,
    sub: &str,
    exec_dir: &Path,
) -> Result<Repeat> {
    let repo = workdir.join(sub);
    let _ = std::fs::remove_dir_all(&repo);
    templates.instantiate(seq.envelope.shape, &repo)?;
    let home = &templates.home;

    let mut again = None;
    for i in 0..=index {
        again = Some(run_side(bin, &repo, home, &seq.step_case(i))?);
    }
    let again = again.expect("the loop runs at least once");
    Ok(Repeat {
        timed_out: again.timed_out,
        code: again.code,
        stdout: normalize(&again.stdout, &repo, home, exec_dir),
        state: normalize(probe_state(&repo, home).as_bytes(), &repo, home, exec_dir),
        disagreement: None,
    })
}

/// One unit of work for the runner pool: a single invocation, or a whole
/// sequence of them.
///
/// The pool schedules by index and writes results back into per-index slots, so
/// it needs one list of things to run and one way to run them. Two lists run in
/// two passes would report every sequence after every case regardless of what
/// order the corpus declares them in, and would need the progress counter, the
/// `--only` filter and the error latch written twice.
pub enum Job {
    Single(Case),
    Sequence(Sequence),
}

impl Job {
    /// The subcommand this job is scored under — the case's verb, or the
    /// sequence's headline verb. `--only` filters on it.
    pub fn cmd(&self) -> &'static str {
        match self {
            Job::Single(c) => c.cmd,
            Job::Sequence(s) => s.cmd(),
        }
    }

    /// How many invocations per side this job costs, before any repeat. Reported
    /// at startup so the price of the sequence corpus is visible rather than
    /// inferred from a wall clock.
    pub fn invocations(&self) -> usize {
        match self {
            Job::Single(_) => 1,
            Job::Sequence(s) => s.len(),
        }
    }

    pub fn run(&self, zvcs_bin: &Path, templates: &Templates, workdir: &Path) -> Result<Outcome> {
        match self {
            Job::Single(c) => run_case(c, zvcs_bin, templates, workdir),
            Job::Sequence(s) => run_sequence(s, zvcs_bin, templates, workdir),
        }
    }
}

/// Re-run one side in a second pristine repo and report what it produced, so the
/// caller can ask whether that side reproduces *itself* — on **either** stdout or
/// resulting repository state.
///
/// This is the only evidence accepted for calling a side non-reproducible. Git's
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
/// maintained and quietly widen. Asking a binary to reproduce itself needs no
/// pattern and cannot be aimed at a real difference: if the two runs agree on
/// both surfaces, [`repeat_disagreement`] says so and the original verdict stands.
///
/// `sub` names the repeat's own directory under the worker's workdir, so the two
/// sides' repeats never share a repo with each other or with the first runs —
/// and the normalization is done against *that* repo's path, because a digest
/// that still carries the repeat's own root would differ from the first run's for
/// no reason but its location.
fn repeat_side(
    bin: &Path,
    case: &Case,
    templates: &Templates,
    workdir: &Path,
    sub: &str,
    exec_dir: &Path,
) -> Result<Repeat> {
    let repo = workdir.join(sub);
    let _ = std::fs::remove_dir_all(&repo);
    templates.instantiate(case.shape, &repo)?;
    let home = &templates.home;
    let again = run_side(bin, &repo, home, case)?;
    Ok(Repeat {
        timed_out: again.timed_out,
        code: again.code,
        stdout: normalize(&again.stdout, &repo, home, exec_dir),
        state: normalize(probe_state(&repo, home).as_bytes(), &repo, home, exec_dir),
        // Filled by `judge`, which is the only caller holding the first run.
        disagreement: None,
    })
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
    use super::{
        case_timeout, is_unsupported, judge, probe_op_state, repeat_disagreement, step_is_final,
        Case, Compared, Outcome, Repeat, Sequence, StepRef, Surface, Verdict, CASE_TIMEOUT,
        OP_STATE_DIRS, OP_STATE_FILES,
    };
    use crate::fixture::Shape;
    use std::path::PathBuf;
    use std::time::Duration;

    /// A side that answered, with the surfaces spelled out. Everything the
    /// classification reads and nothing else — no binary, no repo, no clock, so
    /// these tests can never flake on the thing they exist to detect.
    fn side<'a>(code: i32, stdout: &'a str, state: &'a str) -> Compared<'a> {
        Compared { timed_out: false, code: Some(code), stdout, stderr: "", state }
    }

    /// A side the harness killed. `stdout` is whatever it managed to write before
    /// the kill — deliberately non-empty in the tests, because the bug being
    /// guarded is a partial capture reaching the diff.
    fn killed(stdout: &str) -> Compared<'_> {
        Compared { timed_out: true, code: None, stdout, stderr: "", state: "" }
    }

    fn repeat(stdout: &str, state: &str) -> Repeat {
        Repeat {
            timed_out: false,
            code: Some(0),
            stdout: stdout.to_string(),
            state: state.to_string(),
            disagreement: None,
        }
    }

    /// A repeat the harness killed: partial stdout, nothing else.
    fn killed_repeat() -> Repeat {
        Repeat { timed_out: true, code: None, stdout: "half".into(), ..repeat("", "") }
    }

    /// Two zvcs runs that disagree are a flake, not the content difference the
    /// first run made them look like.
    ///
    /// This is the whole point of the symmetric repeat: before it, `merge` and
    /// `filter-branch` cases that no two runs agreed on were reported as a
    /// state-diff and a stdout-diff and were handed on as bugs to chase.
    #[test]
    fn a_zvcs_side_that_disagrees_with_itself_lands_in_the_flake_bucket() {
        // Differing stdout: stock reproduces itself, zvcs does not.
        let (v, r) = judge(
            false,
            &side(0, "stock\n", "S"),
            &side(0, "zvcs-a\n", "S"),
            &mut || Ok(repeat("stock\n", "S")),
            &mut || Ok(repeat("zvcs-b\n", "S")),
        )
        .unwrap();
        assert_eq!(v, Verdict::ZvcsNondeterministic);
        assert_eq!(r.unwrap().disagreement, Some(Surface::Stdout));

        // Differing post-state, identical stdout — the `merge` shape of the bug.
        let (v, r) = judge(
            false,
            &side(0, "same\n", "stock-state"),
            &side(0, "same\n", "zvcs-state-a"),
            &mut || Ok(repeat("same\n", "stock-state")),
            &mut || Ok(repeat("same\n", "zvcs-state-b")),
        )
        .unwrap();
        assert_eq!(v, Verdict::ZvcsNondeterministic);
        assert_eq!(r.unwrap().disagreement, Some(Surface::State));
    }

    /// A difference both sides reproduce keeps the content verdict it earned.
    /// The repeat may only ever explain a failure, never dissolve one.
    #[test]
    fn a_reproducible_difference_keeps_its_content_verdict() {
        for (stock, zvcs, want) in [
            (side(0, "a\n", "S"), side(0, "b\n", "S"), Verdict::StdoutDiff),
            (side(0, "a\n", "S"), side(0, "a\n", "T"), Verdict::StateDiff),
            (side(0, "a\n", "S"), side(1, "a\n", "S"), Verdict::ExitDiff),
        ] {
            let (v, r) = judge(
                false,
                &stock,
                &zvcs,
                &mut || Ok(repeat(stock.stdout, stock.state)),
                &mut || Ok(repeat(zvcs.stdout, zvcs.state)),
            )
            .unwrap();
            assert_eq!(v, want);
            // The repeat is kept even when it agreed: "this failure reproduced"
            // is the fact a reader wants attached to it.
            assert_eq!(r.unwrap().disagreement, None);
        }
    }

    /// A killed child's partial output must never be reportable as a content
    /// difference. Both timeout arms sit above every byte comparison, so the
    /// half-written stdout below cannot become the verdict.
    #[test]
    fn a_timed_out_side_lands_in_the_timeout_bucket_not_a_content_bucket() {
        // Stock killed: not a verdict about zvcs at all, and no repeat is worth
        // taking — the oracle never answered once.
        let (v, r) = judge(
            false,
            &killed("partial"),
            &side(0, "full\n", "S"),
            &mut || panic!("no repeat is taken for a timeout"),
            &mut || panic!("no repeat is taken for a timeout"),
        )
        .unwrap();
        assert_eq!(v, Verdict::StockTimeout);
        assert!(r.is_none());

        // zvcs killed: a hang, never a stdout-diff against its own truncated output.
        let (v, r) = judge(
            false,
            &side(0, "full\n", "S"),
            &killed("partial"),
            &mut || panic!("no repeat is taken for a timeout"),
            &mut || panic!("no repeat is taken for a timeout"),
        )
        .unwrap();
        assert_eq!(v, Verdict::Hang);
        assert!(r.is_none());
    }

    /// A repeat that hit the timeout is evidence of nothing, in either direction.
    ///
    /// Its partial stdout differs from a complete run's every time, so accepting
    /// it would let a loaded machine manufacture "stock does not reproduce
    /// itself" out of a real zvcs failure and drop it from the denominator —
    /// which is what the stock repeat did before it looked at `timed_out`.
    #[test]
    fn a_repeat_that_timed_out_proves_nothing() {
        assert_eq!(repeat_disagreement("full\n", "S", &killed_repeat()), None);

        // Stock's repeat killed: the measured difference stands, un-excluded.
        let (v, _) = judge(
            false,
            &side(0, "stock\n", "S"),
            &side(0, "zvcs\n", "S"),
            &mut || Ok(killed_repeat()),
            &mut || Ok(repeat("zvcs\n", "S")),
        )
        .unwrap();
        assert_eq!(v, Verdict::StdoutDiff);

        // zvcs's repeat killed: not a flake either, for the same reason.
        let (v, _) = judge(
            false,
            &side(0, "stock\n", "S"),
            &side(0, "zvcs\n", "S"),
            &mut || Ok(repeat("stock\n", "S")),
            &mut || Ok(killed_repeat()),
        )
        .unwrap();
        assert_eq!(v, Verdict::StdoutDiff);
    }

    /// When neither side reproduces itself, the stock finding wins and the zvcs
    /// repeat is not even taken: no implementation can match a value stock does
    /// not stably produce, so its answer could not change anything.
    #[test]
    fn stock_nondeterminism_outranks_a_zvcs_flake() {
        let (v, r) = judge(
            false,
            &side(0, "stock-a\n", "S"),
            &side(0, "zvcs-a\n", "S"),
            &mut || Ok(repeat("stock-b\n", "S")),
            &mut || panic!("the zvcs repeat must not be taken once stock has disagreed with itself"),
        )
        .unwrap();
        assert_eq!(v, Verdict::Nondeterministic);
        assert!(r.is_none());
    }

    /// The repeat is failure-triggered. A matching case pays for neither side's
    /// second run — the property that keeps a full sweep from doubling.
    #[test]
    fn a_matching_case_takes_no_repeat_at_all() {
        let (v, r) = judge(
            false,
            &side(0, "same\n", "S"),
            &side(0, "same\n", "S"),
            &mut || panic!("a match must not pay for a repeat"),
            &mut || panic!("a match must not pay for a repeat"),
        )
        .unwrap();
        assert_eq!(v, Verdict::Match);
        assert!(r.is_none());
    }

    /// The sleep allowance is additive and reaches exactly the commands whose
    /// correct behaviour includes sleeping — never a global ceiling raise, which
    /// would buy every other command a budget that hides a real hang.
    #[test]
    fn the_sleep_allowance_reaches_only_the_commands_that_sleep() {
        let case = |cmd| Case::new(cmd, &[cmd], Shape::Linear);
        assert_eq!(case_timeout(&case("status")), CASE_TIMEOUT);
        assert_eq!(case_timeout(&case("merge")), CASE_TIMEOUT);
        // `git-filter-branch.sh` sleeps 10s before doing anything; the port
        // reproduces it (`porcelain/filter_branch.rs:587`).
        assert_eq!(case_timeout(&case("filter-branch")), CASE_TIMEOUT + Duration::from_secs(10));
    }


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

    // -----------------------------------------------------------------------
    // Sequences
    // -----------------------------------------------------------------------

    /// A four-step workflow with a payload on one step, used by the id and
    /// composition tests below.
    fn workflow() -> Sequence {
        Sequence::new("cherry-pick", "conflict-continue", Shape::Conflicted)
            .step(&["merge", "--abort"])
            .step(&["cherry-pick", "theirs"])
            .step_stdin(&["am"], b"payload\n")
            .step(&["cherry-pick", "--continue"])
    }

    /// The identity of a case that is **not** a sequence is byte for byte what
    /// it was before sequences existed.
    ///
    /// Spelled as literals rather than derived from the code, which is the whole
    /// point: the report and `scripts/split_failures.pl` key on these strings,
    /// and a refactor that renamed every existing failure would be invisible to
    /// a test that rebuilt the expectation the same way `id()` does.
    #[test]
    fn a_plain_case_id_is_unchanged_by_the_sequence_grammar() {
        assert_eq!(
            Case::new("status", &["status", "--porcelain"], Shape::Dirty).id(),
            "dirty::status::status --porcelain"
        );
        assert_eq!(
            Case::strict("reset", &["reset", "--mixed"], Shape::BehindRemote)
                .in_dir(".remote.git")
                .id(),
            "!behind-remote::reset::reset --mixed::cwd[.remote.git]"
        );
        assert_eq!(
            Case::new("log", &["log", "--oneline"], Shape::Branched)
                .with_config(&[("core.abbrev", "4")])
                .with_globals(&[&["-C", "src"]])
                .id(),
            "branched::log::-c core.abbrev=4 -C src log --oneline"
        );
        assert_eq!(
            Case::with_stdin("mktree", &["mktree"], Shape::Linear, b"garbage\n")
                .with_env(&[("GIT_DIR", "{repo}/.git")])
                .id(),
            "linear::mktree::mktree::env[GIT_DIR={repo}/.git]::stdin[8B/4c4485da341d8b72]"
        );
    }

    /// A step's identity names the step, its own argv, and the whole script —
    /// the three things a reader needs to know *what failed* and *how to get
    /// back to it*.
    #[test]
    fn a_sequence_step_id_names_the_step_and_carries_the_whole_script() {
        let seq = workflow();
        assert_eq!(
            seq.step_id(1),
            "conflicted::cherry-pick::seq[conflict-continue]::step2/4::cherry-pick theirs\
             ::script[merge --abort | cherry-pick theirs | am <stdin[8B/acb27c196e1c811d] \
             | cherry-pick --continue]"
        );
        // The step's own payload is in its identity as well, exactly as it is
        // for a single case — two steps with one argv and different input are
        // different invocations.
        assert!(seq.step_id(2).contains("::step3/4::am::stdin[8B/acb27c196e1c811d]::script["));
        // Each step is named by its own index; nothing collapses to the last.
        for i in 0..seq.len() {
            assert!(seq.step_id(i).contains(&format!("::step{}/4::", i + 1)));
        }
    }

    /// A sequence failure header is filed under its subcommand by
    /// `scripts/split_failures.pl`, whose regex is reproduced here verbatim.
    ///
    /// The script is the only route from a report to a per-command brief, and it
    /// fails *silently* — an id whose first two segments stopped being a
    /// space-free shape and command would simply not match, and every sequence
    /// failure would vanish from the briefs while the summary still counted it.
    #[test]
    fn a_sequence_header_still_files_under_its_subcommand() {
        let re = regex::Regex::new(r"^\[([A-Z-]+)\]\s+\S+?::(\S+?)::").unwrap();
        let seq = workflow();
        for i in 0..seq.len() {
            let header = format!("[STATE-DIFF] {}", seq.step_id(i));
            let caps = re.captures(&header).unwrap_or_else(|| {
                panic!("split_failures.pl would not file this header:\n{header}")
            });
            assert_eq!(&caps[1], "STATE-DIFF");
            assert_eq!(&caps[2], "cherry-pick");
        }
        // And a strict sequence keeps the `!` in front of the shape, where the
        // regex tolerates it, rather than growing a segment of its own.
        let strict = Sequence::new("merge", "refusals", Shape::Linear).strict().step(&["merge"]);
        assert!(strict.step_id(0).starts_with("!linear::merge::seq["));
        assert!(re.is_match(&format!("[STDERR-DIFF] {}", strict.step_id(0))));
    }

    /// Every envelope dimension reaches every step, and only argv and stdin
    /// differ between them. This is the composition rule the sequence corpus is
    /// written against; a step that quietly lost the envelope's `-c` or `cwd`
    /// would still run, and would measure a different invocation than the one
    /// the corpus spells.
    #[test]
    fn a_sequence_step_inherits_every_envelope_dimension() {
        let seq = Sequence::new("cherry-pick", "env-carried", Shape::Conflicted)
            .strict()
            .in_dir("src")
            .with_env(&[("GIT_DIR", "{repo}/.git")])
            .with_config(&[("rerere.enabled", "true")])
            .with_globals(&[&["--no-advice"]])
            .step(&["cherry-pick", "theirs"])
            .step_stdin(&["am"], b"payload\n");

        for i in 0..seq.len() {
            let case = seq.step_case(i);
            assert_eq!(case.cmd, "cherry-pick");
            assert_eq!(case.shape, Shape::Conflicted);
            assert!(case.compare_stderr);
            assert_eq!(case.cwd, Some("src"));
            assert_eq!(case.env, vec![("GIT_DIR".to_string(), "{repo}/.git".to_string())]);
            assert_eq!(
                case.config,
                vec![("rerere.enabled".to_string(), "true".to_string())]
            );
            assert_eq!(case.globals, vec![vec!["--no-advice".to_string()]]);
            // The envelope's own argv and payload are never executed: the step's
            // are substituted in every time.
            assert_eq!(case.args, seq.steps[i].args);
            assert_eq!(case.stdin, seq.steps[i].stdin);
        }
        // …and the config and globals are rendered into the step's command line
        // in the same order a single case renders them.
        assert_eq!(
            seq.step_case(0).argv(),
            vec!["-c", "rerere.enabled=true", "--no-advice", "cherry-pick", "theirs"]
        );
    }

    /// A sequence reports the *first* step that diverged, and otherwise its
    /// last step — never a middle step that matched, and never a step after a
    /// divergence.
    ///
    /// Continuing past a difference would compare two repositories that are no
    /// longer the same premise, and would file the consequences of one bug as
    /// further bugs against innocent commands.
    #[test]
    fn a_sequence_stops_at_the_first_divergence_or_at_its_end() {
        // A matching middle step is not final: the workflow goes on.
        assert!(!step_is_final(Verdict::Match, 0, 5));
        assert!(!step_is_final(Verdict::Match, 3, 5));
        // The last step is, match or not — that is how a clean sequence scores
        // exactly one `Match` rather than one per step.
        assert!(step_is_final(Verdict::Match, 4, 5));
        assert!(step_is_final(Verdict::Match, 0, 1));
        // Every way of not matching stops it, including the buckets nothing can
        // score: a step whose oracle timed out has no premise to hand forward
        // either.
        for v in [
            Verdict::Unsupported,
            Verdict::StdoutDiff,
            Verdict::ExitDiff,
            Verdict::StateDiff,
            Verdict::StderrDiff,
            Verdict::Crash,
            Verdict::Hang,
            Verdict::ZvcsNondeterministic,
            Verdict::Nondeterministic,
            Verdict::StockTimeout,
        ] {
            assert!(step_is_final(v, 1, 5), "{} must end the sequence", v.label());
        }
    }

    /// The report asks the outcome for its name, and the outcome answers with
    /// the step id when it has one.
    ///
    /// Getting this wrong is silent: a sequence step printed under its bare argv
    /// reads as an ordinary failure against a pristine repository, which is the
    /// one thing it is not, and a reader would try to reproduce it by running
    /// that one command.
    #[test]
    fn an_outcome_reports_the_step_id_when_it_has_one() {
        let plain = Outcome {
            case: Case::new("status", &["status"], Shape::Dirty),
            step: None,
            verdict: Verdict::StdoutDiff,
            stock_stdout: String::new(),
            zvcs_stdout: String::new(),
            stock_stderr: String::new(),
            zvcs_stderr: String::new(),
            stock_code: Some(0),
            zvcs_code: Some(0),
            stock_state: String::new(),
            zvcs_state: String::new(),
            zvcs_repeat: None,
        };
        assert_eq!(plain.id(), "dirty::status::status");

        let seq = workflow();
        let stepped = Outcome {
            case: seq.step_case(1),
            step: Some(StepRef {
                index: 2,
                total: seq.len(),
                id: seq.step_id(1),
                script: seq.script(1),
            }),
            ..plain
        };
        assert_eq!(stepped.id(), seq.step_id(1));
        // The script marks the reported step and nothing else, so the failure
        // block says where the run stopped without a reader counting lines.
        let script = seq.script(1);
        assert_eq!(script[1], "-> 2  cherry-pick theirs");
        assert_eq!(script[0], "   1  merge --abort");
        assert_eq!(script.iter().filter(|l| l.starts_with("->")).count(), 1);
    }
}
