use crate::gitcomp::*;

/// `options[]` (builtin/clean.c:936-949).
pub(super) const CLEAN_OPTIONS: &[Opt] = &[
    OPT__QUIET(),
    OPT__DRY_RUN(),
    OPT__FORCE(PARSE_OPT_NOCOMPLETE),
    OPT_BOOL("interactive"),
    OPT_BOOL(NULL),
    OPT_CALLBACK_F("exclude", PARSE_OPT_NONEG),
    OPT_BOOL(NULL),
    OPT_BOOL(NULL),
];
