use crate::gitcomp::*;

/// `show_index_options[]` (builtin/show-index.c:28-32).
pub(super) const SHOW_INDEX_OPTIONS: &[Opt] = &[
    OPT_STRING("object-format"),
];
