use crate::gitcomp::*;

/// `options[]` (builtin/repo.c:937-941).
pub(super) const REPO_OPTIONS: &[Opt] = &[
    OPT_SUBCOMMAND("info"),
    OPT_SUBCOMMAND("structure"),
];
