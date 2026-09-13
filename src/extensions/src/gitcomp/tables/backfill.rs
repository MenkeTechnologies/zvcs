use crate::gitcomp::*;

/// `options[]` (builtin/backfill.c:153-161).
pub(super) const BACKFILL_OPTIONS: &[Opt] = &[
    OPT_UNSIGNED("min-batch-size"),
    OPT_BOOL("sparse"),
    OPT_BOOL("include-edges"),
];
