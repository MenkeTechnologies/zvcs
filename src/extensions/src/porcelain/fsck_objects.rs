use anyhow::Result;
use std::process::ExitCode;

/// `git fsck-objects` — a synonym for `git fsck`.
///
/// Stock git wires both names to the same `cmd_fsck` entry in `builtin.c`, and
/// `git-fsck-objects(1)` says so outright: "This is a synonym for git-fsck(1)."
/// There is no behavioral difference — same flags, same output, same exit codes
/// — so this port delegates verbatim to [`super::fsck::fsck`] rather than
/// duplicating the traversal.
///
/// Consequences of that delegation, stated plainly so this doc claims nothing
/// the code does not do:
///   * Every flag `fsck` ports is ported here, byte-for-byte identical.
///   * Every flag `fsck` refuses is refused here, with `fsck`'s own message —
///     which names `fsck`'s ported flag set, not a separate one.
///   * The known divergences documented on [`super::fsck::fsck`] (no fsck
///     message layer, no `git refs verify`, no re-hashing, coarse corruption
///     exit code, gitlinks not walked, and the `obj_hash` output-ordering
///     restriction) apply here unchanged. Read them before trusting a clean
///     result from this command.
pub fn fsck_objects(args: &[String]) -> Result<ExitCode> {
    // Tolerate the subcommand being present at index 0 regardless of how the
    // dispatcher slices argv. `fsck` performs the same defensive strip for its
    // own name, so the remaining tail is handed over unchanged.
    let args: &[String] = match args.first() {
        Some(a) if a == "fsck-objects" => &args[1..],
        _ => args,
    };
    super::fsck::fsck(args)
}
