use crate::gitcomp::*;

/// `fsck_opts[]` (builtin/fsck.c:989-1006).
pub(super) const FSCK_OPTS: &[Opt] = &[
    OPT__VERBOSE(),
    OPT_BOOL("unreachable"),
    OPT_BOOL("dangling"),
    OPT_BOOL("tags"),
    OPT_BOOL("root"),
    OPT_BOOL("cache"),
    OPT_BOOL("reflogs"),
    OPT_BOOL("full"),
    OPT_BOOL("connectivity-only"),
    OPT_BOOL("strict"),
    OPT_BOOL("lost-found"),
    OPT_BOOL("progress"),
    OPT_BOOL("name-objects"),
    OPT_BOOL("references"),
];
