//! Differential corpus cases for two surfaces every verb touches and no other
//! module owns: the **hooks** a command runs, and the **identity** it writes.
//!
//! Populated per-command; every case here is compared against stock git for
//! stdout, exit code and post-command repository state.
//!
//! # Hooks
//!
//! [`Shape::HooksFail`] is the only fixture whose hooks refuse. Everything in
//! the first half runs against it, and the state probe is what reads the
//! result: each hook writes `hook-<name>.txt` into the worktree naming what it
//! was handed, so `probe_worktree_content` turns "which hooks ran, in what
//! order, with which argv and which stdin" into bytes a case compares. A hook
//! that only exited would be indistinguishable from one that was never called.
//!
//! `crate::corpus::fixture_gaps2::hooks_that_refuse` already owns the first
//! pass over that shape — the plain/`--no-verify` pairs for `commit`, `merge`,
//! `push` and `rebase`, the `checkout`/`switch`/`stash`/`status`/`pull`
//! invocations, and `core.hooksPath=no-such-hooks` for `commit` and `push`.
//! Nothing here repeats one of those argvs. What this module adds is the part
//! that pass could not reach:
//!
//! * **`git hook run`.** The one verb that dispatches a hook as its whole
//!   purpose, so a hook can be measured without a verb's own control flow in
//!   front of it. It is also the only way to reach `pre-auto-gc` at all:
//!   `gc --auto` on this fixture never fires it, because the loose-object
//!   estimate is far below the threshold (verified on stock 2.55.0 — `-c
//!   gc.auto=1 gc --auto` exits 0 and writes no `hook-pre-auto-gc.txt`), so
//!   every existing `gc --auto` case measures the *absence* of the hook.
//! * **`pre-rebase` at all.** `Shape::HooksFail` has a dirty worktree, so a
//!   bare `rebase` is refused by the dirty-worktree check *before* the hook is
//!   consulted (`error: cannot rebase: You have unstaged changes.`). Every
//!   `rebase` case that exists therefore measures that check and not the hook.
//!   `-c rebase.autoStash=true` clears the worktree first and is what makes
//!   `pre-rebase`, and the `post-checkout`/`post-rewrite`/`prepare-commit-msg`
//!   trio a successful rebase fires, reachable.
//! * **The `--verify` spelling**, which is not the same argv as omitting
//!   `--no-verify`: it is a separate parse path on `commit`, `merge`, `push`
//!   and `rebase`.
//! * **`post-checkout` from a path checkout.** `checkout <tree-ish> -- <path>`
//!   and `restore --source=<ref> <path>` both run it with flag `0` on stock
//!   2.55.0, which is a different argument than the `1` a branch switch passes.
//! * **The receiving side.** The peer's `update` hook refusing one ref while
//!   another lands, `--atomic` turning that partial acceptance into a total
//!   refusal, deletions, and `receive.deny*` — which reach the peer at all only
//!   because `-c` is exported to the local `receive-pack` child through
//!   `GIT_CONFIG_PARAMETERS`.
//!
//! ## `core.hooksPath`
//!
//! Every `core.hooksPath` value here names a path **inside the fixture**, and
//! every one of them resolves either to a directory the fixture itself built or
//! to nothing at all. `.remote.git/hooks` is the peer's hook directory: it
//! holds the fixture's own `update` hook plus git's `*.sample` files, none of
//! which is a hook name a local verb dispatches, so pointing at it turns every
//! local hook off while still naming a real directory. `side-base.txt` is a
//! *file*, so the lookup fails with `ENOTDIR`. `.` is the worktree root, which
//! holds no hook-named file. None of them can run a program the fixture did not
//! install, and none can block: the only executables reachable are the eleven
//! shell scripts in `.git/hooks` and the peer's `update`, and every one of them
//! writes a file and exits.
//!
//! ## What is not measurable here
//!
//! * **A hook that is not executable.** `fixture::install_hooks` chmods every
//!   hook `0755` and a case cannot create a file, so no path in the fixture
//!   holds a non-executable file under a hook's name. Reaching it needs a
//!   fixture change, which is out of this module's scope.
//! * **`am`'s hooks.** `applypatch-msg`, `pre-applypatch` and `post-applypatch`
//!   are installed by no shape, and `am` runs none of the commit hooks — stock
//!   2.55.0 applies the mailbox below on `Shape::HooksFail` at exit 0 with no
//!   `hook-*.txt` written, with and without `--no-verify`. The `am` cases here
//!   therefore pin that `--no-verify`/`--verify` are *accepted and inert*, not
//!   that they gate anything. A hook-gated `am` needs an applypatch hook in the
//!   fixture.
//!
//! # Identity, and what `env::harden` makes unreachable
//!
//! `env::harden` pins `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`,
//! `GIT_COMMITTER_NAME` and `GIT_COMMITTER_EMAIL` on both sides, and
//! `env::is_pinned` forbids a case from re-pointing any of them. Environment
//! beats configuration in `ident.c`, so **the entire config-identity path is
//! shadowed**: with those four always set, `user.name` and `user.email` never
//! reach a commit no matter which scope delivers them, and `user.useConfigOnly`
//! never fires — its refusal is the branch taken when neither the environment
//! nor the config has an ident, and the environment always does. Verified on
//! stock 2.55.0: `-c user.useConfigOnly=true commit --allow-empty` exits 0, and
//! `-c user.name=Config Only -c user.email=cfg@example.invalid var
//! GIT_AUTHOR_IDENT` prints the *hardened* identity.
//!
//! That is a property of the harness and not of either binary, so the cases
//! below assert it rather than pretend to measure the other half: config
//! identity is delivered through `-c` **and** through a scope file, and the
//! contract is that both sides still report the environment's identity while
//! persisting the setting into `.git/config` where `probe_state`'s
//! `config --list --local` reads it. A port that let config win, or that
//! dropped the write, diverges. The half that stays unreachable — a repository
//! with *no* usable identity, and the `fatal: unable to auto-detect email
//! address` refusal that follows — needs `harden` to stop pinning the four
//! variables, which `env::is_pinned` exists to prevent.
//!
//! Everything else about identity *is* reachable and is covered here:
//! `--author=` in the spellings git accepts and several it does not,
//! `--committer=` (which `git commit` has never had), the signing refusals that
//! `gpg.program` pointed at a nonexistent program makes deterministic, the
//! `.mailmap` readers on [`Shape::Attributes`], and `i18n.commitEncoding` /
//! `i18n.logOutputEncoding`.
//!
//! `crate::corpus::shape_reach::mailmap` and `crate::corpus::log_format` own
//! the plain `check-mailmap`/`log --use-mailmap`/`shortlog`/`blame` pass; what
//! is added here is the *source* of the mailmap — `mailmap.file` and
//! `mailmap.blob`, pointed inside the fixture and at nothing — and the readers
//! those two modules do not call.

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope};

