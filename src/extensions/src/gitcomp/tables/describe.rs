use crate::gitcomp::*;

/// `options[]` (builtin/describe.c:648-686).
pub(super) const DESCRIBE_OPTIONS: &[Opt] = &[
    OPT_BOOL("contains"),
    OPT_BOOL("debug"),
    OPT_BOOL("all"),
    OPT_BOOL("tags"),
    OPT_BOOL("long"),
    OPT_BOOL("first-parent"),
    OPT__ABBREV(),
    OPT_CALLBACK_F("exact-match", PARSE_OPT_NOARG),
    OPT_INTEGER("candidates"),
    OPT_STRING_LIST("match"),
    OPT_STRING_LIST("exclude"),
    OPT_BOOL("always"),
    option(Type::String, "dirty", PARSE_OPT_OPTARG),
    option(Type::String, "broken", PARSE_OPT_OPTARG),
];
