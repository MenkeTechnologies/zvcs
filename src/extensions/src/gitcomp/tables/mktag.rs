use crate::gitcomp::*;

/// `builtin_mktag_options[]` (builtin/mktag.c:80-84).
pub(super) const BUILTIN_MKTAG_OPTIONS: &[Opt] = &[
    OPT_BOOL("strict"),
];
