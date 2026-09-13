use crate::gitcomp::*;

/// `diagnose_options[]` (builtin/diagnose.c:29-38).
pub(super) const DIAGNOSE_OPTIONS: &[Opt] = &[
    OPT_STRING("output-directory"),
    OPT_STRING("suffix"),
    OPT_CALLBACK_F("mode", PARSE_OPT_NONEG),
];