pub fn cases(out: &mut Vec<Case>) {
    hook_run(out);
    hook_order(out);
    verify_flags(out);
    hooks_path(out);
    receiving_side(out);
    identity_config(out);
    author_spellings(out);
    signing_refusals(out);
    mailmap_sources(out);
    encodings(out);
}

/// One [`Shape::HooksFail`] case.
fn hf(cmd: &'static str, args: &[&str], out: &mut Vec<Case>) {
    out.push(Case::new(cmd, args, Shape::HooksFail));
}

/// One [`Shape::HooksFail`] case carrying `-c` settings.
fn hfc(cmd: &'static str, args: &[&str], config: &[(&str, &str)], out: &mut Vec<Case>) {
    out.push(Case::new(cmd, args, Shape::HooksFail).with_config(config));
}

/// One [`Shape::Attributes`] case — the only shape with a `.mailmap` and with
/// commits by identities it rewrites.
fn at(cmd: &'static str, args: &[&str], out: &mut Vec<Case>) {
    out.push(Case::new(cmd, args, Shape::Attributes));
}

/// One [`Shape::Attributes`] case carrying `-c` settings.
fn atc(cmd: &'static str, args: &[&str], config: &[(&str, &str)], out: &mut Vec<Case>) {
    out.push(Case::new(cmd, args, Shape::Attributes).with_config(config));
}

// ---------------------------------------------------------------------------
// `git hook run`: a hook with no verb in front of it
// ---------------------------------------------------------------------------

/// Dispatch each installed hook directly, so its exit status, its argv and the
/// file it writes are measured without a verb's own control flow deciding
/// whether it runs at all.
///
/// Three things are only reachable this way on [`Shape::HooksFail`]:
///
/// * `pre-auto-gc`. `gc --auto` never fires it here — the fixture has a couple
///   of dozen loose objects and the auto threshold is 6700, so stock 2.55.0
///   exits 0 with no `hook-pre-auto-gc.txt` even at `-c gc.auto=1`.
/// * `post-commit`'s exit status. It exits 1 on purpose, and `commit` discards
///   that; `hook run` is where the 1 is visible (stock: exit 1, no output).
/// * `post-rewrite`'s stdin. Run under `hook run` its stdin is the harness's
///   `/dev/null`, so the hook records an empty ref list — the control against
///   the two-line list a `commit --amend` or a rebase feeds it.
///
/// `--allow-unknown-hook-name` is the pair that separates the two refusals git
/// has here: without it an unknown *name* is rejected by the parser
/// (`unknown hook event`), with it the name is accepted and the *lookup* fails
/// (`cannot find a hook named`). Both are strict — the message is the contract.
fn hook_run(out: &mut Vec<Case>) {
    // The refusals, each reached with no verb in front of it.
    for name in ["pre-commit", "pre-push", "pre-rebase", "pre-auto-gc", "post-commit"] {
        hf("hook", &["hook", "run", name], out);
    }
    // The hooks that record and exit 0.
    for name in ["post-merge", "post-checkout", "post-rewrite", "pre-merge-commit", "commit-msg"] {
        hf("hook", &["hook", "run", name], out);
    }
    // Arguments, handed over `--` exactly as the verb would pass them.
    hf("hook", &["hook", "run", "post-checkout", "--", "HEAD", "HEAD", "1"], out);
    // A hook fed a file rather than the harness's /dev/null: `pre-push` echoes
    // its stdin, so the tracked file's bytes come back out through the hook.
    hf("hook", &["hook", "run", "--to-stdin=side-base.txt", "pre-push"], out);
    // A hook the fixture does not install, tolerated by the flag that exists
    // for it: stock exits 0 and prints nothing.
    hf("hook", &["hook", "run", "--ignore-missing", "pre-applypatch"], out);
    // Two different refusals over the same unknown name.
    out.push(Case::strict("hook", &["hook", "run", "no-such-hook"], Shape::HooksFail));
    out.push(Case::strict(
        "hook",
        &["hook", "run", "--allow-unknown-hook-name", "no-such-hook"],
        Shape::HooksFail,
    ));
}

// ---------------------------------------------------------------------------
// Which verb runs which hook, and with what
// ---------------------------------------------------------------------------

