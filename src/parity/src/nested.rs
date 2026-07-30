//! Nested-option coverage: one declared matrix per subcommand, expanded to every
//! combination of the flags that interact.
//!
//! # Why this exists apart from the hand-written corpus
//!
//! The corpus is a list of invocations someone thought of. That works for a
//! command whose flags are independent — `rev-parse --short` and `rev-parse
//! --abbrev-ref` cannot break each other — and fails for the commands where the
//! flags are the semantics. `stash push -u -k -- <path>` is four decisions about
//! *what moves where*, and each pair of them has its own answer; `pull --rebase
//! --autostash` over a dirty tree is a different command from either flag alone.
//! Those are exactly the commands whose bugs kept reaching a human instead of the
//! harness, which is the whole reason for this module.
//!
//! A matrix is a base argv plus a list of *slots*. Each slot is a set of
//! alternatives, one of which is usually "nothing at all", and the expansion is
//! their cartesian product. Twelve flags in four slots is 40-ish cases rather
//! than the 4096 an unstructured product would give, because the slots are chosen
//! to hold the flags that actually interact and to leave out the ones that do not.
//!
//! # Why these cases compare stderr
//!
//! Every command here can *refuse*, and for a refusal the message is the whole
//! observable behaviour: `git pull` over a dirty tree, `git stash pop` on an empty
//! stack, `git merge` that would overwrite a local edit. Each of those shipped
//! with the wrong message at least once while stdout, exit code and resulting
//! state all agreed — so these cases opt into byte-comparing stderr
//! ([`Case::strict`]). The rest of the corpus keeps the old policy, so no
//! previously reported score moves because of this file.

use crate::fixture::Shape;
use crate::runner::Case;

/// One subcommand's option matrix.
struct Matrix {
    /// Scoring bucket, e.g. `stash`.
    cmd: &'static str,
    /// Argv every expansion starts with, e.g. `["stash", "push"]`.
    base: &'static [&'static str],
    /// The interacting flags, grouped so that one alternative per group is chosen.
    /// An empty alternative means "this group contributes nothing".
    slots: &'static [&'static [&'static [&'static str]]],
    /// Argv appended after every expansion — a pathspec, usually, which has to
    /// stay last.
    tail: &'static [&'static str],
    shape: Shape,
}

impl Matrix {
    /// Expand to one case per combination, skipping the all-empty one when the
    /// base alone is already in the corpus.
    fn expand(&self, out: &mut Vec<Case>) {
        let mut argvs: Vec<Vec<&'static str>> = vec![self.base.to_vec()];
        for slot in self.slots {
            let mut next = Vec::with_capacity(argvs.len() * slot.len());
            for argv in &argvs {
                for alt in *slot {
                    let mut one = argv.clone();
                    one.extend_from_slice(alt);
                    next.push(one);
                }
            }
            argvs = next;
        }
        for mut argv in argvs {
            argv.extend_from_slice(self.tail);
            out.push(Case::strict(self.cmd, &argv, self.shape));
        }
    }
}

/// `git stash`'s decisions about what moves: which kinds of change are taken, what
/// is left behind, and whether a pathspec narrows it.
///
/// `-u`/`-a` (untracked, then ignored too), `-k` (leave the index), `-S` (stage
/// only) and a pathspec each change the answer, and they change it *together*:
/// `-k -S` is a contradiction git resolves one way, `-u -- <path>` another.
const STASH_PUSH: Matrix = Matrix {
    cmd: "stash",
    base: &["stash", "push"],
    slots: &[
        &[&[], &["-u"], &["-a"]],
        &[&[], &["-k"], &["--no-keep-index"]],
        &[&[], &["-S"]],
        &[&[], &["-q"]],
    ],
    tail: &[],
    shape: Shape::Stashed,
};

/// The same decisions with a pathspec, which git treats as a different code path
/// (`--` forces the push form and changes which flags are legal).
const STASH_PUSH_PATHS: Matrix = Matrix {
    cmd: "stash",
    base: &["stash", "push"],
    slots: &[
        &[&[], &["-u"]],
        &[&[], &["-k"]],
        &[&[], &["-m", "msg"]],
    ],
    tail: &["--", "counter.txt"],
    shape: Shape::Stashed,
};

/// Taking an entry back out: which entry, whether the index comes with it, and
/// whether the entry survives.
const STASH_RESTORE: Matrix = Matrix {
    cmd: "stash",
    base: &["stash"],
    slots: &[
        &[&["pop"], &["apply"]],
        &[&[], &["--index"]],
        &[&[], &["-q"]],
        &[&[], &["stash@{1}"], &["stash@{9}"]],
    ],
    tail: &[],
    shape: Shape::Stashed,
};

