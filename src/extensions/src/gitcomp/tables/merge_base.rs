use crate::gitcomp::*;

/// `options[]` (builtin/merge-base.c:160-171).
pub(super) const MERGE_BASE_OPTIONS: &[Opt] = &[
    OPT_BOOL("all"),
    OPT_CMDMODE("octopus"),
    OPT_CMDMODE("independent"),
    OPT_CMDMODE("is-ancestor"),
    OPT_CMDMODE("fork-point"),
];
