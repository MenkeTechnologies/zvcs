use crate::gitcomp::*;

/// `options[]` (builtin/symbolic-ref.c:53-61).
pub(super) const SYMBOLIC_REF_OPTIONS: &[Opt] = &[
    OPT__QUIET(),
    OPT_BOOL("delete"),
    OPT_BOOL("short"),
    OPT_BOOL("recurse"),
    OPT_STRING(NULL),
];
