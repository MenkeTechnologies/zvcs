use crate::gitcomp::*;

/// `check_mailmap_options[]` (builtin/check-mailmap.c:20-25).
pub(super) const CHECK_MAILMAP_OPTIONS: &[Opt] = &[
    OPT_BOOL("stdin"),
    OPT_FILENAME("mailmap-file"),
    OPT_STRING("mailmap-blob"),
];
