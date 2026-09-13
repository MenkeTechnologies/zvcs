use crate::gitcomp::*;

/// `check_attr_options[]` (builtin/check-attr.c:25-33).
pub(super) const CHECK_ATTR_OPTIONS: &[Opt] = &[
    OPT_BOOL("all"),
    OPT_BOOL("cached"),
    OPT_BOOL("stdin"),
    OPT_BOOL(NULL),
    OPT_STRING("source"),
];
