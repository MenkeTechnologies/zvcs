use crate::gitcomp::*;

/// `options[]` (builtin/prune.c:160-169).
pub(super) const PRUNE_OPTIONS: &[Opt] = &[
    OPT__DRY_RUN(),
    OPT__VERBOSE(),
    OPT_BOOL("progress"),
    OPT_EXPIRY_DATE("expire"),
    OPT_BOOL("exclude-promisor-objects"),
];
