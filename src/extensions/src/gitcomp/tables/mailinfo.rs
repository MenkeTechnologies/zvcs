use crate::gitcomp::*;

/// `options[]` (builtin/mailinfo.c:62-84).
pub(super) const MAILINFO_OPTIONS: &[Opt] = &[
    OPT_BOOL(NULL),
    OPT_BOOL(NULL),
    OPT_BOOL("message-id"),
    OPT_SET_INT_F(NULL, PARSE_OPT_NONEG),
    OPT_SET_INT_F(NULL, PARSE_OPT_NONEG),
    OPT_CALLBACK_F("encoding", PARSE_OPT_NONEG),
    OPT_BOOL("scissors"),
    OPT_CALLBACK_F("quoted-cr", PARSE_OPT_NONEG),
    OPT_HIDDEN_BOOL("inbody-headers"),
];
