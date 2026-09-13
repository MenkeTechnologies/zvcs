use crate::gitcomp::*;

/// `builtin_add_options[]` (builtin/add.c:254-285), which `cmd_add()` passes to
/// `parse_options()` for both `add` and `stage`.
pub(super) const BUILTIN_ADD_OPTIONS: &[Opt] = &[
    OPT__DRY_RUN(),
    OPT__VERBOSE(),
    OPT_GROUP(),
    OPT_BOOL("interactive"),
    OPT_BOOL("patch"),
    OPT_BOOL("auto-advance"),
    OPT_DIFF_UNIFIED(),
    OPT_DIFF_INTERHUNK_CONTEXT(),
    OPT_BOOL("edit"),
    OPT__FORCE(0),
    OPT_BOOL("update"),
    OPT_BOOL("renormalize"),
    OPT_BOOL("intent-to-add"),
    OPT_BOOL("all"),
    OPT_CALLBACK_F("ignore-removal", PARSE_OPT_NOARG),
    OPT_BOOL("refresh"),
    OPT_BOOL("ignore-errors"),
    OPT_BOOL("ignore-missing"),
    OPT_BOOL("sparse"),
    OPT_STRING("chmod"),
    OPT_HIDDEN_BOOL("warn-embedded-repo"),
    OPT_PATHSPEC_FROM_FILE(),
    OPT_PATHSPEC_FILE_NUL(),
];
