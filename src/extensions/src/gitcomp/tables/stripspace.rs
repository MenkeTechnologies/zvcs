use crate::gitcomp::*;

/// `options[]` (builtin/stripspace.c:42-50).
pub(super) const STRIPSPACE_OPTIONS: &[Opt] = &[
    OPT_CMDMODE("strip-comments"),
    OPT_CMDMODE("comment-lines"),
];
