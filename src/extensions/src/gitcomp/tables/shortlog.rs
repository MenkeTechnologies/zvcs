use crate::gitcomp::*;

/// `options[]` (builtin/shortlog.c:392-408).
pub(super) const SHORTLOG_OPTIONS: &[Opt] = &[
    OPT_BIT("committer"),
    OPT_BOOL("numbered"),
    OPT_BOOL("summary"),
    OPT_BOOL("email"),
    OPT_CALLBACK_F(NULL, PARSE_OPT_OPTARG),
    OPT_CALLBACK("group"),
];
