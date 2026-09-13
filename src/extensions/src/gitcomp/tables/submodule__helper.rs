use crate::gitcomp::*;

/// `options[]` (builtin/submodule--helper.c:3813-3832).
pub(super) const SUBMODULE__HELPER_OPTIONS: &[Opt] = &[
    OPT_SUBCOMMAND("migrate-gitdir-configs"),
    OPT_SUBCOMMAND("gitdir"),
    OPT_SUBCOMMAND("clone"),
    OPT_SUBCOMMAND("add"),
    OPT_SUBCOMMAND("update"),
    OPT_SUBCOMMAND("foreach"),
    OPT_SUBCOMMAND("init"),
    OPT_SUBCOMMAND("status"),
    OPT_SUBCOMMAND("sync"),
    OPT_SUBCOMMAND("deinit"),
    OPT_SUBCOMMAND("summary"),
    OPT_SUBCOMMAND("push-check"),
    OPT_SUBCOMMAND("absorbgitdirs"),
    OPT_SUBCOMMAND("set-url"),
    OPT_SUBCOMMAND("set-branch"),
    OPT_SUBCOMMAND("create-branch"),
    OPT_SUBCOMMAND("get-default-remote"),
];
