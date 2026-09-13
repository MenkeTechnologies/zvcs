use crate::gitcomp::*;

/// `options[]` (builtin/interpret-trailers.c:147-167).
pub(super) const INTERPRET_TRAILERS_OPTIONS: &[Opt] = &[
    OPT_BOOL("in-place"),
    OPT_BOOL("trim-empty"),
    OPT_CALLBACK("where"),
    OPT_CALLBACK("if-exists"),
    OPT_CALLBACK("if-missing"),
    OPT_BOOL("only-trailers"),
    OPT_BOOL("only-input"),
    OPT_BOOL("unfold"),
    OPT_CALLBACK_F("parse", PARSE_OPT_NOARG | PARSE_OPT_NONEG),
    OPT_BOOL("no-divider"),
    OPT_CALLBACK("trailer"),
];
