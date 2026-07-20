//! Combinatorial flag fuzzing with deterministic seeding and shrinking.
//!
//! The corpus covers what a human thought to check. This covers what nobody
//! thought to check: flag combinations, argument orderings, and rev-spec forms
//! that a real caller will eventually produce.
//!
//! Determinism is a hard requirement — a parity failure nobody can reproduce is
//! not actionable. Every case is a pure function of `(seed, index)`, so a failing
//! run replays exactly from the seed printed in its report.

use crate::fixture::Shape;
use crate::runner::Case;

/// xorshift64*. Chosen for being reproducible and dependency-free rather than
/// statistically excellent — case selection does not need cryptographic quality.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // A zero state is absorbing for xorshift; remap it.
        Self(if seed == 0 { 0x9E3779B97F4A7C15 } else { seed })
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }

    fn chance(&mut self, num: u64, denom: u64) -> bool {
        self.next() % denom < num
    }

    /// A count biased toward the low end but with a real tail: most draws are
    /// small, yet `max` still comes up often enough to exercise deep stacking.
    fn count_upto(&mut self, max: usize) -> usize {
        // Two rolls, take the min — triangular, tail toward 0, but the full
        // range is reachable. Deep combinations stay rare without being absent.
        let a = self.below(max + 1);
        let b = self.below(max + 1);
        a.min(b).max(if self.chance(1, 6) { max } else { 0 })
    }
}

/// What a subcommand accepts, as a grammar the generator samples from.
pub struct Grammar {
    pub cmd: &'static str,
    /// Flags safe to combine freely.
    pub flags: &'static [&'static str],
    /// Positional arguments — revs, paths, or refs depending on the command.
    pub positionals: &'static [&'static str],
    /// Shapes this command is meaningful against.
    pub shapes: &'static [Shape],
}

const REV_SHAPES: &[Shape] = &[Shape::Linear, Shape::Branched, Shape::Merged, Shape::Detached];
const ALL_SHAPES: &[Shape] = &[
    Shape::Linear,
    Shape::Branched,
    Shape::Merged,
    Shape::Dirty,
    Shape::Conflicted,
    Shape::Detached,
    Shape::AwkwardPaths,
];

/// Rev-specs worth throwing at anything that resolves one. Includes forms that
/// *should* fail, because agreeing on rejection is also parity, and the hard
/// forms git's own `rev-parse` grammar allows: peels, ranges, reflog walks,
/// `:path` object specs, `:/text` searches, and raw oids.
const REVS: &[&str] = &[
    "HEAD", "HEAD^", "HEAD^^", "HEAD^2", "HEAD~1", "HEAD~2", "HEAD~3",
    "HEAD^0", "HEAD^{}", "HEAD^{tree}", "HEAD^{commit}", "HEAD^{tag}",
    "main", "@", "@~1", "@{-1}", "HEAD@{0}", "HEAD@{1}", "HEAD@{now}",
    "main..HEAD", "main...HEAD", "HEAD~2..HEAD", "^HEAD",
    "HEAD:README.md", ":/fixture", ":0:src/lib.rs", "refs/heads/main",
    "0000000000000000000000000000000000000000", "deadbeef",
    "does-not-exist", "@{999}", "HEAD~999", "",
];

/// Path arguments including magic pathspecs, which have their own parser in git
/// and are a rich source of divergence.
const PATHS: &[&str] = &[
    "README.md", "src/lib.rs", "src", "src/", ".", "./README.md", "..",
    "*.md", "**/*.rs", "no/such/path",
    ":(glob)**/*.rs", ":(icase)readme.md", ":!src", ":(exclude)*.md",
    ":(top)README.md", ":(attr:text)", "with space.txt", "üñïçødé.txt",
];

/// Replacement values for `--flag=value` mutation: empty, boundary, overflow,
/// and garbage. A parser that only ever saw well-formed values in the corpus
/// meets malformed ones here.
const VALUES: &[&str] = &[
    "", "0", "1", "-1", "999999999", "99999999999999999999999999",
    "abc", "true", "false", "v1", "=", "%H%n", "\t", "0x10",
];

