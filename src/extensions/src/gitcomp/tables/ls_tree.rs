use crate::gitcomp::*;

/// `ls_tree_options[]` (builtin/ls-tree.c:351-377).
pub(super) const LS_TREE_OPTIONS: &[Opt] = &[
    OPT_BIT(NULL),
    OPT_BIT(NULL),
    OPT_BIT(NULL),
    OPT_BOOL(NULL),
    OPT_CMDMODE("long"),
    OPT_CMDMODE("name-only"),
    OPT_CMDMODE("name-status"),
    OPT_CMDMODE("object-only"),
    OPT_BOOL("full-name"),
    OPT_BOOL("full-tree"),
    OPT_STRING_F("format", PARSE_OPT_NONEG),
    OPT__ABBREV(),
];
