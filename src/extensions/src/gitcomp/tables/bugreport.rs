use crate::gitcomp::*;

/// `bugreport_options[]` (builtin/bugreport.c:111-120).
pub(super) const BUGREPORT_OPTIONS: &[Opt] = &[
    OPT_CALLBACK_F("diagnose", PARSE_OPT_OPTARG),
    OPT_STRING("output-directory"),
    OPT_STRING("suffix"),
];