/// The verbs whose hook set is not what the existing pass already measured.
///
/// Each entry was run against stock 2.55.0 on a copy of the shape and the hook
/// files it left behind recorded; the comment beside it is that observation, so
/// a reader can tell an implementation difference from a fixture change.
///
/// The two that are worth naming: a merge that is allowed to run *does* fire
/// `commit-msg` (and `pre-merge-commit`, and `post-merge`, and
/// `prepare-commit-msg` with `merge` as its second argument) while never firing
/// `pre-commit` — so `merge` and `commit` disagree about what "the commit
/// hooks" are; and `checkout <tree-ish> -- <path>` fires `post-checkout` with
/// the *file* flag `0` and identical old/new heads, which is a different call
/// than the branch switch that passes `1`.
fn hook_order(out: &mut Vec<Case>) {
    // merge: pre-merge-commit, prepare-commit-msg(merge), commit-msg, post-merge.
    hf("merge", &["merge", "hf-side"], out);
    hf("merge", &["merge", "-s", "ours", "--no-ff", "-m", "hooks: ours", "hf-side"], out);
    // `--squash` commits nothing, so only `post-merge` runs.
    hf("merge", &["merge", "--squash", "--no-verify", "hf-side"], out);
    hf("merge", &["merge", "--ff-only", "hf-side"], out);

    // commit --amend: prepare-commit-msg(commit HEAD), post-commit, and
    // post-rewrite fed `amend` plus one `<old> <new>` line on stdin.
    hf("commit", &["commit", "--amend", "-m", "hooks: amended", "--no-verify"], out);
    hf("commit", &["commit", "--amend", "--no-edit", "--no-verify", "--no-post-rewrite"], out);

    // A path checkout and a restore: post-checkout with flag 0.
    hf("checkout", &["checkout", "HEAD~1", "--", "side-base.txt"], out);
    hf("restore", &["restore", "--source=hf-side", "side-base.txt"], out);
    // A branch switch and a detach: post-checkout with flag 1.
    hf("checkout", &["checkout", "--detach", "hf-side"], out);
    hf("checkout", &["checkout", "-B", "hf-new", "hf-side"], out);
    hf("switch", &["switch", "--detach", "hf-side"], out);

    // rebase, with the worktree cleared first. Without `rebase.autoStash` the
    // dirty-worktree check refuses before `pre-rebase` is ever consulted, which
    // is what every existing rebase case on this shape measures.
    hfc("rebase", &["rebase", "hf-side"], &[("rebase.autoStash", "true")], out);
    hfc("rebase", &["rebase", "--onto", "hf-side", "main~1"], &[("rebase.autoStash", "true")], out);
    // The successful rebase: post-checkout, prepare-commit-msg(message),
    // post-commit, and post-rewrite fed `rebase` plus one line per commit.
    hfc(
        "rebase",
        &["rebase", "--no-verify", "hf-side"],
        &[("rebase.autoStash", "true")],
        out,
    );
    hfc(
        "rebase",
        &["rebase", "--no-verify", "--onto", "hf-side", "main~1"],
        &[("rebase.autoStash", "true")],
        out,
    );
    hfc(
        "rebase",
        &["rebase", "--no-verify", "-i", "hf-side"],
        &[("rebase.autoStash", "true")],
        out,
    );

    // pull: fetch is a no-op against a peer already at the pushed tip, so what
    // is left is which of merge's or rebase's hooks the integration half runs.
    hfc(
        "pull",
        &["pull", "--rebase", "origin", "main"],
        &[("rebase.autoStash", "true")],
        out,
    );
}

// ---------------------------------------------------------------------------
// `--verify` / `--no-verify` / `-n`, on every verb that has one
// ---------------------------------------------------------------------------

/// A one-patch mailbox for the `am` cases.
///
/// It touches a path no other commit in [`Shape::HooksFail`] touches, so it
/// applies cleanly over the shape's dirty worktree, and its author differs from
/// the hardened committer so the resulting commit's two identities are not the
/// same string. Literal bytes rather than a file read at generation time: the
/// corpus must be constructible without touching the filesystem.
const MBOX: &[u8] = b"\
From 1122334455667788990011223344556677889900 Mon Sep 17 00:00:00 2001
From: Patch Author <patch@example.invalid>
Date: Tue, 14 Nov 2023 22:13:20 +0000
Subject: [PATCH] hooks-fail: patch from a mailbox

---
 mbox-added.txt | 1 +
 1 file changed, 1 insertion(+)
 create mode 100644 mbox-added.txt

diff --git a/mbox-added.txt b/mbox-added.txt
new file mode 100644
index 0000000..7b57bd2
--- /dev/null
+++ b/mbox-added.txt
@@ -0,0 +1 @@
+from the mailbox
-- 
2.55.0

";

/// The gate flags, in all three spellings, on the five verbs that have them.
///
/// `--verify` is the spelling the existing pass never used, and it is not the
/// same argv as leaving the flag off — it is a separate `OPT_BOOL` toggle that
/// a port can accept, reject, or wire backwards independently of `--no-verify`.
///
/// `-n` is deliberately spelled on both `commit` and `push`, because it means
/// *different things*: on `commit` it is `--no-verify`, on `push` it is
/// `--dry-run`. A port that shares one flag table between them inverts one of
/// the two, and the pair here is what separates the two failures.
///
/// The `am` block pins the null result described in the module header:
/// `Shape::HooksFail` installs no `applypatch-msg`, `pre-applypatch` or
/// `post-applypatch`, and `am` runs none of the commit hooks, so all four
/// spellings apply the mailbox at exit 0 and write no `hook-*.txt` on stock
/// 2.55.0. What they measure is that the flag parses and changes nothing —
/// which is exactly what a port that mapped `--no-verify` onto the *commit*
/// hooks would break.
fn verify_flags(out: &mut Vec<Case>) {
    // commit: --verify is the explicit opposite of the refusal.
    hf("commit", &["commit", "--verify", "-am", "hooks: explicit verify"], out);
    hf("commit", &["commit", "--verify", "--no-verify", "-am", "hooks: last wins"], out);
    hf("commit", &["commit", "--no-verify", "--verify", "-am", "hooks: first loses"], out);
    hf("commit", &["commit", "-n", "--allow-empty", "-m", "hooks: short empty"], out);

    // merge.
    hf("merge", &["merge", "--verify", "--no-ff", "-m", "hooks: merge verify", "hf-side"], out);

    // push: `--dry-run` and `--no-verify` are independent, and `-n` here means
    // `--dry-run` rather than the `--no-verify` it means on `commit`.
    hf("push", &["push", "--verify", "origin", "main"], out);
    hf("push", &["push", "--no-verify", "--dry-run", "origin", "main"], out);
    hf("push", &["push", "--verify", "--dry-run", "origin", "main"], out);

    // rebase, over a worktree the autostash has cleared.
    hfc("rebase", &["rebase", "--verify", "hf-side"], &[("rebase.autoStash", "true")], out);
    hfc(
        "rebase",
        &["rebase", "--no-verify", "--verify", "hf-side"],
        &[("rebase.autoStash", "true")],
        out,
    );

    // am: accepted and inert, for the reason in the doc comment above.
    for args in [
        &["am"][..],
        &["am", "--no-verify"][..],
        &["am", "--verify"][..],
        &["am", "--no-verify", "-3"][..],
    ] {
        out.push(Case::with_stdin("am", args, Shape::HooksFail, MBOX));
    }
}

