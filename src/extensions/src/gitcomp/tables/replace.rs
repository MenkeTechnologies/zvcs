use crate::gitcomp::*;

/// `options[]` (builtin/replace.c:562-573).
pub(super) const REPLACE_OPTIONS: &[Opt] = &[
    OPT_CMDMODE("list"),
    OPT_CMDMODE("delete"),
    OPT_CMDMODE("edit"),
    OPT_CMDMODE("graft"),
    OPT_CMDMODE("convert-graft-file"),
    OPT_BOOL_F("force", PARSE_OPT_NOCOMPLETE),
    OPT_BOOL("raw"),
    OPT_STRING("format"),
];
