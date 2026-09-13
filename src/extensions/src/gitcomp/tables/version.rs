use crate::gitcomp::*;

/// `cmd_version()`'s `options[]` (help.c:841-845).
pub(super) const VERSION_OPTIONS: &[Opt] = &[
    OPT_BOOL("build-options"),
];
