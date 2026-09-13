use crate::gitcomp::*;

/// `init_db_options[]` (builtin/init-db.c:90-114).
pub(super) const INIT_DB_OPTIONS: &[Opt] = &[
    OPT_STRING("template"),
    OPT_SET_INT("bare"),
    option(Type::Callback, "shared", PARSE_OPT_OPTARG | PARSE_OPT_NONEG),
    OPT_BIT("quiet"),
    OPT_STRING("separate-git-dir"),
    OPT_STRING("initial-branch"),
    OPT_STRING("object-format"),
    OPT_STRING("ref-format"),
];
