use crate::gitcomp::*;

/// `show_ref_options[]` (builtin/show-ref.c:309-333).
pub(super) const SHOW_REF_OPTIONS: &[Opt] = &[
    OPT_BOOL("tags"),
    OPT_BOOL("branches"),
    OPT_HIDDEN_BOOL("heads"),
    OPT_BOOL("exists"),
    OPT_BOOL("verify"),
    OPT_HIDDEN_BOOL(NULL),
    OPT_BOOL("head"),
    OPT_BOOL("dereference"),
    OPT_CALLBACK_F("hash", PARSE_OPT_OPTARG),
    OPT__ABBREV(),
    OPT__QUIET(),
    OPT_CALLBACK_F("exclude-existing", PARSE_OPT_OPTARG | PARSE_OPT_NONEG),
];