/// Read-only grammars only. Fuzzing mutating commands with random flags
/// produces cases whose *stock* behavior is itself ambiguous (interactive
/// prompts, editor spawns), which yields noise rather than findings. Mutating
/// coverage stays curated in the corpus.
pub fn grammars() -> Vec<Grammar> {
    vec![
        Grammar {
            cmd: "rev-parse",
            flags: &[
                "--abbrev-ref", "--short", "--verify", "--quiet", "--git-dir",
                "--show-toplevel", "--is-inside-work-tree", "--is-bare-repository",
                "--symbolic", "--symbolic-full-name", "--all", "--branches", "--tags",
            ],
            positionals: REVS,
            shapes: REV_SHAPES,
        },
        Grammar {
            cmd: "status",
            flags: &[
                "--porcelain", "--porcelain=v1", "--short", "--branch", "--long",
                "--untracked-files=all", "--untracked-files=no", "--untracked-files=normal",
                "--ignored", "--no-renames", "--find-renames",
            ],
            positionals: &[],
            shapes: ALL_SHAPES,
        },
        Grammar {
            cmd: "log",
            flags: &[
                "--oneline", "-1", "-2", "--max-count=3", "--format=%H", "--format=%h %s",
                "--pretty=oneline", "--pretty=short", "--pretty=format:%an", "--name-only",
                "--name-status", "--stat", "--graph", "--all", "--reverse", "--no-merges",
                "--merges", "--date-order", "--topo-order",
            ],
            positionals: &["HEAD", "main", ""],
            shapes: REV_SHAPES,
        },
        Grammar {
            cmd: "rev-list",
            flags: &[
                "--count", "--max-count=2", "--all", "--reverse", "--no-merges",
                "--merges", "--objects", "--parents", "--topo-order",
            ],
            positionals: &["HEAD", "main"],
            shapes: REV_SHAPES,
        },
        Grammar {
            cmd: "cat-file",
            flags: &["-t", "-s", "-p", "-e"],
            positionals: REVS,
            shapes: REV_SHAPES,
        },
        Grammar {
            cmd: "ls-tree",
            flags: &["-r", "-t", "-d", "--name-only", "--name-status", "--full-tree", "--abbrev=7", "-z"],
            positionals: &["HEAD", "HEAD^{tree}", "main"],
            shapes: REV_SHAPES,
        },
        Grammar {
            cmd: "ls-files",
            flags: &[
                "--cached", "--stage", "--modified", "--deleted", "--others",
                "--unmerged", "--full-name", "-z", "--abbrev",
            ],
            positionals: PATHS,
            shapes: ALL_SHAPES,
        },
        Grammar {
            cmd: "diff",
            flags: &[
                "--cached", "--staged", "--stat", "--shortstat", "--numstat",
                "--name-only", "--name-status", "--raw", "--no-color", "--unified=1",
                "--ignore-all-space", "--find-renames",
            ],
            positionals: &["", "HEAD", "HEAD~1"],
            shapes: ALL_SHAPES,
        },
        Grammar {
            cmd: "show",
            flags: &["--oneline", "--no-patch", "--stat", "--name-only", "--format=%H", "--raw"],
            positionals: REVS,
            shapes: REV_SHAPES,
        },
        Grammar {
            cmd: "branch",
            flags: &["--list", "-a", "-r", "-v", "-vv", "--show-current", "--all", "--format=%(refname)"],
            positionals: &[""],
            shapes: ALL_SHAPES,
        },
        Grammar {
            cmd: "tag",
            flags: &["--list", "-l", "-n", "--sort=refname", "--format=%(refname:short)"],
            positionals: &["", "v0.*"],
            shapes: &[Shape::Branched, Shape::Linear],
        },
        Grammar {
            cmd: "describe",
            flags: &["--always", "--tags", "--all", "--long", "--abbrev=7", "--dirty"],
            positionals: &["", "HEAD"],
            shapes: &[Shape::Branched, Shape::Linear, Shape::Dirty],
        },
        Grammar {
            cmd: "config",
            flags: &["--list", "--get", "--get-all", "--local", "--name-only"],
            positionals: &["core.bare", "user.name", "no.such.key"],
            shapes: &[Shape::Linear],
        },
        Grammar {
            cmd: "blame",
            flags: &["--porcelain", "--line-porcelain", "-s", "-l", "--show-name"],
            positionals: &["README.md", "src/lib.rs"],
            shapes: &[Shape::Linear, Shape::Branched],
        },
    ]
}

/// Every grammar the fuzzer draws from: the hand-written ones above, plus the
/// per-command grammars generated from git's own documentation.
fn all_grammars() -> Vec<Grammar> {
    let mut all = grammars();
    all.extend(crate::grammars_generated::generated());
    all
}

