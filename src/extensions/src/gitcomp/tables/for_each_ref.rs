use crate::gitcomp::*;

/// `opts[]` (builtin/for-each-ref.c:23-53).
pub(super) const FOR_EACH_REF_OPTIONS: &[Opt] = &[
    OPT_BIT("shell"),
    OPT_BIT("perl"),
    OPT_BIT("python"),
    OPT_BIT("tcl"),
    OPT_BOOL("omit-empty"),
    OPT_GROUP(),
    OPT_INTEGER("count"),
    OPT_STRING("format"),
    OPT_STRING("start-after"),
    OPT__COLOR(),
    OPT_REF_FILTER_EXCLUDE(),
    OPT_REF_SORT(),
    OPT_CALLBACK("points-at"),
    OPT_MERGED(),
    OPT_NO_MERGED(),
    OPT_CONTAINS(),
    OPT_NO_CONTAINS(),
    OPT_BOOL("ignore-case"),
    OPT_BOOL("stdin"),
    OPT_BOOL("include-root-refs"),
];
