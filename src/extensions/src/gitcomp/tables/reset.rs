use crate::gitcomp::*;

/// `options[]` (builtin/reset.c:350-383).
pub(super) const RESET_OPTIONS: &[Opt] = &[
    OPT__QUIET(),
    OPT_BOOL("no-refresh"),
    OPT_SET_INT_F("mixed", PARSE_OPT_NONEG),
    OPT_SET_INT_F("soft", PARSE_OPT_NONEG),
    OPT_SET_INT_F("hard", PARSE_OPT_NONEG),
    OPT_SET_INT_F("merge", PARSE_OPT_NONEG),
    OPT_SET_INT_F("keep", PARSE_OPT_NONEG),
    OPT_CALLBACK_F("recurse-submodules", PARSE_OPT_OPTARG),
    OPT_BOOL("patch"),
    OPT_BOOL("auto-advance"),
    OPT_DIFF_UNIFIED(),
    OPT_DIFF_INTERHUNK_CONTEXT(),
    OPT_BOOL("intent-to-add"),
    OPT_PATHSPEC_FROM_FILE(),
    OPT_PATHSPEC_FILE_NUL(),
];
