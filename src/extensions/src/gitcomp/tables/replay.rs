use crate::gitcomp::*;

/// `replay_options[]` (builtin/replay.c:91-116, git 2.56).
pub(super) const REPLAY_OPTIONS: &[Opt] = &[
    OPT_BOOL("contained"),
    OPT_STRING_F("onto", PARSE_OPT_NONEG),
    OPT_STRING_F("advance", PARSE_OPT_NONEG),
    OPT_STRING_F("revert", PARSE_OPT_NONEG),
    OPT_STRING_F("ref", PARSE_OPT_NONEG),
    OPT_STRING_F("ref-action", PARSE_OPT_NONEG),
    OPT_BOOL("linearize"),
];
