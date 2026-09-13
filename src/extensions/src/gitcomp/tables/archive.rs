use crate::gitcomp::*;

/// `local_opts[]` (builtin/archive.c:86-94).
pub(super) const ARCHIVE_OPTIONS: &[Opt] = &[
    OPT_FILENAME("output"),
    OPT_STRING("remote"),
    OPT_STRING("exec"),
];
