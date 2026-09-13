use crate::gitcomp::*;

/// `options[]` (builtin/for-each-repo.c:44-50).
pub(super) const FOR_EACH_REPO_OPTIONS: &[Opt] = &[
    OPT_STRING("config"),
    OPT_BOOL("keep-going"),
];
