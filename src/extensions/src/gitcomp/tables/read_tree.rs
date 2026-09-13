use crate::gitcomp::*;

/// `read_tree_options[]` (builtin/read-tree.c:122-165).
pub(super) const READ_TREE_OPTIONS: &[Opt] = &[
    OPT__SUPER_PREFIX(),
    OPT_CALLBACK_F("index-output", PARSE_OPT_NONEG),
    OPT_BOOL("empty"),
    OPT__VERBOSE(),
    OPT_GROUP(),
    OPT_BOOL(NULL),
    OPT_BOOL("trivial"),
    OPT_BOOL("aggressive"),
    OPT_BOOL("reset"),
    option(Type::String, "prefix", PARSE_OPT_NONEG),
    OPT_BOOL(NULL),
    OPT_CALLBACK_F("exclude-per-directory", PARSE_OPT_NONEG),
    OPT_BOOL(NULL),
    OPT__DRY_RUN(),
    OPT_BOOL("no-sparse-checkout"),
    OPT_BOOL("debug-unpack"),
    OPT_CALLBACK_F("recurse-submodules", PARSE_OPT_OPTARG),
    OPT__QUIET(),
];
