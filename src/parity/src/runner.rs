//! The differential runner: one case, run twice, compared five ways.
//!
//! A case is judged on stdout bytes, exit code, the *resulting repository
//! state*, and whether **stock git still reads the two finished repositories the
//! same way**. The last two are what make this more than an output diff. A
//! command can print the right thing and still corrupt the index; probing the
//! post-state with stock git in both repos catches that.
//!
//! stderr is deliberately not byte-compared. Error prose is not a compatibility
//! surface and zvcs is specified to be terser than git. It is still recorded so
//! a human can read it, and whether the command *errored at all* is compared
//! via the exit code.
//!
//! # A state probe re-derives; an interop probe reads
//!
//! Every probe in [`probe_state`] asks stock git what the repository *means* and
//! recomputes the answer from scratch — `status` walks the worktree, `ls-files
//! --stage` prints the entries, `cat-file --batch-all-objects` enumerates the
//! objects. Each of those answers correctly from a repository git would never
//! have written, because everything a repository holds beyond its logical
//! content is an accelerator or a record the logical view can rebuild without
//! it: the index cache-tree, the untracked cache, the split index, pack indexes,
//! bitmaps, the multi-pack-index. A logical probe is blind to every one of them
//! by construction, and stays blind however many more are added.
//!
//! That blindness shipped a defect. `zvcs add` destroyed the index cache-tree —
//! a 168-byte index where stock writes 229 — so every stock `write-tree`,
//! `commit` or `status` afterwards had to rebuild it. Stdout, exit code, refs,
//! objects, index entries and config all agreed, and the case scored `Match`. It
//! was found by hand (`30c23c0799`).
//!
//! [`probe_interop`] closes that class by asking stock git two questions it is
//! never otherwise asked: *is this valid* (`fsck --strict`) and *can you use it
//! as written, or must you repair it first* (`write-tree`, with every write
//! redirected out of the repository). A disagreement there is
//! [`Verdict::InteropDiff`] and gets its own column, because "the port wrote
//! something stock reads differently" is a different finding from "the port
//! printed the wrong thing" and from "the repository's contents differ".
//!
//! It costs three invocations per side — two stock git, one of the binary under
//! test for the mirror direction — and only on a case that wrote under the git
//! directory. See [`git_fingerprint`] for the gate and [`probe_interop`] for the
//! full cost argument.
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
//!
//! # A second oracle: telling a port defect from a version difference
//!
//! Everything above compares the port with **one** git and treats that git's
//! answer as the answer. That is a real limit, not a simplification. On a machine
//! with `/usr/bin/git` at 2.50.1 and `/opt/homebrew/bin/git` at 2.55.0 the harness
//! picks the newer one and never asks the other, so a difference between the port
//! and 2.55.0 is reported identically whether the port is wrong or whether *git*
//! changed between the two releases and the port reproduces the older behaviour.
//! Both read as `stdout-diff` with a specific diff attached, and one of them is
//! an afternoon spent making code match a behaviour upstream deliberately moved.
//!
//! So when [`crate::stock::alt_git`] finds a second real git, a failing case is
//! run against it too and the three answers are classified together
//! ([`adjudicate`]):
//!
//!   * **the two gits agree, the port differs** — the strongest signal this
//!     harness can produce. Not one git's opinion: two independent releases say
//!     the port is wrong. The verdict it already earned stands, and the report
//!     says it was corroborated.
//!   * **the two gits disagree with each other, and the port reproduces the
//!     second one** — [`Verdict::VersionSkew`]. The port is not producing a wrong
//!     answer, it is producing an *older git's* answer, and naming that is the
//!     entire point of the dimension.
//!   * **the two gits disagree and the port matches neither** — the verdict
//!     stands, because no choice of target version makes the port right here. The
//!     disagreement is still recorded and listed, because it says the *expected
//!     value* on this case is version-dependent — which is exactly the shape of a
//!     curated expectation that was captured against the wrong git.
//!   * **the second oracle would not reproduce itself, or was killed by the case
//!     timeout** — inconclusive, and said so. A disagreement is corroborated by a
//!     second run of the second oracle before it is believed, because some values
//!     are re-rolled every run and two samples can agree by luck: the first
//!     version of this dimension reported `filter-branch`'s
//!     `(N seconds passed, remaining M predicted)` progress line as a version
//!     difference between 2.55.0 and 2.50.1. A dimension whose headline finding is
//!     manufactured by machine load is worse than no dimension. See
//!     [`alt_reproduced`].
//!
//! ## What it does to the denominator
//!
//! [`Verdict::VersionSkew`] is **inside** the parity denominator, counted as a
//! failure, exactly like [`Verdict::ZvcsNondeterministic`] and for the same
//! reason. The two alternatives are both worse:
//!
//! *Excluding it* — dropping the case from `Tally::scored` the way stock
//! non-determinism is dropped — would be an exclusion **the binary under test can
//! trigger**. Stock's exclusion is safe only because no port behaviour can reach
//! it; this one is reached by the port emitting a particular string, so a port
//! that reproduced 2.50 behaviour on its hardest cases would shrink its own
//! denominator and outscore a port that tried to reproduce 2.55 and missed. That
//! is the self-serving exclusion this crate exists to refuse.
//!
//! *Counting it as a match* would be worse still: the number would then mean "the
//! port agrees with some git somewhere", which is not a claim anybody wants and
//! degrades as the number of installed gits grows.
//!
//! So the headline number keeps its single meaning — *matches the git this port
//! targets*, the version `stock::git` already refuses to measure below — and the
//! version difference is made visible instead of being paid for: its own verdict,
//! its own counter, its own column, its own report line, and a listing of every
//! case where the two gits disagreed at all. A reader who wants the other number
//! is given it explicitly on a second line rather than having it folded silently
//! into the first.
//!
//! The consequence, stated rather than hidden: `parity` is *bit-identical* with
//! and without a second oracle. The dimension can move a case from one failure
//! bucket to another and can never move one into or out of the numerator or the
//! denominator. That is a property, not an accident — see
//! [`the_second_oracle_cannot_move_the_parity_number`].
//!
//! ## What it costs
//!
//! One extra invocation and one extra state probe, **only** on a case that
//! already failed against the primary oracle and whose verdict is one the second
//! oracle can speak to ([`alt_speaks_to`]) — plus a second one on the far smaller
//! set where the two gits actually disagreed and the disagreement has to be
//! corroborated ([`alt_reproduced`]). A run in which everything matches pays
//! nothing at all, which is the same gate the repeat uses and for the same
//! reason: runtime is not neutral, a harness people run less often measures less.
//! `--alt-git-every-case` lifts the gate, at one extra invocation per case, and
//! buys the one thing the gate cannot see — a case where the port matches 2.55.0
//! and 2.50.1 would have said something else. Every run prints which of the two
//! it paid for and what it cost.

use crate::env;
use crate::fixture::{Shape, Templates};
use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// One invocation to compare.
#[derive(Clone, Debug)]
pub struct Case {
    /// Subcommand, e.g. `rev-parse`. Used for per-command scoring.
    pub cmd: &'static str,
    /// Full argv after the binary name, including the subcommand.
    pub args: Vec<String>,
    /// Configuration the case runs under, each entry naming the **scope** it is
    /// delivered from, applied identically to both sides.
    ///
    /// The largest surface the harness could not reach. Git's behaviour is a
    /// function of its configuration at least as much as of its argv —
    /// `core.abbrev` decides how wide every id prints, `diff.renames` decides
    /// whether a rename is a rename, `status.showUntrackedFiles` decides what
    /// `status` even lists — and with no case able to set a key, every one of
    /// those defaults was the only value ever measured. A port that ignores a
    /// setting entirely scored exactly the same as one that honours it.
    ///
    /// **The scope used to be fixed at `-c`, and that was half a measurement.**
    /// The reasoning was that a case is one invocation against a pristine copy
    /// and cannot write a file first, so the command line is the one source
    /// needing no prior state. The premise was wrong: the runner *builds* the
    /// pristine copy, so it can put a file in it before the invocation runs, and
    /// [`install_config`] now does — see [`ConfigScope`] for the three classes of
    /// behaviour that only a non-`-c` scope can reach (precedence, file parsing,
    /// and the sources that are gated on another key).
    ///
    /// Order is significant and preserved. Two entries naming the same key in
    /// the same file scope are written as two stanzas, in this order, which is
    /// how "the last value in a file wins" becomes observable; two entries in
    /// different scopes are how precedence between scopes becomes observable.
    ///
    /// A key that makes stock git **die** is kept, not filtered. Agreeing on the
    /// refusal is parity; excluding it would be measuring only the values git
    /// likes, which is the same error as scoring an unported command as a skip.
    ///
    /// Values are literal, like every other argv token — nothing here is
    /// substituted, so a value must not name an absolute path for the same
    /// reason [`Case::env`] values may not.
    pub config: Vec<ConfigEntry>,
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

// ---------------------------------------------------------------------------
// Configuration scopes
// ---------------------------------------------------------------------------

/// Where a configuration setting is delivered from.
///
/// Git does not have *a* configuration; it has a sequence of sources read in a
/// fixed order (`config.c:do_git_config_sequence`), and each one is a different
/// parser reached by a different code path. Until this enum existed every case
/// delivered every key through `-c`, which is the *last* source in that sequence
/// and the only one that is not a file. Three classes of behaviour were
/// therefore structurally unmeasurable, and none of them is exotic:
///
///  * **Precedence.** A port that reads a key correctly from `-c` and never
///    looks in `.git/config` scores exactly the same as one that reads both, and
///    a port that has the order backwards — repository beating the command
///    line — scores the same again. Precedence is only observable when one key
///    is set twice, in two scopes, to two values.
///  * **File parsing.** `-c` hands the parser an already-split `key=value`. A
///    file has section headers, subsections, comments, quoting, escapes and
///    continuation, and it can be malformed in ways no command line can express.
///    `fatal: bad config line 12 in file .git/config` names a line number, and a
///    line number is a fact only a file has. It also has *order*: two settings of
///    one key in one file is the last-value-wins rule, which `-c` has its own
///    separate implementation of.
///  * **The gated sources.** `.git/config.worktree` is not read at all unless
///    `extensions.worktreeConfig` is true in `.git/config`, and `.gitmodules` is
///    read for `submodule.*` and nothing else. Reaching either means writing two
///    files, which no single `-c` can do.
///
/// # What is *not* here, and why
///
/// `$HOME/.gitconfig` and `/etc/gitconfig` — the paths a human means by "global"
/// and "system" — stay unreachable on purpose. [`env::harden`] pins
/// `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` to `/dev/null` and sets
/// `GIT_CONFIG_NOSYSTEM`, precisely so the machine's own files cannot leak into
/// the comparison, and [`env::is_pinned`] forbids a *case* from re-pointing them
/// for the reason `corpus/shape_reach.rs` states: a case that could aim
/// `GIT_CONFIG_GLOBAL` anywhere could aim it at the user's real file.
///
/// [`ConfigScope::Global`] and [`ConfigScope::System`] therefore mean "a file at
/// that *position in the precedence order*", delivered by the runner re-pointing
/// the variable at a path it computed itself from the side's own fixture root
/// ([`scope_file`]). The case never names the path and cannot; the file lives
/// inside a copy that is deleted when the case ends; and the pin is left at
/// `/dev/null` for every case that does not draw the scope, so no existing case
/// changes by a byte. What is measured is the layering, which is the part a port
/// gets wrong — not the filesystem location, which is the part `harden` exists to
/// keep out.
///
/// # Order
///
/// The variants are declared **lowest precedence first**, and
/// [`config_scope_declaration_order_is_gits_precedence_order`] pins that against
/// the order measured from stock git 2.55.0 rather than against this comment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfigScope {
    /// `.gitmodules` in the worktree root.
    ///
    /// Outside the layered sequence rather than at the bottom of it:
    /// `submodule-config.c` reads this file on its own, for `submodule.*` keys
    /// only, and `.git/config` overrides it whatever the rest of the order says.
    /// It is declared first because "not in the order" has to sit somewhere and
    /// the bottom is the least misleading place for it.
    Modules,
    /// `$GIT_CONFIG_SYSTEM`, pointed at a file inside this side's own fixture.
    /// Drawing it also clears `GIT_CONFIG_NOSYSTEM`, which otherwise suppresses
    /// the whole scope — verified against stock 2.55.0, which reads the file only
    /// once the variable is gone.
    System,
    /// `$GIT_CONFIG_GLOBAL`, pointed at a file inside this side's own fixture.
    Global,
    /// `.git/config`.
    Repo,
    /// `.git/config.worktree`, inert until `extensions.worktreeConfig` is set —
    /// so drawing this scope writes *two* files, and a port that honours the
    /// file without checking the gate diverges on a case that writes only one.
    Worktree,
    /// `GIT_CONFIG_COUNT` / `GIT_CONFIG_KEY_<n>` / `GIT_CONFIG_VALUE_<n>`.
    Env,
    /// `-c key=value`: the original scope, and until now the only one.
    CommandLine,
}

impl ConfigScope {
    /// The scopes git layers in a defined order, lowest first. `Modules` is
    /// absent because it is not part of that sequence; see the variant.
    pub const ORDERED: &'static [ConfigScope] = &[
        ConfigScope::System,
        ConfigScope::Global,
        ConfigScope::Repo,
        ConfigScope::Worktree,
        ConfigScope::Env,
        ConfigScope::CommandLine,
    ];

    /// Every scope, for a sampler that wants to draw one.
    pub const ALL: &'static [ConfigScope] = &[
        ConfigScope::Modules,
        ConfigScope::System,
        ConfigScope::Global,
        ConfigScope::Repo,
        ConfigScope::Worktree,
        ConfigScope::Env,
        ConfigScope::CommandLine,
    ];

    /// The scopes delivered by writing a file into the fixture. The rest are
    /// delivered on the command line or through the environment and cost no I/O.
    pub const FILES: &'static [ConfigScope] = &[
        ConfigScope::Modules,
        ConfigScope::System,
        ConfigScope::Global,
        ConfigScope::Repo,
        ConfigScope::Worktree,
    ];

    /// Short slug rendered into [`Case::id`]. Lower-case and free of spaces, so
    /// the id stays one whitespace-delimited token per entry.
    pub fn name(self) -> &'static str {
        match self {
            ConfigScope::Modules => "modules",
            ConfigScope::System => "system",
            ConfigScope::Global => "global",
            ConfigScope::Repo => "repo",
            ConfigScope::Worktree => "worktree",
            ConfigScope::Env => "env",
            ConfigScope::CommandLine => "cmdline",
        }
    }

    /// Whether this scope is delivered by writing a file.
    pub fn is_file(self) -> bool {
        Self::FILES.contains(&self)
    }
}

/// One configuration fact a case carries: a setting, or a raw line in a file.
///
/// Raw lines are the whole reason this is not a `(scope, key, value)` triple.
/// A file scope can hold content that is not a setting at all — a missing
/// section header, an unterminated quote, a stray `]`, a key with no value — and
/// that content is exactly what produces git's line-numbered
/// `bad config line %d in file %s`, a diagnostic `-c` has no way to reach.
/// Modelling it as a setting with a funny value would not work: the malformed
/// part is the *line*, not the value, and half of these forms have no key to
/// hang a value on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigEntry {
    pub scope: ConfigScope,
    /// The `section.key` this entry sets, or `None` when the entry is a raw line
    /// written verbatim into the scope's file.
    pub key: Option<String>,
    /// The value for a keyed entry; the whole line, newlines and all, for a raw
    /// one.
    pub value: String,
}

impl ConfigEntry {
    /// `key = value`, delivered through `scope`.
    pub fn set(scope: ConfigScope, key: impl Into<String>, value: impl Into<String>) -> Self {
        Self { scope, key: Some(key.into()), value: value.into() }
    }

    /// A line written verbatim into `scope`'s file. Only meaningful for a file
    /// scope — a command line and an environment pair have no lines — and
    /// [`install_config`] is the only thing that reads it, so a raw entry in a
    /// non-file scope is silently inert rather than an error the sampler has to
    /// avoid at run time. [`crate::fuzz`] asserts it never produces one.
    pub fn raw(scope: ConfigScope, line: impl Into<String>) -> Self {
        Self { scope, key: None, value: line.into() }
    }

    /// True when this entry is a raw file line rather than a setting.
    pub fn is_raw(&self) -> bool {
        self.key.is_none()
    }

    /// How the entry reads in [`Case::id`]: `<scope>:<key>=<value>` for a
    /// setting, `<scope>:~<escaped line>` for a raw one.
    ///
    /// The raw form is escaped because a line can carry a newline or a tab, and
    /// an id with a newline in it stops being one line of a report. `~` marks it
    /// as a line rather than a setting so a reader who copies it back knows to
    /// paste it rather than to run `git config`.
    pub fn render(&self) -> String {
        match &self.key {
            Some(k) => format!("{}:{k}={}", self.scope.name(), self.value),
            None => format!("{}:~{}", self.scope.name(), self.value.escape_default()),
        }
    }
}

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
    ///
    /// Still spelled as bare pairs, and still command-line scoped, because that
    /// is what every curated call site means when it writes one: the setting is
    /// part of the invocation being described. A case that wants a scope says so
    /// with [`Case::with_scoped_config`]. Keeping this signature is also what
    /// makes the scope widening free of a corpus rewrite — no existing case's
    /// [`Case::id`] moves by a byte, so no existing failure is renamed.
    pub fn with_config(self, config: &[(&str, &str)]) -> Self {
        Self {
            config: config
                .iter()
                .map(|(k, v)| ConfigEntry::set(ConfigScope::CommandLine, *k, *v))
                .collect(),
            ..self
        }
    }

    /// Run this case with configuration delivered from the scopes each entry
    /// names. Order is preserved: within one file scope it is the order the
    /// stanzas are written in, which is what decides the last-wins outcome.
    pub fn with_scoped_config(self, config: Vec<ConfigEntry>) -> Self {
        Self { config, ..self }
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
        // Only the command-line scope renders here. Everything else is a file
        // written into the fixture or a variable set on the child, and rendering
        // it as `-c` would deliver the setting through the one scope the entry
        // said it was not using — which is the whole thing being measured.
        for entry in &self.config {
            if entry.scope != ConfigScope::CommandLine {
                continue;
            }
            if let Some(key) = &entry.key {
                argv.push("-c".to_string());
                argv.push(format!("{key}={}", entry.value));
            }
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
    /// The grammar `[!]<shape>::<cmd>::<argv>[::config[…]][::cwd[…]][::env[…]][::stdin[…]]`
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
        // Everything the argv segment could not show: a setting that came from a
        // file or from the environment is invisible on the command line, and an
        // id that showed only the `-c` half would name a case a reader could not
        // reproduce. Rendered as `<scope>:<key>=<value>` per entry, in delivery
        // order, so the segment is a recipe — write these lines into that scope's
        // file, in this order, and the case is back.
        let scoped: Vec<String> = self
            .config
            .iter()
            .filter(|e| e.scope != ConfigScope::CommandLine)
            .map(ConfigEntry::render)
            .collect();
        if !scoped.is_empty() {
            id.push_str(&format!("::config[{}]", scoped.join(" ")));
        }
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

    /// Run every step under configuration delivered from the scopes each entry
    /// names.
    ///
    /// The file-scoped half is installed **once**, into each side's copy, before
    /// the first step — see [`run_sequence`]. It is deliberately not re-written
    /// between steps: a workflow whose own steps edit `.git/config` (and several
    /// do) would otherwise have its edit silently reverted before the step that
    /// was supposed to read it, and the sequence would be measuring the harness
    /// rather than the port.
    pub fn with_scoped_config(self, config: Vec<ConfigEntry>) -> Self {
        self.map_envelope(|c| c.with_scoped_config(config))
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
    /// [!]<shape>::<cmd>::seq[<name>]::step<i>/<n>::<argv>[::config[…]][::cwd[…]][::env[…]][::stdin[…]]::script[<argv1> | <argv2> | …]
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
    /// stdout, exit code and post-state all agree, and stock git still reads the
    /// two repositories differently: [`probe_interop`] — git's own validator and
    /// git's own use of the index — disagreed across the two sides.
    ///
    /// **Its own verdict because it is its own claim.** A stdout diff says the
    /// port printed the wrong thing; a state diff says the repository means
    /// something different; this says the repository *means the same thing* and
    /// is nevertheless not what git would have written. The cache-tree defect
    /// (`30c23c0799`) is the worked example: `zvcs add` stripped the `TREE`
    /// extension stock had written, so every subsequent stock `write-tree`,
    /// `commit` or `status` had to rebuild it. Every existing comparison passed —
    /// identical stdout, identical exit code, identical refs, objects, index
    /// entries and config — because the port read its own index back perfectly
    /// and every state probe re-derives its answer from the entries rather than
    /// from the extension. Filing that under `state-diff` would have told a
    /// reader the repository's contents differed, which is exactly what they do
    /// not.
    ///
    /// **Counted as a failure, inside the parity denominator.** An index stock
    /// has to repair before it can use it is a defect on the machine this port
    /// exists for, where sixteen agents and stock git share one worktree.
    InteropDiff,
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
    /// The two stock gits do not agree with each other on this invocation, and
    /// the port reproduces the **second** one — the one the report is not
    /// measured against. Only reachable when a second oracle was resolved; see
    /// [`crate::stock::alt_git`] and [`adjudicate`].
    ///
    /// **Its own verdict because it is its own claim.** Every other failure
    /// bucket says the port produced something no git produced. This one says
    /// the port produced something a real git produces, and that git is not the
    /// one this port targets. The two are different findings and cost different
    /// afternoons: the first is a bug to fix, the second is a decision about
    /// which release to track, and a reader who cannot tell them apart will
    /// "fix" code to match behaviour upstream changed on purpose.
    ///
    /// **Counted as a failure, inside the parity denominator, never excluded.**
    /// The argument is [`Verdict::ZvcsNondeterministic`]'s, and it is the reason
    /// this is not the exclusion it superficially looks like it should be: the
    /// condition is half a property of the oracles (they disagree) and half a
    /// property of the binary under test (it matched the older one), and the
    /// *conjunction* is therefore something the port can trigger. An exclusion a
    /// port can trigger pays a port for triggering it — reproduce 2.50 on the
    /// hard cases and the denominator shrinks. The number stays defined as
    /// "matches the git this port targets", which is the version
    /// [`crate::stock::git`] already refuses to measure below.
    ///
    /// The report prints the forgiving number too, on a line of its own, so
    /// nothing is hidden by the choice — only kept out of the headline.
    VersionSkew,
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
            Verdict::InteropDiff => "INTEROP-DIFF",
            Verdict::StderrDiff => "STDERR-DIFF",
            Verdict::Crash => "CRASH",
            Verdict::Hang => "HANG",
            Verdict::ZvcsNondeterministic => "ZVCS-NONDETERMINISTIC",
            Verdict::VersionSkew => "VERSION-SKEW",
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
            // `VersionSkew` is deliberately **not** here. This listing is headed
            // "unmeasurable + flaky", and a bucket that is neither would be
            // filed under a heading that misdescribes it — the exact error the
            // separate verdict exists to avoid. It gets its own section, printed
            // only by a run that had a second oracle, which is also what keeps a
            // one-git machine's report byte-identical to what it was.
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
    /// What stock git — and the binary under test — made of each side's
    /// *finished* repository; see [`probe_interop`]. Both carry the "not probed"
    /// marker on a case that left the git directory untouched, which is how they
    /// compare equal for free.
    pub stock_interop: String,
    pub zvcs_interop: String,
    /// Whether the interop probe actually ran, as opposed to being skipped
    /// because neither side wrote anything under the git directory.
    ///
    /// Reported rather than inferred: the whole cost argument for this dimension
    /// is that mutating cases are a minority, and a claim about a minority that
    /// nothing counts is a claim nobody can check. `report.rs` prints the
    /// fraction beside the parity line, so every run states its own price.
    pub interop_probed: bool,
    /// The second zvcs run, present exactly when one was taken — that is, on a
    /// case that failed and whose stock side reproduced itself.
    ///
    /// Kept even when it *agreed* with the first run, because "this failure was
    /// reproduced" is the fact a reader most wants attached to a failure they
    /// are about to spend an afternoon on, and it has already been paid for.
    pub zvcs_repeat: Option<Repeat>,
    /// What the **second** oracle said, present exactly when one was resolved and
    /// asked — a failing case whose verdict [`alt_speaks_to`], or any case at all
    /// under `--alt-git-every-case`.
    ///
    /// `None` covers both "the machine has one git" and "this case did not need a
    /// second opinion", and the report distinguishes them from the run-level
    /// counts rather than from this field: a per-case `None` cannot say which,
    /// and the difference matters only in aggregate.
    pub alt: Option<AltRun>,
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
    /// The interop digest of the repeat's own repository, present only when the
    /// repeat was asked for one — that is, only on a case whose verdict was
    /// [`Verdict::InteropDiff`]. Empty otherwise, and never compared then; see
    /// [`interop_disagreement`] for why that restriction is load-bearing rather
    /// than an optimization.
    pub interop: String,
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
    Interop,
}

impl Surface {
    pub fn name(self) -> &'static str {
        match self {
            Surface::Stdout => "stdout",
            Surface::State => "post-state",
            Surface::Interop => "interop probe",
        }
    }
}

/// The surface two *oracles* differed on.
///
/// A separate enum from [`Surface`] rather than two more variants on it, and the
/// difference is the point: [`Surface`] is the set of things a side is asked to
/// reproduce about *itself*, where exit code is deliberately excluded (widening
/// it would widen an exclusion — see [`repeat_disagreement`]). This is the set of
/// things two gits are compared on, where the exit code is a first-class answer
/// and the message is one too for a case that opted in. Sharing one enum would
/// have made "add a variant here" silently mean "widen an exclusion there".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OracleSurface {
    Exit,
    Stdout,
    State,
    Stderr,
}

impl OracleSurface {
    pub fn name(self) -> &'static str {
        match self {
            OracleSurface::Exit => "exit code",
            OracleSurface::Stdout => "stdout",
            OracleSurface::State => "post-state",
            OracleSurface::Stderr => "stderr",
        }
    }
}

/// What the second oracle turned a failing case into, once its answer has been
/// compared with both the primary oracle's and the port's.
///
/// Four states, and none of them collapses into another. In particular
/// [`AltFinding::Inconclusive`] is not folded into [`AltFinding::GitsAgree`]:
/// "the second git was killed before it answered" would then read as "the second
/// git corroborates the defect", which is a claim nothing measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AltFinding {
    /// The two gits produced the same answer. The port's difference is the
    /// port's — corroborated by an independent release, which is the strongest
    /// statement this harness can make about a defect.
    GitsAgree,
    /// The two gits produced different answers and the port reproduced the
    /// second one's. The port is tracking that git's behaviour, not producing a
    /// wrong one. Becomes [`Verdict::VersionSkew`].
    PortTracksAlt,
    /// The two gits produced different answers and the port reproduced neither.
    /// The verdict stands — no choice of target version makes the port right —
    /// but the case is one where "parity" has no single answer, so it is listed.
    GitsDisagree,
    /// Nothing could be concluded: the second oracle hit the case timeout, or it
    /// reported a disagreement it then failed to reproduce.
    ///
    /// One bucket for both because they are one claim — *this sample is not
    /// evidence* — and splitting it would invite a reader to treat one of them as
    /// a weak finding. A killed run's partial output differs from a complete one
    /// every time; an unreproducible one differs from itself. See
    /// [`alt_reproduced`] for the `filter-branch` wall-clock line that put the
    /// second case here.
    Inconclusive,
}

impl AltFinding {
    /// Whether the two gits disagreed with each other at all — the listing this
    /// dimension exists to produce, independent of what the port did.
    pub fn gits_disagreed(self) -> bool {
        matches!(self, AltFinding::PortTracksAlt | AltFinding::GitsDisagree)
    }

    pub fn label(self) -> &'static str {
        match self {
            AltFinding::GitsAgree => "gits-agree",
            AltFinding::PortTracksAlt => "port-tracks-alt",
            AltFinding::GitsDisagree => "gits-disagree",
            AltFinding::Inconclusive => "inconclusive",
        }
    }
}

/// One run of the **second** oracle, reduced to the surfaces the three-way
/// comparison reads, plus what that comparison concluded.
///
/// stderr is captured here though the primary comparison does not byte-compare
/// it, because a case that *opted into* stderr comparison can fail on it, and a
/// second oracle that could not speak to the surface the verdict was about would
/// have to be skipped for exactly the cases where prose is being compared. The
/// interop digest is **not** captured, deliberately: it would cost three more
/// invocations per adjudicated case, and it is produced by asking the *primary*
/// stock git to read a finished repository — pointing an older git at it answers
/// a question about the reader, not about what the port wrote. So
/// [`Verdict::InteropDiff`] is simply not a verdict the second oracle is asked
/// about; see [`alt_speaks_to`].
#[derive(Clone, Debug)]
pub struct AltRun {
    /// Which git this was, so every printed line can name it. Carried per
    /// outcome rather than looked up at print time so a report can never
    /// attribute one git's bytes to the other's version.
    pub version: (u32, u32, u32),
    pub timed_out: bool,
    pub code: Option<i32>,
    /// Normalized exactly like the other two sides', against this run's own repo.
    pub stdout: String,
    pub stderr: String,
    pub state: String,
    pub finding: AltFinding,
    /// The first surface the two gits differed on, `None` when they agreed (or
    /// when nothing could be concluded). Recorded rather than recomputed by the
    /// report for the same reason [`Repeat::disagreement`] is: the verdict and
    /// the printed explanation have to come from one comparison, or a report can
    /// say "the two gits differ on stdout" about a case classified on its exit
    /// code.
    pub surface: Option<OracleSurface>,
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

// ---------------------------------------------------------------------------
// Installing a case's configuration into a fixture copy
// ---------------------------------------------------------------------------

/// This side's git directory: `.git` when it is one, the fixture root otherwise.
///
/// Every shape `fixture::build` produces has a real `.git` directory at its
/// root, so the fallback is defensive rather than load-bearing — but a shape
/// added tomorrow that is bare at the root would otherwise write its
/// configuration into a directory git never reads, and a premise git never read
/// is a case that measures nothing while looking like it measured something.
pub(crate) fn git_dir(repo: &Path) -> PathBuf {
    let dot = repo.join(".git");
    if dot.is_dir() {
        dot
    } else {
        repo.to_path_buf()
    }
}

/// The file a file-backed scope is delivered through, always inside `repo`.
///
/// The paths are computed from the side's own fixture root and from nothing
/// else. That is the property that keeps [`ConfigScope::Global`] and
/// [`ConfigScope::System`] compatible with [`env::harden`]'s pins: a case says
/// only *which scope*, never *which file*, so there is no way for a case to aim
/// either variable at the machine's real configuration —
/// [`scope_files_stay_inside_the_fixture`] asserts it.
///
/// The two synthetic files live under the git directory rather than beside the
/// worktree on purpose. A file in the worktree would show up as `?? …` in every
/// `status` the case runs and in the state probe, which is a difference the case
/// did not ask for and which would drown the setting it did ask for.
pub(crate) fn scope_file(repo: &Path, scope: ConfigScope) -> Option<PathBuf> {
    let git = git_dir(repo);
    Some(match scope {
        ConfigScope::Repo => git.join("config"),
        ConfigScope::Worktree => git.join("config.worktree"),
        ConfigScope::Global => git.join("parity-global.config"),
        ConfigScope::System => git.join("parity-system.config"),
        ConfigScope::Modules => repo.join(".gitmodules"),
        ConfigScope::Env | ConfigScope::CommandLine => return None,
    })
}

/// Split `section.key` / `section.subsection.key` the way git's own config
/// writer does: the section is everything up to the first dot, the key is
/// everything after the last, and whatever is between them is the subsection.
///
/// A subsection may itself contain dots (`branch.feature.x.merge` is branch
/// `feature.x`), which is exactly why the split is first-dot/last-dot and not
/// a three-way `split('.')`.
pub(crate) fn split_config_key(key: &str) -> Option<(&str, Option<&str>, &str)> {
    let (section, rest) = key.split_once('.')?;
    if section.is_empty() || rest.is_empty() {
        return None;
    }
    Some(match rest.rsplit_once('.') {
        Some((sub, name)) => (section, Some(sub), name),
        None => (section, None, rest),
    })
}

/// Quote a value so the file delivers **the same string** `-c` would.
///
/// Unquoted, git's config reader strips surrounding whitespace, stops at a `#`
/// or `;`, and treats an absent value as boolean true. Every one of those would
/// silently change the value under test, so the scope — the one variable — would
/// no longer be the only difference from the `-c` case. Quoting removes all
/// three: inside quotes whitespace and comment characters are literal, and `""`
/// is the empty string rather than an implicit true.
///
/// The escapes are exactly the five `config.c:parse_value` accepts
/// (`\"`, `\\`, `\n`, `\t`, `\b`); anything else after a backslash is a parse
/// error, which is why a literal backslash has to become `\\` rather than being
/// passed through. Reaching that parse error is a job for a raw line, where it
/// is deliberate and visible in the id.
pub(crate) fn quote_config_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The bytes one entry contributes to its scope's file.
///
/// One `[section]` header per setting, even when consecutive settings share a
/// section. Git accepts a section repeated any number of times, and repeating it
/// is what keeps a *repeated key* readable as two independent stanzas — the
/// last-value-wins premise this whole ordering exists to test. Merging them into
/// one header would work too and would be one line shorter to read; it would also
/// make a two-entry draw and a one-entry draw look the same in the file, which is
/// the distinction the case is about.
///
/// A key that is not `section.key` at all cannot be written as a stanza, so it is
/// emitted as a bare line — which is what git would make of it anyway, and which
/// its own parser then rejects with the diagnostic that is the point.
pub(crate) fn render_config_entry(entry: &ConfigEntry) -> String {
    let Some(key) = &entry.key else {
        return format!("{}\n", entry.value);
    };
    match split_config_key(key) {
        Some((section, sub, name)) => {
            let header = match sub {
                Some(sub) => format!("[{section} \"{}\"]", sub.replace('\\', "\\\\").replace('"', "\\\"")),
                None => format!("[{section}]"),
            };
            format!("{header}\n\t{name} = {}\n", quote_config_value(&entry.value))
        }
        None => format!("{key} = {}\n", quote_config_value(&entry.value)),
    }
}

/// Append `text` to `path`, creating it if it does not exist.
fn append_file(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {} to install case configuration", path.display()))?;
    f.write_all(text.as_bytes())
        .with_context(|| format!("writing case configuration to {}", path.display()))
}

/// Write a case's file-scoped configuration into one already-instantiated
/// fixture copy.
///
/// # Cost, and why this is the cheap shape
///
/// One `open`/`write`/`close` per **file scope actually drawn**, per side, per
/// case — at most five small appends into a directory tree the runner has just
/// created and will delete when the case ends. No extra child process, no extra
/// fixture template, no extra comparison, and nothing at all for the cases that
/// draw no file scope, which is most of them.
///
/// The two obvious alternatives are both far more expensive, and the first is
/// also *wrong*:
///
///  * **Run `git config --file …` to install each key.** That is one child
///    process per key per side, doubling the process count of a config-heavy
///    case — and on the zvcs side it would be the *implementation under test*
///    writing the premise. A port with a broken config writer would corrupt its
///    own premise and the case would then measure the writer twice instead of
///    measuring the key once, which is the one thing a differential harness must
///    never do. Writing the bytes here means both sides start from a file this
///    crate produced, byte for byte.
///  * **A fixture template per scope.** Twenty-two shapes times the scope
///    combinations, built at start-up, for a premise that is four lines of text.
///
/// Ordering inside a file is the entry order in `config`, filtered to that
/// scope — that is what makes "the last value wins" observable.
///
/// [`ConfigScope::Worktree`] writes twice: the gate
/// (`extensions.worktreeConfig = true` in `.git/config`) and then the file
/// itself. Without the gate git ignores `.git/config.worktree` entirely, so a
/// case that drew the scope would have measured nothing while looking like it
/// measured a setting. Verified against stock 2.55.0, which reads the file with
/// the gate set and `core.repositoryFormatVersion` still 0 — the extension is
/// honoured regardless of the format version, so no version bump is done here and
/// the shape stays the shape every other case sees.
pub fn install_config(repo: &Path, entries: &[ConfigEntry]) -> Result<()> {
    if !entries.iter().any(|e| e.scope.is_file()) {
        return Ok(());
    }
    if entries.iter().any(|e| e.scope == ConfigScope::Worktree) {
        append_file(&git_dir(repo).join("config"), "[extensions]\n\tworktreeConfig = true\n")?;
    }
    for scope in ConfigScope::FILES {
        let text: String = entries
            .iter()
            .filter(|e| e.scope == *scope)
            .map(render_config_entry)
            .collect();
        if text.is_empty() {
            continue;
        }
        let path = scope_file(repo, *scope).expect("a file scope has a file");
        append_file(&path, &text)?;
    }
    Ok(())
}

/// What a case's non-command-line configuration looks like on disk and in the
/// environment, as `(where, what)` pairs for the failure block.
///
/// The id already carries every entry, and that is what makes a failure
/// *reproducible*; this is what makes it *readable*. A reader looking at
/// `repo:core.abbrev=4 repo:core.abbrev=auto` has to reconstruct two stanzas in
/// their head to see that the second one wins, while the rendered file says it.
/// Paths are relative to the fixture root, because the absolute one names a
/// temporary directory that no longer exists by the time anybody reads the
/// report.
pub fn config_premise(entries: &[ConfigEntry]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if entries.iter().any(|e| e.scope == ConfigScope::Worktree) {
        out.push((".git/config".to_string(), "[extensions]\n\tworktreeConfig = true\n".to_string()));
    }
    for scope in ConfigScope::FILES {
        let text: String = entries
            .iter()
            .filter(|e| e.scope == *scope)
            .map(render_config_entry)
            .collect();
        if text.is_empty() {
            continue;
        }
        let where_ = match scope {
            ConfigScope::Repo => ".git/config",
            ConfigScope::Worktree => ".git/config.worktree",
            ConfigScope::Global => "$GIT_CONFIG_GLOBAL → .git/parity-global.config",
            ConfigScope::System => "$GIT_CONFIG_SYSTEM → .git/parity-system.config",
            ConfigScope::Modules => ".gitmodules",
            _ => unreachable!("ConfigScope::FILES holds only file scopes"),
        };
        out.push((where_.to_string(), text));
    }
    let env: Vec<&ConfigEntry> =
        entries.iter().filter(|e| e.scope == ConfigScope::Env && !e.is_raw()).collect();
    if !env.is_empty() {
        let mut text = format!("GIT_CONFIG_COUNT={}\n", env.len());
        for (i, e) in env.iter().enumerate() {
            text.push_str(&format!(
                "GIT_CONFIG_KEY_{i}={}\nGIT_CONFIG_VALUE_{i}={}\n",
                e.key.as_deref().unwrap_or_default(),
                e.value
            ));
        }
        out.push(("environment".to_string(), text));
    }
    out
}

/// Apply the environment half of a case's configuration: the two synthetic file
/// pins and the `GIT_CONFIG_KEY_<n>` pairs.
///
/// Split from [`apply_case_env`] because the two have opposite rules. A case's
/// *own* environment may only add variables `harden` left unset, and
/// [`env::is_pinned`] enforces that against the case. This function is the
/// **runner** re-pointing two of `harden`'s pins at paths it computed itself from
/// the fixture root — see [`ConfigScope`] for why that is not the leak
/// `is_pinned` exists to stop, and note that the re-point happens only for a case
/// that drew the scope, so every other case keeps `/dev/null` exactly as before.
///
/// `GIT_CONFIG_NOSYSTEM` is *removed* rather than set to `0`: git checks only
/// whether the variable exists (`config.c:git_config_system`), so `0` would
/// suppress the system scope just as `1` does, and the case would have written a
/// file nothing reads.
fn apply_config_env(cmd: &mut Command, repo: &Path, case: &Case) {
    let entries = &case.config;
    if entries.iter().any(|e| e.scope == ConfigScope::Global) {
        cmd.env("GIT_CONFIG_GLOBAL", scope_file(repo, ConfigScope::Global).expect("file scope"));
    }
    if entries.iter().any(|e| e.scope == ConfigScope::System) {
        cmd.env("GIT_CONFIG_SYSTEM", scope_file(repo, ConfigScope::System).expect("file scope"));
        cmd.env_remove("GIT_CONFIG_NOSYSTEM");
    }

    let env: Vec<&ConfigEntry> =
        entries.iter().filter(|e| e.scope == ConfigScope::Env && !e.is_raw()).collect();
    if env.is_empty() {
        return;
    }
    // A case that sets the same variables through `Case::env` — the curated
    // discovery cases do, and they predate this scope — would have its pairs
    // silently overwritten here, so the case would run under a configuration its
    // own id does not describe. Asserted rather than merged: merging would need
    // this function to parse the case's environment back into pairs, and a
    // corpus entry that wants both is a corpus bug with one obvious fix, which
    // `no_case_sets_the_env_config_scope_twice` catches at `cargo test` time.
    assert!(
        !case.env.iter().any(|(k, _)| k.starts_with("GIT_CONFIG_")),
        "case sets GIT_CONFIG_* through Case::env and through ConfigScope::Env at once"
    );
    cmd.env("GIT_CONFIG_COUNT", env.len().to_string());
    for (i, e) in env.iter().enumerate() {
        cmd.env(format!("GIT_CONFIG_KEY_{i}"), e.key.as_deref().unwrap_or_default());
        cmd.env(format!("GIT_CONFIG_VALUE_{i}"), &e.value);
    }
}

/// How long [`drain`] keeps reading a pipe whose write end nobody has closed.
///
/// Only reached when a process the case started is still holding the pipe open,
/// because a pipe every writer has closed answers EOF on the first read. That
/// makes this a bound on the pathological path and not a budget the ordinary one
/// spends: a case whose child has exited pays one non-blocking read.
const DRAIN_GRACE: Duration = Duration::from_millis(250);

/// Kill `child` and everything still in its process group, then reap it.
///
/// The group is the child's own — [`CommandExt::process_group`] gave it one at
/// spawn — so the negative pid reaches the helpers it started without ever
/// naming a process this harness did not create. The child is killed by pid as
/// well, because a group kill is not a guarantee: anything that called `setsid`
/// has left the group, and only the direct child is certain to still be in it.
///
/// Ordered kill-then-reap deliberately: `wait` frees the pid, and a pid that has
/// been freed may be reused, so signalling after the reap is signalling a number
/// rather than a process.
fn kill_group(child: &mut Child) {
    // SAFETY: `kill` is async-signal-safe and takes no memory from this process;
    // the pid is the child's, which is still unreaped here.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Read what a finished child left in a pipe, without waiting on writers it left
/// behind.
///
/// `read_to_end` was the obvious spelling and it deadlocked a full run. A pipe
/// answers EOF only when *every* write end is closed, and the child's descendants
/// inherit those write ends: a `git daemon` case exited, the daemon it had
/// already forked kept both ends open with no parent left to reap it, and the
/// worker parked in `read_to_end` for as long as that daemon lived. Nothing else
/// bounded it — the case timeout covers waiting for the child and had already
/// been paid — so one case stalled the whole run indefinitely. Observed on a
/// corpus run: the worker's stack sat in `run_side`, and `lsof` named an orphaned
/// `git-daemon` holding the other end of both pipes.
///
/// Non-blocking reads make that case terminate: EOF ends the loop the moment the
/// last writer closes, and `WouldBlock` is retried only until [`DRAIN_GRACE`]
/// runs out. The bytes a finished child wrote are already in the pipe buffer, so
/// nothing a case legitimately produced is lost to the deadline; what the
/// deadline gives up on is output that has not been written yet by a process the
/// case forked and abandoned, which is not a comparison either side can rely on.
fn drain<H: Read + AsRawFd>(mut h: H) -> Vec<u8> {
    // SAFETY: `fcntl` on a fd this function owns for the duration of the call.
    // A failure leaves the fd blocking, which is the behaviour that predates
    // this function — the loop below still terminates on EOF.
    unsafe {
        let flags = libc::fcntl(h.as_raw_fd(), libc::F_GETFL);
        if flags != -1 {
            libc::fcntl(h.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    let start = Instant::now();
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match h.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if start.elapsed() >= DRAIN_GRACE {
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(_) => break,
        }
    }
    out
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
    // After the case's own environment, because it re-points two of `harden`'s
    // pins and the case is not allowed to have touched them — see
    // `apply_config_env` for why the runner may and a case may not.
    apply_config_env(&mut cmd, repo, case);
    cmd.current_dir(&dir)
        .args(case.argv())
        // Closed stdin stays the default. A command that reads input it was not
        // given must still hit EOF rather than block, or the `Hang` verdict
        // stops meaning anything.
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own process group, so a verb that spawns helpers — a hook, a
    // transport, the `git` a porcelain runs for itself — leaves a tree the
    // timeout path can kill in one call instead of a child whose descendants
    // outlive it. See `kill_group`.
    cmd.process_group(0);
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
            kill_group(&mut child);
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    // Safe to join only now: the child has exited (or been killed), so a writer
    // still holding unwritten bytes gets EPIPE and returns instead of blocking.
    if let Some(w) = writer {
        let _ = w.join();
    }

    // Pipes are drained after exit, with a deadline — see `drain` for the case
    // that made the deadline necessary.
    let stdout = child.stdout.take().map(drain).unwrap_or_default();
    let stderr = child.stderr.take().map(drain).unwrap_or_default();

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
///
/// The command probes above ask git what the repository *means*; the sections
/// appended below read what is on disk, and each one was added because a class
/// of command was being scored on its report alone. In digest order:
/// [`probe_storage`] (object layout), [`probe_reflogs`], [`probe_rr_cache`],
/// [`probe_op_state`] (in-progress operations, and `shallow`),
/// [`probe_worktrees`] (a linked worktree's own HEAD, reflog and operation
/// state), [`probe_pack_headers`] (what a midx or bitmap claims to cover),
/// [`probe_worktree_content`] (the bytes a merge or a checkout actually wrote),
/// [`probe_index_meta`] (the index's version and extension chain),
/// [`probe_fetch_head`], [`probe_peer`] (the bare repository a push landed on),
/// [`probe_modules`] (a submodule's own git directory) and
/// [`probe_pack_contents`] (which objects went into which pack, everywhere).
/// Every one of them appends: no existing line moves, so no probe here can turn
/// a failing case into a passing one.
fn probe_state(repo: &Path, home: &Path) -> String {
    const PROBES: &[&[&str]] = &[
        &["status", "--porcelain=v1", "--untracked-files=all"],
        &["for-each-ref", "--format=%(refname) %(objecttype) %(objectname)"],
        &["rev-parse", "--abbrev-ref", "HEAD"],
        &["rev-parse", "HEAD"],
        // `-v` prefixes each entry with the tag `ls-files` computes from the
        // index flags — `H` cached, `S` skip-worktree, `h` assume-unchanged, `M`
        // unmerged. Those bits are invisible to `--stage`, which prints mode,
        // object id and stage number and nothing else, so `update-index
        // --skip-worktree` / `--assume-unchanged` (and every sparse-checkout
        // command that sets them) moved a bit no probe read. Measured on stock
        // 2.55.0 over one path: `S 100644 <oid> 0\tc.txt` after
        // `--skip-worktree`, `h …` after `--assume-unchanged`, `H …` otherwise
        // — including for a file deleted from the worktree, so the tag is a
        // function of the index and not of the filesystem underneath it.
        //
        // A column added to a line that was already compared: every existing
        // difference is still a difference, so this can only add.
        &["ls-files", "--stage", "-v"],
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
            Ok(out) if out.status.success() => decode_exact(out.stdout),
            Ok(_) => "<err>\n".to_string(),
            Err(_) => "<spawn-failed>\n".to_string(),
        };
        digest.push_str(&format!("# {}\n{}", probe.join(" "), rendered));
    }
    digest.push_str(&probe_storage(repo));
    digest.push_str(&probe_reflogs(repo));
    digest.push_str(&probe_rr_cache(repo));
    digest.push_str(&probe_op_state(repo));
    digest.push_str(&probe_worktrees(repo));
    digest.push_str(&probe_pack_headers(repo));
    digest.push_str(&probe_worktree_content(repo));
    digest.push_str(&probe_index_meta(repo));
    digest.push_str(&probe_fetch_head(repo));
    digest.push_str(&probe_peer(repo, home));
    digest.push_str(&probe_modules(repo, home));
    digest.push_str(&probe_pack_contents(repo));
    digest
}

// ---------------------------------------------------------------------------
// Interop: what stock git makes of the repository each side left behind
// ---------------------------------------------------------------------------

/// A cheap content-free fingerprint of the git directory, used only to decide
/// whether a case touched the repository at all.
///
/// `(relative path, length, mtime)` per file, in [`walk_files`]'s sorted order.
/// Pure `stat` calls — no child process, no file read, no hashing — because this
/// runs for **every** case on **both** sides, twice each, and the whole cost
/// argument for the interop dimension is that the expensive half is paid only
/// where the repository actually changed.
///
/// The git directory rather than the whole fixture, and that boundary is the
/// claim: every structure [`probe_interop`] inspects — the index and its
/// extensions, the object store, the refs, the reflogs — lives under it, so a
/// case that wrote nothing there cannot have written a structure for stock to
/// misread. A command that only rewrote worktree bytes has changed nothing stock
/// reads *as git*, and `probe_state`'s `status` probe already compares that.
///
/// The bounded blind spot, recorded rather than left to be discovered: a write
/// that lands the same byte length *and* the same modification timestamp is
/// invisible here. On APFS and ext4 the timestamp has nanosecond resolution and
/// a rewrite always moves it, so this is a theoretical hole rather than a
/// practical one — but it is a hole, and the safe direction is the one it errs
/// in for everything else: [`compare_in`] opens the gate when **either** side's
/// fingerprint moved, so a case where only the port wrote is still probed on
/// both sides and the two digests stay comparable.
fn git_fingerprint(repo: &Path) -> String {
    let mut out = String::new();
    for (rel, path) in walk_files(&git_dir(repo)) {
        let (len, mtime) = std::fs::symlink_metadata(&path)
            .map(|m| {
                let t = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos());
                (m.len(), t)
            })
            .unwrap_or((0, 0));
        out.push_str(&format!("{rel} {len} {mtime}\n"));
    }
    out
}

/// The marker both sides carry when the gate stayed shut, so an unprobed case
/// compares equal without either side having been asked anything.
const INTEROP_UNPROBED: &str = "# interop\n<not probed: neither side wrote under the git directory>\n";

/// Ask **stock git** whether it can still work with the repository this side left
/// behind, report what it had to do about it, and put the same question to the
/// binary under test so the mirror direction is measured too.
///
/// # The gap this closes
///
/// [`probe_state`] already runs stock git in both repositories, so "stock reads
/// the port's repository" is not by itself the new thing. What every probe up
/// there has in common is that it asks stock what the repository *means* and
/// re-derives the answer from scratch: `status` walks the worktree, `ls-files
/// --stage` prints the index entries, `cat-file --batch-all-objects` enumerates
/// the object set, `for-each-ref` reads the refs. Every one of those questions
/// can be answered correctly from a repository that git would never have
/// written, because everything a git repository holds *beyond* its logical
/// content is an accelerator or a record that the logical view can reconstruct
/// without: the index cache-tree, the untracked cache, the split index, pack
/// indexes and bitmaps, the multi-pack-index. A logical probe is blind to all of
/// them by construction, and it stays blind however many logical probes are
/// added.
///
/// That blindness was not theoretical. `zvcs add` destroyed the index
/// cache-tree — the port wrote a 168-byte index where stock writes 229 — and
/// every single comparison in this harness passed, because the port read its own
/// index back perfectly and no probe above ever asked about the extension. It
/// was found by hand (`30c23c0799`).
///
/// # The two questions, and why these two
///
/// Both are stock git, and both read a structure rather than re-deriving past
/// it:
///
///  * **`fsck --strict --no-progress --no-dangling` — git's own validator.**
///    Inflates and parses every object, checks tree entry ordering and mode
///    bytes, checks ref names, verifies pack index integrity, and — measured,
///    not assumed — checks the index's cache-tree: an index whose cache-tree
///    names an object that is not there gets `error: <oid>: invalid sha1 pointer
///    in cache-tree of .git/index` and exit 8 from stock 2.55.0, with no
///    `--cache` needed. Exit code, stdout and stderr are all folded in, because
///    fsck says almost everything it has to say on stderr.
///  * **`write-tree` — git's own *use* of the index.** The tree id it prints is
///    computed from the cache-tree where the cache-tree is valid, so a port that
///    writes a cache-tree disagreeing with its own index entries makes stock
///    print the wrong tree: verified by pointing an index's root cache-tree at a
///    real-but-wrong tree object, whereupon stock 2.55.0 printed that object's id
///    (`f741aa06…`) instead of the index's true tree (`9c05a71a…`) and `fsck`
///    said nothing. And where the cache-tree is *absent*, stock has to rebuild it
///    and writes the index back — which is the destroyed-cache-tree signal, and
///    the reason the probe reports the index's byte length before and after.
///    Verified on the worked example: stock's own index was left untouched at 261
///    bytes, while the stripped one went 176 → 261 and came out byte-identical to
///    stock's.
///
/// # Why the probe is not allowed to mutate, and how it manages that
///
/// `write-tree` writes: it creates tree objects and it rewrites the index it
/// read. Running it in the repository would corrupt the very thing being
/// measured — the object store the *next* probe reports, and, in a
/// [`Sequence`], the premise the next *step* runs against. Copying the whole
/// repository first would work and is what an earlier draft did; it costs a
/// recursive tree copy per side per mutating case, and for the `Packed` shape
/// that is real bytes.
///
/// So the writes are redirected instead, with git's own three variables:
/// `GIT_INDEX_FILE` at a copy of the index, `GIT_OBJECT_DIRECTORY` at an empty
/// scratch directory, and `GIT_ALTERNATE_OBJECT_DIRECTORIES` at the real object
/// store so every existing object is still readable. Cost: one file copy and one
/// `mkdir`. Verified: after the probe, nothing under the repository's `.git` had
/// been written, and the repaired index — the whole finding — was sitting in the
/// scratch copy where it could be compared.
///
/// The count of objects stock had to *create* in that scratch directory is
/// reported too, and it is free: a non-zero count means the port's own index
/// implies trees its own object store does not contain.
///
/// # What is deliberately not probed
///
/// Each of these was tried against stock 2.55.0 and rejected for a measured
/// reason, recorded here so the next person does not re-derive it:
///
///  * **`ls-files --debug`.** Prints each index entry's `ctime`, `dev` and
///    `ino`. Those are facts about the filesystem, not the repository, and they
///    differ between the two sides' copies the moment either side rewrites the
///    index — the two repos in one case were measured at `ino: 1076105459` and
///    `1076105460` for the same path. Comparing it would report the inode
///    allocator as a parity defect.
///  * **`GIT_TEST_CHECK_CACHE_TREE=1`.** Present in the reference binary and it
///    does fire — `error: cache-tree for path  does not match. Expected
///    9c05a71a… got f741aa06…` — but only when the probing command happens to
///    write the index back *and* has not invalidated the corrupted node first,
///    which for a read-only probe means forcing a stat refresh by touching a
///    worktree file. The one defect it uniquely catches is the wrong-cache-tree
///    case, and `write-tree` already prints the wrong tree id for that with no
///    touching and no dependence on whether an index write happened to occur.
///  * **`count-objects -v`.** Its `size` and `size-pack` fields are pack byte
///    counts, and the vendored gitoxide cannot reproduce git's pack bytes (see
///    [`probe_storage`], which states the same relaxation). Every `repack` case
///    would fail on a difference that is already known to be legitimate.
///  * **`rev-list --all --objects`.** A full walk that `fsck` already performs,
///    more strictly, in the same pass.
///
/// # The mirror: the port reading what stock wrote
///
/// A port that writes what git cannot read and a port that cannot read what git
/// writes are the same class of bug, and the second one is *structurally*
/// unmeasured by the rest of this harness. Every case starts from a stock-built
/// fixture, so "the port reads a stock repository" is covered — for the pristine
/// fixture, and for nothing else. A repository stock has just `gc`'d, `repack
/// --write-midx`'d, `pack-refs`'d or `commit-graph write`'n is a repository the
/// port has never been asked to read, and those commands are precisely the ones
/// that produce the structures a port misreads: multi-pack indexes, bitmaps,
/// commit-graphs, `packed-refs`, split indexes.
///
/// So the same `write-tree` question is put to **the binary under test** as
/// well, about the same repository, with the same three variables redirecting
/// its writes — verified against the port: repository byte-identical afterwards,
/// correct tree id, scratch index rewritten. Its answer goes into the digest
/// beside stock's, so the two sides' digests differ whenever the port reads one
/// repository differently from the other.
///
/// **The bounded blind spot, stated rather than left to be found.** This is a
/// differential comparison, so a port that misreads *both* repositories in the
/// *same* way renders the same line on both sides and is invisible here. That is
/// inherent to differential measurement and is already true of every other
/// surface in this crate; what makes it tolerable is that the failures worth
/// catching are asymmetric by construction. A structure only stock produces
/// cannot be misread symmetrically, because the port's own repository does not
/// contain one.
///
/// # Cost
///
/// **Three invocations per side, and only on a case that wrote under the git
/// directory.** Two are stock git (`fsck`, `write-tree`) and the third is the
/// binary under test answering `write-tree` about the same repository — the
/// mirror. A **fourth** is spent only where the repository holds an accelerator
/// structure at all; see [`accelerator_read_probe`], which also records the four
/// candidates rejected as measurably redundant with the `fsck` above.
/// Everything else pays two `stat` walks of `.git` per side (see
/// [`git_fingerprint`]) and nothing more: no child process, no copy, no
/// comparison. A case that does mutate goes from the 18 child processes it
/// already pays (two invocations plus two eight-probe state digests) to 24, and
/// buys two small file copies per side.
///
/// The alternatives were both worse. Probing every case would spend those six
/// processes on `log`, `diff`, `rev-parse`, `cat-file` and the rest of a corpus
/// that is mostly read-only, for an answer that cannot differ — a repository
/// neither side wrote to is the fixture, and the fixture is byte-identical on
/// both sides by construction. Probing only where the *post-state digest*
/// differed would be cheaper still and would measure nothing: this dimension
/// exists precisely for the cases whose post-state digests agree.
///
/// The fraction of the corpus the gate actually opens for is printed by every
/// run rather than estimated here, because a number in a comment goes stale the
/// first time a case is added.
fn probe_interop(repo: &Path, home: &Path, scratch: &Path, zvcs_bin: &Path) -> String {
    let Ok(stock) = crate::stock::git() else {
        return "# interop\n<no-stock-git>\n".to_string();
    };
    // Cleared at both ends. The tail removal is the one that keeps the worker's
    // workdir from growing; this one is what makes `objects-written` mean
    // anything after a run that was killed mid-probe and left a scratch behind,
    // because a stale object counted here would be attributed to this case.
    let _ = std::fs::remove_dir_all(scratch);
    let mut out = String::from("# interop\n");

    // 1. git's own validator, read-only, straight in the repository.
    let mut cmd = Command::new(stock);
    env::harden(&mut cmd, home);
    cmd.current_dir(repo).args(["fsck", "--strict", "--no-progress", "--no-dangling"]);
    out.push_str("## fsck --strict\n");
    match cmd.output() {
        Ok(o) => {
            out.push_str(&format!("exit: {:?}\n", o.status.code()));
            // stderr as well as stdout: fsck reports almost every finding there,
            // and a probe that read only stdout would call a repository stock
            // rejects outright "clean".
            out.push_str(&decode_exact(o.stdout));
            out.push_str(&decode_exact(o.stderr));
        }
        Err(_) => out.push_str("<spawn-failed>\n"),
    }

    // 2. git's own use of the index, and then the same question put to the
    //    binary under test — the mirror. Both have every write redirected out of
    //    the repository; see the header for the three variables and why.
    out.push_str("## write-tree\n");
    out.push_str(&write_tree_probe("stock", stock, repo, home, &scratch.join("stock")));
    out.push_str(&write_tree_probe("zvcs", zvcs_bin, repo, home, &scratch.join("zvcs")));
    let _ = std::fs::remove_dir_all(scratch);

    // 3. The mirror, widened: the port reading the structures only stock
    //    writes. Gated on one of them being there, so most cases pay a handful
    //    of `stat` calls and no process at all.
    out.push_str(&accelerator_read_probe(zvcs_bin, repo, home));
    out
}

/// The accelerator files a repository holds, in a fixed order: the ones a
/// reader has to parse rather than re-derive.
///
/// Fixed rather than enumerated from the directory so the line is one fact per
/// name and the two sides line up positionally even when only one of them wrote
/// the structure. `.bitmap` is a wildcard because its name carries the pack's
/// checksum.
fn accelerators(repo: &Path) -> Vec<&'static str> {
    let git = git_dir(repo);
    let objects = git.join("objects");
    let mut out: Vec<&'static str> = Vec::new();
    if objects.join("info/commit-graph").is_file() {
        out.push("commit-graph");
    }
    if objects.join("info/commit-graphs").is_dir() {
        out.push("commit-graph-chain");
    }
    if objects.join("pack/multi-pack-index").is_file() {
        out.push("multi-pack-index");
    }
    if walk_files(&objects.join("pack")).iter().any(|(rel, _)| rel.ends_with(".bitmap")) {
        out.push("bitmap");
    }
    if git.join("packed-refs").is_file() {
        out.push("packed-refs");
    }
    out
}

/// Ask **the binary under test** to enumerate the repository through whatever
/// accelerator it finds there, and report what it said.
///
/// # Why this, and why the four obvious candidates are not here
///
/// [`probe_interop`]'s charter is the structures a logical probe is blind to,
/// and the natural way to widen it is to ask stock more questions about the
/// port's repository. Four were considered and each was rejected on a
/// measurement rather than on taste, recorded here so the next reader does not
/// re-derive them:
///
///  * **`commit-graph verify`.** Redundant: `fsck --strict` already performs the
///    identical check. Measured on stock 2.55.0 against a commit-graph whose
///    `CDAT` chunk was edited to name the wrong root tree, with the trailing
///    checksum re-stamped so the file is internally consistent — `fsck` printed
///    `root tree OID for commit 13168c53… in commit-graph is b7130d30…faa8… !=
///    b7130d30…f9a8…` and exited 16, and `commit-graph verify` printed the same
///    sentence and exited 1. A corrupted checksum is caught by both as well
///    (`the commit-graph file has incorrect checksum and is likely corrupt`,
///    exit 16 and exit 2). Adding it would spend an invocation to print a
///    message the digest already carries.
///  * **`multi-pack-index verify`.** Redundant for the same measured reason: one
///    byte moved inside a `multi-pack-index` with the checksum re-stamped gives
///    `fatal: bad pack-int-id: 16777216 (1 total packs)` from `fsck` at exit 32
///    and the same sentence from `multi-pack-index verify` at exit 128.
///  * **`verify-pack`.** Its validity check is `fsck`'s — a `.bitmap` with one
///    byte moved is already `error: bitmap file … has invalid checksum` and exit
///    128 from `fsck` — and of its six columns, `size-in-pack`, `offset`,
///    `depth` and `base` are the compression choices this crate has ruled
///    uncomparable while `type` and `size` are in the batch listing. The one
///    column that was missing is *which objects are in which pack*, and that is
///    now [`probe_pack_contents`], read out of the `.idx` for no process at all.
///  * **`for-each-ref` and `ls-files --stage`.** Both are already asked, of
///    stock, in [`probe_state`] — the second with `-v`, which is a strict
///    superset. Asking them again here would compare a line against its own
///    copy.
///
/// What is left is the direction that is genuinely thin. Stock reading the
/// port's repository has `fsck`, `write-tree` and eight `probe_state`
/// invocations behind it. **The port reading stock's repository has one
/// question**: `write-tree`, which reads the index and the object store and
/// touches no accelerator at all. A repository stock has just `gc`'d,
/// `repack --write-bitmap-index`'d, `pack-refs`'d, `commit-graph write`'n or
/// `multi-pack-index write`'n holds exactly the structures a port is most
/// likely to misread, and nothing asked the port to read one.
///
/// `rev-list --objects --all` is the question that goes through all of them at
/// once: it resolves every ref (so `packed-refs`), walks every commit (so the
/// commit-graph, and the commit-graph chain), and names every tree and blob (so
/// the pack index, the multi-pack-index and the bitmap). A port that reads any
/// of them wrongly answers differently about a repository whose logical content
/// is identical on both sides.
///
/// # Direction
///
/// Appended below `## write-tree`, so nothing above it moves. The output is
/// **sorted** before it is rendered: `--objects` emits in walk order, and this
/// probe is about the object set rather than about the order a walk reached it.
/// Sorting can only make two answers agree that would otherwise differ, which is
/// the direction this file is allowed to fail in; it cannot make two equal
/// answers differ.
///
/// A repository holding none of the five structures prints one line naming
/// that, on both sides, and spawns nothing.
///
/// # Cost
///
/// A handful of `stat` calls on every case that opened the interop gate, and
/// **one** invocation of the binary under test on those that hold an
/// accelerator — the fourth on a case that pays three today, and only for the
/// cases it can speak to. `rev-list` is a read-only command in git, so it needs
/// none of the write redirection [`write_tree_probe`] documents; it has the same
/// standing as the `fsck` invocation above it.
fn accelerator_read_probe(zvcs_bin: &Path, repo: &Path, home: &Path) -> String {
    let present = accelerators(repo);
    let mut out = format!("## accelerator-read\npresent: {}\n", present.join(" "));
    if present.is_empty() {
        out.push_str("<none present: not asked>\n");
        return out;
    }
    let mut cmd = Command::new(zvcs_bin);
    env::harden(&mut cmd, home);
    cmd.current_dir(repo).args(["rev-list", "--objects", "--all"]);
    match cmd.output() {
        Ok(o) => {
            out.push_str(&format!("zvcs exit: {:?}\n", o.status.code()));
            // Sorted **on the bytes**, before they are rendered: `decode_exact`
            // renders a stream that is not text through `render_binary`, whose
            // hex lines are positional, and sorting those would scramble a
            // rendering built to be read.
            let mut lines: Vec<&[u8]> = trim_bytes(&o.stdout).split(|b| *b == b'\n').collect();
            lines.sort_unstable();
            out.push_str(&decode_exact(lines.join(&b'\n')));
            // The trailing newline `trim_bytes` took off, so this section ends
            // the way every other one does and the next line a reader sees is a
            // heading rather than the tail of an object listing.
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }
        Err(_) => out.push_str("zvcs exit: <spawn-failed>\n"),
    }
    out
}

/// Ask one binary to build a tree from this repository's index, with every write
/// it would make redirected into `scratch`, and report both its answer and what
/// it had to do to the index to produce it.
///
/// `label` names the binary in every line, so a reader of a failing digest can
/// see at a glance whether it was git or the port that answered differently.
/// Each fact is one line, because `report.rs` pairs the two sides' digests by
/// line position to name whichever fact moved.
fn write_tree_probe(
    label: &str,
    bin: &Path,
    repo: &Path,
    home: &Path,
    scratch: &Path,
) -> String {
    let mut out = String::new();
    let objects = scratch.join("objects");
    let index_copy = scratch.join("index");
    if std::fs::create_dir_all(&objects).is_err() {
        return format!("{label}: <scratch-failed>\n");
    }
    if std::fs::copy(git_dir(repo).join("index"), &index_copy).is_err() {
        // A repository with no index at all — an `init` before anything is
        // staged, or a bare one. Reported as the fact it is rather than skipped,
        // so the two sides still have a line to disagree on when only one of
        // them has an index.
        return format!("{label}: <no-index>\n");
    }
    let before = std::fs::read(&index_copy).unwrap_or_default();

    let mut cmd = Command::new(bin);
    env::harden(&mut cmd, home);
    cmd.current_dir(repo)
        .env("GIT_INDEX_FILE", &index_copy)
        .env("GIT_OBJECT_DIRECTORY", &objects)
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", git_dir(repo).join("objects"))
        .arg("write-tree");
    match cmd.output() {
        Ok(o) => {
            out.push_str(&format!("{label} exit: {:?}\n", o.status.code()));
            // One line, so [`escape_bytes`] rather than [`decode_exact`]: this
            // fact is paired with the other side's by line position, and a
            // multi-line hex rendering spliced in here would shift every
            // following fact. The trim is on the bytes, because a failing
            // `write-tree` prints nothing and a succeeding one prints an object
            // id — and a port that prints neither prints whatever it prints,
            // exactly.
            out.push_str(&format!(
                "{label} tree: {}\n",
                escape_bytes(trim_bytes(&o.stdout))
            ));
        }
        Err(_) => out.push_str(&format!("{label} exit: <spawn-failed>\n")),
    }

    // The finding from the worked example, as separate one-line facts.
    let after = std::fs::read(&index_copy).unwrap_or_default();
    out.push_str(&format!(
        "{label} index-repaired: {}\n",
        if before == after { "no" } else { "yes" }
    ));
    out.push_str(&format!("{label} index-bytes-before: {}\n", before.len()));
    out.push_str(&format!("{label} index-bytes-after: {}\n", after.len()));
    // Trees this binary had to create to answer: non-zero means the repository's
    // own index implies objects its own store does not hold.
    out.push_str(&format!("{label} objects-written: {}\n", walk_files(&objects).len()));
    out
}

/// Root-level files and refs that record an **in-progress operation**, plus the
/// one root file that records a *persistent* one: `shallow`.
///
/// Enumerated from git 2.55.0 rather than globbed over `.git`, because a glob
/// would sweep in `index`, `COMMIT_EDITMSG` and the hook samples — machine-local
/// scratch — and would make the probe's meaning depend on whatever else happens
/// to sit in the directory. Each name below is cited to the code that writes or
/// deletes it:
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
///
/// **`shallow` is here, and it is the odd one out.** It is not an operation in
/// progress; it is the graft boundary of a shallow repository, and it is the
/// entire state `--unshallow`, `--deepen` and a second `--depth` act on. Nothing
/// else in this crate reads it: `for-each-ref` and `cat-file
/// --batch-all-objects` both report the object and ref set a shallow fetch
/// produced without saying a word about the boundary that makes it shallow, so
/// `fetch --depth=1`, `--deepen=1` and `--unshallow` were measured on their
/// reports and their exit codes alone. Its content is one abbreviated-free
/// object id per line — measured, not assumed: after `git fetch --depth=1 origin
/// main` against a two-commit peer, stock 2.55.0 left
/// `b015a4d60ee11600ec4c9413e74ce42c7a9593d3\n` and nothing else — so it is a
/// function of repository content, has no timestamp, no path and no pid in it,
/// and two correct implementations must write the same bytes. `read_as_value`
/// flattens it to one line like every other fact here.
///
/// This can only ever *add* a difference: every existing name still produces its
/// own line in its own position, and a repository that is not shallow reports
/// `shallow: <absent>` on both sides.
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
    // Lower-cased and last because it is the one name here git does not spell in
    // capitals, and the one fact that outlives the command that wrote it.
    "shallow",
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
        out.push_str(&format!("{name}: {}\n", read_as_value(repo, &git.join(name))));
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
            out.push_str(&format!("{dir}/{rel}: {}\n", read_as_value(repo, &file)));
        }
    }
    out
}

/// What a **symlink** inside a metadata directory is reported as, and whether
/// the reader is allowed to open what it points at.
///
/// # The hole
///
/// [`walk_files`] takes `symlink_metadata`, so it does not *descend* through a
/// symlinked directory — but it yields a symlink to a file as an ordinary
/// entry, and every reader that then calls `std::fs::read` on it follows the
/// link. `Shape::Symlinks` puts a link that points **outside the fixture** in
/// on purpose, so a probe could splice a file the case never wrote — anything
/// on the machine the harness can read — straight into a parity digest. That is
/// pre-existing, it is under `.git` where the fixture's own worktree probe does
/// not reach, and [`collect_worktree`] has always refused to do it on the
/// worktree side ("symlink targets are read, symlinks are not followed").
///
/// # The rule, and why it cannot lose a difference
///
/// The **target string is always reported**, which is new information and can
/// only add. What happens to the content behind it splits on one question:
///
///  * **The link resolves inside the fixture root** — the content is read, as
///    before, *after* the target line. Nothing that was compared stops being
///    compared, and the target is compared as well.
///  * **It resolves outside, or nowhere** — the content is not read, and the
///    reader says so. This cannot lose a difference either, and the reason is
///    arithmetic rather than judgement: an absolute path outside both fixtures
///    is *the same file* for both sides, so its bytes were identical in both
///    digests and never contributed a difference — while its bytes could change
///    between the two sides' probes and contribute a false one, which is the
///    other half of why it should not be read.
///
/// The two roots are canonicalised before the comparison, because on macOS the
/// harness's own temporary directory is reached through a `/tmp` symlink and a
/// prefix test on the uncanonicalised paths would call every link outside.
fn link_note(root: &Path, path: &Path) -> Option<(String, bool)> {
    use std::os::unix::ffi::OsStrExt;
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_symlink() {
        return None;
    }
    let rendered = std::fs::read_link(path)
        .map_or_else(|_| "<unreadable>".to_string(), |t| escape_bytes(t.as_os_str().as_bytes()));
    let inside = match (path.canonicalize(), root.canonicalize()) {
        (Ok(target), Ok(root)) => target.starts_with(root),
        _ => false,
    };
    Some((format!("<symlink -> {rendered}>"), inside))
}

/// One state file as a single `value` field: `<absent>` when it is not there,
/// otherwise its bytes with backslash, newline and carriage return escaped so
/// the fact occupies exactly one line — and every byte that is not part of a
/// valid UTF-8 sequence written `\xNN`, so two values that differ only there are
/// two values. See [`escape_bytes`] for why that is not a detail.
///
/// `root` is the fixture root, and it is only consulted when the entry turns
/// out to be a symlink: see [`link_note`].
fn read_as_value(root: &Path, path: &Path) -> String {
    let prefix = match link_note(root, path) {
        Some((note, false)) => return format!("{note} <outside fixture: not read>"),
        Some((note, true)) => format!("{note} "),
        None => String::new(),
    };
    match std::fs::read(path) {
        Ok(bytes) => format!("{prefix}{}", escape_bytes(&bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => format!("{prefix}<absent>"),
        Err(_) => format!("{prefix}<unreadable>"),
    }
}

/// Bytes as one line: backslash, newline and carriage return escaped, so a value
/// carrying its own newlines cannot shift every following fact in the digest.
///
/// **Exact, not lossy.** This opened with `String::from_utf8_lossy`, which is the
/// same hole [`mask_paths`] was fixed for and reaches the same conclusion by the
/// same route: every invalid byte becomes U+FFFD, so `\xc0\x80` and `\xc1\x80`
/// collapse onto one string and compare equal. It is reached by every value
/// [`read_as_value`] reports — `.git/MERGE_MSG` carrying a non-UTF-8 commit
/// message, an `am` patch of a binary file under `rebase-apply/`, a
/// `NOTES_MERGE_WORKTREE` note whose conflicted content is not text — and by
/// every symlink target in [`collect_worktree`], where a path is bytes on every
/// Unix and need not be UTF-8 at all.
///
/// A byte that is not part of a valid UTF-8 sequence is written `\xNN` instead.
/// The escaping stays injective, because a literal backslash in the input is
/// already doubled: input `\x41` renders `\\x41`, which no invalid byte can
/// produce.
///
/// Text is untouched. On valid UTF-8 this emits exactly what the three chained
/// `replace` calls emitted, in the same order — backslash first, so the
/// backslashes the newline escape introduces are not re-escaped — so no value
/// that was compared before is compared differently now.
fn escape_bytes(bytes: &[u8]) -> String {
    fn push_text(out: &mut String, text: &str) {
        for ch in text.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                _ => out.push(ch),
            }
        }
    }

    let mut out = String::with_capacity(bytes.len());
    let mut rest = bytes;
    loop {
        match std::str::from_utf8(rest) {
            Ok(text) => {
                push_text(&mut out, text);
                return out;
            }
            Err(e) => {
                let (good, bad) = rest.split_at(e.valid_up_to());
                push_text(&mut out, std::str::from_utf8(good).unwrap_or_default());
                // `error_len() == None` means the input ends mid-sequence: the
                // remaining bytes are all unrenderable, and all of them are kept.
                let skip = e.error_len().unwrap_or(bad.len()).max(1);
                for b in bad.iter().take(skip) {
                    out.push_str(&format!("\\x{b:02x}"));
                }
                rest = &bad[skip.min(bad.len())..];
            }
        }
    }
}

/// One file's bytes as **text**, verbatim where they are text and as an exact
/// hex rendering where they are not.
///
/// The multi-line counterpart to [`escape_bytes`], for the two probes that splice
/// a whole file into the digest rather than one line of it. Both opened with
/// `String::from_utf8_lossy` and both are reachable by bytes that are not text:
///
///  * A **reflog** is text by construction — until a ref name or a committer
///    ident is not UTF-8, which git neither requires nor validates.
///  * An **rr-cache preimage of a binary conflict** is the file's own bytes with
///    conflict markers around them. `rerere` records one whenever a merge
///    conflicts on a file git treats as binary, and comparing two of those
///    through `from_utf8_lossy` compares two U+FFFD runs — which is to say it
///    compares their lengths, and nothing else.
///
/// [`render_binary`] rather than a second rendering of my own: it is the exact
/// one this crate already trusts for a non-text surface, it is bounded, and it
/// puts 32 bytes on a line so `report.rs` can name the window that moved.
/// `root` is the fixture root, consulted only when the entry is a symlink: see
/// [`link_note`]. The note gets its own line here rather than a prefix, because
/// this is the multi-line splice and a prefix would run into the first line of
/// the file.
fn read_as_text(root: &Path, path: &Path) -> String {
    let prefix = match link_note(root, path) {
        Some((note, false)) => return format!("{note} <outside fixture: not read>\n"),
        Some((note, true)) => format!("{note}\n"),
        None => String::new(),
    };
    match std::fs::read(path) {
        Ok(bytes) => format!("{prefix}{}", decode_exact(bytes)),
        Err(_) => format!("{prefix}<unreadable>\n"),
    }
}

/// A **child process's own output** as text, verbatim where it is text and as an
/// exact hex rendering where it is not.
///
/// # The hole this closes
///
/// [`normalize`] was fixed for exactly this in `f0c9ac6` — two different binary
/// stdouts decoded to one U+FFFD run and compared EQUAL — and the fix stopped at
/// the *compared* stdout, one layer up. Every probe in this file that splices
/// **stock git's own stdout** into a digest still opened with
/// `String::from_utf8_lossy`, so the same collapse was live on the state and
/// interop surfaces:
///
///  * [`probe_state`] — `for-each-ref --format=%(refname) …` prints a ref name
///    verbatim, and git neither requires nor validates that a ref name is UTF-8;
///    `ls-files --stage -v`, `status --porcelain` and `config --list --local`
///    print paths, values and keys, and a path is bytes on every Unix. `status
///    --porcelain=v1` quotes a non-ASCII path only while `core.quotePath` is on
///    — the corpus sets `core.quotePath=false` — and `--porcelain=v1` never
///    quotes at all for the `-z`-adjacent fields; `config --list` never quotes a
///    value.
///  * [`probe_interop`] — `fsck`'s stdout and stderr both name refs and paths;
///    `write-tree`'s stdout is an object id until the command fails.
///  * [`module_refs`] and [`peer_section`] — the same two ref/object listings,
///    asked inside a submodule git directory and inside a peer.
///
/// Two ports that write two *different* non-UTF-8 ref names therefore produced
/// one digest line and matched. Reachable, and not hypothetically: `refs/heads/`
/// followed by any byte sequence with no ASCII control character, no space and
/// no `~^:?*[\` is a legal ref name to `check-ref-format`, and the port's own
/// ref-name validator is one of the things this harness exists to compare.
///
/// # Why this is not a third rendering
///
/// It is the two that already exist, dispatched on the one question that
/// separates them — [`render_binary`] for output that is not text, the bytes
/// themselves for output that is. [`read_as_text`] was already exactly this
/// against a file's bytes and now delegates here, so the file surface and the
/// child-process surface render a non-text byte the same way.
///
/// # Direction
///
/// Text is bit-identical to what `from_utf8_lossy` returned, because on valid
/// UTF-8 the two agree by definition. On invalid input the rendering is
/// injective where `from_utf8_lossy` was not: `render_binary` writes the length
/// and then every byte in hex, so two outputs that differ in any byte still
/// differ in the digest. Nothing that was a difference stops being one.
fn decode_exact(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) => render_binary(e.as_bytes()),
    }
}

/// ASCII whitespace trimmed from both ends of a byte slice, for the one-line
/// facts that used to reach `.trim()` through a lossy decode.
fn trim_bytes(bytes: &[u8]) -> &[u8] {
    let start = bytes.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(bytes.len());
    let end = bytes.iter().rposition(|b| !b.is_ascii_whitespace()).map_or(start, |p| p + 1);
    &bytes[start..end]
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

/// The **bytes in the worktree**, which nothing else in this crate reads.
///
/// # The gap
///
/// Every probe above reports what git *thinks*: `status --porcelain` says `AA
/// conflict.txt` whatever bytes are on disk, `ls-files --stage` prints the three
/// stages the index holds, and neither moves when the file itself changes. So
/// the whole family of commands whose entire job is to write a particular byte
/// sequence into the worktree was measured on its exit code:
/// `checkout --ours`/`--theirs`/`-m`, `--conflict=diff3`/`merge`/`zdiff3`,
/// `merge -X ours`/`-X theirs`, `restore`, and every conflict-marker style.
/// `checkout --ours` and `checkout --theirs` produced *identical* digests.
///
/// The corpus knows it: `corpus/merge_family.rs`'s header documents three routes
/// a content case has to take to be asserted at all — print the result on stdout
/// with `merge-file -p`, hide it in a tree id with `merge-tree --write-tree`, or
/// pick a strategy backend that stages its result so `ls-files --stage` carries
/// the blob — and records that "cases that merely rewrite a worktree file in
/// place are kept for their exit code". This probe is what those workarounds
/// were working around.
///
/// # What is compared
///
/// One line per worktree entry, `path <x|-> : value`, sorted. The `x` column is
/// the executable bit and nothing else of the mode: the rest is umask-derived.
///
/// The value is the file's own bytes when it is UTF-8 and small, and a length
/// otherwise. That split is not a convenience, it is the same relaxation
/// [`probe_storage`] makes and for the same reason: a `bundle`, a `pack` or an
/// `archive` written into the worktree is a pack stream, the vendored gitoxide
/// cannot reproduce git's pack bytes, and comparing them would fail every
/// valid-but-different pack. Binary is therefore compared by length; text — the
/// merged file, the conflict markers, the patch, the checked-out blob — is
/// compared byte for byte.
///
/// # What is not walked
///
///  * **`.git`**, and **any nested directory that is itself a repository**
///    (`HEAD` + `objects/` + `refs/`). The fixture's bare peer at `.remote.git`
///    is one; a `clone` target's git directory is another. Their presence is
///    still reported as a line, so a case that fails to create one differs from
///    a case that creates one; what is inside them belongs to [`probe_peer`] and
///    to `probe_storage`'s counting rules, not to a byte comparison.
///  * **Symlink targets are read, symlinks are not followed.** Following one
///    would walk out of the fixture — `Shape::Symlinks` contains a link that
///    points outside it on purpose — and the target string is the fact anyway.
///
/// # Determinism
///
/// Paths go through [`stable_entry_name`] before they are sorted, exactly as in
/// `probe_storage`: a `pack-objects` or `bundle` invocation writes a
/// checksum-named or `.tmp-<pid>-`named file into the working directory, and a
/// pid is machine noise that would make thousands of cases flaky. Contents carry
/// no timestamp: `env::harden` pins the identities and the dates that end up
/// inside patches and archives. Absolute paths inside a file are handled by
/// [`normalize`], which is applied to the whole digest — which is also why the
/// content is kept as text rather than hashed: a hash cannot be normalized.
///
/// # Direction
///
/// A new section appended to the digest: every existing line keeps its content
/// and its position, both sides are asked the same question, and no line is ever
/// removed. It can only ever add differences.
fn probe_worktree_content(repo: &Path) -> String {
    let mut out = String::from("# worktree-content\n");
    if git_dir(repo) == repo {
        out.push_str("<bare: no worktree>\n");
        return out;
    }
    let mut lines: Vec<String> = Vec::new();
    collect_worktree(repo, "", &mut lines);
    lines.sort();
    // A ceiling rather than an unbounded walk: a case that unpacks a large tree
    // into the worktree would otherwise put megabytes into every digest, and the
    // digests are held in memory for the whole comparison. Both sides truncate
    // the same sorted listing at the same point, so the cut is symmetric.
    let truncated = lines.len().saturating_sub(WORKTREE_ENTRY_CAP);
    for line in lines.iter().take(WORKTREE_ENTRY_CAP) {
        out.push_str(line);
        out.push('\n');
    }
    if truncated > 0 {
        out.push_str(&format!("<truncated: {truncated} more entries>\n"));
    }
    out
}

/// Entries listed by [`probe_worktree_content`] before it truncates.
const WORKTREE_ENTRY_CAP: usize = 4096;
/// Largest file whose bytes are compared; above this only the length is.
const WORKTREE_CONTENT_MAX: u64 = 64 * 1024;

/// Whether `dir` is a git directory — bare or otherwise — by the same three
/// things `setup.c` looks for.
fn looks_like_git_dir(dir: &Path) -> bool {
    dir.join("HEAD").is_file() && dir.join("objects").is_dir() && dir.join("refs").is_dir()
}

fn collect_worktree(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            out.push(format!("{} -: <unreadable>", stable_entry_name(&rel)));
            continue;
        };
        let rel = stable_entry_name(&rel);
        if meta.is_symlink() {
            // The target's own bytes, not `to_string_lossy` of them: a symlink
            // target is bytes on every Unix and need not be UTF-8, and decoding
            // it lossily first would hand `escape_bytes` a U+FFFD run in place of
            // the two different targets it is there to tell apart.
            use std::os::unix::ffi::OsStrExt;
            let target = std::fs::read_link(&path)
                .map(|t| escape_bytes(t.as_os_str().as_bytes()))
                .unwrap_or_else(|_| "<unreadable>".to_string());
            out.push(format!("{rel} -: <symlink -> {target}>"));
        } else if meta.is_dir() {
            if name == ".git" {
                out.push(format!("{rel} -: <git directory>"));
            } else if looks_like_git_dir(&path) {
                out.push(format!("{rel} -: <repository>"));
            } else {
                out.push(format!("{rel} -: <dir>"));
                collect_worktree(&path, &rel, out);
            }
        } else if meta.is_file() {
            let x = if meta.permissions().mode() & 0o111 != 0 { 'x' } else { '-' };
            out.push(format!("{rel} {x}: {}", file_value(&path, meta.len())));
        } else {
            // A socket, a fifo, a device. Named, never opened: `open` on a fifo
            // with no writer blocks forever, and nothing in this crate puts a
            // timeout around a state probe — one such file in one fixture would
            // park a worker for the rest of the run. The type is the fact.
            let kind = {
                use std::os::unix::fs::FileTypeExt;
                let t = meta.file_type();
                if t.is_fifo() {
                    "fifo"
                } else if t.is_socket() {
                    "socket"
                } else if t.is_block_device() {
                    "block-device"
                } else if t.is_char_device() {
                    "char-device"
                } else {
                    "unknown"
                }
            };
            out.push(format!("{rel} -: <special {kind}>"));
        }
    }
}

/// One worktree file as a single line: its text where the bytes are text, its
/// length where they are not.
fn file_value(path: &Path, len: u64) -> String {
    if len > WORKTREE_CONTENT_MAX {
        return format!("<{len} bytes>");
    }
    match std::fs::read(path) {
        Ok(bytes) => match std::str::from_utf8(&bytes) {
            Ok(text) => escape_bytes(text.as_bytes()),
            Err(_) => format!("<binary {} bytes>", bytes.len()),
        },
        Err(_) => "<unreadable>".to_string(),
    }
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

/// The **headers** of the two accelerator files [`probe_storage`] can only count:
/// the multi-pack-index and every `*.bitmap`.
///
/// `probe_storage` enumerates the directory, so `objects/pack/multi-pack-index`
/// and `pack-<hash>.bitmap` do appear there — as names. A name is presence, and
/// presence is where that probe deliberately stops, because a pack's *bytes* are
/// not reproducible by the vendored gitoxide. The consequence is that a
/// multi-pack-index covering the wrong packs, or a bitmap built over a different
/// object set, matched a correct one line for line: `multi-pack-index write
/// --bitmap` was caught by its exit code and by nothing else.
///
/// **Only counts and versions are taken — never bytes.** Both formats put every
/// fact this probe reports in a fixed-offset header, and every one of them is a
/// function of repository content rather than of byte layout:
///
///  * **`MIDX`** — verified against stock 2.55.0's own output: `4d 49 44 58`,
///    version `1`, hash version `1`, chunk count `6`, base-midx count `0`, then
///    a big-endian pack count at offset 8 (`1`). The chunk table that follows is
///    `(id, offset)` pairs, and the ids name what the file actually contains
///    (`PNAM OIDF OIDL OOFF RIDX BTMP` for a `write --bitmap`, and no `BTMP`
///    without it). The object count is the last entry of the `OIDF` fanout —
///    read at `OIDF + 1020`, and cross-checked against `cat-file
///    --batch-all-objects`: the midx said 9 where the repository held 11, the
///    two loose objects being outside the pack, which is exactly the fact a
///    logical probe cannot see.
///  * **`BITM`** — `42 49 54 4d`, a 2-byte version (`1`), 2-byte flags (`5`),
///    then a big-endian entry count (`3` for a three-object pack).
///
/// The *offsets* inside both files are byte layout and are not reported; the ids,
/// counts and versions are what the file claims about the repository.
///
/// **Direction: appended, so it can only add differences.** The section is new
/// and both sides are asked the same question; a repository with neither file
/// reports an empty section on both sides.
///
/// **The bounded relaxation, stated rather than left to be found.** An entry
/// count is a count of the objects a pack or midx covers, so a port that packs
/// the same objects into a different *grouping* can report a different count
/// legitimately. That is the same class of difference `probe_storage` already
/// reports by counting packs, so this adds no new class of false positive — but
/// it is a difference reported on a valid repository, and it is recorded here.
fn probe_pack_headers(repo: &Path) -> String {
    let pack = git_dir(repo).join("objects").join("pack");
    let mut lines: Vec<String> = Vec::new();
    for (rel, path) in walk_files(&pack) {
        let name = rel.rsplit('/').next().unwrap_or(rel.as_str()).to_string();
        // A link out of the fixture is reported as the link it is and never
        // opened: see [`link_note`]. The name still produces its own line, so a
        // side that has the entry still differs from a side that does not.
        let header = if let Some((note, false)) = link_note(repo, &path) {
            format!("{note} <outside fixture: not read>")
        } else if name == "multi-pack-index" {
            midx_header(&path)
        } else if name.ends_with(".bitmap") {
            bitmap_header(&path)
        } else {
            continue;
        };
        // Elided and sorted afterwards for the same reason `probe_storage` does
        // it: `multi-pack-index-<hash>.bitmap` names its own checksum, and the
        // pre-elision order of two names that collapse together is arbitrary.
        lines.push(format!("{}: {header}", stable_entry_name(&rel)));
    }
    lines.sort();
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    format!("# pack-headers\n{body}")
}

/// Big-endian `u32` at `off`, or `None` past the end.
fn be32(bytes: &[u8], off: usize) -> Option<u32> {
    bytes.get(off..off + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// The multi-pack-index header: version, pack count, chunk ids, object count.
fn midx_header(path: &Path) -> String {
    let Ok(b) = std::fs::read(path) else { return "<unreadable>".to_string() };
    if b.len() < 12 || &b[..4] != b"MIDX" {
        return format!("<not-a-midx {} bytes>", b.len());
    }
    let (version, hash, chunks, base) = (b[4], b[5], b[6] as usize, b[7]);
    let packs = be32(&b, 8).map_or("<truncated>".to_string(), |n| n.to_string());
    let mut ids: Vec<String> = Vec::new();
    let mut objects = "<no-oidf>".to_string();
    for i in 0..chunks {
        let off = 12 + i * 12;
        let (Some(id), Some(start)) = (
            b.get(off..off + 4),
            b.get(off + 4..off + 12).map(|w| {
                u64::from_be_bytes([w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7]]) as usize
            }),
        ) else {
            ids.push("<truncated>".to_string());
            break;
        };
        let id = escape_bytes(id);
        // The fanout's last bucket is the object count; the chunk is 256 4-byte
        // buckets, so it sits 1020 bytes in.
        if id == "OIDF" {
            objects = be32(&b, start + 1020).map_or("<truncated>".to_string(), |n| n.to_string());
        }
        ids.push(id);
    }
    format!(
        "MIDX v{version} hash={hash} base={base} packs={packs} chunks=[{}] objects={objects}",
        ids.join(",")
    )
}

/// A pack or midx bitmap header: version, flags, and how many objects it covers.
fn bitmap_header(path: &Path) -> String {
    let Ok(b) = std::fs::read(path) else { return "<unreadable>".to_string() };
    if b.len() < 12 || &b[..4] != b"BITM" {
        return format!("<not-a-bitmap {} bytes>", b.len());
    }
    let version = u16::from_be_bytes([b[4], b[5]]);
    let flags = u16::from_be_bytes([b[6], b[7]]);
    let entries = be32(&b, 8).map_or("<truncated>".to_string(), |n| n.to_string());
    format!("BITM v{version} flags={flags} entries={entries}")
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
    format!("# reflogs\n{}", reflog_listing(repo, &repo.join(".git").join("logs")))
}

/// Every reflog under one `logs` directory, verbatim.
///
/// Split out of [`probe_reflogs`] so [`probe_peer`] can ask the same question of
/// the bare peer: `receive-pack` writes `logs/refs/heads/<branch>` there when the
/// peer enables reflogs, and nothing in this crate has ever looked.
fn reflog_listing(root: &Path, logs: &Path) -> String {
    let mut out = String::new();
    for (rel, path) in walk_files(logs) {
        out.push_str(&format!("## {rel}\n{}", read_as_text(root, &path)));
    }
    out
}

/// A **linked worktree's** administrative directory: `.git/worktrees/<name>/**`.
///
/// [`probe_reflogs`] walks `.git/logs` and stops there, and that is the whole
/// blind spot: a linked worktree does not share the main worktree's `HEAD`,
/// `ORIG_HEAD`, index, reflog or operation state. Every one of those lives under
/// `.git/worktrees/<name>/` (`git worktree add --relative-paths -b linked wt`
/// creates `HEAD`, `commondir`, `gitdir`, `index` and `logs/HEAD` there, as the
/// `Worktree` shape builds and this probe's test asserts). So `git -C wt reset
/// --hard HEAD~1` moved a ref, wrote a reflog line and wrote `ORIG_HEAD` — in a
/// directory nothing in this crate read. `worktree` scored 100% and that number
/// was not evidence.
///
/// **Direction: this can only add differences.** It is a new section appended to
/// the digest; every existing line keeps its content and its position, and a
/// repository with no linked worktree reports the single line `<absent>` on both
/// sides.
///
/// **What is reported, and what is deliberately not.** Every file under the
/// directory, one `path: value` line each, contents included — the same
/// "contents, not presence" rule [`probe_op_state`] states, and for the same
/// reason: a `logs/HEAD` with the wrong message or the wrong old-value is the
/// silent-but-wrong case the reflog probe exists to catch, and presence alone
/// would pass it.
///
/// The one exception is a file named `index`. A worktree index carries `ctime`,
/// `ino` and `dev` for every entry — facts about the filesystem, not the
/// repository, which differ between the two sides the moment either writes one.
/// (The fixture builder says the same thing where it has to `read-tree` the
/// linked worktree's index to make the shape reproducible at all.) Comparing
/// those bytes would report the inode allocator as a parity defect, so only the
/// file's length is taken: a length is content-derived — entry count and
/// extensions determine it — and stat data does not move it.
fn probe_worktrees(repo: &Path) -> String {
    let dir = git_dir(repo).join("worktrees");
    let mut out = String::from("# linked-worktrees\n");
    if !dir.is_dir() {
        out.push_str("<absent>\n");
        return out;
    }
    for (rel, path) in walk_files(&dir) {
        let is_index = Path::new(&rel).file_name().and_then(|n| n.to_str()) == Some("index");
        let value = if let Some((note, false)) = link_note(repo, &path) {
            // Never followed out of the fixture; see [`link_note`].
            format!("{note} <outside fixture: not read>")
        } else if is_index {
            match std::fs::metadata(&path) {
                Ok(m) => format!("<index {} bytes>", m.len()),
                Err(_) => "<unreadable>".to_string(),
            }
        } else {
            read_as_value(repo, &path)
        };
        out.push_str(&format!("{rel}: {value}\n"));
    }
    out
}

/// A **submodule's real git directory**: `.git/modules/<name>/**`.
///
/// # The gap
///
/// `Shape::Submodule` is the topology zvcs states as its target, and a
/// submodule's repository does not live in the worktree. `sub/.git` is a file
/// holding `gitdir: ../.git/modules/sub`; the refs, the HEAD, the index, the
/// config, the reflogs and the object store are all under `.git/modules/sub/`,
/// and until this probe **nothing in this crate read one byte of it**.
/// [`probe_worktree_content`] walks `sub/` and stops at the `.git` file;
/// [`probe_state`]'s commands run in the superproject, where a submodule is one
/// gitlink entry in the index and one line of `status`.
///
/// So every `submodule` and `submodule--helper` verb was measured on its stdout,
/// its exit code, and one `160000` mode in the superproject's index. A corpus
/// agent already found what that hides: `submodule add` writes the url
/// unresolved and never writes `submodule.<name>.active` — both facts live in a
/// file under here. A port that printed the right report and left the module
/// directory empty, or wrote a `HEAD` pointing at nothing, or fetched no
/// objects, matched.
///
/// # What is compared
///
/// One `path: value` line per file under `.git/modules`, contents included — the
/// "contents, not presence" rule [`probe_op_state`] states — followed, for each
/// directory under it that is shaped like a repository, by the object census and
/// by two **stock git** invocations run inside it, exactly as [`probe_peer`] runs
/// them inside the peer:
///
///  * `for-each-ref` — which refs the submodule has and what they point at.
///  * `cat-file --batch-check --batch-all-objects` — whether the objects behind
///    those refs actually arrived, with their types and sizes. A `HEAD` that
///    names an object the module does not have is the corrupt-submodule failure
///    that `for-each-ref` alone calls a success.
///
/// Both were verified against a real stock-built submodule fixture with the
/// working directory set to `.git/modules/sub` under the hardened environment:
/// three refs and three objects, exit 0 each. Nested submodules are covered by
/// construction — a module's own modules live at
/// `.git/modules/<a>/modules/<b>`, which the same walk reaches.
///
/// # Three values that are not compared as bytes, and why
///
///  * **`index`** — length and [`index_meta`] only. A module index carries
///    `ctime`, `ino` and `dev` per entry, which are facts about the filesystem
///    rather than the repository; this is the identical exception
///    [`probe_worktrees`] makes, for the identical reason. The version, the entry
///    count and the extension chain *are* comparable and are taken.
///  * **`objects/**`** — routed to [`storage_of`], never read. A loose object is
///    zlib of its content and the deflate level is the writer's choice; a pack's
///    bytes the vendored gitoxide cannot reproduce at all (see [`probe_storage`]).
///    The census counts and lists instead, which is what catches the module that
///    fetched nothing.
///  * **`hooks/**`** — length only. Those are the git installation's own sample
///    templates, copied verbatim by `init`; their bytes describe *which git is
///    installed*, not what the command did. A length still moves when a
///    different template set is installed or none is.
///
/// # Determinism
///
/// The fixture's upstream is an absolute path recorded in the module's `config`
/// and in its clone reflog. It is the *template's* upstream — `Templates::build_all`
/// builds each shape once and `instantiate` copies it — so both sides carry the
/// same bytes, and where a case re-resolves it against its own fixture root
/// [`normalize`]'s `<REPO>` substitution covers it: the upstream is
/// `<fixture>-upstream`, which has the fixture root as its prefix. Verified by
/// building the shape twice at two roots and diffing the two `.git/modules`
/// trees: `config`, `logs/HEAD` and the two ref reflogs differ in exactly that
/// one path and in nothing else, and `index` differs in stat data — which is the
/// one this probe measures by length.
///
/// # Cost
///
/// Gated on `.git/modules` being a directory. No corpus shape but `Submodule`
/// has one, so every other case pays a single `is_dir` and both sides print
/// `<absent>`. Where it does exist the walk is a few dozen small files and two
/// child processes per module.
///
/// # Direction
///
/// A new section appended to the digest, asked identically of both sides,
/// `<absent>` on both where there is nothing. Nothing above it moves, so it can
/// only ever add differences.
fn probe_modules(repo: &Path, home: &Path) -> String {
    let modules = git_dir(repo).join("modules");
    let mut out = String::from("# submodule-gitdirs\n");
    if !modules.is_dir() {
        out.push_str("<absent>\n");
        return out;
    }

    for (rel, path) in walk_files(&modules) {
        let parts: Vec<&str> = rel.split('/').collect();
        let name = parts.last().copied().unwrap_or_default();
        // The object store is censused below and never read as bytes — except
        // for the `info/` files that are not objects at all but a list of
        // paths, which nothing in this crate read.
        if parts.iter().any(|p| *p == "objects") && !is_alternates(&parts) {
            continue;
        }
        // Hooks first, so a file that happens to be *called* `index` inside the
        // hooks directory is still a hook. That was the previous ordering's
        // `parts.iter().all(|p| *p != "hooks")` guard, said once instead of
        // twice.
        let value = if let Some((note, false)) = link_note(repo, &path) {
            // Never followed out of the fixture; see [`link_note`].
            format!("{note} <outside fixture: not read>")
        } else if parts.iter().any(|p| *p == "hooks") {
            hook_value(repo, &path, name)
        } else if name == "index" {
            match std::fs::metadata(&path) {
                Ok(m) => format!("<index {} bytes> {}", m.len(), index_meta(&path)),
                Err(_) => "<unreadable>".to_string(),
            }
        } else {
            read_as_value(repo, &path)
        };
        out.push_str(&format!("{}: {value}\n", stable_entry_name(&rel)));
    }

    for gitdir in module_gitdirs(&modules) {
        let rel = gitdir
            .strip_prefix(&modules)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        out.push_str(&format!("## {rel} storage\n"));
        out.push_str(&storage_of(&gitdir.join("objects")));
        out.push_str(&module_refs(&gitdir, home, &rel));
    }
    out
}

/// Whether a path under a module's git directory is one of the two `objects/`
/// files that hold **paths rather than objects**.
///
/// `objects/info/alternates` is the borrowed object store a `clone --shared`,
/// a `clone --reference` or a `submodule add --reference` writes, and it is the
/// difference between a module whose objects are reachable and one whose are
/// not. `http-alternates` is the same fact for the dumb HTTP transport.
/// [`storage_of`] lists both by name and has never read either.
///
/// Their content is a list of directory paths, which is exactly what
/// [`normalize`]'s `<REPO>` and `<HOME>` substitution exists for, so a path
/// that legitimately differs between the two roots does not become a
/// difference. Nothing else under `objects/` is opened: a loose object is zlib
/// at the writer's chosen level and a pack's bytes the vendored gitoxide cannot
/// reproduce, which is [`probe_storage`]'s standing relaxation.
fn is_alternates(parts: &[&str]) -> bool {
    let n = parts.len();
    n >= 3
        && parts[n - 3] == "objects"
        && parts[n - 2] == "info"
        && matches!(parts[n - 1], "alternates" | "http-alternates")
}

/// One hook file under a module git directory, as `x <content>` for a hook the
/// case installed and `<N bytes> x` for one the git installation shipped.
///
/// # What was measured by length, and why only half of it had to be
///
/// Every file under `hooks/` was `<N bytes>`, on the reasoning that hooks are
/// the installation's own sample templates: `init` copies fourteen `*.sample`
/// files verbatim out of the template directory, their bytes describe *which
/// git is installed* rather than what the command did, and comparing them would
/// fail every submodule case on a difference that is not the port's.
///
/// That reasoning is right, and it is only about the samples. A hook whose name
/// does **not** end in `.sample` was never copied by `init` — git's template
/// directory contains no such file — so it is there because the case put it
/// there, or because the port put it there, and its bytes are the whole fact.
/// A `core.hooksPath` redirect, a `submodule add` that installs a hook into the
/// module, a port that writes an *active* `pre-commit` where git writes only a
/// sample: all of them were one integer.
///
/// The **executable bit** is reported for both kinds, because a hook that is not
/// executable does not run, which makes it the one mode bit that is a fact
/// about behaviour rather than about the umask. It is the same single-column
/// rendering [`collect_worktree`] uses for the worktree.
///
/// # Direction
///
/// A sample keeps its byte count and gains a column. A non-sample trades its
/// byte count for its bytes, and two files of different lengths have different
/// bytes, so no difference that was reported before stops being reported.
fn hook_value(root: &Path, path: &Path, name: &str) -> String {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return "<unreadable>".to_string() };
    let x = if meta.permissions().mode() & 0o111 != 0 { 'x' } else { '-' };
    if name.ends_with(".sample") {
        format!("<{} bytes> {x}", meta.len())
    } else {
        format!("{x} {}", read_as_value(root, path))
    }
}

/// Every directory under `.git/modules` that is itself a repository, sorted.
///
/// Recursive because a submodule may have submodules: the inner one's git
/// directory is `<outer>/modules/<inner>`, and a probe that stopped at the first
/// level would leave exactly the nested case — the one a port is most likely to
/// get wrong — unmeasured. The recursion does not descend into a repository's
/// `objects`, `refs` or `logs`, only into a `modules` directory it holds, so the
/// walk is bounded by the submodule nesting depth.
fn module_gitdirs(modules: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(modules) else { return out };
    let mut names: Vec<PathBuf> = rd.filter_map(Result::ok).map(|e| e.path()).collect();
    names.sort();
    for dir in names {
        if !dir.is_dir() {
            continue;
        }
        if looks_like_git_dir(&dir) {
            out.push(dir.clone());
            out.extend(module_gitdirs(&dir.join("modules")));
        } else {
            // A module name may itself contain a slash (`submodule add … a/b`
            // makes `.git/modules/a/b`), so an intermediate directory that is not
            // a repository is descended into rather than skipped.
            out.extend(module_gitdirs(&dir));
        }
    }
    out
}

/// The questions **stock git** is asked inside one module git directory.
///
/// Stock, never the binary under test, on the same rule every probe in this file
/// but [`probe_interop`] follows. A failure to spawn or a non-zero exit is
/// recorded as a value rather than hidden, so a module directory stock cannot
/// open is itself a reported difference.
///
/// # The third question, and what the first two could not answer
///
/// A module's object *bytes* are not comparable — a loose object is zlib at the
/// writer's chosen level — and the previous pass left them at the census, on
/// the argument that a census is what catches the module that fetched nothing.
/// It is, and there is a second half to the argument that the census cannot
/// carry: **the census believes the filenames**. Measured against stock 2.55.0,
/// with one loose blob in a real module rewritten to hold different content
/// under its original name:
///
/// ```text
/// cat-file --batch-check --batch-all-objects
///     28ce6a8b…  blob 2        exit 0     — the id is the filename, and it is unchanged
/// fsck --strict --no-progress --no-dangling
///     error: 62d8fe9f…: hash-path mismatch, found at: ./objects/28/ce6a8b…
///     missing blob 28ce6a8b…                 exit 3
/// ```
///
/// So the inflated content of every object — which is what a loose object's
/// zlib bytes actually *mean*, and the only comparable thing in them — is
/// pinned by the object's own name only if something checks that the name is
/// the hash of the content. `fsck` is the thing that checks, git's own
/// validator, and it was running on the superproject ([`probe_interop`]) and on
/// nothing under `.git/modules`. Exit code, stdout and stderr are all folded
/// in, because `fsck` says almost everything it has to say on stderr — the same
/// three facts and the same flags [`probe_interop`] uses, so a reader comparing
/// the two sections is comparing like with like. The paths it names are
/// relative to the git directory it ran in (`./objects/28/…`), so nothing
/// machine-specific reaches the digest; the ids it names are content.
///
/// # Direction
///
/// Appended after the two existing sections, which keep their content and their
/// position. A module store stock is happy with prints one exit line and
/// nothing else, on both sides.
fn module_refs(gitdir: &Path, home: &Path, rel: &str) -> String {
    const PROBES: &[&[&str]] = &[
        &["for-each-ref", "--format=%(refname) %(objecttype) %(objectname)"],
        &["cat-file", "--batch-check", "--batch-all-objects"],
    ];
    let Ok(stock) = crate::stock::git() else {
        return format!("## {rel} <no-stock-git>\n");
    };
    let mut out = String::new();
    for probe in PROBES {
        let mut cmd = Command::new(stock);
        env::harden(&mut cmd, home);
        cmd.current_dir(gitdir).args(*probe);
        let rendered = match cmd.output() {
            Ok(o) if o.status.success() => decode_exact(o.stdout),
            Ok(_) => "<err>\n".to_string(),
            Err(_) => "<spawn-failed>\n".to_string(),
        };
        out.push_str(&format!("## {rel} {}\n{}", probe.join(" "), rendered));
    }
    // The validator, appended: see the header for what the census above cannot
    // see. Its exit code is a fact in its own right, so it is not folded into
    // the success/`<err>` shape the two listings use. [`fsck_section`] is the
    // one copy of it — a peer needs the identical question, and two spellings of
    // it would be two things to keep right.
    out.push_str(&fsck_section(stock, home, gitdir, rel));
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
///
/// "Byte for byte" was not true of the rendering: the bodies went through
/// `String::from_utf8_lossy`, and a preimage is only text when the conflicted
/// file was. `rerere` records a preimage for a binary conflict too — the file's
/// own bytes with markers around them — and two different ones decoded to two
/// equal U+FFFD runs. [`read_as_text`] renders those exactly.
fn probe_rr_cache(repo: &Path) -> String {
    let rr = repo.join(".git").join("rr-cache");
    let mut out = String::from("# rr-cache\n");
    for (rel, path) in walk_files(&rr) {
        out.push_str(&format!("## {rel}\n{}", read_as_text(repo, &path)));
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
    format!("# storage-layout\n{}", storage_of(&repo.join(".git").join("objects")))
}

/// The census itself, for one object directory: the loose count and the elided,
/// sorted listing of `pack/` and `info/`.
///
/// Split out of [`probe_storage`] so [`probe_peer`] can take the same census of
/// the bare peer's object store — the same question, asked of the repository the
/// case pushed *to*, where the answer was previously nobody's.
///
/// # What the census does not read, and what pins it instead
///
/// It reads no object. A loose object is zlib at the writer's chosen level and a
/// pack carries a delta window and an object ordering, so neither one's *bytes*
/// are comparable between two implementations, and that relaxation is
/// permanent. Three things stand in its place, and between them the objects'
/// **content** is pinned even though their bytes are not:
///
///  * The object's *name is the hash of its inflated content*, and
///    `cat-file --batch-check --batch-all-objects` lists every name with its
///    type and its inflated size. Two stores holding different content
///    therefore list different names — **provided something checks that a
///    file's name is the hash of what is in it**.
///  * [`fsck_section`] is that something, and it now runs in the fixture
///    ([`probe_interop`]), in every submodule ([`module_refs`]) and in every
///    peer ([`peer_section`]). Measured: a loose blob rewritten to hold
///    different content under its own name is `blob 2, exit 0` to the census and
///    `hash-path mismatch, exit 3` to `fsck`. `fsck` inflates and re-hashes the
///    objects inside packs too, which is the same guarantee for the packed half.
///  * [`probe_pack_contents`] reads which objects went into which pack out of
///    the `.idx`, which is the one fact about a pack that is neither a
///    compression choice nor already in the batch listing.
///
/// What is left uncompared, deliberately and stated rather than carried
/// silently: the zlib stream of a loose object, the bytes of a pack, and the
/// bytes of an `init` template hook (see [`hook_value`], which compares an
/// installed hook by content and a `*.sample` by length because a sample's
/// bytes describe which git is installed rather than what the command did).
fn storage_of(objects: &Path) -> String {
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

    format!("loose {loose}\n{listing}")
}

/// **Which objects are in which pack**, for every object store this crate can
/// reach: the fixture's own, every peer's, and every submodule's.
///
/// # What was compared, and what that left out
///
/// Object *bytes* are the standing relaxation of this crate: a loose object is
/// zlib at the writer's chosen level, and a pack's bytes carry a delta window,
/// a compression level and an object ordering that the vendored gitoxide does
/// not reproduce. [`storage_of`] therefore counts — loose objects by number,
/// packs by elided name — and `cat-file --batch-check --batch-all-objects`
/// lists the object set with types and sizes.
///
/// Between the two of them, one fact falls straight through: **the partition**.
/// The census says "two packs"; the batch listing says "eleven objects"; nothing
/// says which eleven went into which two. So `repack` splitting a store the
/// wrong way, `gc` putting reachable objects in the cruft pack, `repack -a -d`
/// leaving half the objects in the old pack it should have replaced, and a
/// `clone` whose single pack is missing an object that arrived loose beside it
/// were all one census line and one batch listing — identical to the correct
/// answer.
///
/// # Why the `.idx` is read rather than git asked
///
/// `verify-pack -v` prints the list, and every line of it is
/// `<oid> <type> <size> <size-in-pack> <offset>` plus, for a delta, `<depth>
/// <base>` — of which `size-in-pack`, `offset`, `depth` and `base` are the
/// compression choices this crate has already ruled uncomparable, and `type`
/// and `size` are already in the batch listing. The one new column is the first
/// one, and it is in the pack index verbatim: `\xfftOc`, version 2, a 256-entry
/// fan-out whose last cell is the object count, then that many object ids **in
/// sorted order**. Reading it costs no child process at all — measured against
/// stock 2.55.0, `verify-pack -v` and the ids at offset 1032 of the same `.idx`
/// list the same nine objects.
///
/// Everything after the id table — the CRCs (of compressed data), the offsets,
/// the trailing checksums — is skipped for the reason above.
///
/// # Determinism
///
/// The ids are content and the table is sorted by git, so the rendering is a
/// function of the pack's object set and nothing else. The pack's *name* is a
/// checksum, so it is elided by [`stable_entry_name`] exactly as it is in the
/// census, and the lines are sorted **after** eliding — which means two packs
/// are compared as an unordered pair of object sets rather than by which
/// checksum-named file happens to hold which, and a store that packs the same
/// objects into the same shaped packs matches however the two files are named.
///
/// A store with more objects in one pack than [`PACK_LISTING_MAX`] prints the
/// count and an FNV-1a of the id table instead of the table, on both sides
/// alike: the same pure function of the same bytes, so a difference in the set
/// is still a difference in the line.
///
/// A v1 index, a truncated one, or a repository whose ids are not 20 bytes
/// (SHA-256) reads `<unparsed>` — symmetrically, since both sides are handed
/// the same fixture, which is the same not-a-false-pass argument
/// [`probe_index_meta`] makes for a v4 index.
///
/// # Direction
///
/// A new section appended below every section [`probe_state`] already emits, so
/// no existing line moves. A store with no packs prints its label and nothing
/// else, on both sides.
///
/// # Cost
///
/// No child process, and one `read` of each `.idx` — files that are a few
/// kilobytes in this corpus. Stores with no `pack/` directory cost one failed
/// `read_dir`.
fn probe_pack_contents(repo: &Path) -> String {
    let mut out = String::from("# pack-contents\n");
    let mut stores: Vec<(String, PathBuf)> =
        vec![("self".to_string(), git_dir(repo).join("objects"))];
    // The peers, named exactly as `probe_peer` names them, so a reader can pair
    // the two sections by eye.
    let named = repo.join(PEER_DIR);
    if looks_like_git_dir(&named) {
        stores.push((format!("peer {PEER_DIR}"), named.join("objects")));
    }
    for (name, dir) in other_peers(repo) {
        stores.push((format!("peer {name}"), dir.join("objects")));
    }
    // And the submodules, named as `probe_modules` names them.
    for gitdir in module_gitdirs(&git_dir(repo).join("modules")) {
        let rel = gitdir
            .strip_prefix(git_dir(repo).join("modules"))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        stores.push((format!("module {rel}"), gitdir.join("objects")));
    }
    for (label, objects) in stores {
        out.push_str(&format!("## {label}\n"));
        for line in pack_contents_of(&objects) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// The pack listings of one object store, sorted after eliding; see
/// [`probe_pack_contents`] for why the sort comes second.
fn pack_contents_of(objects: &Path) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for (rel, path) in walk_files(&objects.join("pack")) {
        if !rel.ends_with(".idx") {
            continue;
        }
        lines.push(format!("{}: {}", stable_entry_name(&rel), idx_object_list(&path)));
    }
    lines.sort();
    lines
}

/// Above this many objects in one pack, the listing collapses to a count and a
/// hash of the id table.
///
/// A number rather than no limit at all because the digest is compared line by
/// line and printed in full by `--verbose`: a `clone` of something large would
/// otherwise put a hundred thousand ids on one line of a failure report. Every
/// fixture in this corpus packs tens of objects, so the cap is not reached; it
/// is there so that a future shape cannot make the report unreadable.
const PACK_LISTING_MAX: usize = 1024;

/// The sorted object ids of one pack index v2, as `<count> <oid> <oid> …`.
///
/// The four-byte magic `\xfftOc` distinguishes v2 from the v1 layout, which has
/// no magic at all and interleaves offsets with the ids; git has written v2
/// since 1.5.2 and this returns `<unparsed>` for anything else.
fn idx_object_list(path: &Path) -> String {
    const UNPARSED: &str = "<unparsed>";
    const MAGIC: &[u8] = b"\xfftOc";
    // Magic, version, then 256 four-byte fan-out cells.
    const IDS_AT: usize = 8 + 1024;
    let Ok(bytes) = std::fs::read(path) else { return "<unreadable>".to_string() };
    if bytes.len() < IDS_AT || &bytes[..4] != MAGIC || be32(&bytes, 4) != Some(2) {
        return UNPARSED.to_string();
    }
    // The last fan-out cell is the total object count.
    let Some(count) = be32(&bytes, IDS_AT - 4) else { return UNPARSED.to_string() };
    let count = count as usize;
    let Some(ids) = count.checked_mul(OID_LEN).and_then(|n| bytes.get(IDS_AT..IDS_AT + n)) else {
        return UNPARSED.to_string();
    };
    if count > PACK_LISTING_MAX {
        return format!("{count} objects {:016x}", fnv1a64(ids));
    }
    let mut out = count.to_string();
    for id in ids.chunks(OID_LEN) {
        out.push(' ');
        out.push_str(&hex(id));
    }
    out
}

/// `FETCH_HEAD`, which was excluded from [`OP_STATE_FILES`] and is not
/// "already-measured" after all.
///
/// # Re-reading the exclusion
///
/// The reasoning that kept it out was that a glob over `.git` would sweep in
/// "machine-local scratch and already-measured facts" — `index`,
/// `COMMIT_EDITMSG`, `FETCH_HEAD`, `shallow`, the hook samples. That argument is
/// right about `COMMIT_EDITMSG` and about the sample hooks. It is wrong about
/// `FETCH_HEAD`, on both halves:
///
///  * **Not machine-local.** Its three fields are an object id, the
///    `not-for-merge` marker, and a description ending in the URL *as the case
///    named it* — for the fixture's peer, the relative `./.remote.git`, which
///    git renders with the suffix stripped. Verified against stock 2.55.0:
///    `b015a4d6…\\t\\tbranch 'main' of ./.remote` after `fetch origin`, and the
///    same line with `not-for-merge` in the middle field on the second fetch.
///    No timestamp, no pid, no temp name, and an absolute URL — the one case
///    that names a path outside the fixture — is covered by [`normalize`], which
///    is applied to this digest like every other surface.
///  * **Not already measured.** `git fetch <remote> <ref>` updates no branch,
///    and `--no-write-fetch-head` differs from the default in nothing else at
///    all: `for-each-ref` and `cat-file --batch-all-objects` report the same
///    refs and the same objects either way. For that whole family of
///    invocations `FETCH_HEAD` *is* the result, and the harness was scoring them
///    on their report alone.
///
/// So it is probed, in its own section rather than inside the operation-state
/// list, because it is not operation state: nothing consults it to continue or
/// abort anything.
///
/// # Direction
///
/// One appended line, `<absent>` on a repository that has never fetched, flattened
/// to a single line by [`read_as_value`] like every other file-backed fact.
fn probe_fetch_head(repo: &Path) -> String {
    format!("# fetch-head\nFETCH_HEAD: {}\n", read_as_value(repo, &git_dir(repo).join("FETCH_HEAD")))
}

/// What the **index file itself** is, as opposed to what it holds: its version,
/// its entry count and the extensions chained onto the end of it.
///
/// # The gap
///
/// `ls-files --stage` prints one line per entry — mode, object id, stage, path —
/// and that is the whole of the index this crate saw. Everything *about* the
/// index was invisible: which on-disk version was written (`index.version=2/3/4`
/// are three different file formats for the same entries), and which extensions
/// are chained after the entries — `TREE` (the cache-tree), `REUC` (the
/// resolve-undo record that makes `checkout -m` able to recreate a conflict),
/// `UNTR` (the untracked cache), `link` (a split index), `EOIE`/`IEOT` (the
/// index-offset tables `index.threads` needs), `sdir` (sparse directories).
///
/// [`probe_interop`] catches part of this indirectly and it is worth being exact
/// about which part, because the rest is what this probe is for. It hands the
/// index to stock `write-tree` and reports whether stock had to *repair* it and
/// how the byte length moved, which detects a missing or wrong **cache-tree**
/// and nothing else — and only on a case that wrote under the git directory.
/// What it cannot see: a `REUC` that was dropped (`checkout -m` then silently
/// fails to recreate the conflict, and nothing until the *next* command
/// notices), an `UNTR` that was written when git writes none or left stale, a
/// `link`/`sdir` extension missing on a split or sparse index, the index version
/// itself, and an entry count that disagrees with the entries actually present.
/// This probe reports all of those, on every case, for the cost of one file
/// read.
///
/// # Why the file is parsed here rather than asked of git
///
/// There is no porcelain for it: `ls-files --debug` is the closest and it prints
/// `ctime`, `dev` and `ino` per entry — filesystem facts that differ between the
/// two sides' copies, which is exactly why [`probe_interop`] rejected it. The
/// header and the extension chain, by contrast, are fixed-offset and
/// content-derived. Parsed against the documented layout: `DIRC`, a big-endian
/// version and entry count, then entries of 62 bytes plus a NUL-terminated name
/// padded so each entry is a multiple of eight (64 bytes plus name for a v3
/// entry carrying extended flags), then `(4-byte signature, 4-byte length,
/// data)` triples until the trailing checksum.
///
/// **v4 is not parsed.** Its entry names are prefix-compressed and unpadded, so
/// the extension chain cannot be reached without decompressing every name;
/// version and entry count are still reported and the chain reads
/// `<unparsed>` — on both sides, since both were handed the same fixture. The
/// same fallback covers a truncated or corrupt index and a repository using an
/// object format whose ids are not 20 bytes. Reporting `<unparsed>` symmetrically
/// is a blind spot, not a false pass: it is exactly what was reported before this
/// probe existed.
///
/// # Direction
///
/// Appended, and every value is a function of the index's own bytes rather than
/// of where it sits. Sizes are included with the signatures because a `TREE`
/// that covers fewer subtrees is a smaller `TREE`, and that is the shape of the
/// cache-tree defect this harness has already had once.
fn probe_index_meta(repo: &Path) -> String {
    let git = git_dir(repo);
    let mut lines: Vec<String> = vec![format!("index: {}", index_meta(&git.join("index")))];
    // A split index parks the shared half beside it under a checksum-bearing
    // name; the name is elided the way every other checksum-bearing name in this
    // crate is, and what it *is* is reported.
    if let Ok(rd) = std::fs::read_dir(&git) {
        for entry in rd.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("sharedindex.") {
                lines.push(format!(
                    "{}: {}",
                    stable_entry_name(&name),
                    index_meta(&entry.path())
                ));
            }
        }
    }
    // A linked worktree has an index of its own, and `probe_worktrees` reports
    // only its length — deliberately, because its stat fields are not comparable.
    // Its version and extension chain are.
    if let Ok(rd) = std::fs::read_dir(git.join("worktrees")) {
        for entry in rd.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            lines.push(format!(
                "worktrees/{name}/index: {}",
                index_meta(&entry.path().join("index"))
            ));
        }
    }
    lines.sort();
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    format!("# index-meta\n{body}")
}

/// One index file reduced to `v<version> entries=<n> ext=[SIG:len,…]`.
fn index_meta(path: &Path) -> String {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return "<absent>".to_string(),
        Err(_) => return "<unreadable>".to_string(),
    };
    // 12-byte header plus the trailing checksum is the smallest possible index.
    if bytes.len() < 32 || &bytes[..4] != b"DIRC" {
        return format!("<not-an-index {} bytes>", bytes.len());
    }
    let (Some(version), Some(entries)) = (be32(&bytes, 4), be32(&bytes, 8)) else {
        return format!("<truncated {} bytes>", bytes.len());
    };
    format!("v{version} entries={entries} ext={}", index_extensions(&bytes, version, entries))
}

/// The extension chain of a v2/v3 index: `[TREE:53,REUC:117]`, or `<unparsed>`
/// where the entries cannot be walked to find it.
///
/// `<unparsed>` is returned rather than a guess for every case the layout is not
/// certain about — a v4 index, a hash that is not 20 bytes, a truncated file, a
/// signature that is not four ASCII letters. Both sides return it for the same
/// input, so an unparsed chain reports no difference where there is none.
fn index_extensions(bytes: &[u8], version: u32, entries: u32) -> String {
    const UNPARSED: &str = "<unparsed>";
    if version != 2 && version != 3 {
        return UNPARSED.to_string();
    }
    // The trailing checksum is not part of the chain.
    let end = bytes.len() - 20;
    let mut pos = 12usize;
    for _ in 0..entries {
        // 62 bytes of fixed fields: two timestamps, dev, ino, mode, uid, gid,
        // size, a 20-byte object id and the 2-byte flags.
        if pos + 62 > end {
            return UNPARSED.to_string();
        }
        let flags = u16::from_be_bytes([bytes[pos + 60], bytes[pos + 61]]);
        // Bit 0x4000 is the extended flag: two more bytes before the name.
        let name_at = pos + 62 + if flags & 0x4000 != 0 { 2 } else { 0 };
        let Some(nul) = bytes.get(name_at..end).and_then(|w| w.iter().position(|b| *b == 0)) else {
            return UNPARSED.to_string();
        };
        // Padded with NULs so the whole entry is a multiple of eight.
        let used = name_at + nul + 1 - pos;
        pos += used.div_ceil(8) * 8;
    }
    let mut exts: Vec<String> = Vec::new();
    while pos < end {
        let (Some(sig), Some(len)) = (bytes.get(pos..pos + 4), be32(bytes, pos + 4)) else {
            return UNPARSED.to_string();
        };
        if !sig.iter().all(|b| b.is_ascii_alphabetic()) {
            return UNPARSED.to_string();
        }
        let Some(next) = pos.checked_add(8).and_then(|p| p.checked_add(len as usize)) else {
            return UNPARSED.to_string();
        };
        if next > end {
            return UNPARSED.to_string();
        }
        let body = &bytes[pos + 8..next];
        exts.push(format!("{}:{len}{}", String::from_utf8_lossy(sig), ext_detail(sig, body)));
        pos = next;
    }
    format!("[{}]", exts.join(","))
}

/// What one extension's **body** says, for the extensions whose body is
/// provably a function of repository content.
///
/// # Why this is not "compare the bytes"
///
/// The chain was `SIG:len` and nothing else, so two different `TREE` extensions
/// of the same length compared equal — and a cache tree of a given shape has a
/// fixed length whatever object ids and entry counts it holds, which is to say
/// *every* wrong-but-same-shape cache tree matched. The obvious fix, comparing
/// the raw body, is wrong for most of this chain: half of what an index
/// extension stores is stat data or byte offsets, and demanding those agree
/// would report the inode allocator and the file layout as parity defects.
///
/// So the split is per signature, and the two that are decided here are the two
/// whose every field is derived from tracked content:
///
///  * **`TREE`** — the cache tree (`gitformat-index`, "Cache tree"): per entry a
///    NUL-terminated path component, an ASCII entry count (`-1` for an
///    invalidated subtree), a space, an ASCII subtree count, a newline, and the
///    tree's object id where the count is not negative. Every one of those is a
///    fact about the committed tree. This is the extension `probe_index_meta`
///    was added for and the one it could still only measure by length.
///  * **`REUC`** — resolve-undo (`gitformat-index`, "Resolve undo"): per entry a
///    NUL-terminated path, three NUL-terminated ASCII octal modes, and one
///    object id per non-zero mode. Path, mode and blob id of each conflict
///    stage — again all content.
///
/// # The other five, re-examined
///
/// The previous pass left `UNTR`, `FSMN`, `EOIE`, `IEOT` and `link` at
/// signature and length with one-line reasons. Read against `man 5
/// gitformat-index` on this machine — the authority, not memory — three of the
/// five turn out to carry a **comparable substructure in front of** the part
/// that is not comparable, and a body is not all-or-nothing: a parser can stop.
///
///  * **`UNTR`** (untracked cache) — **now parsed, up to the bitmaps.** The
///    stat data is real and it is second and third in the body, so it is
///    *skipped by width* (two fixed 36-byte `stat_data` records, `struct
///    ondisk_untracked_cache`) rather than being a reason to skip the body.
///    Everything around it is content: the `dir_flags` word, the two hashes of
///    the exclude files (a hash *of file content*, not of stat data — the doc
///    is explicit that a null hash means the file does not exist), the
///    per-directory exclude filename, the directory-block count, and then, per
///    block, the untracked-entry count, the sub-directory count, the directory
///    name and **every untracked filename in it**. That last list is the whole
///    point of the extension and it was invisible. The parse stops at the first
///    of the three trailing ewah bitmaps, and never reaches the per-directory
///    stat array behind them.
///  * **`FSMN`** (fsmonitor) — **version only.** The doc's own ordering is
///    32-bit version first, and only *then* the v1 64-bit nanosecond clock or
///    the v2 opaque token. The clock and the token stop the parse; the version
///    is in front of them and a port that writes v1 where git writes v2 is a
///    difference two equal lengths can hide.
///  * **`link`** (split index) — **null-or-not, whether the bitmaps are there,
///    and now the bit sets they hold.** The shared index's checksum stays
///    unread for the reason already given, but the doc says a *null* object id
///    there is a distinct meaning ("the index does not require a split index"),
///    and a body that is exactly one hash long carries no `delete`/`replace`
///    bitmap at all. Both facts are a function of what the command did, both
///    fit in two words, and neither exposes one byte of the checksum. The two
///    ewah bitmaps behind them are **decoded** — see [`ewah_detail`], and the
///    re-examination in [`split_index_detail`]: an ewah *encoding* is a
///    compression choice, but the bit set it encodes is one bit per shared-index
///    entry in path order, which is content, and "which entries this command
///    deleted and which it replaced" is the whole meaning of a split index.
///  * **`EOIE`** — **left, and for a provable reason rather than a cautious
///    one.** Its two fields are a 32-bit offset to the end of the index
///    entries, which is byte layout, and a hash over "the extension types and
///    their sizes (but not their contents)" — which is to say a hash over
///    exactly the `SIG:len` pairs this chain already prints in full. It cannot
///    report a difference the chain does not already report, so parsing it
///    would add nothing. Re-examined and unchanged.
///  * **`IEOT`** — **version only, and the offset table stays unread after a
///    second look.** Behind the 32-bit version is a table of (offset, entry
///    count) pairs. Both columns were re-examined field by field and neither
///    survives: the offsets are byte positions inside this index file, which is
///    layout by definition; the counts are how the writer chose to *partition*
///    its entries across threads — git takes the block count from
///    `index.threads`, or from the online CPU count when that is `0` — so a
///    correct implementation on a machine with a different core count writes a
///    different table for a byte-identical set of entries. What the table says
///    about the repository is the sum of its counts, and that sum is the entry
///    count in the index header, which [`index_meta`] already prints on its own
///    line. There is nothing left in it that is not one of those two things.
///
/// Every one of the five still contributes its signature and its length,
/// exactly as before. Nothing is removed.
///
/// # Direction
///
/// The detail is **appended inside the element**, after the length, and always
/// opens with `(` — a character no length can contain. Two chains that differed
/// before therefore still differ: the difference was in a signature or in a
/// length, and neither is edited. That is the whole safety argument, and
/// `index_extension_detail_cannot_collapse_a_difference` pins it.
///
/// And the argument holds for a parser that is *wrong*, which is worth stating
/// because three new ones arrive here. Every function below is a pure function
/// of the body, so it can map two different bodies onto one string — a
/// difference not found, which is the direction this file is allowed to fail in
/// — but it can never map one body onto two strings. A reported difference is
/// always a real byte difference. What a parser must never do is *reach* a byte
/// that is legitimately allowed to differ, and that is the one thing each of
/// them is bounded to avoid.
fn ext_detail(sig: &[u8], body: &[u8]) -> String {
    match sig {
        b"TREE" => cache_tree_detail(body),
        b"REUC" => resolve_undo_detail(body),
        b"UNTR" => untracked_cache_detail(body),
        // Both open with a 32-bit version and then stop being comparable, so
        // both take the same four bytes and nothing else.
        b"FSMN" | b"IEOT" => version_detail(body),
        b"link" => split_index_detail(body),
        _ => String::new(),
    }
}

/// git's own variable-width integer, from `varint.c`.
///
/// Not LEB128: the continuation branch adds one to the accumulator before it
/// shifts, so the encodings are dense and a value has exactly one encoding.
/// Ported rather than approximated, because a decoder that is off by one on the
/// second byte would walk the untracked cache into its own stat data.
///
/// Returns the value and the new position, or `None` if the buffer ends inside
/// the number.
fn decode_varint(body: &[u8], pos: usize) -> Option<(u64, usize)> {
    let mut pos = pos;
    let mut c = *body.get(pos)?;
    pos += 1;
    let mut val = u64::from(c & 127);
    while c & 128 != 0 {
        val = val.checked_add(1)?;
        c = *body.get(pos)?;
        pos += 1;
        val = val.checked_shl(7)?.checked_add(u64::from(c & 127))?;
    }
    Some((val, pos))
}

/// The on-disk width of one `struct stat_data`: **36 bytes**, not 40.
///
/// This is the one place the man page will mislead a reader, so the number is
/// justified here rather than cited. `gitformat-index` describes the untracked
/// cache's two records as "Stat data of `$GIT_DIR/info/exclude`. See 'Index
/// entry' section from ctime field until 'file size'" — and that span of an
/// *index entry* is 40 bytes, because an index entry carries a 32-bit mode
/// between `ino` and `uid`. `struct stat_data`, which is what
/// `ondisk_untracked_cache` actually embeds, has no mode field: `ctime` (8),
/// `mtime` (8), `dev`, `ino`, `uid`, `gid`, `size` (4 each) = 36.
///
/// Measured, because guessing wrong here walks the parse straight into the
/// stat data it exists to avoid. Against an untracked cache stock 2.55.0 wrote:
/// at 36 the two records are followed by `dir_flags = 0x00000006`, a real
/// 20-byte hash, a null 20-byte hash and the string `.gitignore`, and the first
/// record reads back `uid = 501`, `gid = 0`, `size = 240` — 240 being the exact
/// length of the `info/exclude` git had just written. At 40 the same body
/// yields `dir_flags = 0x9b10bb92` and a hash with `.gitigno` inside it. The
/// test `untracked_cache_detail_matches_an_index_stock_wrote` pins it.
const STAT_DATA_LEN: usize = 36;

/// One untracked cache as
/// `(flags=00000006 ex=<oid> exf=<oid> perdir=.gitignore blocks=2 <root>=1/1[a.txt] sub=1/0[b.txt])`.
///
/// Everything up to the first ewah bitmap, and not one byte past it. The two
/// leading `stat_data` records are stepped over by width and never rendered:
/// they are `ctime`/`mtime`/`dev`/`ino` for `info/exclude` and for
/// `core.excludesFile`, which differ between two copies of one repository and
/// would report the filesystem as a parity defect. The hashes *of* those two
/// files are rendered, because a hash of file content is content.
///
/// A directory block prints its untracked-entry count, its sub-directory count,
/// its name and its entry names. The count is printed beside the names, so a
/// single name containing a space and two names that do not are still two
/// different strings.
fn untracked_cache_detail(body: &[u8]) -> String {
    const UNPARSED: &str = "(<unparsed>)";
    // The ident block: a byte length, then that many bytes of NUL-terminated
    // strings naming the environment the cache was built in. Skipped rather
    // than printed — it is `git version`, `core.excludesFile` and the like,
    // which is a fact about the two binaries and not about the command.
    let Some((ident_len, pos)) = decode_varint(body, 0) else { return UNPARSED.to_string() };
    let Some(pos) = usize::try_from(ident_len).ok().and_then(|n| pos.checked_add(n)) else {
        return UNPARSED.to_string();
    };
    // Two stat_data records, stepped over: see the header.
    let pos = pos + STAT_DATA_LEN * 2;
    let Some(flags) = body.get(pos..pos + 4) else { return UNPARSED.to_string() };
    let flags = u32::from_be_bytes([flags[0], flags[1], flags[2], flags[3]]);
    let mut pos = pos + 4;
    let mut oids: Vec<String> = Vec::new();
    for _ in 0..2 {
        let Some(oid) = body.get(pos..pos + OID_LEN) else { return UNPARSED.to_string() };
        oids.push(hex(oid));
        pos += OID_LEN;
    }
    let Some(per_dir) = take_cstr(body, &mut pos) else { return UNPARSED.to_string() };
    let mut out = format!(
        "(flags={flags:08x} ex={} exf={} perdir={}",
        oids[0],
        oids[1],
        escape_bytes(per_dir)
    );
    let Some((blocks, next)) = decode_varint(body, pos) else {
        out.push_str(" <unparsed>)");
        return out;
    };
    pos = next;
    out.push_str(&format!(" blocks={blocks}"));
    for _ in 0..blocks {
        let Some((untracked_nr, next)) = decode_varint(body, pos) else {
            out.push_str(" <unparsed>)");
            return out;
        };
        pos = next;
        let Some((dirs_nr, next)) = decode_varint(body, pos) else {
            out.push_str(" <unparsed>)");
            return out;
        };
        pos = next;
        let Some(name) = take_cstr(body, &mut pos) else {
            out.push_str(" <unparsed>)");
            return out;
        };
        let name = if name.is_empty() { "<root>".to_string() } else { escape_bytes(name) };
        let mut names: Vec<String> = Vec::new();
        for _ in 0..untracked_nr {
            let Some(entry) = take_cstr(body, &mut pos) else {
                out.push_str(" <unparsed>)");
                return out;
            };
            names.push(escape_bytes(entry));
        }
        out.push_str(&format!(" {name}={untracked_nr}/{dirs_nr}[{}]", names.join(" ")));
    }
    // The three ewah bitmaps and the per-directory stat array behind them start
    // here and are deliberately not read.
    out.push(')');
    out
}

/// The leading 32-bit version word of an extension whose body stops being
/// comparable immediately behind it, as `(v2)`.
///
///  * **`FSMN`** — behind the version is a 64-bit nanosecond clock reading (v1)
///    or an opaque token the file system monitor defines (v2), then a bitmap of
///    the entries the monitor has invalidated. None of the three is a function
///    of the repository, and the version is in front of all of them.
///  * **`IEOT`** — behind the version is a table of (byte offset, entry count)
///    pairs. The offsets are layout and the partition is the writer's threading
///    choice; the counts sum to the entry count `index_meta` already prints.
///
/// A shared reader rather than one per signature, because it is the same four
/// bytes read the same way and a second copy of it would be a second thing to
/// keep right.
fn version_detail(body: &[u8]) -> String {
    match body.get(..4) {
        Some(v) => format!("(v{})", u32::from_be_bytes([v[0], v[1], v[2], v[3]])),
        None => "(<unparsed>)".to_string(),
    }
}

/// One split-index link as `(base=set bitmaps=yes del=0:[] rep=3:[0 1 2])`.
///
/// The shared index's checksum is read only to ask whether it is the null id,
/// which is the format's way of saying "no shared index": its actual value is a
/// checksum over bytes that include per-entry stat data and stays unrendered.
/// `bitmaps` is whether the `delete`/`replace` pair is present at all, which
/// git omits when there is nothing to record.
///
/// # The two bitmaps, re-examined
///
/// They were left at `bitmaps=yes` on the grounds that an ewah bitmap is a
/// compressed encoding. The encoding is; **the bit set it encodes is not**.
/// `gitformat-index` says what each bit means — "each bit represents an entry
/// in the shared index. If a bit is set, its corresponding entry in the shared
/// index will be removed" for `delete`, "…will be replaced with an entry in
/// this index file" for `replace` — and the shared index's entries are sorted
/// by path, so a bit position is a *path*, which is content. Which entries a
/// command deleted and which it replaced is the entire meaning of a split
/// index, and it was one word.
///
/// The **decoded set** is rendered and the encoding is not, which is the
/// stricter of the two readings in the only direction that matters: two
/// encoders that spell the same bit set with a different number of words
/// compare equal, and two different bit sets cannot.
fn split_index_detail(body: &[u8]) -> String {
    let Some(oid) = body.get(..OID_LEN) else { return "(<unparsed>)".to_string() };
    let base = if oid.iter().all(|b| *b == 0) { "null" } else { "set" };
    let bitmaps = if body.len() > OID_LEN { "yes" } else { "no" };
    let mut out = format!("(base={base} bitmaps={bitmaps}");
    let mut pos = OID_LEN;
    if body.len() > OID_LEN {
        for label in ["del", "rep"] {
            match ewah_detail(body, &mut pos) {
                Some(bits) => out.push_str(&format!(" {label}={bits}")),
                None => {
                    out.push_str(&format!(" {label}=<unparsed>"));
                    break;
                }
            }
        }
    }
    out.push(')');
    out
}

/// The largest bit count [`ewah_detail`] will decode: one bit per shared-index
/// entry, and no index in this corpus has a thousandth of that. A body claiming
/// more is a misparse rather than a bitmap, and is reported as one.
const EWAH_BIT_MAX: usize = 1 << 20;

/// One serialised ewah bitmap at `*pos`, as `<bit count>:[<set positions>]`,
/// advancing `*pos` past it.
///
/// # The layout, measured rather than recalled
///
/// `gitformat-index` describes the split-index bitmaps as "ewah-encoded" and
/// stops there; no installed man page on this machine states the serialisation.
/// So it was read off an index **stock 2.55.0 wrote** — three files added and
/// committed, then `update-index --split-index` — whose 68-byte `link` body is
///
/// ```text
///   6981…0dee                                   20  the shared index's id
///   00000000 00000001 0000000000000000 00000000 20  delete: 0 bits, 1 word
///   00000003 00000002 0000000200000000
///                     0000000000000007 00000000 28  replace: 3 bits, 2 words
/// ```
///
/// which fixes the layout as a 32-bit bit count, a 32-bit word count, that many
/// **big-endian 64-bit** words, and a trailing 32-bit position that is an index
/// into the words rather than data. 20 + 20 + 28 = 68, the length the extension
/// header declares.
///
/// The words are run-length encoded: each *run word* carries the repeated bit
/// in position 0, the number of repetitions in positions 1..=32, and the number
/// of literal words that follow it in positions 33..=63. That split is not a
/// guess either — it is forced by the same body. The replace bitmap's run word
/// is `0x0000000200000000`, and the file says two words in total, so exactly one
/// literal word follows it: `>> 33` yields 1 and `>> 32` yields 2, and only the
/// first is consistent with the word count. The literal that follows is
/// `0x…0007`, which is bits 0, 1 and 2 — three entries replaced, in a bitmap
/// whose declared bit count is 3.
///
/// # Direction
///
/// A pure function of the body, like every other parser in this chain: it can
/// map two different bodies onto one string, which is a difference not found,
/// and it can never map one body onto two. A reported difference is always a
/// real byte difference. The walk is bounded by the declared word count and by
/// the body's own length, and it reads nothing past the bitmap it was asked
/// for.
fn ewah_detail(body: &[u8], pos: &mut usize) -> Option<String> {
    let bits = be32(body, *pos)? as usize;
    let words = be32(body, *pos + 4)? as usize;
    // A declared bit count larger than any index could have entries is a
    // misparse, not a bitmap; refusing it keeps the decode bounded by a number
    // the body cannot make arbitrary.
    if bits > EWAH_BIT_MAX {
        return None;
    }
    let start = pos.checked_add(8)?;
    let end = words.checked_mul(8).and_then(|n| start.checked_add(n))?;
    // The trailing run-word position is layout, not content: read only to step
    // past it.
    let after = end.checked_add(4)?;
    if after > body.len() {
        return None;
    }
    let word = |i: usize| -> u64 {
        let at = start + i * 8;
        u64::from_be_bytes(body[at..at + 8].try_into().unwrap_or([0; 8]))
    };
    let mut set: Vec<usize> = Vec::new();
    let mut bit = 0usize;
    let mut i = 0usize;
    while i < words && bit < bits {
        let rlw = word(i);
        i += 1;
        let run_value = rlw & 1;
        let run_len = ((rlw >> 1) & 0xffff_ffff) as usize;
        let literals = (rlw >> 33) as usize;
        for _ in 0..run_len {
            // A run may legitimately declare more clean words than the declared
            // bit count needs; past it there is nothing to record and the walk
            // stops rather than counting to four billion.
            if bit >= bits {
                break;
            }
            if run_value == 1 {
                for b in 0..64 {
                    if bit + b < bits {
                        set.push(bit + b);
                    }
                }
            }
            bit += 64;
        }
        for _ in 0..literals {
            if i >= words {
                break;
            }
            let lit = word(i);
            i += 1;
            for b in 0..64 {
                if lit >> b & 1 == 1 && bit + b < bits {
                    set.push(bit + b);
                }
            }
            bit += 64;
        }
    }
    *pos = after;
    let rendered: Vec<String> = set.iter().map(usize::to_string).collect();
    Some(format!("{bits}:[{}]", rendered.join(" ")))
}

/// One NUL-terminated string from `body` at `pos`, advancing `pos` past the NUL.
///
/// Shared by the three parsers that walk a NUL-separated body, rather than a
/// closure re-written in each: [`resolve_undo_detail`] had one, and the
/// untracked cache walks four kinds of them.
fn take_cstr<'a>(body: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    let nul = body.get(*pos..)?.iter().position(|b| *b == 0)?;
    let out = &body[*pos..*pos + nul];
    *pos += nul + 1;
    Some(out)
}

/// Hash length assumed by the extension parsers, matching the entry walk in
/// [`index_extensions`]: SHA-1. A SHA-256 repository has 32-byte ids, and every
/// parser here reports `<unparsed>` on it rather than a misread — both sides
/// alike, so an unparsed body reports no difference where there is none.
const OID_LEN: usize = 20;

/// One cache tree as `(<root>=6/1:<oid>,src=2/0:<oid>)`.
///
/// `-1` entry counts are kept verbatim — an invalidated subtree is precisely the
/// fact this crate's interop probe found the port getting wrong — and carry no
/// object id, which is what the format says.
fn cache_tree_detail(body: &[u8]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut pos = 0usize;
    while pos < body.len() {
        let Some(nul) = body[pos..].iter().position(|b| *b == 0) else {
            return "(<unparsed>)".to_string();
        };
        let name = &body[pos..pos + nul];
        pos += nul + 1;
        let Some(lf) = body[pos..].iter().position(|b| *b == b'\n') else {
            return "(<unparsed>)".to_string();
        };
        let header = &body[pos..pos + lf];
        pos += lf + 1;
        let Ok(header) = std::str::from_utf8(header) else {
            return "(<unparsed>)".to_string();
        };
        let Some((count, subtrees)) = header.split_once(' ') else {
            return "(<unparsed>)".to_string();
        };
        let (Ok(count), Ok(subtrees)) = (count.parse::<i64>(), subtrees.parse::<i64>()) else {
            return "(<unparsed>)".to_string();
        };
        let name = if name.is_empty() { "<root>".to_string() } else { escape_bytes(name) };
        if count < 0 {
            out.push(format!("{name}={count}/{subtrees}"));
            continue;
        }
        let Some(oid) = body.get(pos..pos + OID_LEN) else {
            return "(<unparsed>)".to_string();
        };
        pos += OID_LEN;
        out.push(format!("{name}={count}/{subtrees}:{}", hex(oid)));
    }
    format!("({})", out.join(" "))
}

/// One resolve-undo extension as `(a.txt=100644:<oid>|100644:<oid>|100644:<oid>)`,
/// one `|`-separated field per conflict stage and `0` where a stage is absent.
fn resolve_undo_detail(body: &[u8]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut pos = 0usize;
    while pos < body.len() {
        let Some(path) = take_cstr(body, &mut pos) else { return "(<unparsed>)".to_string() };
        let mut stages: Vec<String> = Vec::new();
        let mut modes: Vec<String> = Vec::new();
        for _ in 0..3 {
            let Some(mode) = take_cstr(body, &mut pos) else { return "(<unparsed>)".to_string() };
            modes.push(escape_bytes(mode));
        }
        for mode in &modes {
            if mode == "0" {
                stages.push("0".to_string());
                continue;
            }
            let Some(oid) = body.get(pos..pos + OID_LEN) else {
                return "(<unparsed>)".to_string();
            };
            pos += OID_LEN;
            stages.push(format!("{mode}:{}", hex(oid)));
        }
        out.push(format!("{}={}", escape_bytes(path), stages.join("|")));
    }
    format!("({})", out.join(" "))
}

/// Lower-case hex, for the object ids the extension parsers read raw.
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// The **peer repository** the case pushed to, fetched from, or pruned.
///
/// # The gap
///
/// `Shape::BehindRemote` carries a real bare repository inside the fixture at
/// `.remote.git`, reached by the relative URL `./.remote.git` so each side's copy
/// talks to its own. Every probe in this crate runs in the fixture root and
/// never descends, and the fixture builder writes `.remote.git/` into
/// `.git/info/exclude` so even `status --untracked-files=all` says nothing about
/// it. The consequence: `push`, `send-pack` and `fetch --prune` were measured on
/// the report they printed and on the local refs they updated — a port that
/// printed a correct report and wrote *nothing at all* on the peer matched
/// identically. Three `send-pack` cases pass today and are not evidence of
/// anything having arrived.
///
/// # What is asked, and of whom
///
/// **Stock git, run inside the peer**, exactly as [`probe_state`] runs it inside
/// the fixture — never the binary under test. Two invocations:
///
///  * `for-each-ref` — did the ref move, and to what. This is the whole point of
///    a push.
///  * `cat-file --batch-check --batch-all-objects` — did the objects the ref
///    names actually arrive, with the right types and sizes. A ref update
///    without the objects behind it is the failure that leaves a peer corrupt,
///    and `for-each-ref` alone would call it a success.
///
/// Both were verified to work with the working directory set to a bare
/// repository under the same hardened environment (`for-each-ref` and `cat-file`
/// against `.remote.git` returned the ref and the object listing, exit 0).
///
/// Everything else is read from the filesystem and costs no process: `HEAD`
/// (`receive-pack` on an empty peer points it at the branch that arrives), the
/// object-store census [`storage_of`] — so a push that lands a pack instead of
/// loose objects is visible — and the peer's reflogs.
///
/// # Cost, and the gate
///
/// Two child processes per side, **per peer, only where a peer exists**. The gate
/// is the directory being shaped like a repository (`HEAD` + `objects/` +
/// `refs/`); a fixture with nothing but its own repository pays one `is_file`/
/// `is_dir` triple for the named peer, one `read_dir` of the fixture root for the
/// rest (see [`other_peers`]), and prints `<absent>` on both sides.
///
/// The name is no longer the gate. It used to be — `.remote.git` and nothing
/// else — and [`other_peers`] documents what that missed.
///
/// # Determinism
///
/// Every fact is content-derived: object ids and ref names are what both sides
/// pushed, `cat-file`'s sizes are the objects' own, and the pack census counts
/// rather than compares bytes, which is the relaxation [`probe_storage`] already
/// makes for the same reason. Nothing here reads a path, an inode, a timestamp
/// or a pid.
///
/// # Direction
///
/// A new section appended to the digest, identical in shape on both sides,
/// containing `<absent>` wherever there is no peer. Nothing above it moves. It
/// can only ever add differences — and the named peer keeps the first section,
/// so the digests written before [`other_peers`] existed are a prefix of the
/// ones written now.
fn probe_peer(repo: &Path, home: &Path) -> String {
    // The named peer first and unconditionally, so the section every existing
    // digest already carries keeps its content and its position — including the
    // `<absent>` line, which is what a fixture without one reports.
    let mut out = peer_section(home, PEER_DIR, &repo.join(PEER_DIR), repo);
    for (name, dir) in other_peers(repo) {
        out.push_str(&peer_section(home, &name, &dir, repo));
    }
    out
}

/// Every repository inside the fixture root that is **not** the fixture's own
/// and not the named peer.
///
/// # The gap this closes
///
/// [`probe_peer`]'s gate was the literal name `.remote.git`, which is where the
/// *fixture builder* puts a peer — and nothing stops a case from making one
/// somewhere else. `clone . copy`, `init --bare other.git`, `push
/// ../elsewhere.git`, `worktree add`-adjacent shapes: each leaves a second
/// repository in the fixture root that no probe read, so the command that
/// created it was measured on its report and on its exit code. A `clone` that
/// prints its counting line and writes an empty repository matched a correct
/// one.
///
/// # The two shapes a peer takes
///
/// A direct child directory of the fixture root is a peer when it is either
///
///  * **bare** — the directory itself has `HEAD` + `objects/` + `refs/`, which is
///    `.remote.git` and every `init --bare`/`clone --bare` target; or
///  * **a working clone** — the directory holds a `.git` that has them, which is
///    what a plain `clone` writes.
///
/// The fixture's own git directory is excluded by identity rather than by name,
/// so a bare fixture — where `git_dir(repo) == repo` — cannot probe itself.
///
/// # Depth: anywhere in the fixture, not just a direct child
///
/// The previous pass replaced a hard-coded *name* with a shape test and left a
/// hard-coded *depth*: one `read_dir` of the fixture root. `clone . a/b`,
/// `init --bare nested/other.git`, `push ./sub/peer.git` and a `worktree add`
/// under a subdirectory all put a repository one level further down than that,
/// and each was measured on its report and its exit code exactly as
/// `.remote.git` used to be.
///
/// The walk now recurses, and it is bounded by the two things it will not enter
/// rather than by a depth number:
///
///  * **It never descends into a repository it has found.** A peer's own
///    objects, refs and modules belong to [`peer_section`] and to
///    [`storage_of`], which are asked about it as a unit. So the recursion runs
///    over the fixture's ordinary directories only, which is the same set
///    [`collect_worktree`] already walks — it cannot walk into an object store
///    and it cannot walk into `.git`.
///  * **It never follows a symlink**, for the reason the direct-child version
///    already gave: `Shape::Symlinks` points one outside the fixture on
///    purpose.
///
/// A peer's name is now its path relative to the fixture root, each component
/// put through [`stable_entry_name`] separately. A direct child's rendering is
/// therefore unchanged — one component, one elision — so every section that
/// existed before keeps its name and its position.
///
/// # What is still unprobed, and why it is not an oversight
///
/// **A repository outside the fixture root.** It stays unprobed, and it must:
/// the fixture root's parent is the worker's own directory, and it holds *the
/// other side's repository* (`run_case` instantiates `<workdir>/stock` and
/// `<workdir>/zvcs` as siblings), both sides' repeat copies, both sides'
/// submodule upstreams and the interop scratch. A probe that walked up one
/// level would splice the port's repository into stock's digest and stock's
/// into the port's, which is not a stricter measurement but a meaningless one.
/// The boundary is therefore the fixture root, and a case that writes outside
/// it is writing into shared space that the harness cannot attribute to either
/// side — a corpus problem, not a probe problem.
///
/// # Cost
///
/// One `read_dir` per ordinary directory in the fixture, and at most six `stat`
/// calls per directory entry. The fixture worktrees this corpus builds are tens
/// of files across a handful of directories, and [`probe_worktree_content`]
/// already walks the same tree; no child process is spawned until a directory
/// has been established to be a repository.
///
/// # Determinism
///
/// Sorted by rendered name, and the name is put through [`stable_entry_name`]
/// per component for the same reason `probe_storage` does it — a peer created
/// under a `mkstemp` or pid-bearing name would otherwise make every such case
/// flaky. Both sides walk the same fixture, so the section order is the same on
/// both.
fn other_peers(repo: &Path) -> Vec<(String, PathBuf)> {
    let own = git_dir(repo);
    let named = repo.join(PEER_DIR);
    // A **bare** fixture is its own git directory, so its children are `objects`,
    // `refs` and `logs` — a repository's insides, not a worktree. There is
    // nowhere for a case to have put a peer *below* them, and descending would
    // walk the object store's 256 fan-out directories looking for one. So the
    // scan stays at the root there, exactly as it was before this recursion
    // existed; `probe_worktree_content` states the same fact in one line
    // (`<bare: no worktree>`).
    let descend = own != repo;
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    collect_peers(repo, "", &own, &named, descend, &mut found);
    found.sort();
    found
}

/// One directory of the [`other_peers`] walk.
///
/// `prefix` is the path already walked, relative to the fixture root, so a peer
/// two levels down is named `a/b` and a direct child is named exactly as it was
/// before this recursion existed.
fn collect_peers(
    dir: &Path,
    prefix: &str,
    own: &Path,
    named: &Path,
    descend: bool,
    found: &mut Vec<(String, PathBuf)>,
) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        // The fixture's own peer is excluded by *path*, not by name, so a second
        // repository that happens to be called `.remote.git` further down is
        // still a peer. It keeps the first section, which `probe_peer` emits
        // unconditionally.
        if path == *named {
            continue;
        }
        // `symlink_metadata`, so a link is never followed: `Shape::Symlinks`
        // contains one that points outside the fixture on purpose, and a probe
        // that followed it could end up reading a repository on the machine
        // rather than one the case created.
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if path == *own || !meta.is_dir() {
            continue;
        }
        let rel = if prefix.is_empty() {
            stable_entry_name(&name)
        } else {
            format!("{prefix}/{}", stable_entry_name(&name))
        };
        if looks_like_git_dir(&path) {
            // Found, and not descended into: what is inside a repository is
            // `peer_section`'s question, asked of it as a unit.
            found.push((rel, path));
            continue;
        }
        // Not a repository itself, so it is walked. A working clone's `.git` is
        // found by this recursion rather than by a special case for it: one
        // level down, `.git` is a directory that `looks_like_git_dir` accepts,
        // and it comes out named `<clone>/.git` — the same string the
        // direct-child version produced for it. The fixture's own `.git` is
        // excluded by the `path == *own` test above, at the level it appears.
        if descend {
            collect_peers(&path, &rel, own, named, descend, found);
        }
    }
}

/// One peer, asked the questions [`probe_peer`] documents.
fn peer_section(home: &Path, name: &str, peer: &Path, root: &Path) -> String {
    const PROBES: &[&[&str]] = &[
        &["for-each-ref", "--format=%(refname) %(objecttype) %(objectname)"],
        &["cat-file", "--batch-check", "--batch-all-objects"],
    ];
    let mut out = format!("# peer {name}\n");
    if !looks_like_git_dir(peer) {
        out.push_str(if peer.exists() { "<not a repository>\n" } else { "<absent>\n" });
        return out;
    }
    out.push_str(&format!("HEAD: {}\n", read_as_value(root, &peer.join("HEAD"))));

    let Ok(stock) = crate::stock::git() else {
        out.push_str("<no-stock-git>\n");
        return out;
    };
    for probe in PROBES {
        let mut cmd = Command::new(stock);
        env::harden(&mut cmd, home);
        cmd.current_dir(peer).args(*probe);
        let rendered = match cmd.output() {
            Ok(o) if o.status.success() => decode_exact(o.stdout),
            Ok(_) => "<err>\n".to_string(),
            Err(_) => "<spawn-failed>\n".to_string(),
        };
        out.push_str(&format!("## {}\n{}", probe.join(" "), rendered));
    }
    out.push_str("## storage\n");
    out.push_str(&storage_of(&peer.join("objects")));
    out.push_str("## reflogs\n");
    out.push_str(&reflog_listing(root, &peer.join("logs")));
    out.push_str(&fsck_section(stock, home, peer, &format!("peer {name}")));
    out
}

/// `fsck --strict --no-progress --no-dangling` run by **stock git** inside one
/// repository, as `exit:` plus everything it said on both streams.
///
/// # The hole this closes, and it is the same hole twice
///
/// [`module_refs`] earned this question inside `.git/modules/**` and stated why:
/// the object census believes the filenames. A loose blob rewritten to hold
/// different content *under its own name* reads as `blob 2, exit 0` to
/// `cat-file --batch-check --batch-all-objects`, which takes the id from the
/// path, and as `hash-path mismatch, exit 3` to `fsck`, which takes it from the
/// bytes. Every peer had the census and not the validator, so the identical
/// hole sat under `.remote.git` and under every repository [`other_peers`]
/// finds — which is to say under every `push`, `send-pack`, `clone` and
/// `fetch --prune` target in this corpus.
///
/// It is not reachable from anywhere else, either. [`probe_interop`] runs
/// `fsck` in the *fixture*, and [`git_fingerprint`] — the gate that decides
/// whether it runs at all — walks `git_dir(repo)` and nothing else, so a `push`
/// that writes only into the peer does not open it. The peer's object store had
/// no validator on any code path.
///
/// # What the validator sees that a census cannot
///
/// Measured against stock 2.55.0, not recalled: `fsck --strict` verifies more
/// than loose-object naming. A commit-graph whose CDAT chunk names the wrong
/// root tree, with the trailing checksum re-stamped so the file is internally
/// consistent, produces
/// `root tree OID for commit 13168c53… in commit-graph is b7130d30…faa8… !=
/// b7130d30…f9a8…` and exit 16; a `multi-pack-index` with one byte moved and its
/// checksum re-stamped produces `fatal: bad pack-int-id: 16777216 (1 total
/// packs)` and exit 32; a `.bitmap` with one byte moved produces
/// `error: bitmap file … has invalid checksum` and exit 128. So one invocation
/// covers the accelerators the census names but never opens.
///
/// # Determinism
///
/// The paths `fsck` prints are relative to the directory it ran in
/// (`./objects/28/…`), and the ids it prints are content. [`normalize`] is
/// applied to this digest like every other surface, so an absolute path in a
/// message that carries one is masked as it is everywhere else.
///
/// # Direction
///
/// Appended below every section the peer already had, so nothing that was
/// compared before is compared differently. A healthy store prints
/// `exit: Some(0)` and nothing else, on both sides.
///
/// # Cost
///
/// One stock invocation per peer per side, and only where a peer exists — the
/// same gate [`peer_section`] already pays two invocations behind.
fn fsck_section(stock: &Path, home: &Path, dir: &Path, label: &str) -> String {
    let mut cmd = Command::new(stock);
    env::harden(&mut cmd, home);
    cmd.current_dir(dir).args(["fsck", "--strict", "--no-progress", "--no-dangling"]);
    let mut out = format!("## {label} fsck --strict\n");
    match cmd.output() {
        Ok(o) => {
            out.push_str(&format!("exit: {:?}\n", o.status.code()));
            out.push_str(&decode_exact(o.stdout));
            out.push_str(&decode_exact(o.stderr));
        }
        Err(_) => out.push_str("<spawn-failed>\n"),
    }
    out
}

/// Where every fixture that has a peer keeps it: a bare repository inside the
/// fixture root, so the per-case copy carries it and the relative URL in
/// `remote.origin.url` resolves to the copy's own.
const PEER_DIR: &str = ".remote.git";

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
///
/// **The masking happens on bytes, and output that is not text is rendered
/// exactly.** See [`mask_paths`] for the hole that closes — a lossy decode here
/// made two different binary streams compare equal — and [`render_binary`] for
/// what a non-text surface looks like in the digest. Text output is unaffected:
/// on valid UTF-8 the byte substitution and the string substitution produce the
/// same bytes, so nothing that was compared before is compared differently now.
fn normalize(raw: &[u8], repo: &Path, home: &Path, exec_dir: &Path) -> String {
    let masked = mask_paths(raw, repo, home, exec_dir);
    match String::from_utf8(masked) {
        Ok(text) => text,
        Err(e) => render_binary(e.as_bytes()),
    }
}

/// The masking itself, on **bytes**.
///
/// This used to run on `String::from_utf8_lossy(raw)`, and that one call was a
/// hole under every comparison in this crate: `from_utf8_lossy` maps *every*
/// invalid byte to U+FFFD, so two different binary streams collapse to one
/// string and compare equal. Measured on the smallest possible example —
/// `1f 8b c0 80 41` against `1f 8b c1 80 41`: raw equal `False`, lossy equal
/// `True`, both rendering `'\x1f\u{fffd}\u{fffd}\u{fffd}A'`. Every case whose
/// stdout is binary was therefore compared through a filter that erases the
/// differences: `pack-objects --stdout`, `bundle create -`, `archive
/// --format=zip|tar.gz`, `cat-file blob` on a binary object, `fast-export` and
/// `upload-archive`. The byte differences this harness *did* find in that family
/// were the ones that happened to also move a valid-UTF-8 byte or the length.
///
/// Masking on bytes is the direct translation of what the string version did and
/// keeps the same three substitutions in the same order — exec-path first,
/// because on the zvcs side it lives under `home` and masking `home` first would
/// rewrite its prefix. The needles are the paths' own bytes
/// (`OsStr::as_bytes`) rather than their lossy renderings, so a path is masked
/// wherever it appears, including inside a binary stream.
fn mask_paths(raw: &[u8], repo: &Path, home: &Path, exec_dir: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let mut out = raw.to_vec();
    for (path, token) in [(exec_dir, "<EXEC-PATH>"), (repo, "<REPO>"), (home, "<HOME>")] {
        let needle = path.as_os_str().as_bytes();
        if needle.is_empty() {
            continue;
        }
        out = replace_bytes(&out, needle, token.as_bytes());
        // Both the symlinked and resolved forms show up on macOS (/tmp vs /private/tmp).
        if let Ok(canon) = path.canonicalize() {
            out = replace_bytes(&out, canon.as_os_str().as_bytes(), token.as_bytes());
        }
    }
    out
}

/// Every occurrence of `needle` in `hay` replaced by `with`.
///
/// Naive, and deliberately: the needles are three filesystem paths of a few
/// dozen bytes each and the haystacks are one command's output, so the scan is
/// linear in practice and there is no dependency to justify.
fn replace_bytes(hay: &[u8], needle: &[u8], with: &[u8]) -> Vec<u8> {
    let n = needle.len();
    if n == 0 || hay.len() < n {
        return hay.to_vec();
    }
    let mut out = Vec::with_capacity(hay.len());
    let mut i = 0;
    while i + n <= hay.len() {
        if hay[i] == needle[0] && &hay[i..i + n] == needle {
            out.extend_from_slice(with);
            i += n;
        } else {
            out.push(hay[i]);
            i += 1;
        }
    }
    out.extend_from_slice(&hay[i..]);
    out
}

/// Output that is not text, rendered **exactly** rather than lossily.
///
/// Hex, 32 bytes to a line, under a header naming the length. Exact is the whole
/// point — a rendering that can map two different streams onto one string is the
/// defect this replaces — and hex-per-line rather than one long string because
/// `report.rs` pairs the two sides' surfaces by line position, so a human reading
/// a failure is shown the 32-byte window that moved rather than "the output
/// differs".
///
/// Above the cap the length and an FNV-1a fingerprint stand in. That is a hash
/// and hashes can collide, so it is a bounded weakening — bounded at
/// 2^-64 per pair, against a rendering that today collides on *every* pair of
/// binary streams — and it is reachable only by a case whose stdout exceeds a
/// megabyte, which no fixture in this crate can produce (the largest blob any
/// shape builds is 1 KiB).
fn render_binary(bytes: &[u8]) -> String {
    /// Above this many bytes, the exact rendering is traded for a fingerprint.
    const HEX_CAP: usize = 1 << 20;
    if bytes.len() > HEX_CAP {
        return format!("<non-utf8 stream: {} bytes, fnv1a {:016x}>\n", bytes.len(), fnv1a(bytes));
    }
    let mut out = format!("<non-utf8 stream: {} bytes>\n", bytes.len());
    for chunk in bytes.chunks(32) {
        for b in chunk {
            out.push_str(&format!("{b:02x}"));
        }
        out.push('\n');
    }
    out
}

/// FNV-1a, 64-bit. Used only for the over-the-cap case in [`render_binary`].
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
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

/// The second oracle's exec-path, resolved once for the whole run.
///
/// Its own resolution rather than reusing the primary's, and that is load-bearing
/// rather than tidy: the two gits are installed in different trees
/// (`/Library/Developer/CommandLineTools/usr/libexec/git-core` against
/// `/opt/homebrew/libexec/git-core`), and several commands print their exec-path.
/// Normalizing the second oracle's output against the *first* oracle's exec-path
/// would leave that string unmasked in exactly one of the three answers, and the
/// harness would report an install-location difference as a version difference on
/// every case that mentions it.
fn alt_exec_dir(bin: &Path, home: &Path) -> &'static Path {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| exec_path_of(bin, home))
}

/// Whether `--alt-git-every-case` was given: ask the second oracle about every
/// case, not only the ones that already failed.
///
/// A process-wide flag rather than another argument threaded through six call
/// sites, for the same reason the exec-paths above are: it is fixed before the
/// first case runs and read identically by every worker. `set_alt_every_case` is
/// called once from `main`; a second call is ignored, because this is a knob and
/// aborting a five-minute sweep over a duplicated setter trades a real
/// measurement for a tidy one.
static ALT_EVERY_CASE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

pub fn set_alt_every_case(on: bool) {
    let _ = ALT_EVERY_CASE.set(on);
}

fn alt_every_case() -> bool {
    *ALT_EVERY_CASE.get().unwrap_or(&false)
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
    /// What stock git — and the binary under test — made of the finished
    /// repository: [`probe_interop`]'s digest, or [`INTEROP_UNPROBED`] on a case
    /// whose gate stayed shut. The marker is identical on both sides, so an
    /// unprobed case can never be an interop difference.
    pub interop: &'a str,
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
    // Below the state comparison and above the message one, which is where it
    // belongs on both sides. A repository whose *contents* differ is the larger
    // finding and the interop difference would be its consequence, so state
    // wins; a repository stock has to repair is a larger finding than prose the
    // harness's standing policy says is not a compatibility surface at all, so
    // this wins over stderr.
    } else if stock.interop != zvcs.interop {
        Verdict::InteropDiff
    } else if compare_stderr && stock.stderr != zvcs.stderr {
        Verdict::StderrDiff
    } else {
        Verdict::Match
    }
}

/// Whether a second oracle has anything to say about this verdict.
///
/// The gate on the whole dimension, and it is about *meaning* before it is about
/// cost. A second git can only adjudicate a finding it produced a comparable
/// answer for, so the set is exactly the content differences:
///
///   * [`Verdict::ExitDiff`], [`Verdict::StdoutDiff`], [`Verdict::StateDiff`],
///     [`Verdict::StderrDiff`] — the port said something, the oracle said
///     something else, and a second oracle's answer settles which of the two the
///     rest of the world says. Included.
///   * [`Verdict::Unsupported`] — the port refused. It matched no git by
///     construction, so no three-way outcome can change the verdict; the second
///     oracle could only add to the disagreement listing, and buying an
///     invocation per unported command for that is the wrong trade.
///   * [`Verdict::Crash`], [`Verdict::Hang`] — the port crashed or never
///     answered. No git's behaviour exculpates that.
///   * [`Verdict::InteropDiff`] — the finding lives in a digest [`AltRun`] does
///     not carry, and could not sensibly carry: see its doc.
///   * [`Verdict::Nondeterministic`], [`Verdict::StockTimeout`] — there is no
///     stable primary answer to be a second opinion *about*.
///   * [`Verdict::ZvcsNondeterministic`] — the port does not agree with itself,
///     so which of its two answers would a third binary be compared against?
///   * [`Verdict::VersionSkew`] — already adjudicated; it is this function's own
///     output.
///   * [`Verdict::Match`] — the port matched the primary oracle, so nothing is in
///     dispute. `--alt-git-every-case` overrides this one and only this one,
///     because it buys the single thing the gate cannot see: a case where the
///     port matches 2.55.0 and the older git would have said something else.
///     That is a fact about *the corpus*, not about the port — it marks the cases
///     whose expected value is version-dependent — so it is worth paying for
///     deliberately and not worth paying for by default.
fn alt_speaks_to(v: Verdict) -> bool {
    matches!(
        v,
        Verdict::ExitDiff | Verdict::StdoutDiff | Verdict::StateDiff | Verdict::StderrDiff
    )
}

/// The first surface on which the second oracle differs from some other answer,
/// in the same precedence order [`classify`] uses.
///
/// Same order deliberately: a reader comparing "the two gits differ on their exit
/// code" against a verdict of `stdout-diff` needs the two sentences to have been
/// produced by one rule, or they are two rules that will drift.
///
/// stderr participates only for a case that opted into stderr comparison, which
/// is the standing policy everywhere else in this crate — error prose is not a
/// compatibility surface, and two gits phrasing one error differently is not a
/// version difference anybody wants listed as one.
fn oracle_diff(
    alt: &AltRun,
    code: Option<i32>,
    stdout: &str,
    state: &str,
    stderr: &str,
    compare_stderr: bool,
) -> Option<OracleSurface> {
    if alt.code != code {
        Some(OracleSurface::Exit)
    } else if alt.stdout != stdout {
        Some(OracleSurface::Stdout)
    } else if alt.state != state {
        Some(OracleSurface::State)
    } else if compare_stderr && alt.stderr != stderr {
        Some(OracleSurface::Stderr)
    } else {
        None
    }
}

/// Whether the second oracle reproduced its own answer, so a disagreement it
/// reported can be believed.
///
/// The third binary is held to exactly the standard the other two are held to,
/// and it has to be. `judge` establishes that stock and zvcs each reproduce
/// themselves, but that is not enough to make a *third* sample's disagreement
/// mean "the versions differ": some values are re-rolled every run and two
/// samples can agree by luck. `filter-branch` prints
/// `(N seconds passed, remaining M predicted)` from the wall clock, and the first
/// run of this dimension duly reported
/// `branched::filter-branch::filter-branch -f --tree-filter true HEAD` as a
/// version difference between 2.55.0 and 2.50.1 — the two gits had printed
/// `(0 seconds passed)` and `(1 seconds passed)`, and the port happened to land on
/// the second. A dimension whose headline finding is manufactured by machine load
/// is worse than no dimension, because its output still reads like evidence.
///
/// So a disagreement is corroborated by a second run of the second oracle before
/// it is believed, and a second oracle that will not reproduce itself yields
/// [`AltFinding::Inconclusive`] — the same "nothing follows from this" treatment a
/// timeout gets, and for the same reason.
///
/// **Only a disagreement is corroborated, never an agreement.** Two gits agreeing
/// by luck on an unstable value produces [`AltFinding::GitsAgree`], which does not
/// excuse the port of anything — it corroborates a defect the case already had.
/// Erring toward the port being asked to be right is this crate's standing
/// direction, and it keeps the common path at one extra invocation.
///
/// **A repeat that timed out proves nothing**, so the finding stands rather than
/// being dissolved: identical to [`repeat_disagreement`], and load must not be
/// able to erase a real version difference any more than it may invent one.
///
/// **What this does not close, recorded rather than hidden.** Re-running is
/// evidence, not proof. A value drawn from the wall clock can come out the same
/// twice — both of the second oracle's runs finishing inside the same second —
/// while the primary oracle's two runs land on a different one, and the case is
/// then labelled a version difference on a command where no version changed.
/// Measured: over `--only filter-branch --alt-git-every-case`, corroboration
/// moved one such case to [`AltFinding::Inconclusive`] and two survived it. Three
/// facts make that the acceptable error rather than a reason to widen the rule
/// further:
///
///  * It is bounded to the commands whose output embeds a clock, and every one of
///    them is named in the listing with both gits' bytes printed, so the reader
///    sees `(0 seconds passed)` against `(1 seconds passed)` and stops.
///  * It can only move a case **between two failure buckets**. `VersionSkew` and
///    the content diff it replaced are worth the same to both the numerator and
///    the denominator, so no number moves in either direction.
///  * This crate already accepts exactly this imprecision one bucket over:
///    [`Verdict::ZvcsNondeterministic`]'s doc names this same `filter-branch`
///    progress line as a case counted against the port that nothing could pass.
///    Trading it for a wider exclusion is the trade this harness does not make.
fn alt_reproduced(first: &AltRun, again: &AltRun, compare_stderr: bool) -> bool {
    if again.timed_out {
        return true;
    }
    oracle_diff(again, first.code, &first.stdout, &first.state, &first.stderr, compare_stderr)
        .is_none()
}

/// Classify one case three ways, and say whether the verdict changes.
///
/// Pure, and separate from [`judge`] for the same reason [`classify`] is separate
/// from the running: this is the rule that decides whether a difference gets
/// filed as a defect or as a version difference, and a rule nothing can test
/// without a machine that happens to have two gits on it is a rule that drifts.
///
/// **Only one verdict is reachable from here, and only in one direction.**
/// [`Verdict::VersionSkew`] replaces a content difference; nothing else moves.
/// The numerator cannot be reached at all: a case whose verdict is
/// [`Verdict::Match`] has the port and the primary oracle agreeing on every
/// surface [`oracle_diff`] reads, so the two comparisons below are the same
/// comparison and [`AltFinding::PortTracksAlt`] is unreachable for it — which is
/// what makes `--alt-git-every-case` safe to run over a passing corpus. The
/// [`alt_speaks_to`] guard on the rewrite says the same thing structurally, so
/// the property does not depend on that argument staying true.
fn adjudicate(
    verdict: Verdict,
    compare_stderr: bool,
    stock: &Compared<'_>,
    zvcs: &Compared<'_>,
    alt: &AltRun,
) -> (Verdict, AltFinding, Option<OracleSurface>) {
    // A killed second oracle proves nothing in either direction, exactly as a
    // killed repeat does. Concluding "the gits agree" from it would corroborate a
    // defect with a run that never finished; concluding "they disagree" would
    // manufacture a version difference out of machine load.
    if alt.timed_out {
        return (verdict, AltFinding::Inconclusive, None);
    }
    let vs_primary =
        oracle_diff(alt, stock.code, stock.stdout, stock.state, stock.stderr, compare_stderr);
    let Some(surface) = vs_primary else {
        return (verdict, AltFinding::GitsAgree, None);
    };
    let vs_port =
        oracle_diff(alt, zvcs.code, zvcs.stdout, zvcs.state, zvcs.stderr, compare_stderr);
    if vs_port.is_none() && alt_speaks_to(verdict) {
        return (Verdict::VersionSkew, AltFinding::PortTracksAlt, Some(surface));
    }
    (verdict, AltFinding::GitsDisagree, Some(surface))
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

/// Whether a repeat run reproduced the interop digest — asked **only** of a case
/// whose verdict is [`Verdict::InteropDiff`].
///
/// The restriction is the whole design, and it is not an optimization. Interop
/// is a compared surface now, so a side that cannot reproduce it has to be
/// reportable as a flake exactly as it is for stdout and post-state. But
/// [`repeat_disagreement`] is consulted for the *stock* side too, and a
/// disagreement there **excludes the case from the parity denominator**. Folding
/// interop into it unconditionally would mean a case that fails today on stdout
/// could leave the denominator tomorrow because stock's *interop* digest flaked
/// — an existing, measured failure quietly reclassified as unmeasurable by a
/// dimension that had nothing to do with it. This crate does not widen
/// exclusions, least of all as a side effect of adding a probe.
///
/// Gating on the verdict makes the reachable set exactly right: the only case
/// this can reclassify is one that is *already* an interop difference and would
/// otherwise be reported as a defect nobody can reproduce. No pre-existing
/// verdict can move, and no case that was in the denominator yesterday can leave
/// it today.
///
/// It also means the interop probe is only paid for in a repeat when interop is
/// the finding — [`judge`] passes the flag down to the repeat closures rather
/// than having them guess.
///
/// A repeat that timed out proves nothing here for the same reason it proves
/// nothing there: a killed run's digest is whatever it managed to produce.
fn interop_disagreement(verdict: Verdict, first_interop: &str, again: &Repeat) -> Option<Surface> {
    if verdict != Verdict::InteropDiff || again.timed_out {
        return None;
    }
    (again.interop != first_interop).then_some(Surface::Interop)
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
/// The repeat closures take one flag — *also take the interop probe* — because
/// only [`judge`] knows the verdict, and the interop surface is judged for
/// exactly one verdict (see [`interop_disagreement`]). Passing it down is what
/// keeps a failing but non-interop case from paying three more invocations per
/// side for a digest nothing would read.
fn judge(
    compare_stderr: bool,
    stock: &Compared<'_>,
    zvcs: &Compared<'_>,
    stock_repeat: &mut dyn FnMut(bool) -> Result<Repeat>,
    zvcs_repeat: &mut dyn FnMut(bool) -> Result<Repeat>,
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

    let want_interop = verdict == Verdict::InteropDiff;

    let stock_again = stock_repeat(want_interop)?;
    if repeat_disagreement(stock.stdout, stock.state, &stock_again).is_some()
        || interop_disagreement(verdict, stock.interop, &stock_again).is_some()
    {
        return Ok((Verdict::Nondeterministic, None));
    }

    let mut zvcs_again = zvcs_repeat(want_interop)?;
    zvcs_again.disagreement = repeat_disagreement(zvcs.stdout, zvcs.state, &zvcs_again)
        .or_else(|| interop_disagreement(verdict, zvcs.interop, &zvcs_again));
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
    // The file-scoped half of the configuration is part of the *premise*, so it
    // is written once into each pristine copy before anything runs — never
    // re-written between steps, which would clobber whatever the invocation
    // itself wrote to `.git/config`. Identical bytes on both sides, produced
    // here rather than by either binary; see `install_config`.
    install_config(&stock_repo, &case.config)?;
    install_config(&zvcs_repo, &case.config)?;

    let home = &templates.home;
    let stock_exec = stock_exec_dir(home);
    let zvcs_exec = zvcs_exec_dir(zvcs_bin, home);
    compare_in(
        case,
        zvcs_bin,
        &stock_repo,
        &zvcs_repo,
        home,
        &mut |interop| {
            repeat_side(
                crate::stock::git()?,
                case,
                templates,
                workdir,
                "stock-repeat",
                stock_exec,
                zvcs_bin,
                interop,
            )
        },
        &mut |interop| {
            repeat_side(
                zvcs_bin, case, templates, workdir, "zvcs-repeat", zvcs_exec, zvcs_bin, interop,
            )
        },
        &mut || match crate::stock::alt_git() {
            None => Ok(None),
            Some((bin, version)) => alt_side(
                bin,
                version,
                case,
                templates,
                workdir,
                alt_exec_dir(bin, home),
            )
            .map(Some),
        },
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
///
/// `alt_side_run` is a third closure for the same reason and with the same
/// discipline: it answers "run this invocation against the second oracle", which
/// for a step means replaying the prefix, and it returns `None` on a machine with
/// only one git so this function never has to know how an oracle is resolved. It
/// is called last, only for the verdicts [`alt_speaks_to`] admits, and only after
/// [`judge`] has established the difference is reproducible.
#[allow(clippy::too_many_arguments)]
fn compare_in(
    case: &Case,
    zvcs_bin: &Path,
    stock_repo: &Path,
    zvcs_repo: &Path,
    home: &Path,
    stock_repeat: &mut dyn FnMut(bool) -> Result<Repeat>,
    zvcs_repeat: &mut dyn FnMut(bool) -> Result<Repeat>,
    alt_side_run: &mut dyn FnMut() -> Result<Option<AltRun>>,
) -> Result<Outcome> {
    let (stock_repo, zvcs_repo) = (stock_repo.to_path_buf(), zvcs_repo.to_path_buf());
    // The gate for the interop dimension, taken before anything runs. Two `stat`
    // walks per side and no child process; see `git_fingerprint`.
    let stock_before = git_fingerprint(&stock_repo);
    let zvcs_before = git_fingerprint(&zvcs_repo);

    let stock = run_side(crate::stock::git()?, &stock_repo, home, case)?;
    let zvcs = run_side(zvcs_bin, &zvcs_repo, home, case)?;

    // Closed **before** `probe_state`, and that ordering is load-bearing rather
    // than tidy. `probe_state` runs `status`, which refreshes the index and
    // writes it back when the stat data it cached has gone stale — so a gate
    // read after the state probe attributes the harness's own write to the case
    // and opens for everything. Measured: with the reads in the wrong order,
    // 4854 of 4861 curated cases "mutated the repository", including `log`,
    // `rev-parse` and `cat-file`. Taken here, the number is what the case
    // actually did.
    //
    // Either side writing opens the gate for **both**, so the two digests are
    // always comparable. Gating per side would make "only the port wrote
    // anything" show up as an interop difference between a real digest and the
    // unprobed marker, which is a true fact reported under the wrong name — the
    // finding there is what was written, not how stock reads it.
    let interop_probed = stock_before != git_fingerprint(&stock_repo)
        || zvcs_before != git_fingerprint(&zvcs_repo);

    let stock_state = probe_state(&stock_repo, home);
    let zvcs_state = probe_state(&zvcs_repo, home);

    let (stock_interop, zvcs_interop) = if interop_probed {
        (
            probe_interop(&stock_repo, home, &interop_scratch(&stock_repo), zvcs_bin),
            probe_interop(&zvcs_repo, home, &interop_scratch(&zvcs_repo), zvcs_bin),
        )
    } else {
        (INTEROP_UNPROBED.to_string(), INTEROP_UNPROBED.to_string())
    };

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
    // Normalized like every other surface: `fsck` names paths, and the two sides
    // live at different roots.
    let stock_interop_n = normalize(stock_interop.as_bytes(), &stock_repo, home, stock_exec);
    let zvcs_interop_n = normalize(zvcs_interop.as_bytes(), &zvcs_repo, home, zvcs_exec);

    // A failing case might be one neither binary reproduces itself. Each side is
    // re-run in a fresh copy of the same shape and compared against its own first
    // answer; only a disagreement there reclassifies. Both repeats are lazy — the
    // closures are called by `judge` only when the case failed.
    let stock_view = Compared {
        timed_out: stock.timed_out,
        code: stock.code,
        stdout: &stock_stdout,
        stderr: &stock_stderr,
        state: &stock_state_n,
        interop: &stock_interop_n,
    };
    let zvcs_view = Compared {
        timed_out: zvcs.timed_out,
        code: zvcs.code,
        stdout: &zvcs_stdout,
        stderr: &zvcs_stderr,
        state: &zvcs_state_n,
        interop: &zvcs_interop_n,
    };
    let (verdict, zvcs_repeat) =
        judge(case.compare_stderr, &stock_view, &zvcs_view, stock_repeat, zvcs_repeat)?;

    // The second oracle, asked last and only when it has something to say.
    //
    // After `judge` rather than inside it, and that ordering is the whole cost
    // argument. `judge` has already established that the difference is
    // reproducible on both sides — a case that turned out to be a flake never
    // reaches here, so a loaded machine cannot buy a third invocation per flake,
    // and a version difference can never be claimed about an answer neither
    // binary produces twice.
    //
    // `alt_every_case` lifts the failure gate and nothing else: `alt_speaks_to`
    // still decides which *verdicts* can be adjudicated, so an ungated run pays
    // for matching cases (the ones it exists to examine) and still does not pay
    // for crashes, hangs, gaps or unmeasurable cases.
    let mut verdict = verdict;
    let mut alt = None;
    if alt_speaks_to(verdict) || (alt_every_case() && verdict.is_match()) {
        if let Some(mut run) = alt_side_run()? {
            let (mut v, mut finding, mut surface) =
                adjudicate(verdict, case.compare_stderr, &stock_view, &zvcs_view, &run);
            // A disagreement between two gits is corroborated before it is
            // believed; see `alt_reproduced` for the `filter-branch` clock line
            // that made this necessary. Paid for only when there is a
            // disagreement to corroborate, which on this corpus is a handful of
            // cases in five thousand.
            if finding.gits_disagreed() {
                let again = alt_side_run()?;
                let stable = again
                    .as_ref()
                    .is_some_and(|a| alt_reproduced(&run, a, case.compare_stderr));
                if !stable {
                    v = verdict;
                    finding = AltFinding::Inconclusive;
                    surface = None;
                }
            }
            run.finding = finding;
            run.surface = surface;
            verdict = v;
            alt = Some(run);
        }
    }

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
        stock_interop: stock_interop_n,
        zvcs_interop: zvcs_interop_n,
        interop_probed,
        zvcs_repeat,
        alt,
    })
}

/// Where [`probe_interop`] parks the index copy and the redirected object
/// directory for one repository.
///
/// A **sibling** of the repository, never a directory inside it. Anything under
/// the fixture would be seen by the next `status` the case runs — in a
/// [`Sequence`] there *is* a next step — and a probe that shows up as `?? …` in
/// the state digest has made itself into the difference it was measuring. Named
/// from the repository's own directory name so the four repositories a worker
/// juggles (`stock`, `zvcs`, and the two repeats) never share one.
fn interop_scratch(repo: &Path) -> PathBuf {
    let name = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    repo.with_file_name(format!("interop-{name}"))
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
    install_config(&stock_repo, &seq.envelope.config)?;
    install_config(&zvcs_repo, &seq.envelope.config)?;

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
            &mut |interop| {
                repeat_sequence_side(
                    crate::stock::git()?,
                    seq,
                    index,
                    templates,
                    workdir,
                    "stock-repeat",
                    stock_exec,
                    zvcs_bin,
                    interop,
                )
            },
            &mut |interop| {
                repeat_sequence_side(
                    zvcs_bin, seq, index, templates, workdir, "zvcs-repeat", zvcs_exec, zvcs_bin,
                    interop,
                )
            },
            &mut || match crate::stock::alt_git() {
                None => Ok(None),
                Some((bin, version)) => alt_sequence_side(
                    bin,
                    version,
                    seq,
                    index,
                    templates,
                    workdir,
                    alt_exec_dir(bin, home),
                )
                .map(Some),
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
/// this only on a failure, and only up to the step that failed. `interop` is
/// [`judge`]'s answer to "is the interop digest one of the surfaces this repeat
/// has to reproduce", which it is for exactly one verdict — see
/// [`interop_disagreement`].
#[allow(clippy::too_many_arguments)]
fn repeat_sequence_side(
    bin: &Path,
    seq: &Sequence,
    index: usize,
    templates: &Templates,
    workdir: &Path,
    sub: &str,
    exec_dir: &Path,
    zvcs_bin: &Path,
    interop: bool,
) -> Result<Repeat> {
    let repo = workdir.join(sub);
    let _ = std::fs::remove_dir_all(&repo);
    templates.instantiate(seq.envelope.shape, &repo)?;
    install_config(&repo, &seq.envelope.config)?;
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
        interop: repeat_interop(&repo, home, exec_dir, zvcs_bin, interop),
        disagreement: None,
    })
}

/// The interop digest of a repeat's own repository, or nothing when this repeat
/// was not asked for one.
///
/// No gate here, and deliberately: the repeat only ever runs on a failing case,
/// and it is only ever *asked* for this digest when interop is the finding —
/// which means the first run's gate was already open. Re-deriving the gate from
/// a second fingerprint pair would spend two more walks to answer a question
/// [`judge`] has already answered.
fn repeat_interop(
    repo: &Path,
    home: &Path,
    exec_dir: &Path,
    zvcs_bin: &Path,
    wanted: bool,
) -> String {
    if !wanted {
        return String::new();
    }
    let raw = probe_interop(repo, home, &interop_scratch(repo), zvcs_bin);
    normalize(raw.as_bytes(), repo, home, exec_dir)
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
///
/// `interop` is [`judge`]'s answer to "is the interop digest one of the surfaces
/// this repeat has to reproduce". It is true for exactly one verdict, so a
/// failing case that is not an interop difference pays nothing for this
/// dimension in its repeat either — see [`interop_disagreement`].
#[allow(clippy::too_many_arguments)]
fn repeat_side(
    bin: &Path,
    case: &Case,
    templates: &Templates,
    workdir: &Path,
    sub: &str,
    exec_dir: &Path,
    zvcs_bin: &Path,
    interop: bool,
) -> Result<Repeat> {
    let repo = workdir.join(sub);
    let _ = std::fs::remove_dir_all(&repo);
    templates.instantiate(case.shape, &repo)?;
    install_config(&repo, &case.config)?;
    let home = &templates.home;
    let again = run_side(bin, &repo, home, case)?;
    Ok(Repeat {
        timed_out: again.timed_out,
        code: again.code,
        stdout: normalize(&again.stdout, &repo, home, exec_dir),
        state: normalize(probe_state(&repo, home).as_bytes(), &repo, home, exec_dir),
        interop: repeat_interop(&repo, home, exec_dir, zvcs_bin, interop),
        // Filled by `judge`, which is the only caller holding the first run.
        disagreement: None,
    })
}

/// Run one case against the **second** oracle, in a fresh copy of the same
/// shape, and reduce it to the surfaces the three-way comparison reads.
///
/// A sibling of [`repeat_side`] and shaped like it on purpose — same fresh
/// instantiation, same config installed from the same premise, same
/// normalization against its own root — because the second oracle has to be
/// asked *the identical question* the first two were asked. Any difference in
/// how the repository is prepared shows up as a version difference that is
/// really a harness difference, which is the one kind of finding this dimension
/// must never produce: it would exculpate the port for free.
///
/// The post-state is probed the way every other side's is, which means it is read
/// by the **primary** stock git (see [`probe_state`]). That is deliberate and not
/// an oversight. The question here is *what did the second oracle write*, not
/// *how does the second oracle read*; using one reader for all three sides keeps
/// the digests comparable and stops a difference in the reader from being
/// reported as a difference in the writer.
///
/// `finding` and `surface` are left for [`adjudicate`], the only place holding
/// all three answers.
fn alt_side(
    bin: &Path,
    version: (u32, u32, u32),
    case: &Case,
    templates: &Templates,
    workdir: &Path,
    exec_dir: &Path,
) -> Result<AltRun> {
    let repo = workdir.join("alt");
    let _ = std::fs::remove_dir_all(&repo);
    templates.instantiate(case.shape, &repo)?;
    install_config(&repo, &case.config)?;
    let home = &templates.home;
    let run = run_side(bin, &repo, home, case)?;
    Ok(AltRun {
        version,
        timed_out: run.timed_out,
        code: run.code,
        stdout: normalize(&run.stdout, &repo, home, exec_dir),
        stderr: normalize(&run.stderr, &repo, home, exec_dir),
        state: normalize(probe_state(&repo, home).as_bytes(), &repo, home, exec_dir),
        finding: AltFinding::Inconclusive,
        surface: None,
    })
}

/// Run a sequence's first `index + 1` steps against the second oracle and report
/// what the last of them produced.
///
/// The prefix is replayed for the reason [`repeat_sequence_side`] replays it: a
/// step's answer is a function of the state the steps before it built, and asking
/// the second oracle to run `cherry-pick --continue` in a pristine copy would
/// have it answer "no cherry-pick in progress" — a difference from both other
/// sides that says nothing about either git's behaviour and would be filed as a
/// version difference.
fn alt_sequence_side(
    bin: &Path,
    version: (u32, u32, u32),
    seq: &Sequence,
    index: usize,
    templates: &Templates,
    workdir: &Path,
    exec_dir: &Path,
) -> Result<AltRun> {
    let repo = workdir.join("alt");
    let _ = std::fs::remove_dir_all(&repo);
    templates.instantiate(seq.envelope.shape, &repo)?;
    install_config(&repo, &seq.envelope.config)?;
    let home = &templates.home;

    let mut run = None;
    for i in 0..=index {
        run = Some(run_side(bin, &repo, home, &seq.step_case(i))?);
    }
    let run = run.expect("the loop runs at least once");
    Ok(AltRun {
        version,
        timed_out: run.timed_out,
        code: run.code,
        stdout: normalize(&run.stdout, &repo, home, exec_dir),
        stderr: normalize(&run.stderr, &repo, home, exec_dir),
        state: normalize(probe_state(&repo, home).as_bytes(), &repo, home, exec_dir),
        finding: AltFinding::Inconclusive,
        surface: None,
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
        accelerator_read_probe, accelerators, other_peers, peer_section, probe_pack_contents,
        adjudicate, alt_reproduced, alt_speaks_to, case_timeout, classify, config_premise, git_dir,
        decode_varint, escape_bytes, ext_detail, index_meta, walk_files, interop_disagreement, is_unsupported, judge, oracle_diff, probe_op_state,
        probe_fetch_head, probe_index_meta, probe_modules, probe_pack_headers, probe_peer,
        probe_reflogs, probe_rr_cache, probe_worktree_content,
        probe_worktrees,
        quote_config_value, render_config_entry, repeat_disagreement,
        scope_file, split_config_key, step_is_final, AltFinding, AltRun, Case, Compared,
        ConfigEntry, ConfigScope, OracleSurface,
        Outcome, Repeat,
        Sequence, StepRef, Surface, Verdict, CASE_TIMEOUT, OID_LEN, OP_STATE_DIRS, OP_STATE_FILES,
    };
    use super::{drain, DRAIN_GRACE};
    use crate::fixture::Shape;
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// The hang this harness actually suffered: a pipe whose write end a
    /// forgotten grandchild still holds.
    ///
    /// `sh -c 'sleep 5 &'` exits immediately and leaves `sleep` holding the
    /// stdout it inherited, which is the shape of the `git daemon` case that
    /// parked a worker in `read_to_end` for as long as the daemon lived. The
    /// child is reaped first, so the only thing keeping the pipe open is the
    /// process nobody is waiting for — exactly the state [`drain`] has to
    /// return from.
    #[test]
    fn a_pipe_a_grandchild_still_holds_does_not_park_the_drain() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 5 &"])
            .stdout(Stdio::piped())
            .process_group(0)
            .spawn()
            .expect("spawn /bin/sh");
        child.wait().expect("the shell exits at once; the sleep it forked does not");

        let start = Instant::now();
        let out = drain(child.stdout.take().expect("stdout piped"));
        let waited = start.elapsed();

        assert!(out.is_empty(), "the shell wrote nothing, got {out:?}");
        assert!(
            waited < DRAIN_GRACE * 4,
            "drain waited {waited:?} on a pipe with no EOF coming; the deadline is {DRAIN_GRACE:?}"
        );
    }

    /// A side that answered, with the surfaces spelled out. Everything the
    /// classification reads and nothing else — no binary, no repo, no clock, so
    /// these tests can never flake on the thing they exist to detect.
    fn side<'a>(code: i32, stdout: &'a str, state: &'a str) -> Compared<'a> {
        // Both sides carry the "gate stayed shut" marker unless a test says
        // otherwise, which is the shape of every non-mutating case: an unprobed
        // case can never be an interop difference.
        Compared {
            timed_out: false,
            code: Some(code),
            stdout,
            stderr: "",
            state,
            interop: super::INTEROP_UNPROBED,
        }
    }

    /// A side whose repository was probed, with `interop` as what stock made of it.
    fn probed<'a>(code: i32, stdout: &'a str, state: &'a str, interop: &'a str) -> Compared<'a> {
        Compared { interop, ..side(code, stdout, state) }
    }

    /// A side the harness killed. `stdout` is whatever it managed to write before
    /// the kill — deliberately non-empty in the tests, because the bug being
    /// guarded is a partial capture reaching the diff.
    fn killed(stdout: &str) -> Compared<'_> {
        Compared {
            timed_out: true,
            code: None,
            stdout,
            stderr: "",
            state: "",
            interop: super::INTEROP_UNPROBED,
        }
    }

    fn repeat(stdout: &str, state: &str) -> Repeat {
        Repeat {
            timed_out: false,
            code: Some(0),
            stdout: stdout.to_string(),
            state: state.to_string(),
            interop: String::new(),
            disagreement: None,
        }
    }

    /// A repeat that was asked for an interop digest, and produced this one.
    fn repeat_interop_digest(stdout: &str, state: &str, interop: &str) -> Repeat {
        Repeat { interop: interop.to_string(), ..repeat(stdout, state) }
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
            &mut |_| Ok(repeat("stock\n", "S")),
            &mut |_| Ok(repeat("zvcs-b\n", "S")),
        )
        .unwrap();
        assert_eq!(v, Verdict::ZvcsNondeterministic);
        assert_eq!(r.unwrap().disagreement, Some(Surface::Stdout));

        // Differing post-state, identical stdout — the `merge` shape of the bug.
        let (v, r) = judge(
            false,
            &side(0, "same\n", "stock-state"),
            &side(0, "same\n", "zvcs-state-a"),
            &mut |_| Ok(repeat("same\n", "stock-state")),
            &mut |_| Ok(repeat("same\n", "zvcs-state-b")),
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
                &mut |_| Ok(repeat(stock.stdout, stock.state)),
                &mut |_| Ok(repeat(zvcs.stdout, zvcs.state)),
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
            &mut |_| panic!("no repeat is taken for a timeout"),
            &mut |_| panic!("no repeat is taken for a timeout"),
        )
        .unwrap();
        assert_eq!(v, Verdict::StockTimeout);
        assert!(r.is_none());

        // zvcs killed: a hang, never a stdout-diff against its own truncated output.
        let (v, r) = judge(
            false,
            &side(0, "full\n", "S"),
            &killed("partial"),
            &mut |_| panic!("no repeat is taken for a timeout"),
            &mut |_| panic!("no repeat is taken for a timeout"),
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
            &mut |_| Ok(killed_repeat()),
            &mut |_| Ok(repeat("zvcs\n", "S")),
        )
        .unwrap();
        assert_eq!(v, Verdict::StdoutDiff);

        // zvcs's repeat killed: not a flake either, for the same reason.
        let (v, _) = judge(
            false,
            &side(0, "stock\n", "S"),
            &side(0, "zvcs\n", "S"),
            &mut |_| Ok(repeat("stock\n", "S")),
            &mut |_| Ok(killed_repeat()),
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
            &mut |_| Ok(repeat("stock-b\n", "S")),
            &mut |_| panic!("the zvcs repeat must not be taken once stock has disagreed with itself"),
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
            &mut |_| panic!("a match must not pay for a repeat"),
            &mut |_| panic!("a match must not pay for a repeat"),
        )
        .unwrap();
        assert_eq!(v, Verdict::Match);
        assert!(r.is_none());
    }

    // -----------------------------------------------------------------------
    // The second oracle
    // -----------------------------------------------------------------------

    /// A second oracle's answer. Version is the older git this machine has, so
    /// the assertions below read the way a real finding would.
    fn alt(code: i32, stdout: &str, state: &str) -> AltRun {
        AltRun {
            version: (2, 50, 1),
            timed_out: false,
            code: Some(code),
            stdout: stdout.to_string(),
            stderr: String::new(),
            state: state.to_string(),
            // Both are what `adjudicate` fills in; a fixture that pre-filled
            // them could pass a test by agreeing with itself.
            finding: AltFinding::Inconclusive,
            surface: None,
        }
    }

    /// A second oracle the harness killed.
    fn killed_alt() -> AltRun {
        AltRun { timed_out: true, code: None, stdout: "half".into(), ..alt(0, "", "") }
    }

    /// Two independent git releases giving the same answer is the strongest
    /// statement this harness can make about a defect, and it must not change the
    /// verdict the case already earned — corroboration is a fact about the
    /// finding, not a different finding.
    #[test]
    fn two_gits_that_agree_corroborate_the_defect_and_move_nothing() {
        let (v, finding, surface) = adjudicate(
            Verdict::StdoutDiff,
            false,
            &side(0, "git\n", "S"),
            &side(0, "port\n", "S"),
            &alt(0, "git\n", "S"),
        );
        assert_eq!(v, Verdict::StdoutDiff);
        assert_eq!(finding, AltFinding::GitsAgree);
        // Nothing to name: there is no surface on which the two gits differ.
        assert_eq!(surface, None);
    }

    /// The finding the whole dimension exists for: the two gits disagree and the
    /// port reproduces the second one. That is not a wrong answer, it is an
    /// *older git's* answer, and it gets a verdict of its own so nobody spends an
    /// afternoon making code match a behaviour upstream changed on purpose.
    ///
    /// Checked on every surface the two oracles are compared on, because a
    /// version difference in an exit code is as real as one in stdout and a rule
    /// that only fired for stdout would file the others as defects.
    #[test]
    fn a_port_that_reproduces_the_other_git_is_a_version_difference() {
        // stdout
        let (v, finding, surface) = adjudicate(
            Verdict::StdoutDiff,
            false,
            &side(0, "new\n", "S"),
            &side(0, "old\n", "S"),
            &alt(0, "old\n", "S"),
        );
        assert_eq!(v, Verdict::VersionSkew);
        assert_eq!(finding, AltFinding::PortTracksAlt);
        assert_eq!(surface, Some(OracleSurface::Stdout));

        // exit code
        let (v, _, surface) = adjudicate(
            Verdict::ExitDiff,
            false,
            &side(0, "same\n", "S"),
            &side(1, "same\n", "S"),
            &alt(1, "same\n", "S"),
        );
        assert_eq!(v, Verdict::VersionSkew);
        assert_eq!(surface, Some(OracleSurface::Exit));

        // post-state
        let (v, _, surface) = adjudicate(
            Verdict::StateDiff,
            false,
            &side(0, "same\n", "new-state"),
            &side(0, "same\n", "old-state"),
            &alt(0, "same\n", "old-state"),
        );
        assert_eq!(v, Verdict::VersionSkew);
        assert_eq!(surface, Some(OracleSurface::State));
    }

    /// Two gits disagreeing does **not** excuse a port that matches neither.
    ///
    /// No choice of target version makes such a case a pass, so the verdict it
    /// earned stands. The disagreement is still reported — it says the expected
    /// value here is version-dependent, which is exactly the shape of a curated
    /// expectation captured against the wrong git — but "the oracles differ" is
    /// not on its own a reason to stop counting a difference.
    #[test]
    fn a_disagreement_between_gits_does_not_excuse_a_port_that_matches_neither() {
        let (v, finding, surface) = adjudicate(
            Verdict::StdoutDiff,
            false,
            &side(0, "new\n", "S"),
            &side(0, "port\n", "S"),
            &alt(0, "old\n", "S"),
        );
        assert_eq!(v, Verdict::StdoutDiff);
        assert_eq!(finding, AltFinding::GitsDisagree);
        assert_eq!(surface, Some(OracleSurface::Stdout));
        assert!(finding.gits_disagreed(), "it still belongs in the disagreement listing");
    }

    /// A killed second oracle proves nothing in either direction, exactly as a
    /// killed repeat does.
    ///
    /// Its partial stdout differs from a complete run's every time. Read as
    /// agreement it would corroborate a defect with a run that never finished;
    /// read as disagreement it would manufacture a version difference out of
    /// machine load — and that one is the dangerous direction, because it flatters.
    #[test]
    fn a_second_oracle_that_timed_out_concludes_nothing() {
        let (v, finding, surface) = adjudicate(
            Verdict::StdoutDiff,
            false,
            &side(0, "git\n", "S"),
            &side(0, "port\n", "S"),
            &killed_alt(),
        );
        assert_eq!(v, Verdict::StdoutDiff);
        assert_eq!(finding, AltFinding::Inconclusive);
        assert_eq!(surface, None);
        assert!(!finding.gits_disagreed());
    }

    /// The property the module header claims: **the second oracle can never move
    /// the parity number.** It may move a case from one failure bucket to
    /// another; it may not put one into the numerator or take one out of the
    /// denominator.
    ///
    /// Two independent reasons, and both are checked because either alone would
    /// be a coincidence somebody could break. First, a matching case has the port
    /// and the primary oracle agreeing on every surface `oracle_diff` reads, so
    /// "the port matches the second oracle" and "the two gits agree" are the same
    /// comparison and `PortTracksAlt` is unreachable. Second, `alt_speaks_to`
    /// gates the rewrite, so even an impossible input cannot produce it.
    #[test]
    fn the_second_oracle_cannot_move_the_parity_number() {
        // A passing case the older git would have answered differently: reported,
        // never rescored. This is what `--alt-git-every-case` exists to find.
        let (v, finding, surface) = adjudicate(
            Verdict::Match,
            false,
            &side(0, "new\n", "S"),
            &side(0, "new\n", "S"),
            &alt(0, "old\n", "S"),
        );
        assert_eq!(v, Verdict::Match);
        assert_eq!(finding, AltFinding::GitsDisagree);
        assert_eq!(surface, Some(OracleSurface::Stdout));

        // The structural guard, exercised with an input the runner cannot
        // produce: port and second oracle identical while the verdict says
        // `Match`. The rewrite still does not fire, so the property does not
        // depend on the argument above staying true.
        let (v, _, _) = adjudicate(
            Verdict::Match,
            false,
            &side(0, "new\n", "S"),
            &side(0, "old\n", "S"),
            &alt(0, "old\n", "S"),
        );
        assert_eq!(v, Verdict::Match);

        // And no verdict outside the content differences may be rewritten, so a
        // crash, a hang or a gap can never be reclassified as a version
        // difference by a second git that happens to match the port.
        for unrewritable in [
            Verdict::Unsupported,
            Verdict::Crash,
            Verdict::Hang,
            Verdict::InteropDiff,
            Verdict::ZvcsNondeterministic,
            Verdict::Nondeterministic,
            Verdict::StockTimeout,
        ] {
            let (v, _, _) = adjudicate(
                unrewritable,
                false,
                &side(0, "new\n", "S"),
                &side(0, "old\n", "S"),
                &alt(0, "old\n", "S"),
            );
            assert_eq!(v, unrewritable, "{}", unrewritable.label());
        }
    }

    /// A disagreement the second oracle will not reproduce is not a version
    /// difference, and this is the rule that says so.
    ///
    /// Not hypothetical, and not a corner: the first version of this dimension
    /// filed `filter-branch -f --tree-filter true HEAD` as a `VERSION-SKEW`
    /// between 2.55.0 and 2.50.1. The two gits had printed
    /// `(0 seconds passed, remaining 0 predicted)` and
    /// `(1 seconds passed, remaining 1 predicted)` from the wall clock, and the
    /// port happened to land on the second. Nothing about git's behaviour changed
    /// between those releases; the machine was busy. The values below are that
    /// case's, verbatim.
    #[test]
    fn a_disagreement_the_second_oracle_will_not_reproduce_is_not_a_version_difference() {
        let first = alt(0, "Rewrite abc (1/2) (1 seconds passed, remaining 1 predicted)\n", "S");
        let again = alt(0, "Rewrite abc (1/2) (0 seconds passed, remaining 0 predicted)\n", "S");
        assert!(!alt_reproduced(&first, &again, false));

        // The same answer twice is corroboration, and the finding stands.
        assert!(alt_reproduced(&first, &first.clone(), false));
    }

    /// A repeat of the second oracle that timed out proves nothing, so it may not
    /// dissolve a disagreement either.
    ///
    /// The symmetric error to the one above, and the more dangerous direction is
    /// this one: if a killed repeat counted as "did not reproduce", a loaded
    /// machine could erase a real version difference and hand the case back as an
    /// ordinary port defect. Same rule as `repeat_disagreement`.
    #[test]
    fn a_second_oracle_repeat_that_timed_out_may_not_dissolve_a_disagreement() {
        let first = alt(0, "old\n", "S");
        assert!(alt_reproduced(&first, &killed_alt(), false));
    }

    /// The corroborating run compares the same surfaces the two oracles are
    /// compared on, stderr included only for a case that opted in — so a
    /// disagreement found on stderr is corroborated on stderr, and a case that
    /// never compares prose is not asked to reproduce it.
    #[test]
    fn the_corroborating_run_compares_what_the_oracles_were_compared_on() {
        let first = AltRun { stderr: "one".into(), ..alt(0, "same\n", "S") };
        let again = AltRun { stderr: "two".into(), ..alt(0, "same\n", "S") };
        assert!(!alt_reproduced(&first, &again, true));
        assert!(alt_reproduced(&first, &again, false));
    }

    /// Which verdicts a second git is asked about at all.
    ///
    /// The `match` is exhaustive so a new verdict cannot be added without a
    /// decision being made here: the default of "ask about it" would spend an
    /// invocation per case on a question the second oracle has no answer for, and
    /// the default of "do not" would silently shrink the dimension.
    #[test]
    fn only_a_content_difference_is_worth_a_second_opinion() {
        let every = [
            Verdict::Match,
            Verdict::Unsupported,
            Verdict::StdoutDiff,
            Verdict::ExitDiff,
            Verdict::StateDiff,
            Verdict::InteropDiff,
            Verdict::StderrDiff,
            Verdict::Crash,
            Verdict::Hang,
            Verdict::ZvcsNondeterministic,
            Verdict::VersionSkew,
            Verdict::Nondeterministic,
            Verdict::StockTimeout,
        ];
        for v in every {
            let want = match v {
                Verdict::StdoutDiff
                | Verdict::ExitDiff
                | Verdict::StateDiff
                | Verdict::StderrDiff => true,
                Verdict::Match
                | Verdict::Unsupported
                | Verdict::InteropDiff
                | Verdict::Crash
                | Verdict::Hang
                | Verdict::ZvcsNondeterministic
                | Verdict::VersionSkew
                | Verdict::Nondeterministic
                | Verdict::StockTimeout => false,
            };
            assert_eq!(alt_speaks_to(v), want, "{}", v.label());
        }
    }

    /// The two oracles are compared in `classify`'s own precedence order, and
    /// stderr participates only for a case that opted into it.
    ///
    /// Both halves matter. The order is what lets a reader read "the two gits
    /// differ on their exit code" beside a verdict of `exit-diff` without holding
    /// two rules in their head. The stderr gate is this crate's standing policy:
    /// error prose is not a compatibility surface, and two gits phrasing one
    /// message differently is not a version difference anybody wants listed.
    #[test]
    fn the_two_gits_are_compared_in_the_classifier_s_own_order() {
        // Everything differs at once: the exit code is named, being first.
        let a = AltRun { stderr: "alt-msg".into(), ..alt(1, "alt\n", "alt-state") };
        assert_eq!(
            oracle_diff(&a, Some(0), "primary\n", "primary-state", "primary-msg", true),
            Some(OracleSurface::Exit)
        );
        // Same code: stdout is next.
        assert_eq!(
            oracle_diff(&a, Some(1), "primary\n", "primary-state", "primary-msg", true),
            Some(OracleSurface::Stdout)
        );
        // Same code and stdout: the post-state.
        assert_eq!(
            oracle_diff(&a, Some(1), "alt\n", "primary-state", "primary-msg", true),
            Some(OracleSurface::State)
        );
        // Only the message left, and it counts only when the case opted in.
        assert_eq!(
            oracle_diff(&a, Some(1), "alt\n", "alt-state", "primary-msg", true),
            Some(OracleSurface::Stderr)
        );
        assert_eq!(oracle_diff(&a, Some(1), "alt\n", "alt-state", "primary-msg", false), None);
    }

    // -----------------------------------------------------------------------
    // Interop
    // -----------------------------------------------------------------------

    /// The interop digest from the cache-tree defect, as
    /// [`super::probe_interop`] renders it. `stock` is what stock 2.55.0 left
    /// behind; `zvcs` is the same repository with the `TREE` extension stripped,
    /// which is what `zvcs add` did before `30c23c0799`. Both numbers are the
    /// measured ones.
    fn cache_tree_digests() -> (String, String) {
        let head = "# interop\n## fsck --strict\nexit: Some(0)\n";
        let side = |label: &str, repaired: &str, before: usize| {
            format!(
                "{label} exit: Some(0)\n\
                 {label} tree: 9c05a71a986e3294c683f3a285c8651c9ccfe16f\n\
                 {label} index-repaired: {repaired}\n\
                 {label} index-bytes-before: {before}\n\
                 {label} index-bytes-after: 261\n\
                 {label} objects-written: 0\n"
            )
        };
        (
            format!(
                "{head}## write-tree\n{}{}",
                side("stock", "no", 261),
                side("zvcs", "no", 261)
            ),
            format!(
                "{head}## write-tree\n{}{}",
                side("stock", "yes", 176),
                side("zvcs", "yes", 176)
            ),
        )
    }

    /// The defect this dimension exists for: identical stdout, identical exit
    /// code, identical post-state — and stock git has to repair the port's index
    /// before it can use it.
    ///
    /// Every one of the four surfaces that existed before this test was written
    /// agrees here, which is exactly why the real bug scored `Match` and had to
    /// be found by hand. It has to land in its own bucket rather than in
    /// `STATE-DIFF`: the repository's *contents* are identical, and telling a
    /// reader they diverged sends them looking for a difference that is not
    /// there.
    #[test]
    fn a_destroyed_cache_tree_is_an_interop_diff_and_not_a_state_diff() {
        let (stock, zvcs) = cache_tree_digests();
        assert_eq!(
            classify(
                &probed(0, "", "same-state", &stock),
                &probed(0, "", "same-state", &zvcs),
                false,
            ),
            Verdict::InteropDiff
        );
        // …and it is a counted failure, never an exclusion.
        assert!(!Verdict::InteropDiff.is_match());
        assert!(!Verdict::InteropDiff.is_unmeasurable());
        assert!(Verdict::InteropDiff.is_measured_failure());
        assert_eq!(Verdict::InteropDiff.exclusion_reason(), None);
    }

    /// A case that wrote nothing under the git directory carries the same
    /// "unprobed" marker on both sides, so it can never be an interop
    /// difference — the property that makes the gate free rather than a source
    /// of false findings.
    ///
    /// And the ordering: an interop difference never outranks a content one. A
    /// repository whose contents differ is the larger finding and the interop
    /// difference would be its consequence, so a case with both is reported as
    /// the content difference it is.
    #[test]
    fn interop_is_judged_below_content_and_above_message() {
        let (stock, zvcs) = cache_tree_digests();
        // Gate shut on both sides: nothing to disagree about.
        assert_eq!(classify(&side(0, "x\n", "S"), &side(0, "x\n", "S"), false), Verdict::Match);

        // stdout, exit and state each outrank it.
        for (a, b, want) in [
            (probed(0, "a\n", "S", &stock), probed(0, "b\n", "S", &zvcs), Verdict::StdoutDiff),
            (probed(0, "a\n", "S", &stock), probed(1, "a\n", "S", &zvcs), Verdict::ExitDiff),
            (probed(0, "a\n", "S", &stock), probed(0, "a\n", "T", &zvcs), Verdict::StateDiff),
        ] {
            assert_eq!(classify(&a, &b, false), want);
        }

        // …and it outranks the message, which the harness's standing policy says
        // is not a compatibility surface at all.
        let strict_stock = Compared { stderr: "one\n", ..probed(0, "a\n", "S", &stock) };
        let strict_zvcs = Compared { stderr: "two\n", ..probed(0, "a\n", "S", &zvcs) };
        assert_eq!(classify(&strict_stock, &strict_zvcs, true), Verdict::InteropDiff);
    }

    /// The interop repeat is asked for by exactly one verdict, and that
    /// restriction is what keeps this dimension from moving numbers nobody chose
    /// to move.
    ///
    /// `repeat_disagreement` is consulted for the **stock** side, and a
    /// disagreement there drops the case out of the parity denominator. If
    /// interop were folded into it unconditionally, a case failing today on
    /// stdout could become `Nondeterministic` tomorrow because stock's *interop*
    /// digest flaked — an existing measured failure quietly reclassified as
    /// unmeasurable by a probe that had nothing to do with it.
    #[test]
    fn the_interop_surface_is_only_judged_for_an_interop_diff() {
        let (a, b) = ("digest-a", "digest-b");
        // The verdict it is judged for.
        assert_eq!(
            interop_disagreement(
                Verdict::InteropDiff,
                a,
                &repeat_interop_digest("", "", b)
            ),
            Some(Surface::Interop)
        );
        assert_eq!(
            interop_disagreement(Verdict::InteropDiff, a, &repeat_interop_digest("", "", a)),
            None
        );
        // Every other verdict: not judged, whatever the digests say. A stdout
        // difference stays a stdout difference.
        for v in [
            Verdict::Match,
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
            assert_eq!(
                interop_disagreement(v, a, &repeat_interop_digest("", "", b)),
                None,
                "{} must not be reclassified by the interop surface",
                v.label()
            );
        }
        // A killed repeat proves nothing here either: its digest is whatever it
        // managed to produce before the kill.
        assert_eq!(
            interop_disagreement(Verdict::InteropDiff, a, &killed_repeat()),
            None
        );
    }

    /// An interop difference neither side reproduces is classified the same way
    /// every other unreproducible difference is — and the whole point of asking
    /// is that a *reproducible* one keeps its verdict.
    #[test]
    fn an_interop_flake_is_reported_as_a_flake_and_a_stable_one_as_a_defect() {
        let (stock, zvcs) = cache_tree_digests();

        // zvcs does not reproduce its own digest: a flake, counted as a failure.
        let (v, r) = judge(
            false,
            &probed(0, "", "S", &stock),
            &probed(0, "", "S", &zvcs),
            &mut |want| {
                assert!(want, "an interop diff must ask its repeats for the digest");
                Ok(repeat_interop_digest("", "S", &stock))
            },
            &mut |_| Ok(repeat_interop_digest("", "S", "a third digest")),
        )
        .unwrap();
        assert_eq!(v, Verdict::ZvcsNondeterministic);
        assert_eq!(r.unwrap().disagreement, Some(Surface::Interop));

        // Stock does not reproduce its own: nothing could match it, so the case
        // leaves the denominator — the same rule stdout and post-state follow.
        let (v, _) = judge(
            false,
            &probed(0, "", "S", &stock),
            &probed(0, "", "S", &zvcs),
            &mut |_| Ok(repeat_interop_digest("", "S", "something else")),
            &mut |_| panic!("the zvcs repeat must not be taken once stock has disagreed with itself"),
        )
        .unwrap();
        assert_eq!(v, Verdict::Nondeterministic);

        // Both reproduce: the difference stands as the defect it is.
        let (v, r) = judge(
            false,
            &probed(0, "", "S", &stock),
            &probed(0, "", "S", &zvcs),
            &mut |_| Ok(repeat_interop_digest("", "S", &stock)),
            &mut |_| Ok(repeat_interop_digest("", "S", &zvcs)),
        )
        .unwrap();
        assert_eq!(v, Verdict::InteropDiff);
        assert_eq!(r.unwrap().disagreement, None);
    }

    /// A failure that is *not* an interop difference must not pay for the
    /// interop probe in its repeat.
    ///
    /// The cost argument for this whole dimension is that it fires only where it
    /// can find something, and the repeat is the one place where that could
    /// silently stop being true — the repeat runs on every failure, so an
    /// unconditional digest there would be four more stock invocations on every
    /// failing case in the corpus.
    #[test]
    fn a_non_interop_failure_does_not_pay_for_the_interop_probe() {
        let (v, _) = judge(
            false,
            &side(0, "a\n", "S"),
            &side(0, "b\n", "S"),
            &mut |want| {
                assert!(!want, "a stdout diff must not ask its repeat for an interop digest");
                Ok(repeat("a\n", "S"))
            },
            &mut |want| {
                assert!(!want, "a stdout diff must not ask its repeat for an interop digest");
                Ok(repeat("b\n", "S"))
            },
        )
        .unwrap();
        assert_eq!(v, Verdict::StdoutDiff);
    }

    /// Every fact the probe reports occupies exactly one line, and the lines that
    /// differ *are* the diagnosis.
    ///
    /// `report.rs` pairs the two digests by line position to name what moved, so
    /// a fact that spilled across two lines would shift every following one and
    /// print a dozen phantom differences instead of the one real one. On the real
    /// defect the diagnosis is three lines wide and reads in English.
    #[test]
    fn the_interop_digest_is_one_fact_per_line() {
        let (stock, zvcs) = cache_tree_digests();
        let differing: Vec<(String, String)> = stock
            .lines()
            .zip(zvcs.lines())
            .filter(|(a, b)| a != b)
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        assert_eq!(
            differing,
            vec![
                ("stock index-repaired: no".into(), "stock index-repaired: yes".into()),
                ("stock index-bytes-before: 261".into(), "stock index-bytes-before: 176".into()),
                ("zvcs index-repaired: no".into(), "zvcs index-repaired: yes".into()),
                ("zvcs index-bytes-before: 261".into(), "zvcs index-bytes-before: 176".into()),
            ]
        );
        // The two digests have the same number of lines, which is what makes the
        // pairing above meaningful rather than an accident of length.
        assert_eq!(stock.lines().count(), zvcs.lines().count());
        // Every line after the two headings carries exactly one `key: value`.
        for line in stock.lines().filter(|l| !l.starts_with('#')) {
            assert!(line.contains(": "), "not one fact per line: {line:?}");
        }
    }

    /// The probe's scratch directory is a **sibling** of the repository, never
    /// inside it, and the four repositories a worker juggles never share one.
    ///
    /// Anything under the fixture would be seen by the next `status` the case
    /// runs — in a sequence there *is* a next step — and a probe that shows up as
    /// `?? …` in the state digest has made itself into the difference it was
    /// measuring.
    #[test]
    fn the_interop_scratch_never_lands_inside_the_repository() {
        let workdir = Path::new("/w/w0");
        let dirs: Vec<PathBuf> = ["stock", "zvcs", "stock-repeat", "zvcs-repeat"]
            .iter()
            .map(|s| super::interop_scratch(&workdir.join(s)))
            .collect();
        for (repo, scratch) in ["stock", "zvcs", "stock-repeat", "zvcs-repeat"].iter().zip(&dirs) {
            assert!(!scratch.starts_with(workdir.join(repo)), "{scratch:?} is inside the repo");
            assert!(scratch.starts_with(workdir), "{scratch:?} escaped the worker workdir");
        }
        let unique: std::collections::BTreeSet<&PathBuf> = dirs.iter().collect();
        assert_eq!(unique.len(), dirs.len(), "two repositories share one scratch: {dirs:?}");
    }

    /// The gate reports a write and only a write.
    ///
    /// Reading a repository must leave the fingerprint alone, or every case in
    /// the corpus pays the probe and the cost argument evaporates; writing must
    /// move it, or the defect this dimension exists for goes unprobed. Both
    /// halves are checked against a real directory, since the fingerprint is
    /// `stat` data and nothing else.
    #[test]
    fn the_interop_gate_fires_on_a_write_and_not_on_a_read() {
        let repo = scratch("interop-gate");
        let git = repo.join(".git");
        std::fs::write(git.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(git.join("refs/heads")).unwrap();
        std::fs::write(git.join("refs/heads/main"), b"0123456789abcdef0123456789abcdef01234567\n")
            .unwrap();
        let before = super::git_fingerprint(&repo);
        assert!(!before.is_empty(), "the fingerprint saw nothing in a real git directory");

        // Reading changes nothing.
        let _ = std::fs::read(git.join("HEAD")).unwrap();
        assert_eq!(super::git_fingerprint(&repo), before);

        // A write of a *different length* moves it, and so does a new file.
        std::fs::write(git.join("HEAD"), b"ref: refs/heads/other\n").unwrap();
        let after = super::git_fingerprint(&repo);
        assert_ne!(after, before);
        std::fs::write(git.join("ORIG_HEAD"), b"x\n").unwrap();
        assert_ne!(super::git_fingerprint(&repo), after);

        // A file outside the git directory is deliberately not in it: every
        // structure the probe inspects lives inside, so a worktree-only write
        // cannot have produced one for stock to misread.
        let worktree_only = super::git_fingerprint(&repo);
        std::fs::write(repo.join("file.txt"), b"edited\n").unwrap();
        assert_eq!(super::git_fingerprint(&repo), worktree_only);
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

    /// A repository with no linked worktree says so in one line, so the two
    /// digests being compared line up positionally.
    #[test]
    fn linked_worktrees_report_absence_as_one_line() {
        let repo = scratch("worktrees-absent");
        assert_eq!(probe_worktrees(&repo), "# linked-worktrees\n<absent>\n");
    }

    /// The reset-inside-`wt/` blind spot: a reflog line and an `ORIG_HEAD` that
    /// `.git/logs` never sees must not compare equal to their absence, and must
    /// not compare equal to a *different* reflog line.
    #[test]
    fn linked_worktrees_compare_the_admin_files_by_content() {
        let before = scratch("worktrees-before");
        let after = scratch("worktrees-after");
        let other = scratch("worktrees-other");
        for repo in [&before, &after, &other] {
            let wt = repo.join(".git/worktrees/wt");
            std::fs::create_dir_all(wt.join("logs")).unwrap();
            std::fs::write(wt.join("HEAD"), b"ref: refs/heads/linked\n").unwrap();
            std::fs::write(wt.join("commondir"), b"../..\n").unwrap();
        }
        // The state a `git -C wt reset --hard HEAD~1` leaves behind.
        std::fs::write(after.join(".git/worktrees/wt/ORIG_HEAD"), b"1111111111111111111111111111111111111111\n").unwrap();
        std::fs::write(
            after.join(".git/worktrees/wt/logs/HEAD"),
            b"1111111111111111111111111111111111111111 2222222222222222222222222222222222222222 A U Thor <a@u.thor> 1112911993 +0000\treset: moving to HEAD~1\n",
        )
        .unwrap();
        // Same act, different message: the silent-but-wrong case.
        std::fs::write(other.join(".git/worktrees/wt/ORIG_HEAD"), b"1111111111111111111111111111111111111111\n").unwrap();
        std::fs::write(
            other.join(".git/worktrees/wt/logs/HEAD"),
            b"1111111111111111111111111111111111111111 2222222222222222222222222222222222222222 A U Thor <a@u.thor> 1112911993 +0000\tcheckout: moving from linked to HEAD~1\n",
        )
        .unwrap();

        assert_ne!(probe_worktrees(&before), probe_worktrees(&after));
        assert_ne!(probe_worktrees(&after), probe_worktrees(&other));
        // One fact per line: a reflog's own newline must not shift the digest.
        assert!(probe_worktrees(&after).lines().all(|l| !l.starts_with(' ')));
        assert!(probe_worktrees(&after)
            .lines()
            .any(|l| l.starts_with("wt/logs/HEAD: ") && l.ends_with("reset: moving to HEAD~1\\n")));
    }

    /// A worktree index is reported by length only. Its `ctime`/`ino`/`dev`
    /// fields are facts about the filesystem, so two sides that wrote the same
    /// entries must still compare equal.
    #[test]
    fn linked_worktree_index_is_measured_by_length_not_bytes() {
        let a = scratch("worktrees-index-a");
        let b = scratch("worktrees-index-b");
        for (repo, stat) in [(&a, 0x11u8), (&b, 0x22u8)] {
            let wt = repo.join(".git/worktrees/wt");
            std::fs::create_dir_all(&wt).unwrap();
            // Same length, different stat bytes.
            let mut index = b"DIRC".to_vec();
            index.extend(std::iter::repeat(stat).take(60));
            std::fs::write(wt.join("index"), &index).unwrap();
        }
        assert_eq!(probe_worktrees(&a), probe_worktrees(&b));
        assert!(probe_worktrees(&a).contains("wt/index: <index 64 bytes>"));

        // A different *length* — an entry added, an extension dropped — still moves.
        let c = scratch("worktrees-index-c");
        std::fs::create_dir_all(c.join(".git/worktrees/wt")).unwrap();
        std::fs::write(c.join(".git/worktrees/wt/index"), vec![0x11u8; 72]).unwrap();
        assert_ne!(probe_worktrees(&a), probe_worktrees(&c));
    }

    /// A multi-pack-index built the way stock 2.55.0 builds one, as bytes, so the
    /// header parse is pinned against a real layout rather than against itself.
    ///
    /// `packs` and `objects` are the two facts a logical probe cannot see, and
    /// the two this test moves independently.
    fn midx_bytes(packs: u32, objects: u32, with_bitmap_chunk: bool) -> Vec<u8> {
        let chunks: &[&[u8; 4]] =
            if with_bitmap_chunk { &[b"PNAM", b"OIDF", b"BTMP"] } else { &[b"PNAM", b"OIDF"] };
        let table = 12 + (chunks.len() + 1) * 12;
        // PNAM is a stub here; OIDF has to be a real 256-entry fanout, because
        // that is where the object count is read from.
        let pnam_at = table;
        let oidf_at = pnam_at + 8;
        let mut out = Vec::new();
        out.extend_from_slice(b"MIDX");
        out.extend_from_slice(&[1, 1, chunks.len() as u8, 0]);
        out.extend_from_slice(&packs.to_be_bytes());
        for (i, id) in chunks.iter().enumerate() {
            out.extend_from_slice(&id[..]);
            let at = if i == 0 { pnam_at } else { oidf_at + (i - 1) * 1024 };
            out.extend_from_slice(&(at as u64).to_be_bytes());
        }
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(0u64).to_be_bytes());
        out.resize(oidf_at, 0);
        // 256 fanout buckets; only the last one is read.
        for _ in 0..255 {
            out.extend_from_slice(&objects.to_be_bytes());
        }
        out.extend_from_slice(&objects.to_be_bytes());
        out
    }

    fn write_pack_file(repo: &Path, name: &str, bytes: &[u8]) {
        let pack = repo.join(".git/objects/pack");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(pack.join(name), bytes).unwrap();
    }

    /// The `multi-pack-index write --bitmap` blind spot: a midx that covers
    /// different packs, or different objects, or that was written without its
    /// bitmap chunk, must not compare equal to one that was not.
    #[test]
    fn pack_headers_read_the_midx_beyond_its_name() {
        let a = scratch("pack-headers-midx-a");
        write_pack_file(&a, "multi-pack-index", &midx_bytes(2, 9, true));
        let probe = probe_pack_headers(&a);
        assert!(
            probe.contains("multi-pack-index: MIDX v1 hash=1 base=0 packs=2 chunks=[PNAM,OIDF,BTMP] objects=9"),
            "got:\n{probe}"
        );

        // One pack instead of two: same filename, same length class, different
        // repository.
        let b = scratch("pack-headers-midx-b");
        write_pack_file(&b, "multi-pack-index", &midx_bytes(1, 9, true));
        assert_ne!(probe_pack_headers(&a), probe_pack_headers(&b));

        // Same packs, different object coverage.
        let c = scratch("pack-headers-midx-c");
        write_pack_file(&c, "multi-pack-index", &midx_bytes(2, 8, true));
        assert_ne!(probe_pack_headers(&a), probe_pack_headers(&c));

        // Written without `--bitmap`: the chunk list is the difference.
        let d = scratch("pack-headers-midx-d");
        write_pack_file(&d, "multi-pack-index", &midx_bytes(2, 9, false));
        assert_ne!(probe_pack_headers(&a), probe_pack_headers(&d));
    }

    /// A bitmap is measured by what it covers, and its checksum-bearing name is
    /// elided the way `probe_storage` elides every other name in that directory.
    #[test]
    fn pack_headers_read_the_bitmap_beyond_its_name() {
        let bitmap = |entries: u32| {
            let mut out = b"BITM".to_vec();
            out.extend_from_slice(&1u16.to_be_bytes());
            out.extend_from_slice(&5u16.to_be_bytes());
            out.extend_from_slice(&entries.to_be_bytes());
            out.extend_from_slice(&[0xab; 20]);
            out
        };
        let a = scratch("pack-headers-bitmap-a");
        write_pack_file(
            &a,
            "pack-7d1e44081191d78cb9d33f3985bdace415d89e77.bitmap",
            &bitmap(3),
        );
        let probe = probe_pack_headers(&a);
        assert!(
            probe.contains("pack-<hash>.bitmap: BITM v1 flags=5 entries=3"),
            "got:\n{probe}"
        );

        // A different pack checksum is not a difference — the port cannot
        // reproduce git's pack bytes, and `probe_storage` says so.
        let b = scratch("pack-headers-bitmap-b");
        write_pack_file(
            &b,
            "pack-0000000000000000000000000000000000000000.bitmap",
            &bitmap(3),
        );
        assert_eq!(probe_pack_headers(&a), probe_pack_headers(&b));

        // A different object coverage is.
        let c = scratch("pack-headers-bitmap-c");
        write_pack_file(
            &c,
            "pack-7d1e44081191d78cb9d33f3985bdace415d89e77.bitmap",
            &bitmap(2),
        );
        assert_ne!(probe_pack_headers(&a), probe_pack_headers(&c));

        // A repository with neither file reports the section and nothing else,
        // on both sides.
        let empty = scratch("pack-headers-empty");
        assert_eq!(probe_pack_headers(&empty), "# pack-headers\n");
    }

    /// The blind spot itself: `checkout --ours` and `checkout --theirs` leave the
    /// same `status --porcelain` and the same `ls-files --stage`, and differ only
    /// in the bytes on disk.
    #[test]
    fn worktree_content_separates_ours_from_theirs() {
        let ours = scratch("worktree-ours");
        let theirs = scratch("worktree-theirs");
        let diff3 = scratch("worktree-diff3");
        std::fs::write(ours.join("conflict.txt"), b"ours\n").unwrap();
        std::fs::write(theirs.join("conflict.txt"), b"theirs\n").unwrap();
        // And a third marker style over the same unmerged path.
        std::fs::write(
            diff3.join("conflict.txt"),
            b"<<<<<<< ours\nours\n||||||| base\nbase\n=======\ntheirs\n>>>>>>> theirs\n",
        )
        .unwrap();

        assert_ne!(probe_worktree_content(&ours), probe_worktree_content(&theirs));
        assert_ne!(probe_worktree_content(&ours), probe_worktree_content(&diff3));
        assert_ne!(probe_worktree_content(&theirs), probe_worktree_content(&diff3));

        // One fact per line, contents escaped: `report.rs` pairs the two digests
        // by line position, so a conflict marker's own newlines must not shift
        // every following fact.
        let probe = probe_worktree_content(&diff3);
        assert_eq!(
            probe.lines().filter(|l| l.starts_with("conflict.txt")).count(),
            1,
            "the whole file is one fact on one line:\n{probe}"
        );
        assert!(probe.contains("conflict.txt -: <<<<<<< ours\\nours\\n"), "got:\n{probe}");
    }

    /// The walk stops at a git directory — the fixture's bare peer, a clone
    /// target — but still reports that one is there.
    #[test]
    fn worktree_content_does_not_walk_into_repositories() {
        let repo = scratch("worktree-nested");
        let peer = repo.join(".remote.git");
        std::fs::create_dir_all(peer.join("objects")).unwrap();
        std::fs::create_dir_all(peer.join("refs")).unwrap();
        std::fs::write(peer.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
        std::fs::write(peer.join("secret"), b"not compared here\n").unwrap();
        // The main git directory is not worktree content either.
        std::fs::write(repo.join(".git/COMMIT_EDITMSG"), b"scratch\n").unwrap();
        std::fs::write(repo.join("tracked.txt"), b"content\n").unwrap();

        let probe = probe_worktree_content(&repo);
        assert!(probe.contains(".remote.git -: <repository>"), "got:\n{probe}");
        assert!(probe.contains(".git -: <git directory>"), "got:\n{probe}");
        assert!(!probe.contains("not compared here"), "walked into the peer:\n{probe}");
        assert!(!probe.contains("scratch"), "walked into the git directory:\n{probe}");
        assert!(probe.contains("tracked.txt -: content\\n"), "got:\n{probe}");
    }

    /// Binary is compared by length, text by bytes — the same relaxation
    /// `probe_storage` makes, because a pack written into the worktree is not
    /// reproducible byte for byte.
    #[test]
    fn worktree_content_compares_binary_by_length_only() {
        let a = scratch("worktree-binary-a");
        let b = scratch("worktree-binary-b");
        std::fs::write(a.join("out.pack"), [0xffu8, 0x00, 0xfe, 0x01]).unwrap();
        std::fs::write(b.join("out.pack"), [0x00u8, 0xff, 0x01, 0xfe]).unwrap();
        assert_eq!(probe_worktree_content(&a), probe_worktree_content(&b));
        assert!(probe_worktree_content(&a).contains("out.pack -: <binary 4 bytes>"));

        // A different length is still a difference.
        let c = scratch("worktree-binary-c");
        std::fs::write(c.join("out.pack"), [0xffu8, 0x00, 0xfe]).unwrap();
        assert_ne!(probe_worktree_content(&a), probe_worktree_content(&c));
    }

    /// A symlink is reported by its target and never followed: `Shape::Symlinks`
    /// carries one that points out of the fixture on purpose.
    #[test]
    fn worktree_content_reads_symlinks_without_following_them() {
        let repo = scratch("worktree-symlink");
        std::fs::write(repo.join("real.txt"), b"real\n").unwrap();
        std::os::unix::fs::symlink("real.txt", repo.join("link")).unwrap();
        let probe = probe_worktree_content(&repo);
        assert!(probe.contains("link -: <symlink -> real.txt>"), "got:\n{probe}");

        // Retargeted: the same file on the other end, a different fact.
        let other = scratch("worktree-symlink-other");
        std::fs::write(other.join("real.txt"), b"real\n").unwrap();
        std::os::unix::fs::symlink("../outside.txt", other.join("link")).unwrap();
        assert_ne!(probe, probe_worktree_content(&other));
    }

    /// A checkout that loses the executable bit is a difference, and the rest of
    /// the mode — umask-derived — is not compared.
    #[test]
    fn worktree_content_reports_the_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let plain = scratch("worktree-mode-plain");
        let exec = scratch("worktree-mode-exec");
        for repo in [&plain, &exec] {
            std::fs::write(repo.join("run.sh"), b"#!/bin/sh\n").unwrap();
        }
        std::fs::set_permissions(exec.join("run.sh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert!(probe_worktree_content(&plain).contains("run.sh -: "));
        assert!(probe_worktree_content(&exec).contains("run.sh x: "));
        assert_ne!(probe_worktree_content(&plain), probe_worktree_content(&exec));
    }

    /// A checksum- or pid-named file left in the working directory is machine
    /// noise in its *name* only; the elision is `probe_storage`'s, so the fact
    /// that a file was left behind survives.
    #[test]
    fn worktree_content_elides_unreproducible_names() {
        let a = scratch("worktree-name-a");
        let b = scratch("worktree-name-b");
        std::fs::write(a.join("pack-7d1e44081191d78cb9d33f3985bdace415d89e77.pack"), b"x\n")
            .unwrap();
        std::fs::write(b.join("pack-0000000000000000000000000000000000000000.pack"), b"x\n")
            .unwrap();
        assert_eq!(probe_worktree_content(&a), probe_worktree_content(&b));
        assert!(probe_worktree_content(&a).contains("pack-<hash>.pack -: x\\n"));

        // A repository that left nothing behind still differs from one that did.
        let empty = scratch("worktree-name-empty");
        assert_ne!(probe_worktree_content(&a), probe_worktree_content(&empty));
    }

    /// Every shape without a peer says so in one line, so the two digests line up
    /// positionally and the gate costs three `stat`s.
    #[test]
    fn peer_reports_absence_where_there_is_none() {
        let repo = scratch("peer-absent");
        assert_eq!(probe_peer(&repo, &repo), "# peer .remote.git\n<absent>\n");

        // A `.remote.git` that is not a repository is a different fact again —
        // a case that replaced the peer with a file has not left the fixture
        // as it found it.
        let odd = scratch("peer-not-a-repo");
        std::fs::write(odd.join(".remote.git"), b"not a repository\n").unwrap();
        assert_eq!(probe_peer(&odd, &odd), "# peer .remote.git\n<not a repository>\n");
    }

    /// The `send-pack` blind spot: a peer whose ref moved must not compare equal
    /// to a peer whose ref did not, and a ref that moved *without its object*
    /// must not compare equal to one that arrived whole.
    ///
    /// Built with stock git, because that is what the probe runs; this crate
    /// cannot measure anything at all without one.
    #[test]
    fn peer_sees_what_arrived_on_the_other_side() {
        let stock = crate::stock::git().expect("this crate needs a stock git to measure anything");
        let build = |tag: &str, body: &str, point_at_ref: bool| -> (PathBuf, String) {
            let repo = scratch(tag);
            let peer = repo.join(".remote.git");
            let run = |args: &[&str]| {
                let mut cmd = Command::new(stock);
                crate::env::harden(&mut cmd, &repo);
                let out = cmd.current_dir(&peer).args(args).output().expect("stock git runs");
                assert!(
                    out.status.success(),
                    "stock git {args:?} failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            };
            let mut init = Command::new(stock);
            crate::env::harden(&mut init, &repo);
            init.current_dir(&repo)
                .args(["init", "-q", "--bare", ".remote.git"])
                .status()
                .expect("stock git runs");
            let oid = {
                let mut cmd = Command::new(stock);
                crate::env::harden(&mut cmd, &repo);
                let out = cmd
                    .current_dir(&peer)
                    .args(["hash-object", "-w", "-t", "blob", "--stdin"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .spawn()
                    .and_then(|mut c| {
                        use std::io::Write;
                        c.stdin.take().unwrap().write_all(body.as_bytes())?;
                        c.wait_with_output()
                    })
                    .expect("stock git runs");
                String::from_utf8_lossy(&out.stdout).trim().to_string()
            };
            if point_at_ref {
                // A tag rather than a branch: `update-ref` refuses to point a
                // branch at anything that is not a commit, and what this test
                // needs is a ref that arrived, not a history.
                run(&["update-ref", "refs/tags/arrived", &oid]);
            }
            (repo, oid)
        };

        // Nothing pushed: the objects are there, no ref names them.
        let (quiet, _) = build("peer-quiet", "payload\n", false);
        // The push landed.
        let (pushed, _) = build("peer-pushed", "payload\n", true);
        // The push landed something else.
        let (other, _) = build("peer-other", "different payload\n", true);

        let quiet_digest = probe_peer(&quiet, &quiet);
        let pushed_digest = probe_peer(&pushed, &pushed);
        assert_ne!(
            quiet_digest, pushed_digest,
            "a ref that arrived on the peer must be visible:\n{pushed_digest}"
        );
        assert_ne!(
            pushed_digest,
            probe_peer(&other, &other),
            "a peer holding different bytes must not match"
        );
        assert!(
            pushed_digest.contains("refs/tags/arrived blob "),
            "the peer's refs are read with stock git:\n{pushed_digest}"
        );
        assert!(
            pushed_digest.contains("HEAD: ref: refs/heads/"),
            "the peer's HEAD is read from the filesystem:\n{pushed_digest}"
        );
        // The object census is taken of the peer's own store, so a push that
        // wrote a ref and no object is not the same fact as one that wrote both.
        assert!(pushed_digest.contains("## storage\nloose 1\n"), "got:\n{pushed_digest}");
    }

    /// A v2/v3 index laid out the way git lays one out: 12-byte header, entries
    /// of 62 fixed bytes plus a NUL-terminated name padded to a multiple of
    /// eight, then `(signature, length, data)` triples, then the checksum.
    ///
    /// The parser is pinned against real stock 2.55.0 indexes in its doc
    /// comment (`v2 entries=1 ext=[TREE:6,REUC:87]`, `v3 entries=31
    /// ext=[TREE:6,UNTR:396]`, `v4 … ext=<unparsed>`); this builds the same
    /// shape so the extensions can be moved one at a time.
    fn index_bytes(version: u32, names: &[&str], exts: &[(&str, usize)]) -> Vec<u8> {
        let mut out = b"DIRC".to_vec();
        out.extend_from_slice(&version.to_be_bytes());
        out.extend_from_slice(&(names.len() as u32).to_be_bytes());
        for name in names {
            let start = out.len();
            out.extend_from_slice(&[0x11; 60]); // times, dev, ino, mode, uid, gid, size, oid
            out.extend_from_slice(&(name.len() as u16).to_be_bytes()); // flags
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            while (out.len() - start) % 8 != 0 {
                out.push(0);
            }
        }
        for (sig, len) in exts {
            out.extend_from_slice(sig.as_bytes());
            out.extend_from_slice(&(*len as u32).to_be_bytes());
            out.extend(std::iter::repeat(0xab).take(*len));
        }
        out.extend_from_slice(&[0xcd; 20]); // trailing checksum
        out
    }

    /// The index's own shape — version and extension chain — is compared, and a
    /// dropped `TREE` or `REUC` is a difference `ls-files --stage` cannot show.
    #[test]
    fn index_meta_reads_the_extension_chain() {
        let full = scratch("index-meta-full");
        std::fs::write(
            full.join(".git/index"),
            index_bytes(2, &["a.txt"], &[("TREE", 6), ("REUC", 87)]),
        )
        .unwrap();
        // `<unparsed>` for both bodies because `index_bytes` fills them with
        // 0xab: the chain is what this test moves, and a synthetic body is not a
        // cache tree. The real bodies are read in
        // `index_extension_detail_reads_a_real_cache_tree`.
        assert_eq!(
            probe_index_meta(&full),
            "# index-meta\nindex: v2 entries=1 ext=[TREE:6(<unparsed>),REUC:87(<unparsed>)]\n"
        );

        // The cache-tree destroyed: the defect this harness has already had once,
        // found by hand because no probe asked.
        let no_tree = scratch("index-meta-no-tree");
        std::fs::write(no_tree.join(".git/index"), index_bytes(2, &["a.txt"], &[("REUC", 87)]))
            .unwrap();
        assert_ne!(probe_index_meta(&full), probe_index_meta(&no_tree));

        // The resolve-undo record dropped: `checkout -m` silently stops working.
        let no_reuc = scratch("index-meta-no-reuc");
        std::fs::write(no_reuc.join(".git/index"), index_bytes(2, &["a.txt"], &[("TREE", 6)]))
            .unwrap();
        assert_ne!(probe_index_meta(&full), probe_index_meta(&no_reuc));

        // A cache-tree that covers less is a smaller cache-tree.
        let short_tree = scratch("index-meta-short-tree");
        std::fs::write(
            short_tree.join(".git/index"),
            index_bytes(2, &["a.txt"], &[("TREE", 3), ("REUC", 87)]),
        )
        .unwrap();
        assert_ne!(probe_index_meta(&full), probe_index_meta(&short_tree));

        // The on-disk version is a fact in its own right.
        let v3 = scratch("index-meta-v3");
        std::fs::write(
            v3.join(".git/index"),
            index_bytes(3, &["a.txt"], &[("TREE", 6), ("REUC", 87)]),
        )
        .unwrap();
        assert_ne!(probe_index_meta(&full), probe_index_meta(&v3));
    }

    /// What the parser refuses to guess at, it says so about — symmetrically, so
    /// an unparsed chain reports no difference where there is none.
    #[test]
    fn index_meta_reports_what_it_cannot_parse() {
        // v4 names are prefix-compressed; version and entry count still count.
        let v4 = scratch("index-meta-v4");
        std::fs::write(v4.join(".git/index"), index_bytes(4, &["a.txt"], &[("TREE", 6)])).unwrap();
        assert_eq!(probe_index_meta(&v4), "# index-meta\nindex: v4 entries=1 ext=<unparsed>\n");

        // Truncated, corrupt, and absent are three different facts.
        let corrupt = scratch("index-meta-corrupt");
        std::fs::write(corrupt.join(".git/index"), vec![0x00; 64]).unwrap();
        assert!(probe_index_meta(&corrupt).contains("index: <not-an-index 64 bytes>"));

        let absent = scratch("index-meta-absent");
        assert_eq!(probe_index_meta(&absent), "# index-meta\nindex: <absent>\n");
    }

    /// A split index and a linked worktree's index are indexes too, and the
    /// shared half's checksum-bearing name is elided like every other one.
    #[test]
    fn index_meta_covers_shared_and_linked_indexes() {
        let repo = scratch("index-meta-split");
        std::fs::write(repo.join(".git/index"), index_bytes(2, &["a.txt"], &[("link", 24)]))
            .unwrap();
        std::fs::write(
            repo.join(".git/sharedindex.7d1e44081191d78cb9d33f3985bdace415d89e77"),
            index_bytes(2, &["a.txt", "b.txt"], &[("TREE", 6)]),
        )
        .unwrap();
        std::fs::create_dir_all(repo.join(".git/worktrees/wt")).unwrap();
        std::fs::write(
            repo.join(".git/worktrees/wt/index"),
            index_bytes(2, &["a.txt"], &[("TREE", 6)]),
        )
        .unwrap();

        let probe = probe_index_meta(&repo);
        // `index_bytes` fills the body with 0xab, so the split-index link reads
        // as a non-null base with a bitmap pair behind it — and the four filler
        // bytes behind the id are not a serialised ewah bitmap, which the parser
        // says rather than guesses. A real `link` body is decoded down to its
        // two bit sets in
        // [`a_split_index_link_is_read_down_to_its_two_bit_sets`].
        assert!(
            probe.contains("index: v2 entries=1 ext=[link:24(base=set bitmaps=yes del=<unparsed>)]"),
            "got:\n{probe}"
        );
        assert!(
            probe.contains("sharedindex.<hash>: v2 entries=2 ext=[TREE:6(<unparsed>)]"),
            "got:\n{probe}"
        );
        assert!(
            probe.contains("worktrees/wt/index: v2 entries=1 ext=[TREE:6(<unparsed>)]"),
            "got:\n{probe}"
        );
    }

    /// `fetch <remote> <ref>` updates no branch: `FETCH_HEAD` is its whole
    /// result, and it is one line so a multi-ref fetch cannot shift the digest.
    #[test]
    fn fetch_head_is_measured_as_one_line() {
        let none = scratch("fetch-head-absent");
        assert_eq!(probe_fetch_head(&none), "# fetch-head\nFETCH_HEAD: <absent>\n");

        let fetched = scratch("fetch-head-written");
        std::fs::write(
            fetched.join(".git/FETCH_HEAD"),
            b"b015a4d60ee11600ec4c9413e74ce42c7a9593d3\t\tbranch 'main' of ./.remote\n",
        )
        .unwrap();
        assert_eq!(
            probe_fetch_head(&fetched),
            "# fetch-head\nFETCH_HEAD: b015a4d60ee11600ec4c9413e74ce42c7a9593d3\t\tbranch 'main' of ./.remote\\n\n"
        );

        // `--no-write-fetch-head` against the default, and a fetch that recorded
        // the wrong ref: neither moves any other surface.
        assert_ne!(probe_fetch_head(&none), probe_fetch_head(&fetched));
        let other = scratch("fetch-head-other");
        std::fs::write(
            other.join(".git/FETCH_HEAD"),
            b"b015a4d60ee11600ec4c9413e74ce42c7a9593d3\tnot-for-merge\tbranch 'main' of ./.remote\n",
        )
        .unwrap();
        assert_ne!(probe_fetch_head(&fetched), probe_fetch_head(&other));

        // A two-ref fetch is still one line.
        let many = scratch("fetch-head-many");
        std::fs::write(
            many.join(".git/FETCH_HEAD"),
            b"aaa\t\tbranch 'main' of ./.remote\nbbb\tnot-for-merge\tbranch 'div' of ./.remote\n",
        )
        .unwrap();
        assert_eq!(probe_fetch_head(&many).lines().count(), 2);
    }

    /// The hole under every comparison in this crate: `String::from_utf8_lossy`
    /// maps every invalid byte to U+FFFD, so two different binary streams
    /// compared equal. `pack-objects --stdout`, `bundle create -` and `archive`
    /// are all compared through this function.
    #[test]
    fn two_different_binary_streams_do_not_normalize_to_one_string() {
        let root = scratch("normalize-binary");
        let a = [0x1fu8, 0x8b, 0xc0, 0x80, 0x41];
        let b = [0x1fu8, 0x8b, 0xc1, 0x80, 0x41];
        assert_eq!(
            String::from_utf8_lossy(&a),
            String::from_utf8_lossy(&b),
            "the premise: a lossy decode collapses these two"
        );
        let n = |raw: &[u8]| super::normalize(raw, &root, &root, Path::new(""));
        assert_ne!(n(&a), n(&b), "the two streams must not compare equal");
        assert!(n(&a).contains("<non-utf8 stream: 5 bytes>"), "got:\n{}", n(&a));
        // Exact, not hashed: the rendering names the bytes that moved.
        assert!(n(&a).contains("1f8bc08041"), "got:\n{}", n(&a));
        // And a stream that really is the same still matches.
        assert_eq!(n(&a), n(&[0x1fu8, 0x8b, 0xc0, 0x80, 0x41]));
    }

    /// Text is unaffected: the byte substitution masks exactly what the string
    /// substitution masked, so nothing that was compared before is compared
    /// differently now.
    #[test]
    fn text_normalization_is_unchanged_by_the_byte_level_masking() {
        let repo = scratch("normalize-text");
        let home = scratch("normalize-home");
        let raw = format!(
            "fatal: could not read {}/.git/HEAD\nhint: see {}/.gitconfig\n",
            repo.display(),
            home.display()
        );
        let out = super::normalize(raw.as_bytes(), &repo, &home, Path::new(""));
        assert_eq!(out, "fatal: could not read <REPO>/.git/HEAD\nhint: see <HOME>/.gitconfig\n");

        // An empty exec-path masks nothing rather than everything.
        assert_eq!(super::normalize(b"plain\n", &repo, &home, Path::new("")), "plain\n");
    }

    /// A path inside a binary stream is masked too — the two sides run at
    /// different roots, and a pack that embeds one would otherwise differ for a
    /// reason that is not a defect.
    #[test]
    fn a_path_inside_a_binary_stream_is_still_masked() {
        let repo = scratch("normalize-binary-path");
        let mut raw = vec![0x00u8, 0xff];
        raw.extend_from_slice(repo.to_string_lossy().as_bytes());
        raw.extend_from_slice(&[0xfe, 0x00]);
        let out = super::normalize(&raw, &repo, &repo, Path::new(""));
        // `<REPO>` in hex, between the two invalid bytes it sat between.
        let masked: String = "<REPO>".bytes().map(|b| format!("{b:02x}")).collect();
        assert!(out.contains(&format!("00ff{masked}fe00")), "got:\n{out}");
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
                vec![ConfigEntry::set(ConfigScope::CommandLine, "rerere.enabled", "true")]
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

    // -----------------------------------------------------------------------
    // Configuration scopes
    // -----------------------------------------------------------------------

    /// The declaration order of [`ConfigScope`] is git's precedence order,
    /// lowest first — the fact every scoped case is read against.
    ///
    /// Written out as a literal expectation rather than derived from the enum,
    /// because the enum is what is under test: a variant inserted in the wrong
    /// place would still compile, would still run, and would quietly make
    /// [`ConfigScope::ORDERED`] a lie that the module documentation repeats.
    /// The sequence is the one measured from stock git 2.55.0 with
    /// `config --show-origin --get-all`, where a key set in all of them resolves
    /// to the command-line value and lists in this order.
    #[test]
    fn config_scope_declaration_order_is_gits_precedence_order() {
        assert_eq!(
            ConfigScope::ORDERED,
            &[
                ConfigScope::System,
                ConfigScope::Global,
                ConfigScope::Repo,
                ConfigScope::Worktree,
                ConfigScope::Env,
                ConfigScope::CommandLine,
            ]
        );
        // …and the declaration order agrees with it, which is what `Ord` means
        // for this type and what a reader comparing two scopes will assume.
        let mut sorted = ConfigScope::ORDERED.to_vec();
        sorted.sort();
        assert_eq!(sorted, ConfigScope::ORDERED.to_vec());
        // `Modules` is outside the sequence and must not be in it.
        assert!(!ConfigScope::ORDERED.contains(&ConfigScope::Modules));
        assert_eq!(ConfigScope::ALL.len(), ConfigScope::ORDERED.len() + 1);
        // Exactly the file-backed scopes have a file, and no other scope does.
        for scope in ConfigScope::ALL {
            assert_eq!(
                scope.is_file(),
                scope_file(Path::new("/tmp/x"), *scope).is_some(),
                "{} disagrees with itself about having a file",
                scope.name()
            );
        }
    }

    /// Every file a scope is delivered through lives **inside that side's own
    /// fixture copy**, and the two synthetic ones live inside its git directory.
    ///
    /// This is the invariant that lets [`ConfigScope::Global`] and
    /// [`ConfigScope::System`] re-point two of `env::harden`'s pins without
    /// reopening the leak those pins close: the path is a function of the repo
    /// root and nothing else, so no case can aim either variable at the
    /// machine's real `~/.gitconfig` or `/etc/gitconfig`. The git-directory part
    /// matters too — a synthetic file in the worktree would show up as `??` in
    /// every `status` the case runs and in the state probe, which is a difference
    /// the case never asked for.
    #[test]
    fn scope_files_stay_inside_the_fixture() {
        let repo = Path::new("/nowhere/fixture");
        for scope in ConfigScope::FILES {
            let path = scope_file(repo, *scope).expect("a file scope has a file");
            assert!(path.starts_with(repo), "{} escapes the fixture: {:?}", scope.name(), path);
        }
        for scope in [ConfigScope::Global, ConfigScope::System] {
            let path = scope_file(repo, scope).unwrap();
            assert!(
                path.starts_with(git_dir(repo)),
                "{} is visible to the worktree: {:?}",
                scope.name(),
                path
            );
        }
        assert_eq!(scope_file(repo, ConfigScope::Env), None);
        assert_eq!(scope_file(repo, ConfigScope::CommandLine), None);
    }

    /// `section.key` and `section.subsection.key` split the way git's own writer
    /// splits them: first dot, last dot, and everything between is one
    /// subsection — dots included, which is why `branch.a.b.merge` is branch
    /// `a.b` and not a three-level key.
    #[test]
    fn split_config_key_finds_the_subsection() {
        assert_eq!(split_config_key("core.abbrev"), Some(("core", None, "abbrev")));
        assert_eq!(
            split_config_key("branch.main.merge"),
            Some(("branch", Some("main"), "merge"))
        );
        assert_eq!(
            split_config_key("branch.a.b.merge"),
            Some(("branch", Some("a.b"), "merge"))
        );
        // Not a key at all: written as a bare line, which git then rejects — the
        // refusal is the measurement, so it must not be silently repaired.
        assert_eq!(split_config_key("nosection"), None);
        assert_eq!(split_config_key(".key"), None);
        assert_eq!(split_config_key("section."), None);
    }

    /// A value written into a file delivers **the same string** `-c` would.
    ///
    /// Unquoted, git's reader strips surrounding whitespace, stops at `#`, and
    /// reads an absent value as boolean true — three silent rewrites that would
    /// make the scope stop being the only difference between a file case and its
    /// `-c` twin. Every value here is one the fuzz pools actually draw.
    #[test]
    fn file_values_are_quoted_so_the_scope_is_the_only_variable() {
        assert_eq!(quote_config_value("4"), "\"4\"");
        // The empty value: `""` is the empty string, while a bare `key` with
        // nothing after it is boolean true. `-c key=` means the former.
        assert_eq!(quote_config_value(""), "\"\"");
        // Whitespace and comment characters survive verbatim inside quotes.
        assert_eq!(quote_config_value(" "), "\" \"");
        assert_eq!(quote_config_value("# not a comment"), "\"# not a comment\"");
        assert_eq!(quote_config_value("a; b"), "\"a; b\"");
        // The five escapes `config.c:parse_value` accepts, and nothing else —
        // a raw backslash would start an escape git does not know and turn a
        // value into a parse error the case did not ask for.
        assert_eq!(quote_config_value("\t"), "\"\\t\"");
        assert_eq!(quote_config_value("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote_config_value("a\\b"), "\"a\\\\b\"");
        assert_eq!(quote_config_value("a\nb"), "\"a\\nb\"");
    }

    /// A setting becomes one stanza, a subsection is quoted into its header, and
    /// a raw line is written through untouched.
    ///
    /// One header per setting even when two settings share a section: a repeated
    /// section is legal git, and repeating it is what keeps two draws of one key
    /// legible as two stanzas — which is the last-value-wins premise the ordering
    /// exists to test.
    #[test]
    fn a_setting_becomes_one_stanza_and_a_raw_line_stays_a_line() {
        assert_eq!(
            render_config_entry(&ConfigEntry::set(ConfigScope::Repo, "core.abbrev", "4")),
            "[core]\n\tabbrev = \"4\"\n"
        );
        assert_eq!(
            render_config_entry(&ConfigEntry::set(
                ConfigScope::Repo,
                "branch.main.merge",
                "refs/heads/main"
            )),
            "[branch \"main\"]\n\tmerge = \"refs/heads/main\"\n"
        );
        assert_eq!(
            render_config_entry(&ConfigEntry::raw(ConfigScope::Repo, "[core")),
            "[core\n"
        );
        // A key that is not a `section.key` is emitted as the bare line it is,
        // because git's refusal of it is the thing being compared.
        assert_eq!(
            render_config_entry(&ConfigEntry::set(ConfigScope::Repo, "nosection", "1")),
            "nosection = \"1\"\n"
        );
    }

    /// The premise a failure block prints: one entry per file, in draw order,
    /// plus the gate the worktree scope needs and the environment pairs.
    ///
    /// Order inside a file is the whole content of a last-wins case, so it is
    /// asserted rather than assumed — a renderer that grouped by key, or that
    /// sorted, would turn `4` then `12` into a case whose printed premise says
    /// the opposite of what ran.
    #[test]
    fn the_config_premise_is_rendered_per_file_in_draw_order() {
        let entries = vec![
            ConfigEntry::set(ConfigScope::Repo, "core.abbrev", "4"),
            ConfigEntry::set(ConfigScope::Repo, "core.abbrev", "12"),
            ConfigEntry::set(ConfigScope::Worktree, "core.abbrev", "40"),
            ConfigEntry::set(ConfigScope::Env, "diff.renames", "copies"),
            ConfigEntry::set(ConfigScope::CommandLine, "status.short", "true"),
        ];
        let premise = config_premise(&entries);
        let by_place = |name: &str| -> String {
            premise
                .iter()
                .find(|(w, _)| w == name)
                .unwrap_or_else(|| panic!("{name} missing from {premise:?}"))
                .1
                .clone()
        };
        // The gate is written before the file it gates, and into `.git/config`.
        assert_eq!(premise[0].0, ".git/config");
        assert_eq!(premise[0].1, "[extensions]\n\tworktreeConfig = true\n");
        assert_eq!(
            by_place(".git/config.worktree"),
            "[core]\n\tabbrev = \"40\"\n"
        );
        // Two stanzas for one key, in the order drawn: the second wins, and the
        // printed premise has to show why.
        let repo = premise
            .iter()
            .filter(|(w, _)| w == ".git/config")
            .map(|(_, t)| t.clone())
            .collect::<Vec<_>>();
        assert_eq!(repo.len(), 2, "the gate and the settings are separate stanzas");
        assert_eq!(repo[1], "[core]\n\tabbrev = \"4\"\n[core]\n\tabbrev = \"12\"\n");
        assert_eq!(
            by_place("environment"),
            "GIT_CONFIG_COUNT=1\nGIT_CONFIG_KEY_0=diff.renames\nGIT_CONFIG_VALUE_0=copies\n"
        );
        // The command-line entry is not a premise: it is in the argv, and the
        // block that prints this one already prints the argv.
        assert!(!premise.iter().any(|(_, t)| t.contains("status.short")));
    }

    /// Only the command-line scope reaches argv, and only the non-command-line
    /// scopes reach the `::config[…]` segment.
    ///
    /// Both halves matter. Rendering a file-scoped entry as `-c` would deliver
    /// the setting through the one scope the entry said it was *not* using,
    /// which is the entire thing being measured; leaving it out of the id would
    /// make the case unreproducible, since nothing else in the id says a file
    /// was written.
    #[test]
    fn each_scope_is_rendered_exactly_once_and_in_the_right_place() {
        let case = Case::new("status", &["status"], Shape::Linear).with_scoped_config(vec![
            ConfigEntry::set(ConfigScope::CommandLine, "core.abbrev", "4"),
            ConfigEntry::set(ConfigScope::Repo, "core.abbrev", "12"),
            ConfigEntry::set(ConfigScope::Global, "diff.renames", "copies"),
            ConfigEntry::raw(ConfigScope::Worktree, "[core"),
        ]);
        assert_eq!(case.argv(), vec!["-c", "core.abbrev=4", "status"]);
        assert_eq!(
            case.id(),
            "linear::status::-c core.abbrev=4 status\
             ::config[repo:core.abbrev=12 global:diff.renames=copies worktree:~[core]"
        );
        // Dropping one entry drops exactly one fact from the id, which is what
        // makes `fuzz::shrink`'s walk over `config` mean anything.
        assert_eq!(case.size(), 4);
    }

    /// A case may not carry `GIT_CONFIG_*` in its own environment *and* use
    /// [`ConfigScope::Env`], because the runner would overwrite the case's pairs
    /// and the case would run under a configuration its own id does not
    /// describe.
    ///
    /// Checked against the real corpus rather than in the abstract: the curated
    /// discovery cases set `GIT_CONFIG_COUNT`/`KEY_0`/`VALUE_0` directly and
    /// predate the scope, so this is the one place the two mechanisms could
    /// actually collide. `run_side` asserts it again per case; this catches it
    /// at `cargo test` time, before a corpus entry that would abort a sweep is
    /// committed.
    #[test]
    fn no_case_mixes_the_env_config_scope_with_its_own_environment() {
        let mut checked = 0;
        let envelopes = crate::corpus::sequences().into_iter().map(|s| s.envelope);
        for case in crate::corpus::cases().into_iter().chain(envelopes) {
            let hand_written = case.env.iter().any(|(k, _)| k.starts_with("GIT_CONFIG_"));
            let scoped = case.config.iter().any(|e| e.scope == ConfigScope::Env);
            assert!(
                !(hand_written && scoped),
                "{} sets GIT_CONFIG_* twice, by two mechanisms",
                case.id()
            );
            if hand_written {
                checked += 1;
            }
        }
        assert!(checked > 0, "no corpus case sets GIT_CONFIG_* by hand any more — drop the guard");
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
            Verdict::InteropDiff,
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
            stock_interop: String::new(),
            zvcs_interop: String::new(),
            interop_probed: false,
            zvcs_repeat: None,
            alt: None,
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

    // -----------------------------------------------------------------------
    // Exact rendering: the values that were still going through `from_utf8_lossy`
    // -----------------------------------------------------------------------

    /// Two state files that differ only in a byte no UTF-8 decoder can render
    /// must not compare equal.
    ///
    /// `\xc0\x80` and `\xc1\x80` are the smallest pair that makes the point:
    /// both decode to two U+FFFD, so the lossy rendering compared them equal —
    /// and `MERGE_MSG`, a `rebase-apply` patch of a binary file and a conflicted
    /// note are all reachable with bytes that are not text.
    #[test]
    fn a_state_file_that_is_not_text_is_rendered_exactly() {
        let a = scratch("value-nonutf8-a");
        let b = scratch("value-nonutf8-b");
        std::fs::write(a.join(".git/MERGE_MSG"), b"m: \xc0\x80\n").unwrap();
        std::fs::write(b.join(".git/MERGE_MSG"), b"m: \xc1\x80\n").unwrap();
        assert_ne!(
            probe_op_state(&a),
            probe_op_state(&b),
            "two different byte sequences must not collapse onto one string"
        );
        assert!(
            probe_op_state(&a).contains("MERGE_MSG: m: \\xc0\\x80\\n"),
            "got:\n{}",
            probe_op_state(&a)
        );
        // Still exactly one line per fact: the escape introduces no newline.
        assert!(probe_op_state(&a).lines().skip(1).all(|l| l.contains(": ")));

        // Text is untouched — the escaping a valid-UTF-8 value gets is the same
        // one it got before, backslash first.
        let text = scratch("value-text");
        std::fs::write(text.join(".git/MERGE_MSG"), "a\\b\r\nünïcødé\n").unwrap();
        assert!(
            probe_op_state(&text).contains("MERGE_MSG: a\\\\b\\r\\nünïcødé\\n"),
            "got:\n{}",
            probe_op_state(&text)
        );
    }

    /// The rr-cache preimage of a **binary** conflict: bytes with markers around
    /// them, which `rerere` records like any other resolution and which the lossy
    /// rendering compared by length and nothing else.
    #[test]
    fn a_binary_rr_cache_preimage_does_not_collapse_to_one_string() {
        let a = scratch("rr-binary-a");
        let b = scratch("rr-binary-b");
        for (repo, byte) in [(&a, 0xc0u8), (&b, 0xc1u8)] {
            let dir = repo.join(".git/rr-cache/0123456789abcdef0123456789abcdef01234567");
            std::fs::create_dir_all(&dir).unwrap();
            let mut preimage = b"<<<<<<<\n".to_vec();
            preimage.extend_from_slice(&[byte, 0x80, b'\n']);
            preimage.extend_from_slice(b">>>>>>>\n");
            std::fs::write(dir.join("preimage"), &preimage).unwrap();
        }
        assert_ne!(
            probe_rr_cache(&a),
            probe_rr_cache(&b),
            "a binary preimage must be compared exactly:\n{}",
            probe_rr_cache(&a)
        );
        // Rendered as hex under a header naming the length, which is what
        // `render_binary` does for every other non-text surface in this crate.
        assert!(probe_rr_cache(&a).contains("<non-utf8 stream: "), "got:\n{}", probe_rr_cache(&a));

        // A text preimage still goes into the digest verbatim.
        let text = scratch("rr-text");
        let dir = text.join(".git/rr-cache/0123456789abcdef0123456789abcdef01234567");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("preimage"), b"<<<<<<<\nours\n=======\ntheirs\n>>>>>>>\n").unwrap();
        assert!(probe_rr_cache(&text).contains("\nours\n=======\ntheirs\n"));
    }

    /// A reflog is text by construction until a ref name or an ident is not, and
    /// git neither requires UTF-8 nor validates it.
    #[test]
    fn a_reflog_that_is_not_text_is_rendered_exactly() {
        let a = scratch("reflog-nonutf8-a");
        let b = scratch("reflog-nonutf8-b");
        for (repo, byte) in [(&a, 0xc0u8), (&b, 0xc1u8)] {
            std::fs::create_dir_all(repo.join(".git/logs/refs/heads")).unwrap();
            let mut line =
                b"1111111111111111111111111111111111111111 2222222222222222222222222222222222222222 "
                    .to_vec();
            line.extend_from_slice(&[byte, 0x80]);
            line.extend_from_slice(b" <a@u.thor> 1112911993 +0000\tcommit: one\n");
            std::fs::write(repo.join(".git/logs/refs/heads/main"), &line).unwrap();
        }
        assert_ne!(probe_reflogs(&a), probe_reflogs(&b));
    }

    // -----------------------------------------------------------------------
    // `.git/modules/**`
    // -----------------------------------------------------------------------

    /// A repository with no submodule says so in one line, so the two digests
    /// being compared line up positionally.
    #[test]
    fn submodule_gitdirs_report_absence_as_one_line() {
        let repo = scratch("modules-absent");
        assert_eq!(probe_modules(&repo, &repo), "# submodule-gitdirs\n<absent>\n");
    }

    /// Build a module git directory by hand: enough of one for
    /// [`looks_like_git_dir`] to accept it, with the files a port could get
    /// wrong.
    fn fake_module(repo: &Path, rel: &str, head: &str, url: &str) -> PathBuf {
        let dir = repo.join(".git/modules").join(rel);
        std::fs::create_dir_all(dir.join("objects/info")).unwrap();
        std::fs::create_dir_all(dir.join("refs/heads")).unwrap();
        std::fs::create_dir_all(dir.join("hooks")).unwrap();
        std::fs::write(dir.join("HEAD"), format!("ref: {head}\n")).unwrap();
        std::fs::write(
            dir.join("config"),
            format!("[core]\n\tworktree = ../../../sub\n[remote \"origin\"]\n\turl = {url}\n"),
        )
        .unwrap();
        dir
    }

    /// The whole gap in one assertion: a submodule's `config` and `HEAD` live
    /// where nothing looked, and the corpus already found `submodule add`
    /// writing the url unresolved.
    #[test]
    fn submodule_gitdirs_compare_the_module_files_by_content() {
        let a = scratch("modules-a");
        let b = scratch("modules-b");
        fake_module(&a, "sub", "refs/heads/main", "../upstream");
        fake_module(&b, "sub", "refs/heads/main", "/abs/upstream");
        assert_ne!(
            probe_modules(&a, &a),
            probe_modules(&b, &b),
            "an unresolved url is a difference:\n{}",
            probe_modules(&a, &a)
        );

        // A detached module HEAD is not the same fact as a branch.
        let c = scratch("modules-c");
        fake_module(&c, "sub", "refs/heads/other", "../upstream");
        assert_ne!(probe_modules(&a, &a), probe_modules(&c, &c));

        // One fact per line, as everywhere else in the digest.
        let digest = probe_modules(&a, &a);
        assert!(digest.lines().any(|l| l == "sub/HEAD: ref: refs/heads/main\\n"), "got:\n{digest}");
        assert!(digest.contains("## sub storage\nloose 0\n"), "got:\n{digest}");
    }

    /// The module index is measured the way a linked worktree's is: by length
    /// and by its parsed header, never by its stat bytes.
    #[test]
    fn submodule_gitdir_index_is_measured_by_length_not_stat_bytes() {
        let a = scratch("modules-index-a");
        let b = scratch("modules-index-b");
        for (repo, stat) in [(&a, 0x11u8), (&b, 0x22u8)] {
            let dir = fake_module(repo, "sub", "refs/heads/main", "../upstream");
            let mut index = index_bytes(2, &["mod.txt"], &[]);
            // Move only the stat fields of the single entry: same length, same
            // entry count, same extension chain.
            for byte in index.iter_mut().skip(12).take(40) {
                *byte = stat;
            }
            std::fs::write(dir.join("index"), &index).unwrap();
        }
        assert_eq!(
            probe_modules(&a, &a),
            probe_modules(&b, &b),
            "stat data is not comparable between two roots"
        );
        assert!(probe_modules(&a, &a).contains("sub/index: <index 104 bytes> v2 entries=1 ext=[]"));

        // An entry that is not there is a difference the length carries.
        let c = scratch("modules-index-c");
        let dir = fake_module(&c, "sub", "refs/heads/main", "../upstream");
        std::fs::write(dir.join("index"), index_bytes(2, &[], &[])).unwrap();
        assert_ne!(probe_modules(&a, &a), probe_modules(&c, &c));
    }

    /// Hook samples are the installation's own templates: length, never bytes.
    /// The object store is censused, never read — a loose object is zlib at the
    /// writer's chosen level.
    #[test]
    fn submodule_gitdirs_read_neither_hook_bytes_nor_object_bytes() {
        let a = scratch("modules-hooks-a");
        let b = scratch("modules-hooks-b");
        for (repo, body) in [(&a, "#!/bin/sh\nexit 0\n"), (&b, "#!/bin/sh\nexit 1\n")] {
            let dir = fake_module(repo, "sub", "refs/heads/main", "../upstream");
            std::fs::write(dir.join("hooks/pre-commit.sample"), body).unwrap();
            std::fs::create_dir_all(dir.join("objects/ab")).unwrap();
            std::fs::write(dir.join("objects/ab/cdef"), body).unwrap();
        }
        assert_eq!(probe_modules(&a, &a), probe_modules(&b, &b));
        assert!(probe_modules(&a, &a).contains("sub/hooks/pre-commit.sample: <17 bytes>"));
        // The census still counts the object, so a module that fetched nothing
        // is a different digest.
        assert!(probe_modules(&a, &a).contains("## sub storage\nloose 1\n"));
        let empty = scratch("modules-hooks-empty");
        fake_module(&empty, "sub", "refs/heads/main", "../upstream");
        assert_ne!(probe_modules(&a, &a), probe_modules(&empty, &empty));
    }

    /// A hook the *case* installed is not a template, and its bytes are the
    /// whole fact. `init` copies nothing but `*.sample`, so a name without that
    /// suffix is there because something put it there.
    #[test]
    fn an_installed_module_hook_is_compared_by_content_and_by_its_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let a = scratch("modules-hook-live-a");
        let b = scratch("modules-hook-live-b");
        for (repo, body) in [(&a, "#!/bin/sh\nexit 0\n"), (&b, "#!/bin/sh\nexit 1\n")] {
            let dir = fake_module(repo, "sub", "refs/heads/main", "../upstream");
            let hook = dir.join("hooks/pre-commit");
            std::fs::write(&hook, body).unwrap();
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Same length, different bytes — the exact pair the byte count hid.
        assert_ne!(probe_modules(&a, &a), probe_modules(&b, &b));
        assert!(
            probe_modules(&a, &a).contains("sub/hooks/pre-commit: x #!/bin/sh\\nexit 0\\n"),
            "got:\n{}",
            probe_modules(&a, &a)
        );

        // A hook that is not executable does not run, which is the one mode bit
        // that is a fact about behaviour rather than about the umask.
        let inert = scratch("modules-hook-inert");
        let dir = fake_module(&inert, "sub", "refs/heads/main", "../upstream");
        let hook = dir.join("hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_ne!(probe_modules(&a, &a), probe_modules(&inert, &inert));
        assert!(probe_modules(&inert, &inert).contains("sub/hooks/pre-commit: - #!"));
    }

    /// `objects/info/alternates` is a list of paths rather than an object, and
    /// it is the difference between a module whose objects are reachable and one
    /// whose are not. The census listed its name and never opened it.
    #[test]
    fn a_module_alternates_file_is_read_and_the_objects_beside_it_are_not() {
        let a = scratch("modules-alt-a");
        let b = scratch("modules-alt-b");
        for (repo, alt) in [(&a, "/one/objects\n"), (&b, "/two/objects\n")] {
            let dir = fake_module(repo, "sub", "refs/heads/main", "../upstream");
            std::fs::create_dir_all(dir.join("objects/info")).unwrap();
            std::fs::write(dir.join("objects/info/alternates"), alt).unwrap();
            // Same length, different bytes, under the same `objects/` prefix:
            // this one must stay unread.
            std::fs::create_dir_all(dir.join("objects/ab")).unwrap();
            std::fs::write(dir.join("objects/ab/cdef"), alt.to_uppercase()).unwrap();
        }
        let (da, db) = (probe_modules(&a, &a), probe_modules(&b, &b));
        assert_ne!(da, db, "two different alternates files must not compare equal");
        assert!(
            da.lines().any(|l| l == "sub/objects/info/alternates: /one/objects\\n"),
            "got:\n{da}"
        );
        assert!(
            !da.contains("cdef: "),
            "a loose object is still never read as bytes:\n{da}"
        );
    }

    /// A submodule inside a submodule lives at `.git/modules/<a>/modules/<b>`,
    /// and a probe that stopped at the first level would leave the nested case —
    /// the one a port is most likely to get wrong — unmeasured.
    #[test]
    fn a_nested_submodule_gitdir_is_reached() {
        let repo = scratch("modules-nested");
        let outer = fake_module(&repo, "outer", "refs/heads/main", "../upstream");
        std::fs::create_dir_all(outer.join("modules")).unwrap();
        std::fs::create_dir_all(outer.join("modules/inner/objects")).unwrap();
        std::fs::create_dir_all(outer.join("modules/inner/refs")).unwrap();
        std::fs::write(outer.join("modules/inner/HEAD"), b"ref: refs/heads/deep\n").unwrap();

        let digest = probe_modules(&repo, &repo);
        assert!(
            digest.lines().any(|l| l == "outer/modules/inner/HEAD: ref: refs/heads/deep\\n"),
            "got:\n{digest}"
        );
        assert!(digest.contains("## outer/modules/inner storage\n"), "got:\n{digest}");
    }

    /// The same question asked of a **real** submodule built by stock git, so
    /// the probe is pinned against the layout git actually writes rather than
    /// against my own fixture of it.
    #[test]
    fn a_real_submodule_gitdir_is_read_with_stock_git() {
        use std::os::unix::fs::PermissionsExt;
        let stock = crate::stock::git().expect("this crate needs a stock git to measure anything");
        let root = scratch("modules-real");
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let upstream = root.join("upstream");
        let parent = root.join("parent");
        std::fs::create_dir_all(&upstream).unwrap();
        std::fs::create_dir_all(&parent).unwrap();
        let run = |dir: &Path, args: &[&str]| {
            let mut cmd = Command::new(stock);
            crate::env::harden(&mut cmd, &home);
            let out = cmd.current_dir(dir).args(args).output().expect("stock git runs");
            assert!(
                out.status.success(),
                "stock git {args:?} in {dir:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&upstream, &["init", "-q", "-b", "main"]);
        std::fs::write(upstream.join("mod.txt"), b"submodule content\n").unwrap();
        run(&upstream, &["add", "."]);
        run(&upstream, &["commit", "-qm", "submodule initial"]);
        run(&parent, &["init", "-q", "-b", "main"]);
        std::fs::write(parent.join("a.txt"), b"base\n").unwrap();
        run(&parent, &["add", "."]);
        run(&parent, &["commit", "-qm", "base"]);
        run(
            &parent,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                upstream.to_str().unwrap(),
                "sub",
            ],
        );

        let digest = probe_modules(&parent, &home);
        // The module's own refs and objects, read by stock git inside the module
        // git directory — the two facts a superproject `status` cannot show.
        assert!(
            digest.contains("## sub for-each-ref"),
            "the module's refs are asked of stock:\n{digest}"
        );
        assert!(
            digest.lines().any(|l| l.starts_with("refs/heads/main commit ")),
            "got:\n{digest}"
        );
        assert!(
            digest.lines().any(|l| l.starts_with("sub/config: ") && l.contains("worktree")),
            "the module's config is compared by content:\n{digest}"
        );
        assert!(
            digest.lines().any(|l| l.starts_with("sub/index: <index ")),
            "the module index is measured by length:\n{digest}"
        );
        // git's own validator, run inside the module git directory: the half of
        // the object bytes that *is* comparable. A store stock is happy with
        // says exit 0 and nothing else.
        assert!(
            digest.contains("## sub fsck --strict\nexit: Some(0)\n"),
            "stock validates the module's object store:\n{digest}"
        );
        // And it is what the census cannot do. Rewrite one loose object to hold
        // different content under its own name: `cat-file --batch-all-objects`
        // reads the id off the filename and reports the store unchanged.
        let loose: Vec<PathBuf> = walk_files(&parent.join(".git/modules/sub/objects"))
            .into_iter()
            .filter(|(rel, _)| !rel.starts_with("pack/") && !rel.starts_with("info/"))
            .map(|(_, path)| path)
            .collect();
        assert!(!loose.is_empty(), "the module clone brought loose objects over");
        let victim = &loose[0];
        std::fs::set_permissions(victim, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut corrupt = std::fs::read(victim).unwrap();
        // Truncating the zlib stream is a content change stock will notice and
        // needs no compressor here.
        corrupt.truncate(corrupt.len() - 1);
        std::fs::write(victim, &corrupt).unwrap();
        let after = probe_modules(&parent, &home);
        assert_ne!(digest, after, "a corrupt module object must move the digest");
        assert!(
            !after.contains("## sub fsck --strict\nexit: Some(0)\n"),
            "and it is fsck that says so:\n{after}"
        );
        // The blob, the tree and the commit the submodule clone brought over.
        assert!(digest.contains(" blob 18\n"), "the module's objects are listed:\n{digest}");
    }

    // -----------------------------------------------------------------------
    // Index extension bodies
    // -----------------------------------------------------------------------

    /// A cache-tree extension body, in the format `gitformat-index` documents.
    fn tree_ext(entries: &[(&str, i64, i64)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, count, subtrees) in entries {
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            out.extend_from_slice(format!("{count} {subtrees}\n").as_bytes());
            if *count >= 0 {
                out.extend(std::iter::repeat(0x5a).take(20));
            }
        }
        out
    }

    /// Two cache trees of the **same length** that cover different numbers of
    /// entries. This is the gap: the chain reported `TREE:<len>` and nothing
    /// else, so every wrong-but-same-shape cache tree matched a correct one.
    #[test]
    fn index_extension_detail_separates_two_cache_trees_of_one_length() {
        let a = scratch("ext-tree-a");
        let b = scratch("ext-tree-b");
        let one = tree_ext(&[("", 2, 0)]);
        let two = tree_ext(&[("", 9, 0)]);
        assert_eq!(one.len(), two.len(), "the test is only about equal-length bodies");
        for (repo, body) in [(&a, &one), (&b, &two)] {
            let mut index = index_bytes(2, &["a.txt"], &[("TREE", body.len())]);
            // `index_bytes` fills the body with 0xab; overwrite it in place.
            let at = index.len() - 20 - body.len();
            index[at..at + body.len()].copy_from_slice(body);
            std::fs::write(repo.join(".git/index"), &index).unwrap();
        }
        assert_ne!(
            index_meta(&a.join(".git/index")),
            index_meta(&b.join(".git/index")),
            "two cache trees of one length must not compare equal: {}",
            index_meta(&a.join(".git/index"))
        );
        assert!(index_meta(&a.join(".git/index")).contains("TREE:25(<root>=2/0:"));

        // An invalidated subtree carries no object id and says so.
        let c = scratch("ext-tree-invalid");
        let body = tree_ext(&[("", -1, 0)]);
        let mut index = index_bytes(2, &["a.txt"], &[("TREE", body.len())]);
        let at = index.len() - 20 - body.len();
        index[at..at + body.len()].copy_from_slice(&body);
        std::fs::write(c.join(".git/index"), &index).unwrap();
        assert!(
            index_meta(&c.join(".git/index")).contains("TREE:6(<root>=-1/0)"),
            "got: {}",
            index_meta(&c.join(".git/index"))
        );
    }

    /// The safety property, stated as a test: the detail is appended after the
    /// length and always opens with `(`, a character no length contains — so two
    /// chains that differed before still differ.
    #[test]
    fn index_extension_detail_cannot_collapse_a_difference() {
        // `TREE:6` and `TREE:60` differed before. Their details cannot make them
        // equal, whatever the bodies are.
        let short = scratch("ext-collapse-short");
        let long = scratch("ext-collapse-long");
        std::fs::write(short.join(".git/index"), index_bytes(2, &["a.txt"], &[("TREE", 6)]))
            .unwrap();
        std::fs::write(long.join(".git/index"), index_bytes(2, &["a.txt"], &[("TREE", 60)]))
            .unwrap();
        let (a, b) = (index_meta(&short.join(".git/index")), index_meta(&long.join(".git/index")));
        assert_ne!(a, b);
        assert!(a.contains("TREE:6("), "the detail opens with a paren: {a}");
        assert!(b.contains("TREE:60("), "the detail opens with a paren: {b}");
        // An extension whose body is not decided here keeps exactly what it had.
        // `EOIE` is the one that stays undecided on purpose: its two fields are
        // a byte offset and a hash over the very `SIG:len` pairs this chain
        // already prints, so there is nothing behind it to find.
        let eoie = scratch("ext-collapse-eoie");
        std::fs::write(eoie.join(".git/index"), index_bytes(2, &["a.txt"], &[("EOIE", 396)]))
            .unwrap();
        assert_eq!(index_meta(&eoie.join(".git/index")), "v2 entries=1 ext=[EOIE:396]");
    }

    /// Cross-checked against an index **stock git wrote**, so the parse is
    /// pinned to the real format: the cache tree's root entry must name the
    /// commit's own tree and count every entry in the index.
    #[test]
    fn index_extension_detail_reads_a_real_cache_tree() {
        let stock = crate::stock::git().expect("this crate needs a stock git to measure anything");
        let repo = scratch("ext-real-tree");
        let home = repo.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let git = |args: &[&str]| -> String {
            let mut cmd = Command::new(stock);
            crate::env::harden(&mut cmd, &home);
            let out = cmd.current_dir(&repo).args(args).output().expect("stock git runs");
            assert!(
                out.status.success(),
                "stock git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        // `scratch` leaves an empty `.git`; `init` adopts it.
        git(&["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("a.txt"), b"a\n").unwrap();
        std::fs::write(repo.join("src/b.txt"), b"b\n").unwrap();
        git(&["add", "a.txt", "src/b.txt"]);
        git(&["commit", "-qm", "two files"]);
        // `commit` leaves the cache tree fully valid.
        let root_tree = git(&["rev-parse", "HEAD^{tree}"]);
        let src_tree = git(&["rev-parse", "HEAD:src"]);

        let meta = index_meta(&repo.join(".git/index"));
        assert!(
            meta.contains(&format!("<root>=2/1:{root_tree}")),
            "the root cache-tree entry covers both index entries and names the commit's tree:\n{meta}"
        );
        assert!(
            meta.contains(&format!("src=1/0:{src_tree}")),
            "the subtree is named and counted:\n{meta}"
        );

        // Invalidate it the way an unstaged write does, and the digest moves.
        std::fs::write(repo.join("src/b.txt"), b"changed\n").unwrap();
        git(&["add", "src/b.txt"]);
        let after = index_meta(&repo.join(".git/index"));
        assert_ne!(meta, after, "a cache tree that now names a different tree must not match");
    }

    /// A symlink in a metadata directory is reported as the link it is, and is
    /// followed only where it lands inside the fixture.
    ///
    /// `walk_files` takes `symlink_metadata` and so never *descends* through a
    /// link, but it yields a link-to-a-file as an ordinary entry — and every
    /// reader that then called `std::fs::read` on it followed the link. With
    /// `Shape::Symlinks` pointing outside the fixture on purpose, that is a
    /// probe reading a file the case never wrote.
    #[test]
    fn a_symlink_in_a_metadata_directory_is_named_and_not_followed_out_of_the_fixture() {
        use std::os::unix::fs::symlink;
        let outside = scratch("symlink-outside-target");
        std::fs::write(outside.join("secret"), b"bytes the case never wrote\n").unwrap();

        let repo = scratch("symlink-op-state");
        let seq = repo.join(".git/sequencer");
        std::fs::create_dir_all(&seq).unwrap();
        std::fs::write(seq.join("todo"), b"pick abc123 one\n").unwrap();
        symlink(outside.join("secret"), seq.join("out")).unwrap();
        symlink(seq.join("todo"), seq.join("in")).unwrap();

        let digest = probe_op_state(&repo);
        // Out of the fixture: named, never opened.
        assert!(
            digest
                .lines()
                .any(|l| l.starts_with("sequencer/out: <symlink -> ")
                    && l.ends_with("<outside fixture: not read>")),
            "got:\n{digest}"
        );
        assert!(
            !digest.contains("bytes the case never wrote"),
            "a probe must not splice a file from outside the fixture:\n{digest}"
        );
        // Inside the fixture: named *and* read, so nothing that was compared
        // before stops being compared.
        assert!(
            digest
                .lines()
                .any(|l| l.starts_with("sequencer/in: <symlink -> ")
                    && l.ends_with("pick abc123 one\\n")),
            "got:\n{digest}"
        );

        // The target string is a compared fact in its own right: two links to
        // two different outside files are two different digests, where before
        // they were whatever those files happened to hold.
        let other = scratch("symlink-op-state-b");
        let seq_b = other.join(".git/sequencer");
        std::fs::create_dir_all(&seq_b).unwrap();
        std::fs::write(seq_b.join("todo"), b"pick abc123 one\n").unwrap();
        symlink(outside.join("elsewhere"), seq_b.join("out")).unwrap();
        symlink(seq_b.join("todo"), seq_b.join("in")).unwrap();
        assert_ne!(digest, probe_op_state(&other));
    }

    /// **No case id may appear twice in the corpus.**
    ///
    /// # Why this is a test and not a `dedup` call
    ///
    /// A case id is the case's whole identity: [`Case::id`] renders the shape,
    /// the command, the argv, the config, the working directory, the
    /// environment and the stdin payload, so two jobs with one id are the same
    /// question asked twice. Every one of them costs two fixture
    /// instantiations, two invocations, two state digests and up to two
    /// interop probes — and then lands twice in the denominator the report
    /// prints as the parity number. A denominator that counts one case twice
    /// weights it double: it is not "840 questions answered", it is 840
    /// answers to fewer than 840 questions, and no reader of that percentage
    /// can tell which.
    ///
    /// Deduplicating in [`Job`] construction would make the number defensible
    /// and the corpus wrong, silently and permanently — the two modules that
    /// each think they own `linear::prune-packed::prune-packed` would go on
    /// thinking so, and the next duplicate would be invisible too. A duplicate
    /// is a corpus bug. It gets a failing test, and the failure names the
    /// module owners' problem for them.
    ///
    /// # Built in memory, not from `--list-cases`
    ///
    /// The listing cannot be split on newlines: a case may draw
    /// `core.commentChar=<LF>` and put a literal newline inside its own id, so
    /// `--list-cases | sort | uniq -d` under-counts by construction. The ids
    /// here come from the same two functions `main::list_cases` calls, in the
    /// same order, and never round-trip through a rendering. Fuzz cases are
    /// excluded on purpose: they are generated from a seed rather than
    /// authored, so a collision there is a generator question and not a corpus
    /// one.
    ///
    /// The failure message escapes each id through [`escape_bytes`] for the
    /// same reason — one duplicate, one line, whatever bytes it carries.
    #[test]
    fn no_case_id_appears_twice_in_the_corpus() {
        let mut ids: Vec<String> = crate::corpus::cases().iter().map(Case::id).collect();
        for sequence in crate::corpus::sequences() {
            for i in 0..sequence.len() {
                ids.push(sequence.step_id(i));
            }
        }
        let total = ids.len();

        let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for id in &ids {
            *seen.entry(id.as_str()).or_insert(0) += 1;
        }
        let mut dupes: Vec<(&str, usize)> =
            seen.into_iter().filter(|(_, n)| *n > 1).collect();
        dupes.sort();

        let extra: usize = dupes.iter().map(|(_, n)| n - 1).sum();
        assert!(
            dupes.is_empty(),
            "{} case ids appear more than once, so {extra} of the {total} jobs in the \
             denominator are a second copy of a question already asked. Each is a corpus \
             bug in the module that emits it — fix the module, do not deduplicate here:\n{}",
            dupes.len(),
            dupes
                .iter()
                .map(|(id, n)| format!("  {n}x {}", escape_bytes(id.as_bytes())))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    /// The untracked cache, cross-checked against an index **stock git wrote**.
    ///
    /// This one is worth the child processes: the extension's leading records
    /// are `stat_data`, the parse steps over them by a hard-coded width, and a
    /// width that is wrong by four bytes walks the rest of the parse into the
    /// inode numbers it exists to avoid. The assertions are therefore about the
    /// fields *behind* the skip — the two exclude-file hashes, the per-directory
    /// exclude name and the untracked file names — because those only come out
    /// right if the skip was right.
    #[test]
    fn untracked_cache_detail_matches_an_index_stock_wrote() {
        let stock = crate::stock::git().expect("this crate needs a stock git to measure anything");
        let repo = scratch("ext-real-untr");
        // Outside the repository, unlike the other probe tests here: this one
        // asserts on the untracked *set*, and a `home` inside the worktree is a
        // third untracked directory in it.
        let home = scratch("ext-real-untr-home");
        let git = |args: &[&str]| {
            let mut cmd = Command::new(stock);
            crate::env::harden(&mut cmd, &home);
            let out = cmd.current_dir(&repo).args(args).output().expect("stock git runs");
            assert!(
                out.status.success(),
                "stock git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("tracked.txt"), b"t\n").unwrap();
        git(&["add", "tracked.txt"]);
        git(&["commit", "-qm", "one"]);
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        std::fs::write(repo.join("untracked.txt"), b"u\n").unwrap();
        std::fs::write(repo.join("sub/other.txt"), b"v\n").unwrap();
        git(&["config", "core.untrackedCache", "true"]);
        git(&["update-index", "--untracked-cache"]);
        // Twice: the first walk populates the cache, the second writes it out
        // with the directory blocks filled in.
        git(&["status", "--porcelain"]);
        git(&["status", "--porcelain"]);

        let meta = index_meta(&repo.join(".git/index"));
        assert!(meta.contains("UNTR:"), "stock wrote no untracked cache to read:\n{meta}");
        // The two directory blocks, depth-first: the root holds one untracked
        // file and one sub-directory, and the sub-directory holds one file.
        //
        // Their ORDER inside the block is the order the filesystem handed git's
        // directory walk, and nothing git decides — measured with stock git on
        // APFS, three files created `zzz`, `aaa`, `mmm` come back
        // `[mmm.txt aaa.txt zzz.txt]`, which is neither creation order nor
        // sorted order. ext4 walks `sub/` before `untracked.txt` and APFS after,
        // so naming one of the two spellings pinned the author's filesystem and
        // failed on every Linux runner. Both names and both counts are still
        // required: they only come out at all if the two `stat_data` records
        // were stepped over by the right width, which is what this test is for.
        assert!(
            meta.contains("<root>=2/1[untracked.txt sub/]")
                || meta.contains("<root>=2/1[sub/ untracked.txt]"),
            "the root block names what is untracked in it:\n{meta}"
        );
        assert!(
            meta.contains("sub=1/0[other.txt]"),
            "the sub-directory block is walked too:\n{meta}"
        );
        // `core.excludesFile` does not exist under `env::harden`, and the format
        // says a null hash is how that is spelt. Landing exactly on it is the
        // proof the two stat records were stepped over by the right width.
        assert!(
            meta.contains(&format!("exf={} perdir=.gitignore", "0".repeat(40))),
            "the null excludes-file hash and the per-dir name land where they should:\n{meta}"
        );
        assert!(
            !meta.contains(&format!("ex={} ", "0".repeat(40))),
            "the info/exclude hash is a real hash, not a mis-parse:\n{meta}"
        );
        assert!(meta.contains("flags=000000"), "dir_flags is a small flag word:\n{meta}");

        // And it moves when the untracked set does — the whole point of reading
        // the body rather than its length.
        std::fs::write(repo.join("second.txt"), b"w\n").unwrap();
        git(&["status", "--porcelain"]);
        git(&["status", "--porcelain"]);
        let after = index_meta(&repo.join(".git/index"));
        assert_ne!(meta, after, "a new untracked file must move the digest:\n{meta}\n{after}");
        assert!(after.contains("second.txt"), "and it is named:\n{after}");
    }

    /// The three parsers added beside the cache tree, on bodies built here: each
    /// must separate two bodies of one length, which is the whole reason a
    /// detail exists at all.
    #[test]
    fn the_new_extension_parsers_separate_equal_length_bodies() {
        // `FSMN` and `IEOT`: same length, different version word.
        assert_eq!(ext_detail(b"FSMN", &[0, 0, 0, 1, 9, 9]), "(v1)");
        assert_ne!(ext_detail(b"FSMN", &[0, 0, 0, 1, 9, 9]), ext_detail(b"FSMN", &[0, 0, 0, 2, 9, 9]));
        assert_eq!(ext_detail(b"IEOT", &[0, 0, 0, 1, 7, 7, 7, 7]), "(v1)");
        // The clock behind an `FSMN` version is never read: two tokens of one
        // length must not be a difference, because they are not one.
        assert_eq!(
            ext_detail(b"FSMN", &[0, 0, 0, 1, 1, 2, 3, 4]),
            ext_detail(b"FSMN", &[0, 0, 0, 1, 9, 9, 9, 9])
        );

        // `link`: a null base and a real one are the same 20 bytes.
        let null = [0u8; OID_LEN];
        let mut set = [0u8; OID_LEN];
        set[0] = 1;
        assert_eq!(ext_detail(b"link", &null), "(base=null bitmaps=no)");
        assert_eq!(ext_detail(b"link", &set), "(base=set bitmaps=no)");
        // A bitmap pair present is a fact; the checksum's own bytes are not read,
        // so two different real checksums stay equal. A four-byte tail is not a
        // bitmap, and saying so is the same not-a-guess the rest of the chain
        // says — it is *more* than `bitmaps=yes` alone reported, never less.
        let mut with_bitmaps = set.to_vec();
        with_bitmaps.extend_from_slice(&[0, 0, 0, 1]);
        assert_eq!(ext_detail(b"link", &with_bitmaps), "(base=set bitmaps=yes del=<unparsed>)");
        let mut other = set;
        other[19] = 7;
        assert_eq!(ext_detail(b"link", &set), ext_detail(b"link", &other));

        // A body too short to hold what the format promises says so rather than
        // guessing, and says it the same way on both sides.
        assert_eq!(ext_detail(b"FSMN", &[0, 0]), "(<unparsed>)");
        assert_eq!(ext_detail(b"link", &[0, 0]), "(<unparsed>)");
        assert_eq!(ext_detail(b"UNTR", &[]), "(<unparsed>)");
    }

    /// git's varint is not LEB128, and a decoder that treats it as one is off by
    /// one at every continuation byte — which in the untracked cache means
    /// walking into stat data.
    #[test]
    fn the_varint_is_gits_own_and_not_leb128() {
        assert_eq!(decode_varint(&[0x00], 0), Some((0, 1)));
        assert_eq!(decode_varint(&[0x7f], 0), Some((127, 1)));
        // 128 encodes as two bytes, and the accumulator is incremented before
        // the shift: (0 + 1) << 7 | 0 = 128, where LEB128 would say 0.
        assert_eq!(decode_varint(&[0x80, 0x00], 0), Some((128, 2)));
        assert_eq!(decode_varint(&[0x80, 0x01], 0), Some((129, 2)));
        // The exact pair the real fixture's ident length came out of.
        assert_eq!(decode_varint(&[0x80, 0x20], 0), Some((160, 2)));
        // A number the buffer ends inside is not a number.
        assert_eq!(decode_varint(&[0x80], 0), None);
        assert_eq!(decode_varint(&[], 0), None);
    }

    /// The resolve-undo body, cross-checked against one stock git wrote: three
    /// stages of one path, each with its own mode and blob.
    #[test]
    fn index_extension_detail_reads_a_real_resolve_undo() {
        let stock = crate::stock::git().expect("this crate needs a stock git to measure anything");
        let repo = scratch("ext-real-reuc");
        let home = repo.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let git = |args: &[&str]| -> std::process::Output {
            let mut cmd = Command::new(stock);
            crate::env::harden(&mut cmd, &home);
            cmd.current_dir(&repo).args(args).output().expect("stock git runs")
        };
        let ok = |args: &[&str]| {
            let out = git(args);
            assert!(
                out.status.success(),
                "stock git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        ok(&["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("c.txt"), b"base\n").unwrap();
        ok(&["add", "c.txt"]);
        ok(&["commit", "-qm", "base"]);
        ok(&["checkout", "-q", "-b", "other"]);
        std::fs::write(repo.join("c.txt"), b"theirs\n").unwrap();
        ok(&["commit", "-qam", "theirs"]);
        ok(&["checkout", "-q", "main"]);
        std::fs::write(repo.join("c.txt"), b"ours\n").unwrap();
        ok(&["commit", "-qam", "ours"]);
        // Conflict, then resolve: `add` on a conflicted path is what writes REUC.
        let merge = git(&["merge", "other"]);
        assert!(!merge.status.success(), "the merge is supposed to conflict");
        std::fs::write(repo.join("c.txt"), b"resolved\n").unwrap();
        ok(&["add", "c.txt"]);

        let meta = index_meta(&repo.join(".git/index"));
        assert!(meta.contains("REUC:"), "stock wrote no resolve-undo:\n{meta}");
        assert!(
            meta.contains("(c.txt=100644:"),
            "the recorded path and its stage-1 mode are read:\n{meta}"
        );
        // Three stages, each with a blob id: two `|` separators inside the entry.
        let reuc = meta.split("REUC:").nth(1).unwrap();
        assert_eq!(reuc.matches('|').count(), 2, "three stages are recorded:\n{meta}");
    }

    // -----------------------------------------------------------------------
    // Peers under any name
    // -----------------------------------------------------------------------

    /// The gate was the literal name `.remote.git`, so a case that made a peer
    /// anywhere else was unprobed. Both shapes a peer takes are found: a bare
    /// repository, and a working clone whose repository is its `.git`.
    #[test]
    fn a_peer_under_any_name_is_probed() {
        let make = |repo: &Path, at: &str, head: &str| {
            let dir = repo.join(at);
            std::fs::create_dir_all(dir.join("objects")).unwrap();
            std::fs::create_dir_all(dir.join("refs/heads")).unwrap();
            std::fs::write(dir.join("HEAD"), format!("ref: {head}\n")).unwrap();
        };

        let a = scratch("peer-named-a");
        let b = scratch("peer-named-b");
        make(&a, "other.git", "refs/heads/main");
        make(&b, "other.git", "refs/heads/elsewhere");
        let digest = probe_peer(&a, &a);
        assert!(digest.contains("# peer other.git\n"), "got:\n{digest}");
        assert!(digest.contains("HEAD: ref: refs/heads/main\\n"), "got:\n{digest}");
        assert_ne!(digest, probe_peer(&b, &b), "the second peer's HEAD is a compared fact");

        // The named peer keeps its section, first and unchanged, so nothing that
        // was in the digest before moves.
        assert!(digest.starts_with("# peer .remote.git\n<absent>\n"), "got:\n{digest}");

        // A working clone: the repository is the child's `.git`.
        let c = scratch("peer-clone");
        make(&c, "copy/.git", "refs/heads/main");
        assert!(probe_peer(&c, &c).contains("# peer copy/.git\n"), "got:\n{}", probe_peer(&c, &c));
    }

    /// A peer the case put **two levels down** is still a peer. The gate used to
    /// be one `read_dir` of the fixture root, so `clone . a/b` and `init --bare
    /// nested/other.git` were measured on their reports alone.
    #[test]
    fn a_peer_below_the_fixture_root_is_probed_and_a_repository_is_not_descended_into() {
        let make = |at: &Path, head: &str| {
            std::fs::create_dir_all(at.join("objects/ab")).unwrap();
            std::fs::create_dir_all(at.join("refs/heads")).unwrap();
            std::fs::write(at.join("HEAD"), format!("ref: {head}\n")).unwrap();
        };

        let a = scratch("peer-deep-a");
        let b = scratch("peer-deep-b");
        for (repo, head) in [(&a, "refs/heads/main"), (&b, "refs/heads/elsewhere")] {
            make(&repo.join("nested/other.git"), head);
            // A working clone two levels down, whose repository is its `.git`.
            make(&repo.join("one/two/copy/.git"), head);
        }
        let digest = probe_peer(&a, &a);
        assert!(digest.contains("# peer nested/other.git\n"), "got:\n{digest}");
        assert!(digest.contains("# peer one/two/copy/.git\n"), "got:\n{digest}");
        assert_ne!(digest, probe_peer(&b, &b), "a deep peer's HEAD is a compared fact");

        // The named peer still opens the digest, so nothing that was there
        // before has moved.
        assert!(digest.starts_with("# peer .remote.git\n<absent>\n"), "got:\n{digest}");

        // And the walk stops at a repository: an `objects/ab` inside a peer is
        // not itself offered up as a peer, and neither is anything under it.
        assert!(
            !digest.contains("nested/other.git/objects"),
            "the walk must not descend into a repository it has found:\n{digest}"
        );
    }

    /// The fixture's own repository is excluded by identity, not by name, so a
    /// bare fixture cannot probe itself and a normal one cannot probe its `.git`.
    #[test]
    fn a_fixture_never_probes_itself_as_its_own_peer() {
        let repo = scratch("peer-self");
        // Make the fixture's own `.git` repository-shaped.
        std::fs::create_dir_all(repo.join(".git/objects")).unwrap();
        std::fs::create_dir_all(repo.join(".git/refs")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
        assert_eq!(probe_peer(&repo, &repo), "# peer .remote.git\n<absent>\n");

        // A bare fixture: `git_dir(repo) == repo`, and none of `objects/`,
        // `refs/` or `logs/` is itself repository-shaped.
        let bare = scratch("peer-bare");
        std::fs::remove_dir_all(bare.join(".git")).unwrap();
        std::fs::create_dir_all(bare.join("objects")).unwrap();
        std::fs::create_dir_all(bare.join("refs")).unwrap();
        std::fs::write(bare.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
        assert_eq!(probe_peer(&bare, &bare), "# peer .remote.git\n<absent>\n");
    }

    /// A repository **outside the fixture root** is not a peer and must never
    /// become one.
    ///
    /// This is not a hypothetical boundary. [`run_case`] lays the two sides out
    /// as `<workdir>/stock` and `<workdir>/zvcs`, siblings under one parent,
    /// with both repeat copies and both interop scratch directories beside
    /// them. A walk that went up one level would splice the port's repository
    /// into stock's digest and stock's into the port's — a comparison of each
    /// side against the other side's work, which is not a stricter measurement
    /// but a meaningless one. The test builds that exact layout and asserts the
    /// walk stays inside its own root.
    #[test]
    fn a_repository_beside_the_fixture_is_never_probed_as_its_peer() {
        let make = |at: &Path, head: &str| {
            std::fs::create_dir_all(at.join("objects")).unwrap();
            std::fs::create_dir_all(at.join("refs")).unwrap();
            std::fs::write(at.join("HEAD"), format!("ref: {head}\n")).unwrap();
        };
        let workdir = std::env::temp_dir()
            .join(format!("zvcs-parity-sibling-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&workdir);
        // The layout `run_case` builds, plus the interop scratch beside it.
        let stock = workdir.join("stock");
        make(&stock.join(".git"), "refs/heads/main");
        make(&workdir.join("zvcs/.git"), "refs/heads/theirs");
        make(&workdir.join("repeat-stock/.git"), "refs/heads/main");
        make(&workdir.join("interop-stock"), "refs/heads/main");

        assert_eq!(
            other_peers(&stock),
            Vec::new(),
            "nothing outside the fixture root may be offered up as a peer"
        );
        let digest = probe_peer(&stock, &stock);
        assert_eq!(digest, "# peer .remote.git\n<absent>\n", "got:\n{digest}");
        for name in ["zvcs", "repeat-stock", "interop-stock"] {
            assert!(
                !digest.contains(name),
                "the other side's repository must not appear in this side's digest:\n{digest}"
            );
            assert!(
                !probe_pack_contents(&stock).contains(name),
                "nor in the pack census, which walks the same peer list"
            );
        }
        let _ = std::fs::remove_dir_all(&workdir);
    }

    /// The peer gets git's own validator, and the census cannot replace it.
    ///
    /// One loose blob is overwritten with **another real loose object's bytes,
    /// under its original name** — the rewrite the object census is blind to by
    /// construction, because `cat-file --batch-check --batch-all-objects` takes
    /// the id from the path. Both facts are asserted on the same peer: the
    /// census still reports the object it always did, and `fsck` reports the
    /// mismatch. Before this section existed the peer's whole store had no
    /// validator on any code path — [`probe_interop`] runs one in the fixture,
    /// and its gate ([`git_fingerprint`]) walks the fixture's git directory,
    /// which a push into the peer never touches.
    #[test]
    fn a_peer_is_validated_and_not_only_counted() {
        let stock = crate::stock::git().expect("this crate needs a stock git to measure anything");
        let repo = scratch("peer-fsck");
        let peer = repo.join(".remote.git");
        let git = |args: &[&str]| -> std::process::Output {
            let mut cmd = Command::new(stock);
            crate::env::harden(&mut cmd, &repo);
            cmd.current_dir(&repo).args(args).output().expect("stock git runs")
        };
        assert!(git(&["init", "-q", "--bare", ".remote.git"]).status.success());
        let blob = |body: &str| -> String {
            let mut cmd = Command::new(stock);
            crate::env::harden(&mut cmd, &repo);
            let out = cmd
                .current_dir(&peer)
                .args(["hash-object", "-w", "-t", "blob", "--stdin"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .and_then(|mut c| {
                    use std::io::Write;
                    c.stdin.take().unwrap().write_all(body.as_bytes())?;
                    c.wait_with_output()
                })
                .expect("stock git runs");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let loose = |oid: &str| peer.join("objects").join(&oid[..2]).join(&oid[2..]);
        let one = blob("one\n");
        let two = blob("two\n");

        let clean = peer_section(&repo, ".remote.git", &peer, &repo);
        assert!(clean.contains("fsck --strict\nexit: Some(0)\n"), "got:\n{clean}");

        // The rewrite: `one`'s file now holds `two`'s bytes, under `one`'s name.
        let bytes = std::fs::read(loose(&two)).unwrap();
        let _ = std::fs::remove_file(loose(&one));
        std::fs::write(loose(&one), &bytes).unwrap();

        let dirty = peer_section(&repo, ".remote.git", &peer, &repo);
        assert!(
            dirty.contains(&format!("{one} blob 4")),
            "the census still believes the filename, which is why fsck is here:\n{dirty}"
        );
        assert!(
            dirty.contains("hash-path mismatch"),
            "the validator reads the bytes rather than the name:\n{dirty}"
        );
        assert!(
            !dirty.contains("fsck --strict\nexit: Some(0)\n"),
            "and it says so in its exit code too:\n{dirty}"
        );
        assert_ne!(clean, dirty, "the two peers must not compare equal");
    }

    /// Which objects went into which pack, cross-checked against the plumbing
    /// this probe deliberately does **not** spawn.
    ///
    /// `verify-pack -v` is the documented way to ask, and four of its six
    /// columns are compression choices this crate has already ruled
    /// uncomparable. The claim being pinned is that the first column — the one
    /// new fact — is in the `.idx` verbatim, so the listing can be read with no
    /// child process at all and still name the same objects git names.
    #[test]
    fn the_pack_listing_names_what_verify_pack_names() {
        let stock = crate::stock::git().expect("this crate needs a stock git to measure anything");
        let repo = scratch("pack-contents");
        let git = |args: &[&str]| -> std::process::Output {
            let mut cmd = Command::new(stock);
            crate::env::harden(&mut cmd, &repo);
            cmd.current_dir(&repo).args(args).output().expect("stock git runs")
        };
        let ok = |args: &[&str]| {
            let out = git(args);
            assert!(
                out.status.success(),
                "stock git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        ok(&["init", "-q", "-b", "main"]);
        // A store with no pack at all reports its label and nothing under it.
        let empty = probe_pack_contents(&repo);
        assert!(empty.starts_with("# pack-contents\n## self\n"), "got:\n{empty}");

        for n in ["1", "2", "3"] {
            std::fs::write(repo.join(format!("f{n}.txt")), format!("{n}\n")).unwrap();
            ok(&["add", "-A"]);
            ok(&["commit", "-qm", n]);
        }
        ok(&["repack", "-a", "-d", "-q"]);

        let digest = probe_pack_contents(&repo);
        assert_ne!(digest, empty, "packing the store must move the section");

        // The oracle: every id in the first column of `verify-pack -v`.
        let idx = walk_files(&repo.join(".git/objects/pack"))
            .into_iter()
            .find(|(rel, _)| rel.ends_with(".idx"))
            .expect("repack wrote a pack index");
        let out = git(&["verify-pack", "-v", &idx.1.to_string_lossy()]);
        assert!(out.status.success(), "verify-pack must succeed on a pack git just wrote");
        let mut named: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split_whitespace().next().map(str::to_string))
            .filter(|t| t.len() == OID_LEN * 2 && t.chars().all(|c| c.is_ascii_hexdigit()))
            .collect();
        named.sort();
        assert!(named.len() >= 8, "the fixture should hold commits, trees and blobs");

        let line = digest.lines().find(|l| l.starts_with("pack-")).expect("a pack line");
        let listed: Vec<String> =
            line.split_whitespace().skip(2).map(str::to_string).collect();
        assert_eq!(listed, named, "the .idx names exactly what verify-pack names");
        assert!(
            line.contains(&format!(": {} ", named.len())),
            "the count is printed beside the ids:\n{line}"
        );
        assert!(
            line.starts_with("pack-<hash>.idx:"),
            "a pack's checksum name is elided like every other one:\n{line}"
        );
    }

    /// The split-index bitmaps, against the `link` extension **stock 2.55.0
    /// wrote** for three files and one `update-index --split-index`.
    ///
    /// The serialisation is in no man page on this machine, so it was read off
    /// this body; the body is reproduced here so the parser is pinned to
    /// something git produced rather than to its own author's reading. See
    /// [`ewah_detail`] for the derivation of the run-word field split, which
    /// this body decides on its own: two words in the file, one of them the run
    /// word, so the run word declares exactly one literal.
    #[test]
    fn a_split_index_link_is_read_down_to_its_two_bit_sets() {
        let mut body: Vec<u8> = Vec::new();
        // The shared index's id, `sharedindex.6981…0dee`.
        body.extend_from_slice(&[
            0x69, 0x81, 0x65, 0x95, 0x25, 0xce, 0x97, 0xcb, 0xe2, 0xdd, 0x1a, 0x5a, 0x48, 0x60,
            0x7d, 0x02, 0x71, 0xf7, 0x0d, 0xee,
        ]);
        // delete: 0 bits, one all-clear word.
        body.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
        body.extend_from_slice(&[0; 8]);
        body.extend_from_slice(&[0, 0, 0, 0]);
        // replace: 3 bits, a run word declaring one literal, and that literal.
        body.extend_from_slice(&[0, 0, 0, 3, 0, 0, 0, 2]);
        body.extend_from_slice(&[0, 0, 0, 2, 0, 0, 0, 0]);
        body.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 7]);
        body.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(body.len(), 68, "the length the extension header declared");
        assert_eq!(
            ext_detail(b"link", &body),
            "(base=set bitmaps=yes del=0:[] rep=3:[0 1 2])"
        );

        // Two different bit sets of one length must not compare equal — the
        // whole reason for reading the body at all.
        let mut other = body.clone();
        let last = other.len() - 5;
        other[last] = 5;
        assert_eq!(ext_detail(b"link", &other), "(base=set bitmaps=yes del=0:[] rep=3:[0 2])");

        // A run of set bits is decoded as a run, and is bounded by the declared
        // bit count rather than by the run length.
        let mut run: Vec<u8> = vec![0; OID_LEN];
        run[0] = 1;
        run.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
        run.extend_from_slice(&[0; 8]);
        run.extend_from_slice(&[0, 0, 0, 0]);
        run.extend_from_slice(&[0, 0, 0, 70, 0, 0, 0, 1]);
        // Running bit 1, running length 2, no literals: 128 clean set bits, of
        // which only the declared 70 are reported.
        run.extend_from_slice(&(((2u64 << 1) | 1).to_be_bytes()));
        run.extend_from_slice(&[0, 0, 0, 0]);
        let rendered = ext_detail(b"link", &run);
        assert!(rendered.contains("rep=70:[0 1 2"), "got:\n{rendered}");
        assert!(rendered.ends_with("68 69])"), "the run stops at the declared width:\n{rendered}");
    }

    /// The widened mirror costs a process only where there is a structure to
    /// misread, and it names which structures those were.
    ///
    /// Stock git stands in for the binary under test here: the probe's contract
    /// is "ask this binary to enumerate the repository", and which binary is
    /// asked is the caller's business. What is pinned is the gate — a repository
    /// with no accelerator spawns nothing and still emits its line, so the two
    /// sides line up positionally — and the sort, which is what makes an object
    /// *set* comparable when `--objects` emits in walk order.
    #[test]
    fn the_accelerator_read_is_asked_only_where_there_is_one() {
        let stock = crate::stock::git().expect("this crate needs a stock git to measure anything");
        let repo = scratch("interop-accel");
        let git = |args: &[&str]| -> std::process::Output {
            let mut cmd = Command::new(stock);
            crate::env::harden(&mut cmd, &repo);
            cmd.current_dir(&repo).args(args).output().expect("stock git runs")
        };
        let ok = |args: &[&str]| {
            let out = git(args);
            assert!(
                out.status.success(),
                "stock git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        ok(&["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), b"a\n").unwrap();
        ok(&["add", "-A"]);
        ok(&["commit", "-qm", "one"]);

        // A repository git has only committed into holds none of the five.
        assert_eq!(accelerators(&repo), Vec::<&str>::new());
        let quiet = accelerator_read_probe(stock, &repo, &repo);
        assert_eq!(quiet, "## accelerator-read\npresent: \n<none present: not asked>\n");

        // Each structure is named as stock writes it, and each opens the gate.
        ok(&["pack-refs", "--all"]);
        assert_eq!(accelerators(&repo), vec!["packed-refs"]);
        ok(&["commit-graph", "write", "--reachable"]);
        ok(&["repack", "-a", "-d", "-q", "--write-bitmap-index"]);
        ok(&["multi-pack-index", "write"]);
        assert_eq!(
            accelerators(&repo),
            vec!["commit-graph", "multi-pack-index", "bitmap", "packed-refs"]
        );

        let loud = accelerator_read_probe(stock, &repo, &repo);
        assert!(
            loud.starts_with(
                "## accelerator-read\npresent: commit-graph multi-pack-index bitmap packed-refs\n\
                 zvcs exit: Some(0)\n"
            ),
            "got:\n{loud}"
        );
        // The commit, its tree and its blob, and the object set is sorted so it
        // is a set rather than a walk order.
        let ids: Vec<&str> = loud
            .lines()
            .skip(3)
            .filter_map(|l| l.split_whitespace().next())
            .collect();
        assert_eq!(ids.len(), 3, "one commit, one tree, one blob:\n{loud}");
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "the object set is rendered sorted:\n{loud}");
        assert!(loud.contains(" a.txt\n"), "`--objects` names the path too:\n{loud}");
    }
}
