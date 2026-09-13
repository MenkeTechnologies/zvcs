use crate::gitcomp::*;

/// `builtin_rm_options[]` (builtin/rm.c:251-263).
pub(super) const BUILTIN_RM_OPTIONS: &[Opt] = &[
    OPT__DRY_RUN(),
    OPT__QUIET(),
    OPT_BOOL("cached"),
    OPT__FORCE(PARSE_OPT_NOCOMPLETE),
    OPT_BOOL(NULL),
    OPT_BOOL("ignore-unmatch"),
    OPT_BOOL("sparse"),
    OPT_PATHSPEC_FROM_FILE(),
    OPT_PATHSPEC_FILE_NUL(),
];
