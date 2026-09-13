use crate::gitcomp::*;

/// `mt_options[]` (builtin/merge-tree.c:557-590).
pub(super) const MT_OPTIONS: &[Opt] = &[
    OPT_CMDMODE("write-tree"),
    OPT_CMDMODE("trivial-merge"),
    OPT_BOOL("messages"),
    OPT_BOOL_F("quiet", PARSE_OPT_NONEG),
    OPT_SET_INT(NULL),
    OPT_BOOL_F("name-only", PARSE_OPT_NONEG),
    OPT_BOOL_F("allow-unrelated-histories", PARSE_OPT_NONEG),
    OPT_BOOL_F("stdin", PARSE_OPT_NONEG),
    OPT_STRING("merge-base"),
    OPT_STRVEC("strategy-option"),
];