// ---------------------------------------------------------------------------
// `core.hooksPath`
// ---------------------------------------------------------------------------

/// Where git looks for a hook, redirected four ways — all of them inside the
/// fixture, for the reason the module header gives.
///
/// * `.git/hooks` — the default, written out. Identical behaviour to not
///   setting it at all, which is the control: if this diverges from the plain
///   invocation, the setting is being *parsed* wrong rather than resolved wrong.
/// * `.remote.git/hooks` — a real directory holding a real, executable hook
///   (the peer's `update`) that no local verb dispatches. Every local hook is
///   therefore off while the path still exists and is readable, which separates
///   "directory missing" from "hook missing" in whatever the port does next.
/// * `side-base.txt` — a tracked file. The lookup has to fail with `ENOTDIR`
///   rather than treat the path as a directory; stock 2.55.0 commits at exit 0.
/// * `no-such-hooks` — already covered for `commit` and `push` by
///   `fixture_gaps2`; extended here to the verbs it did not reach.
///
/// The `hook run` pair at the end is the same redirection with no verb in
/// front, so the diagnostic is the hook lookup's own and not a verb's summary
/// of it: stock answers both with `error: cannot find a hook named pre-commit`.
fn hooks_path(out: &mut Vec<Case>) {
    const PEER_HOOKS: &str = ".remote.git/hooks";

    // The default, spelled out.
    hfc("commit", &["commit", "-am", "hooks: default path"], &[("core.hooksPath", ".git/hooks")], out);

    // A directory whose hooks are not this repository's hooks.
    for args in [
        &["commit", "-am", "hooks: peer path"][..],
        &["merge", "--no-ff", "-m", "hooks: peer path", "hf-side"][..],
        &["checkout", "hf-side"][..],
        &["push", "origin", "main"][..],
    ] {
        let cmd: &'static str = match args[0] {
            "commit" => "commit",
            "merge" => "merge",
            "checkout" => "checkout",
            _ => "push",
        };
        out.push(Case::new(cmd, args, Shape::HooksFail).with_config(&[("core.hooksPath", PEER_HOOKS)]));
    }
    out.push(
        Case::new("rebase", &["rebase", "hf-side"], Shape::HooksFail)
            .with_config(&[("core.hooksPath", PEER_HOOKS), ("rebase.autoStash", "true")]),
    );

    // A path that is a file, not a directory.
    hfc("commit", &["commit", "-am", "hooks: path is a file"], &[("core.hooksPath", "side-base.txt")], out);
    hfc("push", &["push", "origin", "main"], &[("core.hooksPath", "side-base.txt")], out);

    // The worktree root, which holds no file named after a hook.
    hfc("commit", &["commit", "-am", "hooks: path is the root"], &[("core.hooksPath", ".")], out);

    // A directory that is not there, on the verbs the first pass did not reach.
    for args in [
        &["merge", "--no-ff", "-m", "hooks: nowhere", "hf-side"][..],
        &["checkout", "hf-side"][..],
    ] {
        let cmd: &'static str = if args[0] == "merge" { "merge" } else { "checkout" };
        out.push(
            Case::new(cmd, args, Shape::HooksFail).with_config(&[("core.hooksPath", "no-such-hooks")]),
        );
    }
    out.push(
        Case::new("rebase", &["rebase", "hf-side"], Shape::HooksFail)
            .with_config(&[("core.hooksPath", "no-such-hooks"), ("rebase.autoStash", "true")]),
    );

    // The lookup's own diagnostic, with no verb in front of it.
    for path in [PEER_HOOKS, "no-such-hooks", "side-base.txt"] {
        out.push(
            Case::strict("hook", &["hook", "run", "pre-commit"], Shape::HooksFail)
                .with_config(&[("core.hooksPath", path)]),
        );
    }
}

// ---------------------------------------------------------------------------
// The receiving end: the peer's `update` hook and `receive.deny*`
// ---------------------------------------------------------------------------