/// Generate `per_cmd` cases for each grammar from `seed`.
pub fn generate(seed: u64, per_cmd: usize) -> Vec<Case> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::new();
    for g in all_grammars() {
        for _ in 0..per_cmd {
            out.push(sample(&mut rng, &g));
        }
    }
    out
}

/// Replace the `=value` of a `--flag=value` token with an edge-case value, or
/// return the flag unchanged. Flags without `=` are left alone. This is how a
/// value parser meets empty / overflow / garbage inputs it never saw curated.
fn mutate_value(rng: &mut Rng, flag: &str) -> String {
    match flag.split_once('=') {
        Some((name, _)) if rng.chance(1, 3) => format!("{name}={}", rng.pick(VALUES)),
        _ => flag.to_string(),
    }
}

/// Build one invocation. Far more aggressive than a flag or two: it stacks
/// **repeated** flags (deep enough to trip re-parse and last-wins bugs), mutates
/// flag values, supplies **multiple** positionals, interleaves flags and
/// positionals in argument order, and injects a `--` separator — every degree
/// of freedom a real caller eventually exercises and none of which the corpus
/// covers. Still a pure function of the RNG, so any failure replays from its
/// seed.
fn sample(rng: &mut Rng, g: &Grammar) -> Case {
    // Up to 6 flags, WITH repetition allowed. Repeats are not dilution: a
    // re-declared flag is exactly what surfaces last-wins and re-parse bugs.
    let mut flag_tokens: Vec<String> = Vec::new();
    if !g.flags.is_empty() {
        for _ in 0..rng.count_upto(6) {
            let flag = *rng.pick(g.flags);
            flag_tokens.push(mutate_value(rng, flag));
        }
    }

    // Up to 3 positionals, repetition allowed (`git log HEAD HEAD` is valid and
    // has its own behavior). Empty positionals are dropped, not emitted.
    let mut pos_tokens: Vec<String> = Vec::new();
    if !g.positionals.is_empty() {
        for _ in 0..rng.count_upto(3) {
            let p = *rng.pick(g.positionals);
            if !p.is_empty() {
                pos_tokens.push(p.to_string());
            }
        }
    }

    let mut args = vec![g.cmd.to_string()];

    // Ordering: usually flags-then-positionals as a caller writes it, but a
    // fraction of the time interleave them, which tests that option parsing does
    // not depend on flags preceding operands (git's does not; a buggy port's
    // might). A `--` separator is injected before the positionals sometimes,
    // both with and without interleaving.
    let sep = !pos_tokens.is_empty() && rng.chance(1, 4);
    if rng.chance(1, 3) && !flag_tokens.is_empty() && !pos_tokens.is_empty() {
        // Interleave by draining the two lists in a random order.
        let mut fi = flag_tokens.into_iter().peekable();
        let mut pi = pos_tokens.into_iter().peekable();
        let mut sep_done = !sep;
        while fi.peek().is_some() || pi.peek().is_some() {
            let take_flag = match (fi.peek().is_some(), pi.peek().is_some()) {
                (true, false) => true,
                (false, true) => false,
                _ => rng.chance(1, 2),
            };
            if take_flag {
                args.push(fi.next().unwrap());
            } else {
                if !sep_done {
                    args.push("--".to_string());
                    sep_done = true;
                }
                args.push(pi.next().unwrap());
            }
        }
        if !sep_done {
            // No positional was emitted after all; nothing to separate.
        }
    } else {
        args.extend(flag_tokens);
        if sep {
            args.push("--".to_string());
        }
        args.extend(pos_tokens);
    }

    Case { cmd: g.cmd, args, shape: *rng.pick(g.shapes) }
}

/// Shrink a failing case to a minimal still-failing one by greedily dropping
/// arguments. `still_fails` re-runs the candidate; the subcommand at index 0 is
/// never dropped.
///
/// Reported failures are worth far more minimized: a three-flag failure usually
/// reduces to one flag, which names the actual defect.
pub fn shrink(case: &Case, still_fails: &mut dyn FnMut(&Case) -> bool) -> Case {
    let mut best = case.clone();
    let mut i = 1;
    while i < best.args.len() {
        let mut candidate = best.clone();
        candidate.args.remove(i);
        if still_fails(&candidate) {
            best = candidate; // keep index: the list shifted left under us
        } else {
            i += 1;
        }
    }
    best
}
