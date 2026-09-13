use crate::gitcomp::*;

/// `check_ignore_options[]` (builtin/check-ignore.c:22-35).
pub(super) const CHECK_IGNORE_OPTIONS: &[Opt] = &[
    OPT__QUIET(),
    OPT__VERBOSE(),
    OPT_GROUP(),
    OPT_BOOL("stdin"),
    OPT_BOOL(NULL),
    OPT_BOOL("non-matching"),
    OPT_BOOL("no-index"),
];
