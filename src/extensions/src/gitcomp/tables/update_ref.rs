use crate::gitcomp::*;

/// `options[]` (builtin/update-ref.c:821-832).
pub(super) const UPDATE_REF_OPTIONS: &[Opt] = &[
    OPT_STRING(NULL),
    OPT_BOOL(NULL),
    OPT_BOOL("no-deref"),
    OPT_BOOL(NULL),
    OPT_BOOL("stdin"),
    OPT_BOOL("create-reflog"),
    OPT_BIT("batch-updates"),
];
