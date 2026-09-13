use crate::gitcomp::*;

/// `options[]` (builtin/commit-tree.c:105-126).
pub(super) const COMMIT_TREE_OPTIONS: &[Opt] = &[
    OPT_CALLBACK_F(NULL, PARSE_OPT_NONEG),
    OPT_CALLBACK_F(NULL, PARSE_OPT_NONEG),
    OPT_CALLBACK_F(NULL, PARSE_OPT_NONEG),
    option(Type::String, "gpg-sign", PARSE_OPT_OPTARG),
];
