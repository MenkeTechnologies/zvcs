use crate::gitcomp::*;

/// `builtin_checkout_index_options[]` (builtin/checkout-index.c:227-251).
pub(super) const BUILTIN_CHECKOUT_INDEX_OPTIONS: &[Opt] = &[
    OPT_BOOL("all"),
    OPT_BOOL("ignore-skip-worktree-bits"),
    OPT__FORCE(0),
    OPT__QUIET(),
    OPT_BOOL("no-create"),
    OPT_BOOL("index"),
    OPT_BOOL(NULL),
    OPT_BOOL("stdin"),
    OPT_BOOL("temp"),
    OPT_STRING("prefix"),
    OPT_CALLBACK_F("stage", PARSE_OPT_NONEG),
];
