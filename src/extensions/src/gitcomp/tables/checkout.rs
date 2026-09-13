//! `cmd_checkout()`, `cmd_switch()` and `cmd_restore()` each start from their
//! own array and extend it with the shared `add_*_options()` helpers through
//! `parse_options_dup()` / `parse_options_concat()` (builtin/checkout.c:2125-2128,
//! 2165-2167, 2203-2205), before `checkout_main()` hands the result to
//! `parse_options()` (builtin/checkout.c:1892).
use crate::gitcomp::*;

/// `checkout_options[]` (builtin/checkout.c:2096-2108).
pub(super) const CHECKOUT_OPTIONS: &[Opt] = &[
    OPT_STRING(NULL),
    OPT_STRING(NULL),
    OPT_BOOL(NULL),
    OPT_BOOL("guess"),
    OPT_BOOL("overlay"),
    OPT_BOOL("auto-advance"),
];

/// `switch_options[]` (builtin/checkout.c:2148-2158).
pub(super) const SWITCH_OPTIONS: &[Opt] = &[
    OPT_STRING("create"),
    OPT_STRING("force-create"),
    OPT_BOOL("guess"),
    OPT_BOOL("discard-changes"),
];

/// `restore_options[]` (builtin/checkout.c:2187-2198).
pub(super) const RESTORE_OPTIONS: &[Opt] = &[
    OPT_STRING("source"),
    OPT_BOOL("staged"),
    OPT_BOOL("worktree"),
    OPT_BOOL("ignore-unmerged"),
    OPT_BOOL("overlay"),
];

/// `add_common_options()`'s `options[]` (builtin/checkout.c:1767-1778).
pub(super) const ADD_COMMON_OPTIONS: &[Opt] = &[
    OPT__QUIET(),
    OPT_CALLBACK_F("recurse-submodules", PARSE_OPT_OPTARG),
    OPT_BOOL("progress"),
    OPT_BOOL("merge"),
    OPT_CALLBACK("conflict"),
];

/// `add_common_switch_branch_options()`'s `options[]`
/// (builtin/checkout.c:1787-1802).
pub(super) const ADD_COMMON_SWITCH_BRANCH_OPTIONS: &[Opt] = &[
    OPT_BOOL("detach"),
    OPT_CALLBACK_F("track", PARSE_OPT_OPTARG),
    OPT__FORCE(PARSE_OPT_NOCOMPLETE),
    OPT_STRING("orphan"),
    OPT_BOOL_F("overwrite-ignore", PARSE_OPT_NOCOMPLETE),
    OPT_BOOL("ignore-other-worktrees"),
];

/// `add_checkout_path_options()`'s `options[]` (builtin/checkout.c:1811-1826).
pub(super) const ADD_CHECKOUT_PATH_OPTIONS: &[Opt] = &[
    OPT_SET_INT_F("ours", PARSE_OPT_NONEG),
    OPT_SET_INT_F("theirs", PARSE_OPT_NONEG),
    OPT_BOOL("patch"),
    OPT_DIFF_UNIFIED(),
    OPT_DIFF_INTERHUNK_CONTEXT(),
    OPT_BOOL("ignore-skip-worktree-bits"),
    OPT_PATHSPEC_FROM_FILE(),
    OPT_PATHSPEC_FILE_NUL(),
];