/// Refusals `--no-verify` has no say over, because they happen in the other
/// repository.
///
/// The peer's `update` hook rejects `refs/heads/veto` by name and accepts
/// everything else, so a single push can be half accepted — which is the case
/// `probe_peer` exists to score, since the accepted half is only visible in the
/// bare repository's refs. Every case here passes `--no-verify` so the local
/// `pre-push` refusal is out of the way and the remote's answer is what is
/// being measured.
///
/// Stock 2.55.0 on this shape, for the four shapes of answer:
///
/// * `veto:refs/heads/veto main:refs/heads/other` — `other` is created,
///   `veto` is `[remote rejected] (hook declined)`, exit 1.
/// * the same with `--atomic` — *neither* lands, and `main -> other` is
///   reported as `(atomic push failure)`.
/// * `main:refs/heads/veto` — the hook keys on the destination, so a source
///   the hook never heard of is still refused.
/// * `:refs/heads/hf-side` — a deletion the hook allows, exit 0.
///
/// `receive.*` set with `-c` on this side turns out **not** to reach the peer:
/// verified on stock 2.55.0, `-c receive.denyDeleteCurrent=ignore push origin
/// :refs/heads/main` is still refused, and the peer answers with the
/// `By default, deleting the current branch is denied` text it prints when the
/// variable is *unconfigured*. So these cases pin the propagation boundary
/// rather than the settings: whatever a `-c receive.*` does on the pushing side,
/// it must not change the receiving side, and the peer's own defaults must be
/// what answers. `denyCurrentBranch` is doubly inert — the peer is bare, and
/// that check only applies to a repository with a worktree — so its four values
/// pin that neither side invents a refusal. `denyDeleteCurrent` is where the
/// peer's default does refuse: exit 1, with the ref reported as
/// `(deletion of the current branch prohibited)`. `denyNonFastForwards` is likewise not what
/// rejects the rewind below: the pushing side's own non-fast-forward check
/// answers first (`! [rejected] hf-side -> main (non-fast-forward)`), which is
/// why the same refspec succeeds one case earlier under `--force`.
fn receiving_side(out: &mut Vec<Case>) {
    // A refspec whose two halves have different names, with the local hooks
    // left on. `pre-push` gets one line per update, and its first field is the
    // *local* ref while its third is the remote one, so a renaming refspec is
    // the only invocation where the two are distinguishable. Stock 2.55.0
    // writes `refs/heads/main <sha> refs/heads/other 0000000...`.
    hf("push", &["push", "--verify", "origin", "main:refs/heads/other"], out);

    // One ref accepted, one refused, in the same push.
    hf("push", &["push", "--no-verify", "origin", "veto:refs/heads/veto", "main:refs/heads/other"], out);
    hf(
        "push",
        &["push", "--no-verify", "--atomic", "origin", "veto:refs/heads/veto", "main:refs/heads/other"],
        out,
    );
    // The hook keys on the destination, not the source.
    hf("push", &["push", "--no-verify", "origin", "main:refs/heads/veto"], out);
    // Refspec sets that sweep `veto` in without naming it.
    hf("push", &["push", "--no-verify", "--all", "origin"], out);
    hf("push", &["push", "--no-verify", "--mirror", "origin"], out);
    // A dry run never reaches the peer, so the hook that would refuse never runs.
    hf("push", &["push", "--no-verify", "--dry-run", "origin", "veto"], out);

    // Deletions, which the `update` hook is asked about too.
    hf("push", &["push", "--no-verify", "origin", ":refs/heads/hf-side"], out);
    hf("push", &["push", "--no-verify", "--delete", "origin", "hf-side"], out);
    // A force that rewinds the peer's `main` onto an unrelated tip.
    hf("push", &["push", "--no-verify", "--force", "origin", "hf-side:refs/heads/main"], out);
    hf("push", &["push", "--no-verify", "--force-with-lease", "origin", "hf-side:refs/heads/main"], out);

    // `receive.*` set on the pushing side, which the peer does not see: what
    // answers is the receiving repository's own default.
    for value in ["refuse", "updateInstead"] {
        out.push(
            Case::new("push", &["push", "--no-verify", "origin", "main"], Shape::HooksFail)
                .with_config(&[("receive.denyCurrentBranch", value)]),
        );
    }
    hfc(
        "push",
        &["push", "--no-verify", "origin", ":refs/heads/main"],
        &[("receive.denyDeleteCurrent", "refuse")],
        out,
    );
    hfc(
        "push",
        &["push", "--no-verify", "origin", ":refs/heads/main"],
        &[("receive.denyDeleteCurrent", "ignore")],
        out,
    );
    hfc(
        "push",
        &["push", "--no-verify", "origin", "hf-side:refs/heads/main"],
        &[("receive.denyNonFastForwards", "true")],
        out,
    );
    // The peer's hook is still consulted when the local ones are switched off,
    // because `core.hooksPath` is a property of *this* repository only.
    hfc(
        "push",
        &["push", "origin", "veto"],
        &[("core.hooksPath", "no-such-hooks")],
        out,
    );
}

// ---------------------------------------------------------------------------
// Identity from configuration, under a hardened environment
// ---------------------------------------------------------------------------

/// `user.name` / `user.email`, delivered from every scope a case can reach, and
/// the one identity refusal that survives the environment pins.
///
/// The module header explains why the *values* are shadowed: `env::harden`
/// sets the four identity variables, environment beats configuration, and
/// `env::is_pinned` forbids a case from taking them away. What is left is still
/// worth pinning, and it is two different things.
///
/// The first is that the shadowing itself holds. `git var GIT_AUTHOR_IDENT`
/// and `GIT_COMMITTER_IDENT` are the narrowest readers of an ident there are,
/// and on stock 2.55.0 they print `zvcs parity <parity@example.invalid>` no
/// matter what `user.name` and `user.email` say and no matter which scope says
/// it. A port that resolved config before environment would answer `Cfg Name`
/// here and nowhere else in the corpus.
///
/// The second is that the *setting* still lands. A scope-file entry is written
/// into `.git/config`, which `probe_state`'s `config --list --local` reads, so
/// a commit that ignores the value while dropping the write is caught even
/// though its stdout is right.
///
/// The refusal that is reachable is a `user.email` with **no value**. Git reads
/// `user.email` as a string during `git_default_config`, which every command
/// runs before it decides whether it needs an ident at all, so a value-less key
/// is fatal regardless of the environment. Verified on stock 2.55.0 for `var`,
/// `commit`, `log` and `status` alike:
/// `error: missing value for 'user.email'` then
/// `fatal: bad config variable 'user.email' in file '.git/config' at line 10`,
/// exit 128. That is the whole of the identity-refusal surface this harness can
/// see; `user.useConfigOnly` is on the other side of the pin and commits at
/// exit 0.
fn identity_config(out: &mut Vec<Case>) {
    // The environment wins, from every scope.
    for scope in [ConfigScope::Repo, ConfigScope::Global, ConfigScope::CommandLine] {
        out.push(
            Case::new("var", &["var", "GIT_AUTHOR_IDENT"], Shape::Linear).with_scoped_config(vec![
                ConfigEntry::set(scope, "user.name", "Cfg Name"),
                ConfigEntry::set(scope, "user.email", "cfg@example.invalid"),
            ]),
        );
        out.push(
            Case::new("var", &["var", "GIT_COMMITTER_IDENT"], Shape::Linear).with_scoped_config(vec![
                ConfigEntry::set(scope, "user.name", "Cfg Name"),
                ConfigEntry::set(scope, "user.email", "cfg@example.invalid"),
            ]),
        );
    }

    // The setting lands in `.git/config` even though the commit ignores it.
    out.push(
        Case::new("commit", &["commit", "--allow-empty", "-m", "identity: from config"], Shape::Linear)
            .with_scoped_config(vec![
                ConfigEntry::set(ConfigScope::Repo, "user.name", "Cfg Name"),
                ConfigEntry::set(ConfigScope::Repo, "user.email", "cfg@example.invalid"),
            ]),
    );
    out.push(
        Case::new("commit", &["commit", "--allow-empty", "-m", "identity: worktree scope"], Shape::Linear)
            .with_scoped_config(vec![
                ConfigEntry::set(ConfigScope::Worktree, "user.name", "Worktree Name"),
                ConfigEntry::set(ConfigScope::Worktree, "user.email", "wt@example.invalid"),
            ]),
    );

    // Empty values, which are a different parse than absent ones.
    out.push(
        Case::new("var", &["var", "GIT_AUTHOR_IDENT"], Shape::Linear)
            .with_config(&[("user.name", ""), ("user.email", "")]),
    );

    // A malformed value. Shadowed, so what this pins is that neither side
    // validates an address it is never going to use.
    out.push(
        Case::new("var", &["var", "GIT_COMMITTER_IDENT"], Shape::Linear)
            .with_config(&[("user.email", "no-at-sign")]),
    );

    // `user.useConfigOnly`, which cannot fire while the environment is pinned.
    out.push(
        Case::new("var", &["var", "GIT_COMMITTER_IDENT"], Shape::Linear)
            .with_config(&[("user.useConfigOnly", "true")]),
    );
    out.push(
        Case::new("commit", &["commit", "--allow-empty", "-m", "identity: config only"], Shape::Linear)
            .with_config(&[("user.useConfigOnly", "true")]),
    );
    out.push(
        Case::new("commit", &["commit", "--allow-empty", "-m", "identity: config only, set"], Shape::Linear)
            .with_scoped_config(vec![
                ConfigEntry::set(ConfigScope::Repo, "user.useConfigOnly", "true"),
                ConfigEntry::set(ConfigScope::Repo, "user.name", "Cfg Name"),
                ConfigEntry::set(ConfigScope::Repo, "user.email", "cfg@example.invalid"),
            ]),
    );

    // The one refusal the pins leave reachable: `user.email` with no value.
    // Strict, because the two-line diagnostic and the 128 are the contract.
    for (cmd, args) in [
        ("var", &["var", "GIT_AUTHOR_IDENT"][..]),
        ("commit", &["commit", "--allow-empty", "-m", "identity: value-less email"][..]),
    ] {
        out.push(
            Case::strict(cmd, args, Shape::Linear)
                .with_scoped_config(vec![ConfigEntry::raw(ConfigScope::Repo, "[user]\n\temail")]),
        );
    }
}

