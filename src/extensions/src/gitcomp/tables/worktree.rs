use crate::gitcomp::*;

/// `options[]` (builtin/worktree.c:1470-1480).
pub(super) const WORKTREE_OPTIONS: &[Opt] = &[
    OPT_SUBCOMMAND("add"),
    OPT_SUBCOMMAND("prune"),
    OPT_SUBCOMMAND("list"),
    OPT_SUBCOMMAND("lock"),
    OPT_SUBCOMMAND("unlock"),
    OPT_SUBCOMMAND("move"),
    OPT_SUBCOMMAND("remove"),
    OPT_SUBCOMMAND("repair"),
];
