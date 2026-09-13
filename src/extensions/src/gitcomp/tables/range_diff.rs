use crate::gitcomp::*;

/// `range_diff_options[]` (builtin/range-diff.c:50-72).
pub(super) const RANGE_DIFF_OPTIONS: &[Opt] = &[
    OPT_INTEGER("creation-factor"),
    OPT_BOOL("no-dual-color"),
    OPT_PASSTHRU_ARGV("notes", PARSE_OPT_OPTARG),
    OPT_PASSTHRU_ARGV("diff-merges", 0),
    OPT_CALLBACK("max-memory"),
    OPT_PASSTHRU_ARGV("remerge-diff", PARSE_OPT_NOARG),
    OPT_BOOL("left-only"),
    OPT_BOOL("right-only"),
];
