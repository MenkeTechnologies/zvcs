use crate::gitcomp::*;

/// `option[]` (builtin/mktree.c:167-172).
pub(super) const MKTREE_OPTIONS: &[Opt] = &[
    OPT_BOOL(NULL),
    OPT_SET_INT("missing"),
    OPT_SET_INT("batch"),
];
