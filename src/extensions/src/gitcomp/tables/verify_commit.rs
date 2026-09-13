use crate::gitcomp::*;

/// `verify_commit_options[]` (builtin/verify-commit.c:61-65).
pub(super) const VERIFY_COMMIT_OPTIONS: &[Opt] = &[
    OPT__VERBOSE(),
    OPT_BIT("raw"),
];