/// Reading the stack. These cannot refuse much, but they are where a wrong entry
/// order or a wrong `--include-untracked` rendering shows up.
const STASH_READ: Matrix = Matrix {
    cmd: "stash",
    base: &["stash"],
    slots: &[
        &[&["list"], &["show"]],
        &[&[], &["-p"], &["--stat"], &["--name-only"]],
        &[&[], &["stash@{1}"], &["stash@{2}"]],
    ],
    tail: &[],
    shape: Shape::Stashed,
};

/// Dropping and branching, which mutate the stack and so are judged on the
/// resulting state as much as on the message.
const STASH_DROP: Matrix = Matrix {
    cmd: "stash",
    base: &["stash"],
    slots: &[
        &[&["drop"], &["branch", "off-stash"]],
        &[&[], &["stash@{1}"], &["stash@{9}"]],
    ],
    tail: &[],
    shape: Shape::Stashed,
};

/// `git pull`'s integration policy over a dirty worktree: fast-forward or merge or
/// rebase, whether the dirt is stashed for the duration, and whether a
/// fast-forward is required.
///
/// `main` is behind by three commits that do not touch the dirty file, so the
/// fast-forward has to succeed; `div` has diverged and rewrites a file the
/// worktree is holding dirty, so that one has to refuse — and the refusal is the
/// message this measures.
const PULL_MAIN: Matrix = Matrix {
    cmd: "pull",
    base: &["pull"],
    slots: &[
        &[&[], &["--rebase"], &["--no-rebase"], &["--ff-only"]],
        &[&[], &["--autostash"], &["--no-autostash"]],
        &[&[], &["-q"], &["-v"]],
    ],
    tail: &[],
    shape: Shape::BehindRemote,
};

/// The same policies against the diverged branch, where every one of them has to
/// stop on the dirty path.
const PULL_DIV: Matrix = Matrix {
    cmd: "pull",
    base: &["pull", "origin", "div"],
    slots: &[
        &[&[], &["--rebase"], &["--no-rebase"], &["--ff-only"]],
        &[&[], &["--autostash"]],
        &[&[], &["--no-commit"]],
    ],
    tail: &[],
    shape: Shape::BehindRemote,
};

/// `git merge` from the tracking ref, which is the same integration machinery
/// reached without the fetch — so a difference here localises a pull failure to
/// one half or the other.
const MERGE_TRACKING: Matrix = Matrix {
    cmd: "merge",
    base: &["merge"],
    slots: &[
        &[&["origin/main"], &["origin/div"]],
        &[&[], &["--ff-only"], &["--no-ff"], &["--squash"]],
        &[&[], &["--no-commit"]],
        &[&[], &["-q"]],
    ],
    tail: &[],
    shape: Shape::BehindRemote,
};

/// `git fetch` against the fixture's own remote: the flags that decide what is
/// written, over a repository that is genuinely behind.
const FETCH_REMOTE: Matrix = Matrix {
    cmd: "fetch",
    base: &["fetch"],
    slots: &[
        &[&[], &["origin"], &["origin", "main"]],
        &[&[], &["--dry-run"]],
        &[&[], &["--tags"], &["--no-tags"]],
        &[&[], &["--prune"]],
    ],
    tail: &[],
    shape: Shape::BehindRemote,
};

const MATRICES: &[&Matrix] = &[
    &STASH_PUSH,
    &STASH_PUSH_PATHS,
    &STASH_RESTORE,
    &STASH_READ,
    &STASH_DROP,
    &PULL_MAIN,
    &PULL_DIV,
    &MERGE_TRACKING,
    &FETCH_REMOTE,
];

pub fn cases(out: &mut Vec<Case>) {
    for m in MATRICES {
        m.expand(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The product is the product: one case per combination, and every case keeps
    /// its tail last. A slot list that silently dropped an alternative would
    /// under-measure exactly the interaction this module exists to cover.
    #[test]
    fn expansion_is_the_full_product_with_the_tail_last() {
        let mut cases = Vec::new();
        STASH_PUSH_PATHS.expand(&mut cases);
        assert_eq!(cases.len(), 2 * 2 * 2, "three two-way slots");
        for c in &cases {
            assert_eq!(&c.args[..2], &["stash".to_string(), "push".to_string()]);
            assert_eq!(
                &c.args[c.args.len() - 2..],
                &["--".to_string(), "counter.txt".to_string()],
                "the pathspec has to stay last: {:?}",
                c.args
            );
            assert!(c.compare_stderr, "a refusal's message is its behaviour");
        }
        // The all-empty combination is the bare base plus the tail.
        assert!(cases.iter().any(|c| c.args.len() == 4));
    }

    /// Every matrix contributes, and no two cases share an id — a duplicate would
    /// be counted twice in the denominator.
    #[test]
    fn ids_are_unique_across_every_matrix() {
        let mut all = Vec::new();
        super::cases(&mut all);
        let mut ids: Vec<String> = all.iter().map(Case::id).collect();
        let total = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), total, "duplicate case ids");
        assert!(
            total > 100,
            "the matrices should expand to a real number of cases, got {total}"
        );
    }
}
