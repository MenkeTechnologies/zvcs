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
/// *should* fail, because agreeing on rejection is also parity.
const REVS: &[&str] = &[
    "HEAD", "HEAD^", "HEAD~1", "HEAD~2", "main", "@", "HEAD@{0}",
    "HEAD^{tree}", "HEAD^{commit}", "does-not-exist", "",
];

const PATHS: &[&str] = &["README.md", "src/lib.rs", "src", ".", "*.md", "no/such/path"];

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

/// Generate `per_cmd` cases for each grammar from `seed`.
pub fn generate(seed: u64, per_cmd: usize) -> Vec<Case> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::new();
    for g in grammars() {
        for _ in 0..per_cmd {
            out.push(sample(&mut rng, &g));
        }
    }
    out
}

fn sample(rng: &mut Rng, g: &Grammar) -> Case {
    let mut args = vec![g.cmd.to_string()];

    // 0..=3 flags, drawn without replacement so a combination is never diluted
    // into "the same flag three times".
    let n_flags = rng.below(4);
    let mut chosen: Vec<&str> = Vec::new();
    for _ in 0..n_flags {
        if g.flags.is_empty() {
            break;
        }
        let f = *rng.pick(g.flags);
        if !chosen.contains(&f) {
            chosen.push(f);
        }
    }
    for f in &chosen {
        args.push((*f).to_string());
    }

    // Positionals are optional: many of these commands have meaningfully
    // different no-argument behavior, and that path deserves coverage too.
    if !g.positionals.is_empty() && rng.chance(3, 4) {
        let p = *rng.pick(g.positionals);
        if !p.is_empty() {
            args.push(p.to_string());
        }
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
