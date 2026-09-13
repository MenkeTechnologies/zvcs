use crate::gitcomp::*;

/// `builtin_mv_options[]` (builtin/mv.c:215-223).
pub(super) const BUILTIN_MV_OPTIONS: &[Opt] = &[
    OPT__VERBOSE(),
    OPT__DRY_RUN(),
    OPT__FORCE(PARSE_OPT_NOCOMPLETE),
    OPT_BOOL(NULL),
    OPT_BOOL("sparse"),
];