// ---------------------------------------------------------------------------
// `--author=` and the flag `git commit` has never had
// ---------------------------------------------------------------------------

/// Every spelling of `--author=` git accepts, and several it does not.
///
/// This is the identity surface the environment pins do **not** shadow:
/// `--author` beats `GIT_AUTHOR_NAME`/`GIT_AUTHOR_EMAIL`, so what the argument
/// parses to is visible in the commit object and in `probe_state`'s
/// `for-each-ref`/`cat-file` sections.
///
/// [`Shape::Attributes`] is the shape for it: it is the only one whose commits
/// carry authors other than the hardened identity, and `--author` without angle
/// brackets is a *search* over exactly those. Stock 2.55.0 resolves
/// `--author=Proper` to `Proper Name <proper@example.invalid>` — the
/// mailmap-rewritten form, because the search runs over the same
/// `log --author` matcher, which honours `.mailmap` — and refuses
/// `--author=Alias`, whose raw form is in the history but whose rewritten form
/// is not.
///
/// The accepted forms, verified on stock 2.55.0:
///
/// | argument | result |
/// | --- | --- |
/// | `A U Thor <a@b.invalid>` | taken literally |
/// | `Proper` | search hit, mailmap-resolved |
/// | `  Padded   <  pad@x.invalid  >  ` | whitespace stripped from both halves |
/// | `A <a@b.invalid> trailing` | everything after `>` dropped |
/// | (empty) | the empty pattern matches the newest commit's author |
///
/// The refusals, each `Case::strict` because the message is the contract:
/// `nobrackets` and `A <a@b.invalid` get
/// `fatal: --author '…' is not 'Name <email>' and matches no existing author`,
/// and `<only@email.invalid>` gets
/// `fatal: empty ident name (for <only@email.invalid>) not allowed`.
///
/// `--committer=` is included because `git commit` has never had it — the
/// committer comes from the environment or the config and from nowhere else —
/// so `error: unknown option 'committer=…'` is the answer, and a port that
/// invented the flag would silently write a different committer. The
/// `log`/`shortlog` `--committer` that *does* exist is a filter over history
/// and is spelled here beside it so the two are not confused.
fn author_spellings(out: &mut Vec<Case>) {
    // Accepted.
    at("commit", &["commit", "--allow-empty", "-m", "author: literal", "--author=A U Thor <a@b.invalid>"], out);
    at("commit", &["commit", "--allow-empty", "-m", "author: search", "--author=Proper"], out);
    at("commit", &["commit", "--allow-empty", "-m", "author: padded", "--author=  Padded   <  pad@x.invalid  >  "], out);
    at("commit", &["commit", "--allow-empty", "-m", "author: trailing", "--author=A <a@b.invalid> trailing"], out);
    at("commit", &["commit", "--allow-empty", "-m", "author: empty pattern", "--author="], out);
    at("commit", &["commit", "--allow-empty", "-m", "author: solo", "--author=Solo Name <solo@example.invalid>"], out);
    // An amend, which is the other way the author moves.
    at("commit", &["commit", "--amend", "--no-edit", "--author=A U Thor <a@b.invalid>"], out);

    // Refused.
    out.push(Case::strict(
        "commit",
        &["commit", "--allow-empty", "-m", "author: no brackets", "--author=nobrackets"],
        Shape::Attributes,
    ));
    out.push(Case::strict(
        "commit",
        &["commit", "--allow-empty", "-m", "author: unclosed", "--author=A <a@b.invalid"],
        Shape::Attributes,
    ));
    out.push(Case::strict(
        "commit",
        &["commit", "--allow-empty", "-m", "author: no name", "--author=<only@email.invalid>"],
        Shape::Attributes,
    ));
    out.push(Case::strict(
        "commit",
        &["commit", "--allow-empty", "-m", "author: mailmapped away", "--author=Alias"],
        Shape::Attributes,
    ));
    // A bare address is not a search key either: the matcher wants the whole
    // `Name <email>` form or a substring of the rendered ident, and stock 2.55.0
    // answers `--author=typo@example.invalid` with the same refusal.
    out.push(Case::strict(
        "commit",
        &["commit", "--allow-empty", "-m", "author: bare address", "--author=typo@example.invalid"],
        Shape::Attributes,
    ));
    out.push(Case::strict(
        "commit",
        &["commit", "--allow-empty", "-m", "author: committer flag", "--committer=C <c@d.invalid>"],
        Shape::Attributes,
    ));

    // The `--committer` that does exist: a filter, not an identity.
    at("log", &["log", "--oneline", "--committer=zvcs"], out);
    at("shortlog", &["shortlog", "--group=committer", "-s", "-e", "HEAD"], out);
}

