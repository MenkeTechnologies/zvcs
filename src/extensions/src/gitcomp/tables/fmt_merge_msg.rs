use crate::gitcomp::*;

/// `options[]` (builtin/fmt-merge-msg.c:22-49).
pub(super) const FMT_MERGE_MSG_OPTIONS: &[Opt] = &[
    option(Type::Integer, "log", PARSE_OPT_OPTARG),
    option(Type::Integer, "summary", PARSE_OPT_OPTARG | PARSE_OPT_HIDDEN),
    OPT_STRING("message"),
    OPT_STRING("into-name"),
    OPT_FILENAME("file"),
];
