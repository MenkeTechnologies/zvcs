use crate::gitcomp::*;

/// `hash_object_options[]` (builtin/hash-object.c:83-95).
pub(super) const HASH_OBJECT_OPTIONS: &[Opt] = &[
    OPT_STRING(NULL),
    OPT_BIT(NULL),
    OPT_COUNTUP("stdin"),
    OPT_BOOL("stdin-paths"),
    OPT_BOOL("no-filters"),
    OPT_NEGBIT("literally"),
    OPT_STRING("path"),
];
