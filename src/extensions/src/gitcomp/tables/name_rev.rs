use crate::gitcomp::*;

/// `opts[]` (builtin/name-rev.c:650-673).
pub(super) const NAME_REV_OPTIONS: &[Opt] = &[
    OPT_BOOL("name-only"),
    OPT_BOOL("tags"),
    OPT_STRING_LIST("refs"),
    OPT_STRING_LIST("exclude"),
    OPT_GROUP(),
    OPT_BOOL("all"),
    // `#ifndef WITH_BREAKING_CHANGES` (builtin/name-rev.c:659-665); stock 2.55.0
    // is built without it.
    OPT_BOOL_F("stdin", PARSE_OPT_HIDDEN),
    OPT_BOOL("annotate-stdin"),
    OPT_BOOL("undefined"),
    OPT_BOOL("always"),
    OPT_HIDDEN_BOOL("peel-tag"),
];

/// `opts[]` (builtin/name-rev.c:828-843).
pub(super) const FORMAT_REV_OPTIONS: &[Opt] = &[
    OPT_STRING("format"),
    OPT_STRING("stdin-mode"),
    OPT_STRING_LIST("notes"),
    OPT_CALLBACK_F("null", PARSE_OPT_NOARG | PARSE_OPT_NONEG),
    OPT_BOOL("null-input"),
    OPT_BOOL("null-output"),
];