// ---------------------------------------------------------------------------
// Signing: a refusal made deterministic by a program that is not there
// ---------------------------------------------------------------------------

/// `commit.gpgSign`, `tag.gpgSign` and `-S`, with `gpg.program` pointed at a
/// name no `PATH` entry holds.
///
/// A real signature is not reproducible — it needs a key, and two runs of gpg
/// over the same payload do not produce the same bytes — so the signing paths
/// were unreachable by construction. Pointing `gpg.program` at a nonexistent
/// program makes them deterministic in the other direction: the spawn fails the
/// same way every time, on both sides, and what is being compared is the
/// *plumbing around* the signature — that git asked for one at all, what it
/// says when it cannot have one, and what it leaves behind.
///
/// The chosen name cannot exist and cannot block: it is a fixed literal that
/// names nothing on any machine, and `execvp` of a missing file returns
/// immediately.
///
/// Stock 2.55.0, exit 128 for all of them:
///
/// ```text
/// error: cannot run zvcs-parity-no-such-gpg: No such file or directory
/// error: gpg failed to sign the data:
/// (no gpg output)
/// fatal: failed to write commit object
/// ```
///
/// `tag` ends differently — `error: unable to sign the tag` and
/// `The tag message has been left in .git/TAG_EDITMSG` — and that last line is
/// the interesting one: the refusal leaves a file behind, which the state probe
/// reads. The `gpg.format=ssh` case refuses one step earlier, in configuration
/// validation rather than in the spawn
/// (`fatal: either user.signingkey or gpg.ssh.defaultKeyCommand needs to be
/// configured`), so it pins that the two failure modes stay distinct.
fn signing_refusals(out: &mut Vec<Case>) {
    const NO_GPG: &str = "zvcs-parity-no-such-gpg";

    out.push(
        Case::strict("commit", &["commit", "--allow-empty", "-m", "sign: config"], Shape::Linear)
            .with_config(&[("commit.gpgSign", "true"), ("gpg.program", NO_GPG)]),
    );
    out.push(
        Case::strict(
            "commit",
            &["commit", "--gpg-sign", "--allow-empty", "-m", "sign: long flag"],
            Shape::Linear,
        )
        .with_config(&[("gpg.program", NO_GPG)]),
    );
    out.push(
        Case::new("commit", &["commit", "-S", "--allow-empty", "-m", "sign: dash s"], Shape::Linear)
            .with_config(&[("gpg.program", NO_GPG)]),
    );
    // `--no-gpg-sign` beats the config, so this one has to succeed.
    out.push(
        Case::new("commit", &["commit", "--no-gpg-sign", "--allow-empty", "-m", "sign: opted out"], Shape::Linear)
            .with_config(&[("commit.gpgSign", "true"), ("gpg.program", NO_GPG)]),
    );
    out.push(
        Case::new("commit", &["commit", "--amend", "--no-edit", "--no-gpg-sign"], Shape::Linear)
            .with_config(&[("commit.gpgSign", "true"), ("gpg.program", NO_GPG)]),
    );
    // A signing key that names nothing, which changes the argv git would have
    // handed the program it cannot run.
    out.push(
        Case::new("commit", &["commit", "-S", "--allow-empty", "-m", "sign: keyed"], Shape::Linear)
            .with_config(&[("gpg.program", NO_GPG), ("user.signingKey", "no-such-key")]),
    );

    // tag, which leaves `.git/TAG_EDITMSG` behind when it refuses.
    out.push(
        Case::new("tag", &["tag", "-m", "signed", "v-signed"], Shape::Linear)
            .with_config(&[("tag.gpgSign", "true"), ("gpg.program", NO_GPG)]),
    );
    out.push(
        Case::new("tag", &["tag", "-s", "-m", "signed", "v-signed"], Shape::Linear)
            .with_config(&[("gpg.program", NO_GPG)]),
    );

    // The other refusal: rejected before anything is spawned.
    out.push(
        Case::strict("commit", &["commit", "-S", "--allow-empty", "-m", "sign: ssh"], Shape::Linear)
            .with_config(&[("gpg.format", "ssh"), ("gpg.ssh.program", "zvcs-parity-no-such-ssh-keygen")]),
    );
    // The readers, over history that carries no signature at all.
    at("log", &["log", "--show-signature", "--oneline", "-1"], out);
}

// ---------------------------------------------------------------------------
// Where the mailmap comes from
// ---------------------------------------------------------------------------

