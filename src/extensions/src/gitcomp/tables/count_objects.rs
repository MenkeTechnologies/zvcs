use crate::gitcomp::*;

/// `opts[]` (builtin/count-objects.c:103-108).
pub(super) const COUNT_OBJECTS_OPTIONS: &[Opt] = &[
    OPT__VERBOSE(),
    OPT_BOOL("human-readable"),
];
