use crate::gitcomp::*;

/// `options[]` (builtin/fsmonitor--daemon.c:1575-1585).
pub(super) const FSMONITOR__DAEMON_OPTIONS: &[Opt] = &[
    OPT_BOOL("detach"),
    OPT_INTEGER("ipc-threads"),
    OPT_INTEGER("start-timeout"),
];