/// `mailmap.file` and `mailmap.blob`, and the readers `shape_reach` and
/// `log_format` do not call.
///
/// The rewriting itself is already covered; what is not is the *lookup* — a
/// second mailmap file layered on the worktree one, a `mailmap.file` naming a
/// path that is not there, and a `mailmap.blob` naming a blob that is and one
/// that is not. Every path named here is inside the fixture.
///
/// Stock 2.55.0 treats a missing `mailmap.file` and a missing `mailmap.blob` as
/// "no extra mailmap" rather than as an error: `.mailmap` in the worktree is
/// still read, so `check-mailmap 'Old Name <old@example.invalid>'` still
/// answers `Proper Name <proper@example.invalid>` in both cases. That silence
/// is the thing worth pinning — a port that made either one fatal would break
/// every repository whose `mailmap.file` points at a developer's home
/// directory.
fn mailmap_sources(out: &mut Vec<Case>) {
    // The worktree mailmap, named explicitly, and named as a blob.
    for (key, value) in [
        ("mailmap.file", ".mailmap"),
        ("mailmap.file", "no-such-mailmap"),
        ("mailmap.blob", "HEAD:.mailmap"),
        ("mailmap.blob", "HEAD:no-such-path"),
    ] {
        atc("check-mailmap", &["check-mailmap", "Old Name <old@example.invalid>"], &[(key, value)], out);
        atc("log", &["log", "--format=%aN <%aE>"], &[(key, value)], out);
    }
    // `log.mailmap` off, with a mailmap source still configured: the source has
    // to stay unread rather than be applied by whichever code path found it.
    atc(
        "log",
        &["log", "--format=%aN <%aE>"],
        &[("log.mailmap", "false"), ("mailmap.file", ".mailmap")],
        out,
    );
    atc(
        "log",
        &["log", "--use-mailmap", "--format=%aN <%aE>"],
        &[("log.mailmap", "false"), ("mailmap.blob", "HEAD:.mailmap")],
        out,
    );

    // Readers the adjacent modules do not call.
    at("blame", &["blame", "-e", "sub/nested.txt"], out);
    at("blame", &["blame", "--line-porcelain", "docs/manual.md"], out);
    at("log", &["log", "--format=%an|%aN|%ae|%aE|%cn|%cN|%ce|%cE"], out);
}

// ---------------------------------------------------------------------------
// `i18n.commitEncoding` and `i18n.logOutputEncoding`
// ---------------------------------------------------------------------------

/// The two encoding settings, on the writer and on every reader.
///
/// `commit_family` and `mail_series` already pin the `encoding` header
/// `i18n.commitEncoding` adds to a commit object. What is not pinned is the
/// *reader* half: `i18n.logOutputEncoding` and `--encoding=` decide what
/// `log`, `show` and `format-patch` do with that header, and the two settings
/// interact — a commit written under one encoding and read under another is the
/// only case where the recoding path runs at all.
///
/// Every message in the fixture is ASCII, so the recoded bytes are the same
/// bytes; what is being compared is whether the header is written, whether it
/// is echoed, and whether the reader claims to have recoded. That is the part a
/// port gets wrong without any non-ASCII input to notice it with.
fn encodings(out: &mut Vec<Case>) {
    for enc in ["ISO-8859-1", "none"] {
        atc("log", &["log", "--format=%s", "-1"], &[("i18n.logOutputEncoding", enc)], out);
    }
    atc("log", &["log", "--format=%e", "-1"], &[("i18n.commitEncoding", "ISO-8859-1")], out);
    at("log", &["log", "--encoding=ISO-8859-1", "--format=%s|%e"], out);
    at("show", &["show", "--encoding=ISO-8859-1", "--format=%s|%e", "--no-patch"], out);
    atc(
        "show",
        &["show", "--no-patch", "--format=%s|%e"],
        &[("i18n.logOutputEncoding", "ISO-8859-1"), ("i18n.commitEncoding", "ISO-8859-1")],
        out,
    );
    atc(
        "format-patch",
        &["format-patch", "--stdout", "-1"],
        &[("i18n.logOutputEncoding", "ISO-8859-1")],
        out,
    );
    // The writer, with the reader pinned the other way: the header the commit
    // carries and the encoding the reader announces are two different settings.
    out.push(
        Case::new("commit", &["commit", "--allow-empty", "-m", "encoding: mismatched"], Shape::Linear)
            .with_config(&[("i18n.commitEncoding", "ISO-8859-1"), ("i18n.logOutputEncoding", "UTF-8")]),
    );
    out.push(
        Case::new("commit", &["commit", "--allow-empty", "-m", "encoding: none"], Shape::Linear)
            .with_config(&[("i18n.commitEncoding", "none")]),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every case here runs against one of the three shapes this module claims,
    /// and every hook-side case runs against the only shape whose hooks refuse.
    /// A case that drifted onto another shape would still run and would measure
    /// nothing this module is for.
    #[test]
    fn shapes_are_the_ones_this_module_claims() {
        let mut c = Vec::new();
        cases(&mut c);
        for case in &c {
            assert!(
                matches!(case.shape, Shape::HooksFail | Shape::Attributes | Shape::Linear),
                "{} runs on an unclaimed shape",
                case.id()
            );
        }
    }

    /// No `core.hooksPath` in this module may leave the fixture. A relative path
    /// is resolved against the repository the case runs in; an absolute one, or
    /// one that climbs out with `..`, would point at whatever is on the machine
    /// — which is the one way a hook case can run a program the fixture did not
    /// install.
    #[test]
    fn every_hooks_path_stays_inside_the_fixture() {
        let mut c = Vec::new();
        cases(&mut c);
        let mut seen = 0;
        for case in &c {
            for entry in &case.config {
                if entry.key.as_deref() != Some("core.hooksPath") {
                    continue;
                }
                seen += 1;
                let path = &entry.value;
                assert!(!path.starts_with('/'), "{} points core.hooksPath outside: {path}", case.id());
                assert!(!path.starts_with('~'), "{} points core.hooksPath at a home: {path}", case.id());
                assert!(
                    !path.split('/').any(|part| part == ".."),
                    "{} climbs out of the fixture: {path}",
                    case.id()
                );
            }
        }
        assert!(seen >= 10, "the hooksPath dimension lost its cases: {seen}");
    }

    /// Deliberate error-path cases stay a minority. `Case::strict` is the marker:
    /// stderr is compared byte for byte exactly where the refusal *is* the
    /// contract, and a corpus that drifted into mostly-refusals would be
    /// measuring diagnostics instead of behaviour.
    #[test]
    fn refusals_stay_under_a_fifth() {
        let mut c = Vec::new();
        cases(&mut c);
        let strict = c.iter().filter(|case| case.compare_stderr).count();
        assert!(
            strict * 5 <= c.len(),
            "{strict} strict cases out of {} is over a fifth",
            c.len()
        );
        // The corpus stays in the band this module was scoped to: enough to
        // cover both surfaces, few enough that a full run is still cheap.
        assert!((100..=140).contains(&c.len()), "{} cases is outside the scoped band", c.len());
    }
}
