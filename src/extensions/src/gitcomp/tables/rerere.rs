use crate::gitcomp::*;

/// `options[]` (builtin/rerere.c:61-65).
pub(super) const RERERE_OPTIONS: &[Opt] = &[
    OPT_SET_INT("rerere-autoupdate"),
];
