use crate::gitcomp::*;

/// `options[]` (builtin/column.c:31-40).
pub(super) const COLUMN_OPTIONS: &[Opt] = &[
    OPT_STRING("command"),
    OPT_COLUMN("mode"),
    OPT_UNSIGNED("raw-mode"),
    OPT_INTEGER("width"),
    OPT_STRING("indent"),
    OPT_STRING("nl"),
    OPT_INTEGER("padding"),
];
