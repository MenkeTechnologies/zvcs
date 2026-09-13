use crate::gitcomp::*;

/// `options[]` (builtin/merge-file.c:71-92).
pub(super) const MERGE_FILE_OPTIONS: &[Opt] = &[
    OPT_BOOL("stdout"),
    OPT_BOOL("object-id"),
    OPT_SET_INT("diff3"),
    OPT_SET_INT("zdiff3"),
    OPT_SET_INT("ours"),
    OPT_SET_INT("theirs"),
    OPT_SET_INT("union"),
    OPT_CALLBACK_F("diff-algorithm", PARSE_OPT_NONEG),
    OPT_INTEGER("marker-size"),
    OPT__QUIET(),
    OPT_CALLBACK(NULL),
];
