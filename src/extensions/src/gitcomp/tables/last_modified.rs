use crate::gitcomp::*;

/// `last_modified_options[]` (builtin/last-modified.c:530-540).
pub(super) const LAST_MODIFIED_OPTIONS: &[Opt] = &[
    OPT_SET_INT("recursive"),
    OPT_BOOL("show-trees"),
    OPT_INTEGER_F("max-depth", PARSE_OPT_NONEG),
    OPT_BOOL(NULL),
];
