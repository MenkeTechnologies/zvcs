use crate::gitcomp::*;

/// `builtin_show_branch_options[]` (builtin/show-branch.c:668-712).
pub(super) const BUILTIN_SHOW_BRANCH_OPTIONS: &[Opt] = &[
    OPT_BOOL("all"),
    OPT_BOOL("remotes"),
    OPT__COLOR(),
    option(Type::Integer, "more", PARSE_OPT_OPTARG),
    OPT_SET_INT("list"),
    OPT_BOOL("no-name"),
    OPT_BOOL("current"),
    OPT_BOOL("sha1-name"),
    OPT_BOOL("merge-base"),
    OPT_BOOL("independent"),
    OPT_SET_INT_F("topo-order", PARSE_OPT_NONEG),
    OPT_BOOL("topics"),
    OPT_SET_INT("sparse"),
    OPT_SET_INT_F("date-order", PARSE_OPT_NONEG),
    OPT_CALLBACK_F("reflog", PARSE_OPT_OPTARG | PARSE_OPT_NONEG),
];
