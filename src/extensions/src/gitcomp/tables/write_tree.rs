use crate::gitcomp::*;

/// `write_tree_options[]` (builtin/write-tree.c:30-45).
pub(super) const WRITE_TREE_OPTIONS: &[Opt] = &[
    OPT_BIT("missing-ok"),
    OPT_STRING("prefix"),
    option(Type::Bit, "ignore-cache-tree", PARSE_OPT_HIDDEN | PARSE_OPT_NOARG),
];
