use crate::gitcomp::*;

/// `verify_tag_options[]` (builtin/verify-tag.c:31-36).
pub(super) const VERIFY_TAG_OPTIONS: &[Opt] = &[
    OPT__VERBOSE(),
    OPT_BIT("raw"),
    OPT_STRING("format"),
];
