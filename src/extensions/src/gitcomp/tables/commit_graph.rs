use crate::gitcomp::*;

/// `builtin_commit_graph_options[]` (builtin/commit-graph.c:347-351).
pub(super) const BUILTIN_COMMIT_GRAPH_OPTIONS: &[Opt] = &[
    OPT_SUBCOMMAND("verify"),
    OPT_SUBCOMMAND("write"),
];

/// `common_opts[]` (builtin/commit-graph.c:54-59).
pub(super) const COMMON_OPTS: &[Opt] = &[
    OPT_STRING("object-dir"),
];
