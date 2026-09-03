//! `git apply` — read a unified diff and apply it to the working tree.
//!
//! Unlike most modules here, `apply` has no gitoxide substrate to lean on: the
//! vendored crates ship a diff *producer* (`gix-diff`, `gix-imara-diff`) but no
//! patch *parser* or *applier*. The unified-diff parse and the hunk placement
//! search below are therefore a direct port of git's `apply.c` — specifically
//! `parse_fragment`, `find_pos` (the alternating backwards/forwards scan) and
//! `match_fragment`'s `match_beginning` / `match_end` constraints — so hunk
//! placement, offset tolerance and failure points land where stock git puts
//! them.
//!
//! Supported (output, exit code and resulting worktree match stock git):
//!   * `git apply <patch>...` / stdin (no operand, or `-`)
//!   * `-p<n>`, `-R`/`--reverse`, `--check`, `--numstat`, `--stat`, `--summary`,
//!     `-z`, `--apply`, `--allow-empty`, `--unidiff-zero`, `--no-add`,
//!     `--index`/`--cached` (stage the result into the index; `--cached` skips the
//!     worktree write — see below),
//!     `--exclude=<glob>`/`--include=<glob>` (path filtering via wildmatch),
//!     `-v`/`--verbose` (the `Checking patch`/`Applied patch`/`Hunk #<n> succeeded`
//!     progress on stderr), `--reject` (partial apply, `*.rej` files, exit 1),
//!     `-N`/`--intent-to-add` (see below), `-C<n>` (see below),
//!     `--unsafe-paths` (see below), `--inaccurate-eof` (see below),
//!     `--build-fake-ancestor=<file>` (see below),
//!     `--allow-overlap` (`state->allow_overlap`: the lines a fragment writes are no
//!     longer marked `LINE_PATCHED`, so a later fragment of the same patch may match
//!     against text the patch itself produced — apply.c:2650, :2969),
//!     `--binary`/`--allow-binary-replacement` (accepted, no-op as in modern
//!     git), `-v`/`-q` (git's `OPT__VERBOSITY` counter, so `-v -q` is silent and
//!     `-q -v` is verbose), `--whitespace=<action>` (every action but `fix` under a
//!     non-default `core.whitespace` — see below), `--recount`,
//!     `--directory=<root>` (normalised as `strbuf_normalize_path()` does, so a root
//!     that climbs above the worktree is a usage error before any patch is opened),
//!     `--`, and the `--no-` form of each of git's negatable options
//!   * `-3`/`--3way` and its `--ours`/`--theirs`/`--union` variants — see below
//!   * usage errors: unknown option/switch (git's own usage block on stderr,
//!     exit 129), a missing or non-integer option value, an unrecognised
//!     `--whitespace` action, `--ours`/`--theirs`/`--union` without `--3way`,
//!     `--reject` with `--3way`, and `--3way` outside a repository
//!     (`fatal:`/`error:`, exit 128)
//!   * patch kinds: modification, creation, deletion, rename, copy, mode change,
//!     and symlink blobs; git-style (`diff --git`) and traditional `---`/`+++` diffs
//!
//! Faithful to git on the write side: every patch is checked before any file is
//! touched (atomicity), `write_out_results()`'s two passes are kept — every removal
//! happens before any creation, so a swap-rename cannot clobber its own pre-image —
//! targets are removed and re-created rather than rewritten in place (so the
//! resulting mode is the patch's mode under the process umask, not the old file's),
//! leading directories are created for new paths, and directories emptied by a
//! deletion or rename are pruned.
//!
//! The report modes (`--stat`, `--numstat`, `--summary`) print *after* all of that,
//! where apply.c prints them (apply.c:4993): every earlier failure reaches them
//! through a `goto end` that skips them, so a patch that does not apply produces no
//! report at all. They also turn applying off unless `--apply` (git's `force_apply`)
//! says otherwise — including over `--reject`, which sets `state->apply` four lines
//! earlier and so loses to them. `--no-apply` is `OPT_BOOL` on that same
//! `force_apply`, which starts at zero, so on its own it changes nothing.
//!
//! Argument parsing covers git's whole `apply` option table, because git's own
//! ordering makes that observable: it finishes parsing, runs its usage-level
//! validations, *then* opens the patch files, *then* parses them. The one flag this
//! port cannot honour (`--whitespace=fix` under a non-default `core.whitespace`) is
//! therefore reported only once the input is known to contain at least one patch —
//! the first moment ignoring it could change a result. Until then git has not
//! consulted it either, so `git apply --stat missing-file` and `git apply --3way
//! not-a-patch` report what git reports (`can't open patch` / `No valid patches in
//! input`, exit 128) rather than a premature unsupported-flag error.
//!
//! `--index`/`--cached` update the index (`git apply`'s `update_index` path,
//! served natively through the vendored `gix-index` writer, so tools on PATH — and
//! `git am`, which re-execs `apply --index` — see the same staged state). Both read
//! each file's pre-image from the index blob, exactly as git does when
//! `check_index` is set (`load_patch_target`). After the shared apply engine
//! computes a file's new content, the new blob is written to the odb and the index
//! entry for that path is added (creation), removed (deletion), or replaced with
//! the new oid/mode (modification, rename — remove old path, add new path; a copy
//! leaves the source alone). The
//! whole index is written once, under the repo lock. `--index` additionally writes
//! the worktree (the engine's usual write) and, matching git's `verify_index_match`
//! gate, refuses (`does not match index`) when the worktree file's content differs
//! from the index blob. A file that is *missing* is checked out of the index instead
//! (`checkout_target`, apply.c:3485) — during the check pass, so `--check --index`
//! recreates it too — and the patch then applies to it. `--cached`
//! skips every worktree read, check, and write. `--reject` composes with both, as it
//! does in git: the pre-image still comes from the index, a cleanly applied patch is
//! still staged, and a run that rejected *anything* rolls the whole index update
//! back — `apply_patch()` returns -1 and `apply_all_patches()` never reaches
//! `write_locked_index()` (apply.c:5129, :5173) — while everything already written to
//! the worktree stays. One floor is left under these: the executable bit of a plain
//! modification whose diff carries no `index` mode line and whose pre-image is not in
//! the index.
//!
//! `-N`/`--intent-to-add` is git's `state->ita_only`. The worktree is written as
//! usual, but the index is touched *only* for the paths the patch creates, and only
//! with a placeholder: the empty blob, a zeroed stat, and `CE_INTENT_TO_ADD`
//! (`add_index_file`, apply.c:4443-4460, and `set_object_name_for_intent_to_add_entry`,
//! read-cache.c:704), so the addition still reads as unstaged. A deletion deliberately
//! leaves its entry standing (apply.c:4431), a modification stages nothing, and
//! `check_apply_state()` cancels the flag outright — silently, no error — when
//! `--index`/`--cached` is also given or when there is no repository (apply.c:178).
//! It works alongside `--reject`, where a file whose hunks were only partly rejected
//! is still created — but nothing is *staged* by a run that rejected anything, since
//! that run never reaches `write_locked_index()` at all.
//!
//! `-3`/`--3way` is `try_threeway()`: the patch is replayed onto the blob its
//! `index <old>..` line names, that post-image is merged into the current
//! contents with the named blob as the common ancestor, and only a pre-image the
//! object store cannot supply — or a patch that will not apply even to it — falls
//! back to placing hunks (`Falling back to direct application...`). It implies
//! `check_index` exactly as `check_apply_state()` does, so the result is staged;
//! a merge that does not resolve writes `ll_merge`'s conflict markers, stages the
//! base/ours/theirs trio, prints `U <path>`, and exits 1.
//! `--ours`/`--theirs`/`--union` are `state->merge_variant`. Not ported inside
//! this path: git's `direct_to_threeway`, the add/add case where a creation
//! collides with an existing file — a creation therefore takes the direct route
//! and reports what git reports without `--3way`.
//!
//! Path validation is [`check_unsafe_path`] (apply.c:4036), decided per patch during
//! the check and reported as `error: invalid path '<name>'` with exit 128 for the
//! whole run. It asks [`verify_path`] (read-cache.c:839), so it refuses more than an
//! escape from the worktree: an empty name, a leading or doubled or trailing `/`, and
//! any `.`, `..` or `.git` component — `.git/hooks/pre-commit` never leaves the
//! working tree and is refused all the same. `--unsafe-paths` waives the gate, and
//! `check_apply_state()` then cancels the flag again under `--index`/`--cached`
//! (and so under `--3way`), silently, at apply.c:180. Two things survive the waiver
//! because git puts them elsewhere: `check_preimage()` runs first, so a missing
//! out-of-tree file is reported as missing rather than as an invalid path, and
//! `add_index_entry()` re-checks every name on the way into the index
//! (read-cache.c:1287), which is where `-N --unsafe-paths` on an out-of-tree creation
//! ends — with the file written and the index update rolled back, as in git.
//!
//! Not reproduced inside `verify_path`: the `protect_hfs`/`protect_ntfs` arms, which
//! also refuse the Unicode- and 8.3-obfuscated spellings of `.git`
//! (`is_hfs_dotgit()`, `is_ntfs_dotgit()`). The Windows-only `has_dos_drive_prefix()`
//! and `is_valid_path()` gates are compiled out on POSIX anyway
//! (git-compat-util.h:224-229, :265).
//!
//! `--build-fake-ancestor=<file>` is `build_fake_ancestor()`: an index written to that
//! path holding, for every patch that is not a creation, the blob its `index <old>..`
//! line points at (resolved from however abbreviated a prefix git wrote), or — for a
//! patch that adds and deletes no line — the path's current index entry. A patch that
//! offers neither ends the run with `sha1 information is lacking or useless (<name>).`
//! Naming a fake ancestor turns applying off like the report modes do. The entries
//! carry a zeroed stat rather than the one `refresh_cache_entry()` fills in; nothing
//! that reads a fake ancestor consults it.
//!
//! `--inaccurate-eof` is the block at apply.c:3099-3106: when a hunk's pre- and
//! post-image both end in a newline, both lose it. The pre-image's last line then
//! matches a file that has no final newline — as a prefix, which is what git's
//! flat-buffer `memcmp` over the shortened length amounts to — and the post-image is
//! written without one, so the flag is observable even on a patch that would otherwise
//! apply cleanly.
//!
//! Not implemented — these `bail!` rather than produce plausible-looking wrong
//! results: copy patches and non-UTF-8 paths.
//!
//! One known divergence that is not about options. Several patch files on one command
//! line are read into a single buffer here, while `apply_all_patches()` (apply.c:5102)
//! runs `apply_patch()` once per file, so git's unit of atomicity is the *file*.
//! Measured consequences, all of them in the multiple-operand case only:
//!
//! * `git apply good.patch bad.patch` writes `good.patch`'s result in git and then
//!   fails; nothing is written here. Same for a second operand that cannot even be
//!   opened.
//! * `--stat`/`--numstat`/`--summary` print one report per patch file in git — so two
//!   operands produce two `N files changed` footers — and one combined report here.
//! * `--reject` interleaves `Checking patch …` / `Applied patch … cleanly.` per file
//!   in git, because each file runs its own check-then-write pass; here every check
//!   runs before every write.
//!
//! The index is *not* part of this: `apply_all_patches()` writes it once after the
//! last file (apply.c:5173), which is what this does too.
//!
//! Running below the worktree root behaves as git does. `setup_git_directory()`
//! leaves the command at the top of the worktree and hands it the invocation
//! directory as `prefix`, so [`worktree_prefix`] does both, and the prefix then
//! reaches the same three places apply.c uses it: the patch-file operands are
//! resolved through it, a traditional (non-`diff --git`) patch's names gain it
//! ([`prefix_patch`]), and [`use_patch`] drops every path that does not live
//! strictly below it — silently, exit 0, as git does.
//!
//! Binary patches are applied: the `GIT binary patch` payload is base85-decoded and
//! inflated, then either used whole (`literal`) or applied as a git delta to the
//! pre-image (`delta`, `patch_delta()`'s copy/insert opcodes). Both ends are verified
//! against the ids the `index` line names, so a payload meets the pre-image it was made
//! against or the patch is refused — which also means a patch without a full index line
//! is refused, as git refuses it. `apply_binary()`'s two shortcuts are here as well: a
//! null post-image id is a deletion and produces no content at all, and a post-image the
//! object store already holds is read straight out of it rather than rebuilt. `-R`
//! swaps the pair of ids and consumes the second (reverse) payload. `--reject` makes no
//! difference to any of it — a binary patch has no fragments to reject one at a time, so
//! it either lands whole or is rejected whole, with no `*.rej` file written for it.
//!
//! `--ignore-whitespace`/`--ignore-space-change` (both are the same flag in git,
//! `ws_ignore_action = ignore_ws_change`) relax the search: a hunk that does not
//! land byte for byte is retried with `fuzzy_matchlines()`, which compares the
//! lines with every whitespace run collapsed — a run may differ in width but may
//! not disappear, so `a b` still does not match `ab`, and line endings are ignored
//! on both sides. A hunk that only lands that way then goes through
//! `update_pre_post_images()`: every context line of the result is re-taken from
//! the file rather than the patch, so the file's own indentation survives and only
//! added lines come out of the patch.
//!
//! `-C<n>` reduces context the way `apply_one_fragment()` does, and — since `--reject`
//! is a mode of that same loop rather than a path of its own — it reduces context
//! there too. `state->p_context` is `UINT_MAX` by default, so nothing is reduced
//! unless the flag asks: a hunk that does not land as written first drops its
//! begin/end anchoring, then sheds one context line from whichever end has more of
//! them (both ends when they are equal) and is retried, down to the `<n>` floor. Each
//! leading line dropped also moves git's `pos` a line *back*, which is why the
//! `Hunk #<n> succeeded at <l> (offset <k> lines).` it prints can overstate the
//! distance, and why a `pos` that has gone negative restarts the search at the end of
//! the file (apply.c:2847-2853). `Context reduced to (<a>/<b>) to apply fragment at
//! <l>` follows it, and is printed without `-v`.
//!
//! `--whitespace=fix` is honoured for git's default rule set (`blank-at-eol`,
//! `blank-at-eof`, `space-before-tab`): the trailing run goes and the spaces in front of
//! a tab in the indent go. A repository whose `core.whitespace` asks for
//! `indent-with-non-tab` or `tab-in-indent` keeps the refusal — those reshape the indent
//! in ways this has not been verified against, and a guess would rewrite the user's
//! bytes.
//!
//! Whitespace errors are checked before anything is written, as `check_whitespace()`
//! does: every added line goes through `ws_check()` under `core.whitespace`, and the
//! first five offenders are reported as `<patch>:<line>: <error>.` followed by the
//! line. `warn` (the default) then applies anyway, `nowarn` says nothing at all —
//! `parse_fragment()` skips the check under it, so even the trailing summary is gone
//! (apply.c:1867-1869) — and `error`/`error-all` refuse the whole patch with
//! `error: <n> lines add whitespace errors.` and exit 128. A `whitespace` *attribute*
//! would refine the rule per path; only the config is read.
//!
//! Three details of that carry the observable weight:
//!
//! * **`patch->ws_rule` is per patch and evolves as the fragment is read.**
//!   `check_old_for_crlf()` (apply.c:1716) ORs in `WS_CR_AT_EOL` the moment a context
//!   or removed line ends `\r\n`, so a CRLF patch's added lines stop being
//!   trailing-whitespace errors — but only the ones *after* that line. Under `-R` the
//!   roles invert (apply.c:1855-1869), which [`Patch::reverse`] expresses by flipping
//!   the body markers.
//! * **`set_default_whitespace_mode()` (apply.c:193-197)** turns an unchosen action
//!   into `nowarn` whenever `state->apply` is off, so `--check`, `--stat`,
//!   `--numstat`, `--summary` and `--build-fake-ancestor` say nothing about
//!   whitespace at all unless `--whitespace=`/`apply.whitespace` asks them to.
//! * **The counts print last.** `squelched <n> whitespace errors` and the
//!   `<n> lines add whitespace errors.` line live in `apply_all_patches()`'s tail
//!   (apply.c:5141-5171), past both `write_out_results()` and the `end:` label — so
//!   under `--reject` they follow `Applied patch <name> cleanly.`, and a run whose
//!   patch did not apply prints neither.
//!
//! A fragment that `parse_fragment()` rejects — a header the `@@ -a,b +c,d @@`
//! grammar does not accept, a body that runs out before the header's counts are
//! satisfied, or a body of nothing but context (`!deleted && !added`, which
//! `--recount` exempts) — reproduces git's `error: corrupt patch at <file>:<line>`
//! and exit 128, with the line counted within the input file it came from.
//! One shape is still reported under that message rather than git's own:
//! `parse_single_patch()` only enters the fragment loop on a literal `@@ -`, so a
//! header line like `@@ bogus @@` leaves the patch with no fragments at all and
//! git falls through to `patch with only garbage at <file>:<line>` (the check
//! guarded by `state->apply || state->check` and `metadata_changes()`). The exit
//! code is the same 128; the wording is not.
//!
//! Config: `apply.whitespace` is read as the default `--whitespace` action, the
//! same as git — the command line overrides it. A `warn`/`nowarn` default is the
//! same no-op; a `fix` default is honoured for git's default rule set and refused
//! beyond it, exactly as the flag is; an invalid value there is fatal (128) at startup,
//! before the patch is opened and ahead of any `--whitespace` on the command line,
//! matching git's config parse order. `apply.ignoreWhitespace` is read straight after
//! it, as git does: `change` turns the relaxed match on, `no`/`false`/`never`/`none`
//! off, and any other value is the same startup fatal (`unrecognized whitespace
//! ignore option '<v>'`, 128).
//!
//! Two spellings are accepted here as no-ops where git kills itself: `--no-whitespace`
//! and `--no-directory`. Both are `OPT_CALLBACK` entries (apply.c:5253, :5277) whose
//! table entry is missing `PARSE_OPT_NONEG` while the callback opens with
//! `BUG_ON_OPT_NEG(unset)` (apply.c:5067, :5080) — so parse-options accepts the
//! negation, hands it to the callback, and git aborts on `BUG:` with SIGABRT and exit
//! 134. Measured; these are the *only* two of apply's thirty-odd `--no-` spellings
//! that do it. Reproducing a self-detected programmer error as a crash is not parity
//! in any useful sense, so the flags are ignored instead.
//!
//! `-q`/`--quiet` silences every `error:` diagnostic, matching git, where they
//! all go through `error()`; exit codes are unaffected, and `fatal:` messages and
//! usage errors are not silenced.

use anyhow::{bail, Result};
use gix::bstr::{BString, ByteSlice};
use gix::hash::ObjectId;
use gix::index::entry::{Flags, Mode as IndexMode, Stat};
use gix::merge::blob::builtin_driver::text::{
    Conflict as MergeConflict, Labels as MergeLabels, Level as MergeLevel, Merge as MergeText,
    Rendering as MergeRendering,
};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::{Arg, LongOpt};

/// `apply_parse_options()`'s `struct option builtin_apply_options[]` (apply.c:5202),
/// in table order, as [`super::resolve_long`] reads it.
///
/// `no-add` is an entry spelled with its own `no-`, which parse-options reads as the
/// *unset* sense of `add` — so `--add` and `--no-add` are the two senses of one
/// entry, not two options. `--exclude`/`--include` and the three
/// `--ours`/`--theirs`/`--union` conflict modes carry `PARSE_OPT_NONEG`.
const LONG_OPTS: &[LongOpt] = &[
    LongOpt { name: "exclude",                     neg: false, arg: Arg::Required },
    LongOpt { name: "include",                     neg: false, arg: Arg::Required },
    LongOpt { name: "no-add",                      neg: true,  arg: Arg::None },
    LongOpt { name: "stat",                        neg: true,  arg: Arg::None },
    LongOpt { name: "allow-binary-replacement",    neg: true,  arg: Arg::None },
    LongOpt { name: "binary",                      neg: true,  arg: Arg::None },
    LongOpt { name: "numstat",                     neg: true,  arg: Arg::None },
    LongOpt { name: "summary",                     neg: true,  arg: Arg::None },
    LongOpt { name: "check",                       neg: true,  arg: Arg::None },
    LongOpt { name: "index",                       neg: true,  arg: Arg::None },
    LongOpt { name: "intent-to-add",               neg: true,  arg: Arg::None },
    LongOpt { name: "cached",                      neg: true,  arg: Arg::None },
    LongOpt { name: "unsafe-paths",                neg: true,  arg: Arg::None },
    LongOpt { name: "apply",                       neg: true,  arg: Arg::None },
    LongOpt { name: "3way",                        neg: true,  arg: Arg::None },
    LongOpt { name: "ours",                        neg: false, arg: Arg::None },
    LongOpt { name: "theirs",                      neg: false, arg: Arg::None },
    LongOpt { name: "union",                       neg: false, arg: Arg::None },
    LongOpt { name: "build-fake-ancestor",         neg: true,  arg: Arg::Required },
    LongOpt { name: "whitespace",                  neg: true,  arg: Arg::Required },
    LongOpt { name: "ignore-space-change",         neg: true,  arg: Arg::None },
    LongOpt { name: "ignore-whitespace",           neg: true,  arg: Arg::None },
    LongOpt { name: "reverse",                     neg: true,  arg: Arg::None },
    LongOpt { name: "unidiff-zero",                neg: true,  arg: Arg::None },
    LongOpt { name: "reject",                      neg: true,  arg: Arg::None },
    LongOpt { name: "allow-overlap",               neg: true,  arg: Arg::None },
    LongOpt { name: "verbose",                     neg: true,  arg: Arg::None },
    LongOpt { name: "quiet",                       neg: true,  arg: Arg::None },
    LongOpt { name: "inaccurate-eof",              neg: true,  arg: Arg::None },
    LongOpt { name: "recount",                     neg: true,  arg: Arg::None },
    LongOpt { name: "directory",                   neg: true,  arg: Arg::Required },
    LongOpt { name: "allow-empty",                 neg: true,  arg: Arg::None },
];
/// git's `apply` usage block, printed after `unknown option`/`unknown switch` on
/// stderr with exit 129 (`parse-options`' `PARSE_OPT_ERROR`).
pub(super) const USAGE: &str = r"usage: git apply [<options>] [<patch>...]

    --exclude <path>      don't apply changes matching the given path
    --include <path>      apply changes matching the given path
    -p <num>              remove <num> leading slashes from traditional diff paths
    --no-add              ignore additions made by the patch
    --add                 opposite of --no-add
    --[no-]stat           instead of applying the patch, output diffstat for the input
    --[no-]numstat        show number of added and deleted lines in decimal notation
    --[no-]summary        instead of applying the patch, output a summary for the input
    --[no-]check          instead of applying the patch, see if the patch is applicable
    --[no-]index          make sure the patch is applicable to the current index
    -N, --[no-]intent-to-add
                          mark new files with `git add --intent-to-add`
    --[no-]cached         apply a patch without touching the working tree
    --[no-]unsafe-paths   accept a patch that touches outside the working area
    --[no-]apply          also apply the patch (use with --stat/--summary/--check)
    -3, --[no-]3way       attempt three-way merge, fall back on normal patch if that fails
    --ours                for conflicts, use our version
    --theirs              for conflicts, use their version
    --union               for conflicts, use a union version
    --[no-]build-fake-ancestor <file>
                          build a temporary index based on embedded index information
    -z                    paths are separated with NUL character
    -C <n>                ensure at least <n> lines of context match
    --[no-]whitespace <action>
                          detect new or modified lines that have whitespace errors
    --[no-]ignore-space-change
                          ignore changes in whitespace when finding context
    --[no-]ignore-whitespace
                          ignore changes in whitespace when finding context
    -R, --[no-]reverse    apply the patch in reverse
    --[no-]unidiff-zero   don't expect at least one line of context
    --[no-]reject         leave the rejected hunks in corresponding *.rej files
    --[no-]allow-overlap  allow overlapping hunks
    -v, --[no-]verbose    be more verbose
    -q, --[no-]quiet      be more quiet
    --[no-]inaccurate-eof tolerate incorrectly detected missing new-line at the end of file
    --[no-]recount        do not trust the line counts in the hunk headers
    --[no-]directory <root>
                          prepend <root> to all filenames
    --[no-]allow-empty    don't return error for empty patches

";

/// `usage_with_options_internal()`'s `USAGE_FULL` rendering — what `--help-all`
/// prints. It is [`USAGE`] with the `PARSE_OPT_HIDDEN` entries left in:
/// `--[no-]allow-binary-replacement`, `--[no-]binary`.
/// Captured byte-for-byte from stock git 2.55.0's `git apply --help-all`.
pub(super) const USAGE_ALL: &str = r#"usage: git apply [<options>] [<patch>...]

    --exclude <path>      don't apply changes matching the given path
    --include <path>      apply changes matching the given path
    -p <num>              remove <num> leading slashes from traditional diff paths
    --no-add              ignore additions made by the patch
    --add                 opposite of --no-add
    --[no-]stat           instead of applying the patch, output diffstat for the input
    --[no-]allow-binary-replacement
                          no-op (backward compatibility)
    --[no-]binary         no-op (backward compatibility)
    --[no-]numstat        show number of added and deleted lines in decimal notation
    --[no-]summary        instead of applying the patch, output a summary for the input
    --[no-]check          instead of applying the patch, see if the patch is applicable
    --[no-]index          make sure the patch is applicable to the current index
    -N, --[no-]intent-to-add
                          mark new files with `git add --intent-to-add`
    --[no-]cached         apply a patch without touching the working tree
    --[no-]unsafe-paths   accept a patch that touches outside the working area
    --[no-]apply          also apply the patch (use with --stat/--summary/--check)
    -3, --[no-]3way       attempt three-way merge, fall back on normal patch if that fails
    --ours                for conflicts, use our version
    --theirs              for conflicts, use their version
    --union               for conflicts, use a union version
    --[no-]build-fake-ancestor <file>
                          build a temporary index based on embedded index information
    -z                    paths are separated with NUL character
    -C <n>                ensure at least <n> lines of context match
    --[no-]whitespace <action>
                          detect new or modified lines that have whitespace errors
    --[no-]ignore-space-change
                          ignore changes in whitespace when finding context
    --[no-]ignore-whitespace
                          ignore changes in whitespace when finding context
    -R, --[no-]reverse    apply the patch in reverse
    --[no-]unidiff-zero   don't expect at least one line of context
    --[no-]reject         leave the rejected hunks in corresponding *.rej files
    --[no-]allow-overlap  allow overlapping hunks
    -v, --[no-]verbose    be more verbose
    -q, --[no-]quiet      be more quiet
    --[no-]inaccurate-eof tolerate incorrectly detected missing new-line at the end of file
    --[no-]recount        do not trust the line counts in the hunk headers
    --[no-]directory <root>
                          prepend <root> to all filenames
    --[no-]allow-empty    don't return error for empty patches

"#;

// The reason quoted back by the one flag left that this port cannot honour.
const R_WS: &str = "whitespace fixing is not implemented";

/// How a `--whitespace`/`apply.whitespace` action classifies against the set
/// git's `parse_whitespace_option` accepts.
enum WsAction {
    /// `nowarn`: the check runs but says nothing.
    Silent,
    /// `warn`: report each offending added line, then apply anyway.
    Warn,
    /// `error`/`error-all`: report, then refuse the whole patch (exit 128).
    Error,
    /// `fix`/`strip`: rewrite the offending lines before applying them.
    Fix,
    /// Anything else: git rejects it as an unrecognized whitespace option.
    Invalid,
}

/// Classify a whitespace action string exactly as git's `parse_whitespace_option`
/// does (used for both the `--whitespace` flag and the `apply.whitespace` config).
fn classify_whitespace(v: &str) -> WsAction {
    match v {
        "warn" => WsAction::Warn,
        "nowarn" => WsAction::Silent,
        "error" | "error-all" => WsAction::Error,
        "fix" | "strip" => WsAction::Fix,
        _ => WsAction::Invalid,
    }
}

/// Parsed command-line options for a single `apply` invocation. Only the flags
/// this port honours get a field; the rest live in the `Unhonoured` list.
struct Opts {
    /// `--whitespace=<action>` / `apply.whitespace`, for the actions this port runs
    /// itself. `fix`/`strip` never reach here — they are deferred as unsupported.
    ws: WsAction,
    /// `state->whitespace_option || apply_default_whitespace` — whether the action
    /// above was *chosen* rather than defaulted. `set_default_whitespace_mode()`
    /// (apply.c:193-197) only reaches an unchosen one.
    ws_given: bool,
    strip: usize,               // -p<n>: leading path components to drop (default 1)
    /// `state->p_value_known`: `-p<n>` appeared, so a traditional patch may not
    /// infer its own value through `guess_p_value()`.
    strip_explicit: bool,
    /// `-C<n>`: the fewest context lines a hunk may be reduced to when it does not
    /// apply as written. `None` is git's default of keeping every context line.
    p_context: Option<usize>,
    reverse: bool,              // -R/--reverse: swap pre- and post-image
    /// `state->allow_overlap` (apply.c:5268): stop marking the lines a fragment wrote
    /// with `LINE_PATCHED`, so a later fragment of the same patch may match text this
    /// patch itself produced.
    allow_overlap: bool,
    check: bool,                // --check: validate only, never write
    numstat: bool,              // --numstat: machine-readable added/deleted counts
    stat: bool,                 // --stat: git's scaled diffstat graph
    summary: bool,              // --summary: create/delete/rename/mode-change lines
    nul: bool,                  // -z: NUL-terminate --numstat records
    unidiff_zero: bool,         // --unidiff-zero: relax the begin/end anchoring
    /// `state->ws_ignore_action == ignore_ws_change`, set by
    /// `--ignore-whitespace`/`--ignore-space-change` and `apply.ignoreWhitespace`:
    /// context is matched with `fuzzy_matchlines()` instead of byte equality.
    ignore_ws: bool,
    allow_empty: bool,          // --allow-empty: an input with no patches is not an error
    no_add: bool,               // --no-add: apply context/deletions, drop additions
    /// `state->apply_verbosity`, an `OPT__VERBOSITY` counter: `-v` raises it, `-q`
    /// lowers it, each flipping the sign when it crosses zero, and `--no-verbose`
    /// /`--no-quiet` reset it. `> 0` is `verbosity_verbose` (the `Checking patch`
    /// /`Applied patch`/`Hunk #<n> succeeded` progress); `< 0` is
    /// `verbosity_silent`, which mutes every `error:` and `warning:`.
    verbosity: i32,
    reject: bool,               // --reject: apply what fits, write *.rej for the rest
    recount: bool,              // --recount: derive hunk sizes from the body, not the header
    index: bool,                // --index: apply to the worktree AND the index
    cached: bool,               // --cached: apply to the index only (no worktree touch)
    /// `-N`/`--intent-to-add`: `state->ita_only`. A patch that creates a file also
    /// records an intent-to-add index entry for it (empty-blob placeholder,
    /// `CE_INTENT_TO_ADD`); nothing else in the index is touched, and in particular
    /// a deletion leaves its entry standing (apply.c:4431). `check_apply_state()`
    /// drops the flag entirely when `--index`/`--cached` is also given or when
    /// there is no repository (apply.c:178).
    ita_only: bool,
    /// `--inaccurate-eof`: `patch->inaccurate_eof`, which lets a hunk whose last
    /// line ends in a newline match a file that does not end in one.
    inaccurate_eof: bool,
    /// `--build-fake-ancestor=<file>`: write a temporary index naming, for every
    /// path the patch does not create, the pre-image blob its `index <old>..` line
    /// points at. Like the report modes it turns applying off unless `--apply`
    /// says otherwise (apply.c:169).
    fake_ancestor: Option<String>,
    /// `--unsafe-paths`: waive [`check_unsafe_path`], the per-patch gate that
    /// refuses a path `verify_path()` calls invalid — one that leaves the working
    /// tree, names `.git`, or is otherwise unfit for the index. `check_apply_state()`
    /// clears it again under `--index`/`--cached`/`--3way` (apply.c:180).
    unsafe_paths: bool,
    /// `--directory=<root>`: prepend `<root>` to every path. Stored as
    /// `state->root` is — already run through `strbuf_normalize_path()` and
    /// completed with a trailing `/` — so an empty root means "no root".
    directory: Option<String>,
    limits: Vec<(bool, String)>, // --include/--exclude rules in argv order (true = include)
    has_include: bool,          // whether any rule is an --include
    apply_override: Option<bool>, // --apply / --no-apply
    apply: bool,                // whether the patch is actually applied
    three_way: bool,            // -3/--3way: merge the patch in rather than place its hunks
    /// `--ours`/`--theirs`/`--union`: `state->merge_variant`, which resolves a
    /// 3-way conflict to one side instead of writing conflict markers.
    merge_variant: Option<MergeVariant>,
}

impl Opts {
    /// `apply_verbosity > verbosity_normal`.
    fn verbose(&self) -> bool {
        self.verbosity > 0
    }

    /// `apply_verbosity <= verbosity_silent`, which is where git swaps in its muting
    /// `error()`/`warning()` routines.
    fn quiet(&self) -> bool {
        self.verbosity < 0
    }
}

/// git's `XDL_MERGE_FAVOR_*`, the three ways `--3way` can silence a conflict.
#[derive(Clone, Copy)]
enum MergeVariant {
    Ours,
    Theirs,
    Union,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            // git's default is `warn`.
            ws: WsAction::Warn,
            ws_given: false,
            p_context: None,
            strip: 1,
            strip_explicit: false,
            reverse: false,
            allow_overlap: false,
            check: false,
            numstat: false,
            stat: false,
            summary: false,
            nul: false,
            unidiff_zero: false,
            ignore_ws: false,
            allow_empty: false,
            no_add: false,
            verbosity: 0,
            reject: false,
            recount: false,
            index: false,
            cached: false,
            ita_only: false,
            inaccurate_eof: false,
            fake_ancestor: None,
            unsafe_paths: false,
            directory: None,
            limits: Vec::new(),
            has_include: false,
            apply_override: None,
            apply: true,
            three_way: false,
            merge_variant: None,
        }
    }
}

/// `parse_opt_verbosity_cb()`: `Some(1)` is `-v`, `Some(-1)` is `-q`, and `None` is
/// the `--no-` form of either. Each direction flips the counter's sign when it is
/// currently on the other side of zero, so `-v -q` is silent and `-q -v` is verbose.
fn bump_verbosity(target: &mut i32, dir: Option<i32>) {
    match dir {
        None => *target = 0,
        Some(1) if *target >= 0 => *target += 1,
        Some(1) => *target = 1,
        Some(_) if *target <= 0 => *target -= 1,
        Some(_) => *target = -1,
    }
}

/// `error:` diagnostics, which `-q` silences in git.
fn err(quiet: bool, msg: &str) {
    if !quiet {
        eprintln!("{msg}");
    }
}

/// Fetch the value of a long option, from `--name=value` or the following argv
/// entry.
fn long_value(
    args: &[String],
    i: &mut usize,
    name: &str,
    inline: Option<&str>,
) -> Result<String, ExitCode> {
    if let Some(v) = inline {
        return Ok(v.to_owned());
    }
    match args.get(*i) {
        Some(v) => {
            *i += 1;
            Ok(v.clone())
        }
        None => {
            eprintln!("error: option `{name}' requires a value");
            Err(ExitCode::from(129))
        }
    }
}

/// Parse the whole option table. Diagnostics are printed here; the returned
/// `ExitCode` is git's for that failure (129 for usage errors, 128 for the two
/// `fatal:` paths).
fn parse_opts(
    args: &[String],
    o: &mut Opts,
    sources: &mut Vec<String>,
) -> Result<(), ExitCode> {
    let mut conflict_given = false;
    let mut no_more_opts = false;
    let mut i = 0;

    while i < args.len() {
        let typed = args[i].clone();
        i += 1;

        if no_more_opts || typed == "-" || !typed.starts_with('-') {
            sources.push(typed);
            continue;
        }
        if typed == "--" {
            no_more_opts = true;
            continue;
        }

        // `if (internal_help && !strcmp(arg + 2, "help-all"))`
        // (parse-options.c:1122): tested on the token as typed, after the `--`
        // break above and ahead of the abbreviation resolver, because it is a
        // `strcmp` — `--help-a` and `--help-all=x` stay unknown options. It
        // renders `USAGE_FULL`, which for `apply` keeps the two hidden no-ops.
        if typed == "--help-all" {
            return Err(super::show_usage(USAGE_ALL));
        }

        // Respell a unique abbreviation as the name it resolves to, so `--unidiff`
        // reaches the same arm as `--unidiff-zero` — including the arms that record
        // an option as unhonoured. Short bundles pass through untouched.
        let a = match super::canonical_long(&typed, LONG_OPTS) {
            super::Long::Name(name) => name.into_owned(),
            super::Long::Ambiguous(first, second) => {
                return Err(super::ambiguous_option(&typed, &first, &second, USAGE))
            }
        };

        if let Some(long) = a.strip_prefix("--") {
            let (given, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            // `--no-add` is an option in its own right, not the negation of
            // `--add`, so it must not be split here.
            let (name, neg) = match given.strip_prefix("no-") {
                Some(rest) if given != "no-add" => (rest, true),
                _ => (given, false),
            };

            // `get_value()` (parse-options.c:112): an `=<value>` glued to an entry
            // declared without one is rejected before the entry's own handler runs.
            // The name in the message is the one that was *typed*, so `--no-stat=40`
            // reports `no-stat`.
            if inline.is_some()
                && LONG_OPTS
                    .iter()
                    .any(|o| o.name == name && matches!(o.arg, Arg::None))
            {
                eprintln!("error: option `{given}' takes no value");
                return Err(ExitCode::from(129));
            }

            match name {
                // ---- honoured ----
                "numstat" => o.numstat = !neg,
                "stat" => o.stat = !neg,
                "summary" => o.summary = !neg,
                "check" => o.check = !neg,
                "reverse" => o.reverse = !neg,
                "unidiff-zero" => o.unidiff_zero = !neg,
                "allow-empty" => o.allow_empty = !neg,
                // `parse_opt_verbosity_cb()` (parse-options-cb.c): the negated form
                // of either spelling resets the counter to `verbosity_normal`.
                "quiet" => bump_verbosity(&mut o.verbosity, if neg { None } else { Some(-1) }),
                "verbose" => bump_verbosity(&mut o.verbosity, if neg { None } else { Some(1) }),
                "reject" => o.reject = !neg,
                "recount" => o.recount = !neg,
                "apply" => o.apply_override = Some(!neg),
                "allow-overlap" => o.allow_overlap = !neg,
                // `OPT_BOOL_F(..., PARSE_OPT_NOCOMPLETE)` (apply.c:5230): hidden from
                // completion, but an ordinary boolean, so `--no-unsafe-paths` resolves
                // and turns it back off.
                "unsafe-paths" => o.unsafe_paths = !neg,
                // `apply_option_parse_directory()` (apply.c:5075) normalises the root
                // as it is parsed and fails the whole invocation when it cannot,
                // before any patch file is opened.
                //
                // `--no-directory` is the second of apply's two `BUG_ON_OPT_NEG`
                // spellings (apply.c:5080, table entry at :5277 missing
                // `PARSE_OPT_NONEG`): stock aborts with `BUG: apply.c:5080: option
                // callback does not expect negation` and exit 134. Clearing the root
                // is the reading a user typing it would want, and is what this does.
                "directory" => {
                    if neg {
                        o.directory = None;
                    } else {
                        let v = long_value(args, &mut i, name, inline)?;
                        let Some(mut root) = normalize_path(&v) else {
                            eprintln!("error: unable to normalize directory: '{v}'");
                            return Err(ExitCode::from(129));
                        };
                        // `strbuf_complete(&state->root, '/')`: a non-empty root always
                        // ends in a separator, and an empty one stays empty.
                        if !root.is_empty() && !root.ends_with('/') {
                            root.push('/');
                        }
                        o.directory = Some(root);
                    }
                }
                // `--no-whitespace` resolves in git's parse-options but then trips
                // `BUG_ON_OPT_NEG` in `apply_option_parse_whitespace()` (apply.c:5067),
                // aborting with SIGABRT and exit 134. Accepting it as a no-op is the
                // one thing here that is deliberately not what git does — reproducing
                // an abort would be worse than ignoring the flag.
                "whitespace" if neg => {}
                "whitespace" => {
                    let v = long_value(args, &mut i, name, inline)?;
                    match classify_whitespace(&v) {
                        action @ (WsAction::Silent
                        | WsAction::Warn
                        | WsAction::Error
                        | WsAction::Fix) => {
                            o.ws = action;
                            o.ws_given = true;
                        }
                        WsAction::Invalid => {
                            eprintln!("error: unrecognized whitespace option '{v}'");
                            return Err(ExitCode::from(129));
                        }
                    }
                }
                // Hidden legacy spellings, both `OPT_NOOP_NOARG` (apply.c:5216-5217)
                // — `parse_opt_noop_cb` does nothing in either sense, and neither
                // entry carries `PARSE_OPT_NONEG`, so `--no-binary` and
                // `--no-allow-binary-replacement` resolve and are no-ops too.
                "binary" | "allow-binary-replacement" => {}
                // `--add` is the default; it cancels a preceding `--no-add`.
                "add" if !neg => o.no_add = false,
                "no-add" if !neg => o.no_add = true,
                "exclude" | "include" if !neg => {
                    let pat = long_value(args, &mut i, name, inline)?;
                    o.limits.push((name == "include", pat));
                    if name == "include" {
                        o.has_include = true;
                    }
                }

                // ---- parsed, validated, reported before they could matter ----
                // `state->merge_variant`, handed to `ll_merge()` so a conflict
                // resolves to one side instead of being marked up.
                "ours" | "theirs" | "union" if !neg => {
                    conflict_given = true;
                    o.merge_variant = Some(match name {
                        "ours" => MergeVariant::Ours,
                        "theirs" => MergeVariant::Theirs,
                        _ => MergeVariant::Union,
                    });
                }
                "3way" => o.three_way = !neg,
                // --index (worktree + index) and --cached (index only) are honoured.
                "index" => o.index = !neg,
                "cached" => o.cached = !neg,
                "intent-to-add" => o.ita_only = !neg,
                "inaccurate-eof" => o.inaccurate_eof = !neg,
                // Both spellings run `apply_option_parse_space_change()`
                // (apply.c:5048), which is a plain on/off for `ws_ignore_action`.
                "ignore-space-change" | "ignore-whitespace" => o.ignore_ws = !neg,
                // `OPT_FILENAME` (apply.c:5246); the `--no-` form clears it.
                "build-fake-ancestor" => {
                    o.fake_ancestor = if neg {
                        None
                    } else {
                        Some(long_value(args, &mut i, name, inline)?)
                    }
                }

                // `given`, not `name`: git names the option as it was written.
                _ => {
                    eprintln!("error: unknown option `{long}'");
                    eprint!("{USAGE}");
                    return Err(ExitCode::from(129));
                }
            }
            continue;
        }

        // Short options, which cluster (`-qR`) and may carry their value glued on
        // (`-p2`) or as the next argv entry (`-p 2`).
        let chars: Vec<char> = a[1..].chars().collect();
        let mut k = 0;
        while k < chars.len() {
            let c = chars[k];
            k += 1;
            match c {
                'p' | 'C' => {
                    let glued: String = chars[k..].iter().collect();
                    k = chars.len();
                    let v = if !glued.is_empty() {
                        glued
                    } else {
                        match args.get(i) {
                            Some(v) => {
                                i += 1;
                                v.clone()
                            }
                            None => {
                                eprintln!("error: switch `{c}' requires a value");
                                return Err(ExitCode::from(129));
                            }
                        }
                    };
                    if c == 'p' {
                        // git parses -p itself, so its rejection is `fatal:`/128,
                        // not parse-options' `error:`/129.
                        match v.parse::<usize>() {
                            Ok(n) => {
                                o.strip = n;
                                // `state->p_value_known`: an explicit `-p` stops
                                // `guess_p_value()` overriding it.
                                o.strip_explicit = true;
                            }
                            Err(_) => {
                                eprintln!(
                                    "fatal: option -p expects a non-negative integer, got '{v}'"
                                );
                                return Err(ExitCode::from(128));
                            }
                        }
                    } else {
                        // `-C` is `OPT_UNSIGNED` over an `unsigned int`, so its
                        // range clause reads `[0,4294967295]` and `0x10`/`1k`
                        // are values it accepts.
                        match crate::optint::unsigned_prec(&crate::optint::short_opt('C'), &v, 4) {
                            Ok(n) => o.p_context = Some(n as usize),
                            Err(e) => {
                                eprintln!("error: {e}");
                                return Err(ExitCode::from(129));
                            }
                        }
                    }
                }
                'z' => o.nul = true,
                'R' => o.reverse = true,
                'q' => bump_verbosity(&mut o.verbosity, Some(-1)),
                'v' => bump_verbosity(&mut o.verbosity, Some(1)),
                'N' => o.ita_only = true,
                '3' => o.three_way = true,
                // parse_options_step()'s `internal_help` check sits inside the
                // short-option loop: `-h` answers on stdout at 129, without the
                // `error:` line that precedes a rejection's copy of the block.
                'h' => return Err(super::show_usage(USAGE)),
                _ => {
                    eprintln!("error: unknown switch `{c}'");
                    eprint!("{USAGE}");
                    return Err(ExitCode::from(129));
                }
            }
        }
    }

    // git's one post-parse usage check, run before it opens any patch file.
    if conflict_given && !o.three_way {
        eprintln!("fatal: --ours, --theirs, and --union require --3way");
        return Err(ExitCode::from(128));
    }

    // `check_apply_state()` (apply.c:169): `state->apply` starts at 1 and any
    // report mode turns it off — unless `--apply` was given, which is git's
    // `force_apply` and outranks them. `--reject` does *not* survive that: it sets
    // `state->apply` at apply.c:165, four lines before the report modes clear it
    // again, so `git apply --stat --reject` neither applies nor rejects anything.
    // `--no-apply` is `OPT_BOOL` on the same `force_apply`, so it only cancels an
    // earlier `--apply`; on its own it leaves the default alone.
    o.apply = o.apply_override.unwrap_or(false)
        || !(o.check || o.numstat || o.stat || o.summary || o.fake_ancestor.is_some());

    // `set_default_whitespace_mode()` (apply.c:193-197), which `apply_all_patches()`
    // calls once per operand before `apply_patch()` (apply.c:5125):
    //
    //     if (!state->whitespace_option && !apply_default_whitespace)
    //             state->ws_error_action = (state->apply ? warn_on_ws_error
    //                                                    : nowarn_ws_error);
    //
    // So a run that is not going to *write* anything says nothing about whitespace
    // either: `--check`, `--stat`, `--numstat`, `--summary` and
    // `--build-fake-ancestor` all clear `state->apply`, and the unchosen action then
    // becomes `nowarn`. Measured against stock — `git apply --check` on a patch with
    // eight trailing-whitespace lines prints not one of them. An explicit
    // `--whitespace=warn` (or `apply.whitespace`) opts back in.
    if !o.ws_given {
        o.ws = if o.apply { WsAction::Warn } else { WsAction::Silent };
    }
    Ok(())
}

pub fn apply(args: &[String]) -> Result<ExitCode> {
    let mut o = Opts::default();
    let mut sources: Vec<String> = Vec::new();

    // git reads `apply.whitespace` from config as the default `--whitespace`
    // action, before it parses arguments. An invalid value there is fatal (128)
    // immediately — before the patch input is even opened, and regardless of a
    // valid `--whitespace` on the command line, which git parses only afterward.
    // Every action it can name is honoured here on the same terms as the flag, so a
    // `--whitespace` on the command line simply replaces it.
    if let Ok(repo) = crate::setup::discover() {
        if let Some(v) = repo.config_snapshot().string("apply.whitespace") {
            let v = v.to_str_lossy();
            match classify_whitespace(&v) {
                action @ (WsAction::Silent | WsAction::Warn | WsAction::Error | WsAction::Fix) => {
                    o.ws = action;
                    o.ws_given = true;
                }
                WsAction::Invalid => {
                    eprintln!("error: unrecognized whitespace option '{v}'");
                    return Ok(ExitCode::from(128));
                }
            }
        }
        // `apply.ignorewhitespace`, read straight after it (apply.c:132) and just
        // as fatal when the value is neither the off-spelling nor `change`.
        if let Some(v) = repo.config_snapshot().string("apply.ignorewhitespace") {
            let v = v.to_str_lossy();
            match v.as_ref() {
                "no" | "false" | "never" | "none" => o.ignore_ws = false,
                "change" => o.ignore_ws = true,
                _ => {
                    eprintln!("error: unrecognized whitespace ignore option '{v}'");
                    return Ok(ExitCode::from(128));
                }
            }
        }
    }

    if let Err(code) = parse_opts(args, &mut o, &mut sources) {
        return Ok(code);
    }

    // `check_apply_state()`, in its own order: the `--reject`/`--3way` clash
    // first, then `--3way`'s repository requirement — which also turns
    // `check_index` on, since the merge base comes out of the object store — and
    // only then the same requirement for `--index`/`--cached`.
    if o.reject && o.three_way {
        eprintln!("error: options '--reject' and '--3way' cannot be used together");
        return Ok(ExitCode::from(128));
    }
    if o.three_way && crate::setup::discover().is_err() {
        eprintln!("error: '--3way' outside a repository");
        return Ok(ExitCode::from(128));
    }
    let check_index = o.index || o.cached || o.three_way;
    if check_index && crate::setup::discover().is_err() {
        let flag = if o.index { "--index" } else { "--cached" };
        eprintln!("error: '{flag}' outside a repository");
        return Ok(ExitCode::from(128));
    }
    // apply.c:178 — `-N` is dropped, silently and without an error, when the far
    // stronger `--index`/`--cached` is also in play or when there is no index to
    // record the intent in. Everything downstream then behaves as if it was never
    // given, which is why `git apply -N --index` stages the real blob.
    // apply.c:164-168 — `--reject` implies applying, and raises a *normal* run to
    // verbose. A `-q` run is already below normal, so it stays silent.
    if o.reject {
        if o.verbosity == 0 {
            o.verbosity = 1;
        }
    }
    if o.ita_only && (check_index || crate::setup::discover().is_err()) {
        o.ita_only = false;
    }
    // apply.c:180-181 — the last line of `check_apply_state()`: reading pre-images
    // from the index and writing results back into it only makes sense for paths the
    // index can hold, so `--index`/`--cached` (and `--3way`, which implies
    // `check_index`) cancel `--unsafe-paths` outright. Silently: git prints nothing,
    // and the patch then fails on whichever check it reaches first.
    if check_index {
        o.unsafe_paths = false;
    }

    // `setup_git_directory()` leaves every `RUN_SETUP` builtin standing at the top
    // of the worktree and hands it the directory it was invoked from as `prefix`
    // (with a trailing slash). apply.c then uses that prefix in three places, all
    // reproduced below: the patch-file operands are resolved through it
    // (apply.c's `prefix_filename(state->prefix, arg)` in `apply_all_patches`),
    // a traditional patch's names are prefixed by it ([`prefix_patch`]), and
    // `use_patch()` drops every path that does not live under it.
    let prefix = worktree_prefix()?;
    // `state->root`: already normalised and slash-completed at parse time, exactly
    // as `apply_option_parse_directory()` leaves it.
    let apply_root = o.directory.clone().unwrap_or_default();
    if !prefix.is_empty() {
        for src in &mut sources {
            if src != "-" && !std::path::Path::new(src.as_str()).is_absolute() {
                *src = format!("{prefix}{src}");
            }
        }
    }

    // ---- read the patch text ------------------------------------------------
    let mut buf: Vec<u8> = Vec::new();
    // Where each input's first line lands in `buf`, so a parse error can name
    // the file and the line within it the way `state->patch_input_file` and a
    // per-file `state->linenr` do.
    let mut spans: Vec<(String, usize)> = Vec::new();
    if sources.is_empty() {
        spans.push(("<stdin>".to_string(), 0));
        std::io::stdin().read_to_end(&mut buf)?;
    } else {
        for src in &sources {
            let first_line = buf.iter().filter(|&&b| b == b'\n').count();
            if src == "-" {
                spans.push(("<stdin>".to_string(), first_line));
                std::io::stdin().read_to_end(&mut buf)?;
                continue;
            }
            match std::fs::read(src) {
                Ok(b) => {
                    spans.push((src.clone(), first_line));
                    buf.extend_from_slice(&b);
                }
                Err(e) => {
                    err(
                        o.quiet(),
                        &format!("error: can't open patch '{src}': {}", io_msg(&e)),
                    );
                    return Ok(ExitCode::from(128));
                }
            }
        }
    }

    let spans = InputSpans { spans };
    let mut patches = match parse_patches(
        &split_lines(&buf),
        o.strip,
        o.strip_explicit,
        &prefix,
        &apply_root,
        o.recount,
        &spans,
    ) {
        Ok(p) => p,
        // apply.c reports a corrupt fragment through `error()` and unwinds to
        // `git apply`'s exit 128, rather than dying with the crate's usual
        // `zvcs: apply: …` prefix and exit 1.
        Err(e) => {
            let e = match e.downcast::<CorruptPatch>() {
                Ok(corrupt) => {
                    err(o.quiet(), &format!("error: {corrupt}"));
                    return Ok(ExitCode::from(128));
                }
                Err(e) => e,
            };
            let header = e.downcast::<HeaderError>()?;
            err(o.quiet(), &format!("error: {header}"));
            return Ok(ExitCode::from(128));
        }
    };
    if patches.is_empty() {
        if o.allow_empty {
            return Ok(ExitCode::SUCCESS);
        }
        err(
            o.quiet(),
            "error: No valid patches in input (allow with \"--allow-empty\")",
        );
        return Ok(ExitCode::from(128));
    }

    if let Some(root) = &o.directory {
        for p in &mut patches {
            prefix_names(p, root);
        }
    }
    // `prefix_patch()` (apply.c:2191), which `parse_chunk()` runs on every patch as
    // it is parsed: a traditional diff's names were written relative to the
    // invocation directory, so they gain the prefix. A `diff --git` patch is already
    // root-relative and is left alone.
    if !prefix.is_empty() {
        for p in &mut patches {
            prefix_patch(p, &prefix);
        }
    }
    if o.reverse {
        for p in &mut patches {
            p.reverse();
        }
    }

    // --include/--exclude and the invocation prefix: keep only the patches whose
    // (post-strip, post-prefix) name the rule list admits (git's `use_patch`). An
    // empty result is not an error — the input still held valid patches.
    if !o.limits.is_empty() || !prefix.is_empty() {
        patches.retain(|p| use_patch(p, &prefix, &o.limits, o.has_include));
    }

    // `state->whitespace_error`, which `apply_all_patches()` summarises only once
    // every input file has been through `apply_patch()` (apply.c:5141-5171). Carried
    // out here so the summary can print where git prints it: *after* the write, and
    // not at all when the run failed (a `goto end` jumps clean over the block).
    let mut ws_errors = 0usize;
    // `state->applied_after_fixing_ws`: how many lines `ws_fix_copy()` reported as
    // *fixed*, which is what picks the summary's first wording.
    let mut applied_after_fixing_ws = 0usize;

    // `check_whitespace()`: every added line is checked before anything is written,
    // so `--whitespace=error` refuses the patch with the worktree untouched. The rule
    // comes from `core.whitespace`; a `whitespace` attribute would refine it per path,
    // which this pass does not read.
    if !patches.is_empty() && !matches!(o.ws, WsAction::Invalid) {
        let rule = crate::setup::discover()
            .map(|repo| super::diff_color::whitespace_rule_cfg(&repo))
            .unwrap_or(super::diff_color::WS_DEFAULT_RULE);
        // `--whitespace=fix` reports the offending lines exactly as `warn` does, then
        // rewrites them. Only the default rule set is reproduced byte-for-byte, so any
        // other one keeps the honest refusal.
        if matches!(o.ws, WsAction::Fix) && !ws_fix_supported(rule) {
            bail!(
                "unsupported flag \"--whitespace=fix\": {R_WS} for a non-default \
                 core.whitespace"
            );
        }
        let errors = report_whitespace(&patches, &spans, rule, &o.ws, o.quiet());
        if matches!(o.ws, WsAction::Fix) {
            for p in &mut patches {
                let targets = ws_targets(p, rule);
                for (_, hunk_idx, post_idx, rule) in targets {
                    if let Some(line) = p.hunks[hunk_idx].post.get_mut(post_idx) {
                        if super::diff_files::ws_check(line, rule) != 0 {
                            let (fixed_line, fixed) = ws_fix_default(line, rule);
                            *line = fixed_line;
                            if fixed {
                                applied_after_fixing_ws += 1;
                            }
                        }
                    }
                }
            }
        }
        ws_errors = errors;
        // `if (state->whitespace_error && ws_error_action == die_on_ws_error)
        // state->apply = 0;` (apply.c:4942), inside `apply_patch()` and therefore
        // *before* the check and the write. The summary line itself is printed by
        // the tail block, which nothing else reaches under this action because
        // `state->check || state->apply` is then false.
        if errors > 0 && matches!(o.ws, WsAction::Error) {
            ws_summary(errors, &o.ws, o.apply, applied_after_fixing_ws, o.quiet());
            return Ok(ExitCode::from(128));
        }
    }

    // `apply_one_fragment()`'s `--inaccurate-eof` adjustment (apply.c:3099-3106),
    // done once per hunk here because `patch->inaccurate_eof` never changes within a
    // run: when both images end in a newline, take it off both. The pre-image's last
    // line then matches a file that has no final newline (as a prefix — see
    // [`matches_at`]), and the post-image is written without one, which is what makes
    // the flag observable even on a patch that would otherwise apply.
    //
    // It runs after `-R`, because git reverses the fragments before placing them.
    if o.inaccurate_eof {
        for p in &mut patches {
            for h in &mut p.hunks {
                let (Some(pre), Some(post)) = (h.pre.last(), h.post.last()) else {
                    continue;
                };
                if pre.last() != Some(&b'\n') || post.last() != Some(&b'\n') {
                    continue;
                }
                h.pre.last_mut().expect("checked above").pop();
                h.post.last_mut().expect("checked above").pop();
                // `--no-add` splices the context lines instead, and the last of them
                // is the same line when the hunk ends in context.
                if h.trailing > 0 {
                    if let Some(ctx) = h.context.last_mut() {
                        if ctx.last() == Some(&b'\n') {
                            ctx.pop();
                        }
                    }
                }
                h.eof_fudge = true;
            }
        }
    }

    // git prints its report modes in this fixed order — the scaled --stat graph,
    // then the machine-readable --numstat records, then the --summary lines — and
    // it prints them *last* (apply.c:4993-5000), after the check and the write.
    // Every earlier failure reaches them through a `goto end` that skips them, so
    // a patch that does not apply produces no report at all.
    let reports = |patches: &[Patch]| {
        if o.stat {
            print!("{}", render_stat(patches));
        }
        if o.numstat {
            print!("{}", render_numstat(patches, o.nul));
        }
        if o.summary {
            print!("{}", render_summary(patches));
        }
    };
    // apply.c:4987 — the fake ancestor is built at the end of `apply_patch()`,
    // after any write and before the report modes, on every path that got that far.
    let fake_ancestor = |patches: &[Patch]| -> Result<bool> {
        match &o.fake_ancestor {
            Some(path) => build_fake_ancestor(patches, path, o.quiet()),
            None => Ok(true),
        }
    };
    if !o.apply && !o.check {
        if !fake_ancestor(&patches)? {
            return Ok(ExitCode::from(128));
        }
        reports(&patches);
        return Ok(ExitCode::SUCCESS);
    }

    // ---- index substrate (only when --index/--cached) -----------------------
    // Hold the repo lock across the whole check-and-write span so the index we read
    // pre-images from is the same one we mutate and write — no concurrent writer can
    // slip in between, mirroring how git holds `lock_file` for the operation.
    let (idx_repo, mut idx_index, _idx_lock) = if check_index || o.ita_only {
        let repo = crate::setup::discover()?;
        let lock = crate::lock::RepoLock::acquire(repo.git_dir());
        let index = if repo.index_path().exists() {
            repo.open_index()?
        } else {
            gix::index::File::from_state(
                gix::index::State::new(repo.object_hash()),
                repo.index_path(),
            )
        };
        (Some(repo), Some(index), Some(lock))
    } else {
        (None, None, None)
    };
    // `update_index` gates the mutation itself: with `--check`/`--stat` (apply off)
    // the pre-image still comes from the index, but nothing is written
    // (apply.c:4945, `(check_index || ita_only) && apply`).
    let update_index = (check_index || o.ita_only) && o.apply;

    // ---- check phase: build every result in memory, touching nothing --------
    let mut staged: HashMap<String, Option<Vec<u8>>> = HashMap::new();
    let mut ops: Vec<Op> = Vec::new();
    let mut failed = false;
    // `patch->conflicted_threeway`: paths whose 3-way merge left markers behind,
    // with the stage 1/2/3 blobs `add_conflicted_stages_file()` records.
    let mut conflicted: Vec<(String, u32, [Option<ObjectId>; 3])> = Vec::new();

    for p in &patches {
        // The name git reports progress and success against.
        let name = p.new_name.clone().or_else(|| p.old_name.clone()).unwrap_or_default();
        // The name git reports errors against: the pre-image path when there is
        // one (`apply_fragments`), else the post-image path.
        let label = p.old_name.clone().or_else(|| p.new_name.clone()).unwrap_or_default();

        // `check_patch_list()` (apply.c:4172): `apply_verbosity > verbosity_normal`,
        // which `--reject` reaches without `-v`.
        if verbosity(&o).verbose {
            eprintln!("Checking patch {name}...");
        }

        // A view of the index for this iteration; recomputed each time so no
        // immutable borrow of `idx_index` is held into the mutable write phase.
        // `-N` alone opens the index to *write* it, but git's `check_index` stays
        // off there, so nothing on the read side may consult it.
        let idx_view = if check_index {
            idx_repo.as_ref().zip(idx_index.as_ref())
        } else {
            None
        };

        // `frag->rejected` per fragment; only `--reject` ever leaves a `false` here,
        // because without it the first failure fails the whole patch.
        let mut applied: Vec<bool> = Vec::new();
        // The pre-image bytes, kept whole: a text patch works on its lines, a binary
        // one on the bytes themselves.
        let mut pre_bytes: Vec<u8> = Vec::new();
        let mut image: Vec<Vec<u8>> = if p.is_new {
            Vec::new()
        } else {
            let old = p.old_name.as_deref().unwrap_or_default();
            match read_preimage(&staged, idx_view, o.cached, old, p.is_rename) {
                PreRead::Found(bytes) => {
                    pre_bytes = bytes.clone();
                    split_lines(&bytes).into_iter().map(|l| l.to_vec()).collect()
                }
                PreRead::MissingWorktree => {
                    err(o.quiet(), &format!("error: {old}: No such file or directory"));
                    failed = true;
                    continue;
                }
                PreRead::MissingIndex => {
                    err(o.quiet(), &format!("error: {old}: does not exist in index"));
                    failed = true;
                    continue;
                }
                PreRead::Mismatch => {
                    err(o.quiet(), &format!("error: {old}: does not match index"));
                    failed = true;
                    continue;
                }
                PreRead::CannotCheckout => {
                    err(o.quiet(), &format!("error: cannot checkout {old}"));
                    failed = true;
                    continue;
                }
            }
        };

        // `check_patch()` (apply.c): `check_preimage()` runs *first*, and only then
        // `check_to_create()` for a path that must not already exist — a creation
        // target, a rename destination or a copy destination. Reporting the
        // create-block first inverted the two diagnostics for a reversed copy,
        // where stock names the missing pre-image. git's `check_to_create` reports
        // against the index when `--index`/`--cached`, otherwise the worktree.
        if let Some(new) = &p.new_name {
            if p.is_new || p.is_rename || p.is_copy {
                match create_block(&staged, idx_view, o.cached, new) {
                    Some(Block::InIndex) => {
                        err(o.quiet(), &format!("error: {new}: already exists in index"));
                        failed = true;
                        continue;
                    }
                    Some(Block::InWorktree) => {
                        err(
                            o.quiet(),
                            &format!("error: {new}: already exists in working directory"),
                        );
                        failed = true;
                        continue;
                    }
                    None => {}
                }
            }
        }

        // apply.c:4142 — `check_unsafe_path()`, after the pre-image and
        // already-exists checks have had their say (so a missing out-of-tree file
        // is still reported as missing) and before anything is applied. A refusal
        // here is `-128`: it ends the whole run at once rather than marking this
        // one patch failed, which is why `--reject` writes no `*.rej` for it.
        if !o.unsafe_paths {
            if let Some(bad) = check_unsafe_path(p) {
                err(o.quiet(), &format!("error: invalid path '{bad}'"));
                return Ok(ExitCode::from(128));
            }
        }

        // `apply_data()`: under `--3way` the merge is what applies the patch, and
        // only a pre-image the object store cannot supply — or a patch that will
        // not even apply to that pre-image — falls back to placing hunks.
        let mut merged: Option<ThreeWay> = None;
        if o.three_way && !p.binary {
            let repo = idx_repo.as_ref().expect("--3way implies check_index");
            match try_threeway(repo, p, &pre_bytes, &o)? {
                ThreeWayOutcome::Merged(tw) => {
                    err(
                        o.quiet(),
                        &if tw.stages.is_some() {
                            format!("Applied patch to '{}' with conflicts.", tw.path)
                        } else {
                            format!("Applied patch to '{}' cleanly.", tw.path)
                        },
                    );
                    image = vec![tw.content.clone()];
                    merged = Some(tw);
                }
                ThreeWayOutcome::Fallback(reason) => {
                    if let Some(msg) = reason {
                        err(o.quiet(), &format!("error: {msg}"));
                    }
                    err(o.quiet(), "Falling back to direct application...");
                }
            }
        }

        // `apply_binary()`: the payload rebuilds the whole file, and both ends are
        // checked against the ids the `index` line named.
        if merged.is_some() {
            // The merge already produced the whole post-image.
        } else if p.binary {
            match rebuild_binary(p, &pre_bytes, o.reverse) {
                // An empty post-image is no line at all, so a binary deletion leaves nothing
                // behind for `apply_data`'s removal check to trip over.
                Ok(bytes) => image = if bytes.is_empty() { Vec::new() } else { vec![bytes] },
                Err(msg) => {
                    // `apply_binary()` fails `apply_data()`, so `check_patch()`
                    // (apply.c:4158) adds its own line under git's message.
                    err(o.quiet(), &format!("error: {msg}"));
                    err(o.quiet(), &format!("error: {label}: patch does not apply"));
                    failed = true;
                    continue;
                }
            }
        } else {
            match apply_hunks(
                &mut image,
                p,
                o.unidiff_zero,
                o.no_add,
                o.p_context,
                o.ignore_ws,
                o.allow_overlap,
                verbosity(&o),
                o.reject,
                &label,
            ) {
                Ok(a) => applied = a,
                Err(_) => {
                    // `apply_fragments()` returned -1, so `apply_data()` failed and
                    // `check_patch()` (apply.c:4158) adds its own line under it.
                    err(o.quiet(), &format!("error: {label}: patch does not apply"));
                    failed = true;
                    continue;
                }
            }
        }

        if p.is_delete {
            if !image.is_empty() {
                // Also an `apply_data()` failure (apply.c:3826), so `check_patch()`
                // appends its line.
                err(o.quiet(), "error: removal patch leaves file contents");
                err(o.quiet(), &format!("error: {label}: patch does not apply"));
                failed = true;
                continue;
            }
            let old = p.old_name.clone().unwrap_or_default();
            staged.insert(old.clone(), None);
            ops.push(Op {
                name,
                remove: Some(old),
                prune_dirs: true,
                create: None,
                is_new: false,
                applied,
                rej_body: Vec::new(),
            });
            continue;
        }

        let new = p.new_name.clone().unwrap_or_default();
        let data: Vec<u8> = image.concat();
        // git defaults a modification's new mode to the pre-image mode; under
        // `--index`/`--cached` that pre-image mode is the index entry's, so an
        // executable file stays executable even when the diff carries no mode line.
        let mode = match p.new_mode {
            Some(m) => m,
            None => {
                let from_index = if p.is_new {
                    None
                } else {
                    idx_view.and_then(|(_, index)| {
                        let old = p.old_name.as_deref()?;
                        index
                            .entry_by_path(old.as_bytes().as_bstr())
                            .map(|e| e.mode.bits())
                    })
                };
                from_index.unwrap_or(0o100644)
            }
        };
        // A rename removes its source; a copy does not.
        if let Some(old) = &p.old_name {
            if old != &new && !p.is_copy {
                staged.insert(old.clone(), None);
            }
        }
        staged.insert(new.clone(), Some(data.clone()));
        if let Some(stages) = merged.and_then(|tw| tw.stages) {
            conflicted.push((new.clone(), mode, stages));
        }
        // `write_out_one_reject()` writes the fragments that did not land verbatim,
        // each newline-terminated (apply.c:4794-4796).
        let mut rej_body: Vec<u8> = Vec::new();
        for (idx, ok) in applied.iter().enumerate() {
            if !*ok {
                let raw = &p.hunks[idx].raw;
                rej_body.extend_from_slice(raw);
                if raw.last() != Some(&b'\n') {
                    rej_body.push(b'\n');
                }
            }
        }
        ops.push(Op {
            name,
            remove: if p.is_copy { None } else { p.old_name.clone() },
            prune_dirs: p.is_rename,
            create: Some((new, mode, data)),
            is_new: p.is_new,
            applied,
            rej_body,
        });
    }

    // apply.c:4968 — `check_patch_list()`'s failure ends the run only without
    // `--reject`; with it, the patches that did check out are still written and the
    // refused ones are simply skipped.
    if failed && !o.reject {
        return Ok(ExitCode::from(1));
    }
    if !o.apply {
        if !fake_ancestor(&patches)? {
            return Ok(ExitCode::from(128));
        }
        reports(&patches);
        ws_summary(ws_errors, &o.ws, o.apply, applied_after_fixing_ws, o.quiet());
        return Ok(ExitCode::SUCCESS);
    }

    // ---- write phase: nothing here may fail on a well-formed patch ----------
    // Index mutations are accumulated by path and replayed once at the end (git's
    // `remove_file`/`add_index_file`); `--cached` skips every worktree touch.
    let mut idx_remove: Vec<BString> = Vec::new();
    let mut idx_add: Vec<(BString, ObjectId, IndexMode, Stat, Flags)> = Vec::new();

    // `write_out_results()` walks the whole list twice (apply.c:4817): every removal
    // happens in phase 0 and every creation in phase 1, so a swap-rename between two
    // paths cannot have one side's creation clobber the other side's pre-image.
    for op in &ops {
        let Some(old) = &op.remove else { continue };
        if !o.cached {
            let _ = std::fs::remove_file(old);
            if op.prune_dirs {
                prune_empty_parents(Path::new(old));
            }
        }
        // `remove_file()` (apply.c:4431) drops the index entry only when this is
        // a real index update; under `-N` alone the entry is deliberately left
        // standing, so a deletion shows up as an unstaged removal.
        if update_index && !o.ita_only {
            idx_remove.push(old.clone().into_bytes().into());
        }
    }

    // `write_out_one_reject()` returns non-zero for every patch that left a `*.rej`,
    // which is what makes the run exit 1.
    let mut any_reject = false;
    for op in ops {
        if let Some((path, mode, data)) = op.create {
            if !o.cached {
                // `create_file()`'s `error_errno()` (apply.c) unwinds to `git
                // apply`'s exit 128, not to the crate's `zvcs: apply: …` exit 1.
                if let Err(e) = create_one_file(Path::new(&path), mode, &data) {
                    err(o.quiet(), &format!("error: {e}"));
                    return Ok(ExitCode::from(128));
                }
            }
            // `create_file()` (apply.c:4685): `check_index` stages every result,
            // `ita_only` stages only the paths the patch creates.
            if update_index && (check_index || op.is_new) {
                let repo = idx_repo.as_ref().expect("repo present when update_index");
                let (id, stat, flags) = if o.ita_only {
                    // `set_object_name_for_intent_to_add_entry()` (read-cache.c:704):
                    // the entry names the empty blob, and `make_empty_cache_entry`
                    // leaves its stat zeroed so it can never look up to date.
                    // `EXTENDED` is what makes the index writer emit the v3 entry
                    // that carries `CE_INTENT_TO_ADD` at rest.
                    (
                        repo.write_blob([])?.detach(),
                        Stat::default(),
                        Flags::EXTENDED | Flags::INTENT_TO_ADD,
                    )
                } else {
                    let id = repo.write_blob(&data)?.detach();
                    // For `--index` the entry's stat comes from the file just written
                    // (git's `fill_stat_cache_info`); `--cached` writes no file, so the
                    // stat is zeroed, exactly as `make_empty_cache_entry` leaves it.
                    let stat = if o.cached {
                        Stat::default()
                    } else {
                        let md = gix::index::fs::Metadata::from_path_no_follow(Path::new(&path))?;
                        Stat::from_fs(&md)?
                    };
                    (id, stat, Flags::empty())
                };
                // `add_index_file()` (apply.c:4488) writes the blob *first* and
                // only then calls `add_index_entry()`, so a refusal below still
                // leaves the object in the store — which is what stock does.
                //
                // `add_index_entry_with_check()` (read-cache.c:1287) checks the
                // name on the way in, and `add_index_file()` adds its own line
                // under it. `--unsafe-paths` waived the earlier gate but not this
                // one, so `-N` on a patch that writes outside the tree ends here —
                // with the file already written, as in git.
                if !verify_path(&path, mode) {
                    err(o.quiet(), &format!("error: invalid path '{path}'"));
                    err(o.quiet(), &format!("error: unable to add cache entry for {path}"));
                    return Ok(ExitCode::from(128));
                }
                // `has_dir_name()` (read-cache.c): `add_index_entry()` is called
                // without `ADD_CACHE_OK_TO_REPLACE`, so a file entry whose name is
                // an existing entry's *directory* prefix is refused rather than
                // taking its place.
                let dir_prefix = format!("{path}/");
                let clashes = idx_index.as_ref().is_some_and(|index| {
                    index
                        .entries()
                        .iter()
                        .any(|e| e.path(index).starts_with(dir_prefix.as_bytes()))
                }) || idx_add.iter().any(|(p, ..): &(gix::bstr::BString, _, _, _, _)| {
                    p.starts_with(dir_prefix.as_bytes())
                });
                if clashes {
                    err(
                        o.quiet(),
                        &format!("error: '{path}' appears as both a file and as a directory"),
                    );
                    err(o.quiet(), &format!("error: unable to add cache entry for {path}"));
                    return Ok(ExitCode::from(128));
                }
                idx_add.push((
                    path.clone().into_bytes().into(),
                    id,
                    to_index_mode(mode),
                    stat,
                    flags,
                ));
            }
        }
        // `write_out_one_reject()` (apply.c:4716), which runs for every patch in
        // phase 1 — that is where `Applied patch <name> cleanly.` comes from, both
        // with and without `--reject`.
        let nrej = op.applied.iter().filter(|a| !**a).count();
        if nrej == 0 {
            if verbosity(&o).verbose {
                eprintln!("Applied patch {} cleanly.", op.name);
            }
            continue;
        }
        any_reject = true;
        // "Say this even without --verbose".
        err(
            o.quiet(),
            &format!(
                "Applying patch {} with {nrej} {}...",
                op.name,
                if nrej == 1 { "reject" } else { "rejects" }
            ),
        );
        for (idx, ok) in op.applied.iter().enumerate() {
            err(
                o.quiet(),
                &if *ok {
                    format!("Hunk #{} applied cleanly.", idx + 1)
                } else {
                    format!("Rejected hunk #{}.", idx + 1)
                },
            );
        }
        // git names both sides of the banner with `patch->new_name` (apply.c:4782).
        let rej = format!("{}.rej", op.name);
        std::fs::write(
            &rej,
            [
                format!("diff a/{0} b/{0}\t(rejected hunks)\n", op.name).as_bytes(),
                &op.rej_body,
            ]
            .concat(),
        )?;
    }

    // An update that would touch nothing is skipped outright: git's
    // `write_locked_index` rewrites the same bytes in that case, while rebuilding
    // it here would drop the cached-tree extension for no reason. This is what
    // `-N` on a patch that creates nothing hits.
    // `apply_all_patches()` writes the index only when `apply_patch()` came back
    // non-negative (apply.c:5129, :5173), and `--reject` turns any rejected hunk —
    // or any patch the check refused — into `-1`. So a `--reject` run that rejected
    // anything rolls the whole index update back, including the paths that did
    // apply cleanly. Everything already written to the worktree stays.
    let roll_back_index = o.reject && (failed || any_reject);
    if update_index && !roll_back_index && !(idx_add.is_empty() && idx_remove.is_empty()) {
        let index = idx_index.as_mut().expect("index present when update_index");
        // If two patches in one input touched the same path, keep only the last
        // add for it — git's `add_index_entry` replaces in place, so the final
        // state wins. Reverse, keep first-seen (= original last), let the later
        // `sort_entries` re-order.
        idx_add.reverse();
        let mut seen: HashSet<BString> = HashSet::new();
        idx_add.retain(|(p, _, _, _, _)| seen.insert(p.clone()));
        // Every touched path is dropped (any prior stage) before its fresh stage-0
        // entry is pushed; a pure deletion contributes only a removal.
        let drop_set: HashSet<BString> = idx_remove
            .iter()
            .cloned()
            .chain(idx_add.iter().map(|(p, _, _, _, _)| p.clone()))
            .collect();
        index.remove_entries(|_, path, _| drop_set.contains(&path.to_owned()));
        // `add_conflicted_stages_file()` replaces a conflicted path's stage-0
        // entry with the base/ours/theirs trio, so the path reads as unmerged.
        let conflicted_paths: HashSet<BString> = conflicted
            .iter()
            .map(|(p, _, _)| BString::from(p.clone().into_bytes()))
            .collect();
        for (path, id, mode, stat, flags) in &idx_add {
            if conflicted_paths.contains(path) {
                continue;
            }
            index.dangerously_push_entry(*stat, *id, *flags, *mode, path.as_ref());
        }
        for (path, mode, stages) in &conflicted {
            let path = BString::from(path.clone().into_bytes());
            for (n, id) in stages.iter().enumerate() {
                let Some(id) = id else { continue };
                index.dangerously_push_entry(
                    Stat::default(),
                    *id,
                    Flags::from_stage(match n {
                        0 => gix::index::entry::Stage::Base,
                        1 => gix::index::entry::Stage::Ours,
                        _ => gix::index::entry::Stage::Theirs,
                    }),
                    to_index_mode(*mode),
                    path.as_ref(),
                );
            }
        }
        index.sort_entries();
        // `git apply` is **not** an `unpack_trees()` verb, and repairing here wrote a fully
        // valid cache-tree where git leaves a partly invalidated one — 38 bytes longer than
        // stock's on `--index`, `--cached` and `--3way` alike.
        //
        // `apply_patch()` stages one entry at a time: `add_index_file()` ends in
        // `add_index_entry(state->repo->index, ce, ADD_CACHE_OK_TO_ADD)` (apply.c:4499),
        // `remove_file()` in `remove_file_from_index(state->repo->index, patch->old_name)`
        // (apply.c:4445), and a conflicted `--3way` path goes through both
        // (`add_conflicted_stages_file()`, apply.c:4664-4674). Each of those invalidates the
        // path it touches and every directory above it — `cache_tree_invalidate_path()` from
        // `add_index_entry_with_check()` (read-cache.c:1273-1274) and from
        // `remove_file_from_index()` (read-cache.c:627-637). `apply_all_patches()` then
        // finishes with a plain `write_locked_index()` (apply.c:5188) and repairs nothing.
        //
        // So the shape is the one every entry-mutating verb leaves: the root and the patched
        // directories marked `-1`, and a directory no hunk reached still naming its tree.
        // `drop_set` is exactly the set of paths that went through one of those two calls.
        for path in &drop_set {
            index.invalidate_path_in_tree(path.as_ref());
        }
        super::write_tree::prepare_offset_table(
            idx_repo.as_ref().expect("repo present when update_index"),
            index,
        );
        index.write(crate::config::index_write_options(
            idx_repo.as_ref().expect("repo present when update_index"),
        ))?;
    }

    // `write_out_results()`: the conflicted paths are named once every write is
    // done, in sorted order, and make the whole run fail.
    if !conflicted.is_empty() {
        let mut names: Vec<&str> = conflicted.iter().map(|(p, _, _)| p.as_str()).collect();
        names.sort_unstable();
        for name in names {
            err(o.quiet(), &format!("U {name}"));
        }
        return Ok(ExitCode::from(1));
    }

    // `write_out_results()` returning non-zero is a `goto end` in `apply_patch()`,
    // so a run that rejected anything prints no report and exits 1.
    if failed || any_reject {
        return Ok(ExitCode::from(1));
    }

    if !fake_ancestor(&patches)? {
        return Ok(ExitCode::from(128));
    }
    reports(&patches);
    ws_summary(ws_errors, &o.ws, o.apply, applied_after_fixing_ws, o.quiet());
    Ok(ExitCode::SUCCESS)
}

/// The outcome of `try_threeway()`: either the merge produced the post-image, or
/// the caller must fall back to placing the patch's hunks directly.
enum ThreeWayOutcome {
    Merged(ThreeWay),
    /// `try_threeway()` returned `< 0`. The payload is the `error()` git printed
    /// on the way out, when it printed one.
    Fallback(Option<String>),
}

/// A completed 3-way merge.
struct ThreeWay {
    /// `patch->new_name`, the path git names in its report.
    path: String,
    /// The merged post-image.
    content: Vec<u8>,
    /// `patch->threeway_stage`, set only when the merge did not resolve: the
    /// pre-image (absent for a creation), ours, and theirs.
    stages: Option<[Option<ObjectId>; 3]>,
}

/// Port of `try_threeway()` (apply.c): rebuild the post-image the patch was
/// written to produce, then merge it into the current contents using the blob the
/// patch names as the common ancestor.
///
/// `ours` is the pre-image the caller already read — git's `load_preimage()`
/// result, which is the index blob under `check_index` and the worktree file
/// otherwise.
fn try_threeway(
    repo: &gix::Repository,
    p: &Patch,
    ours: &[u8],
    o: &Opts,
) -> Result<ThreeWayOutcome> {
    // "No point falling back to 3-way merge in these cases". A creation is on
    // the list too: git only merges one through `direct_to_threeway`, the
    // add/add path this port does not build.
    let gitlink = |m: Option<u32>| m.is_some_and(|m| m & 0o170000 == 0o160000);
    if p.is_delete
        || p.is_new
        || gitlink(p.old_mode)
        || gitlink(p.new_mode)
        || (p.is_rename && p.added == 0 && p.deleted == 0)
    {
        return Ok(ThreeWayOutcome::Fallback(None));
    }

    let path = p
        .new_name
        .clone()
        .or_else(|| p.old_name.clone())
        .unwrap_or_default();
    let missing_blob =
        || Ok(ThreeWayOutcome::Fallback(Some(
            "repository lacks the necessary blob to perform 3-way merge.".to_string(),
        )));

    // "Preimage the patch was prepared for": the `index <old>..` id, read as a blob.
    let Some(pre_hex) = p.preimage_id(o.reverse) else {
        return missing_blob();
    };
    let Ok(pre_id) = repo.rev_parse_single(pre_hex.as_bytes().as_bstr()) else {
        return missing_blob();
    };
    let Some(pre_bytes) = repo
        .find_object(pre_id)
        .ok()
        .and_then(|obj| obj.try_into_blob().ok())
        .map(|blob| blob.data.clone())
    else {
        return missing_blob();
    };
    let pre_id = pre_id.detach();

    // "Apply the patch to get the post image" — against that pre-image, not
    // against what is on disk.
    let mut post: Vec<Vec<u8>> = split_lines(&pre_bytes)
        .into_iter()
        .map(<[u8]>::to_vec)
        .collect();
    if apply_hunks(
        &mut post,
        p,
        o.unidiff_zero,
        o.no_add,
        o.p_context,
        o.ignore_ws,
        o.allow_overlap,
        verbosity(o),
        // `try_threeway()` builds the patch's own post-image, which either applies
        // to the blob it was made against or does not; `--reject` has no say here.
        false,
        p.old_name.as_deref().or(p.new_name.as_deref()).unwrap_or_default(),
    )
    .is_err()
    {
        return Ok(ThreeWayOutcome::Fallback(None));
    }
    let post_bytes: Vec<u8> = post.concat();
    let post_id = repo.write_blob(&post_bytes)?.detach();
    let our_id = repo.write_blob(ours)?.detach();

    // `three_way_merge()`'s trivial resolutions, which never reach the merge
    // driver: one side did not move, so the other side is the answer.
    if pre_id == our_id {
        return Ok(ThreeWayOutcome::Merged(ThreeWay {
            path,
            content: post_bytes,
            stages: None,
        }));
    }
    if pre_id == post_id || our_id == post_id {
        return Ok(ThreeWayOutcome::Merged(ThreeWay {
            path,
            content: ours.to_vec(),
            stages: None,
        }));
    }

    // `ll_merge()` with `LL_MERGE_OPTIONS_INIT`: `XDL_MERGE_ZEALOUS`, the
    // configured conflict style, and git's fixed base/ours/theirs labels.
    let style = match super::merge_file::conflict_style_config(Some(repo)) {
        Ok(s) => s,
        Err(code) => return Ok(ThreeWayOutcome::Fallback(Some(format!(
            "merge.conflictStyle is unusable (exit {code:?})"
        )))),
    };
    let conflict = match o.merge_variant {
        Some(MergeVariant::Ours) => MergeConflict::ResolveWithOurs,
        Some(MergeVariant::Theirs) => MergeConflict::ResolveWithTheirs,
        Some(MergeVariant::Union) => MergeConflict::ResolveWithUnion,
        None => MergeConflict::Keep {
            style,
            marker_size: std::num::NonZeroU8::new(7).expect("7 is not zero"),
        },
    };
    let mut content = Vec::new();
    let mut input = gix::diff::blob::InternedInput::default();
    let merge = MergeText::new(
        &mut input,
        ours,
        &pre_bytes,
        &post_bytes,
        gix::diff::blob::Algorithm::Myers,
    );
    let (_resolution, conflicts) = merge.run_with(
        &mut content,
        MergeLabels {
            current: Some(b"ours".as_bstr()),
            ancestor: Some(b"base".as_bstr()),
            other: Some(b"theirs".as_bstr()),
        },
        MergeRendering {
            conflict,
            style: Some(style),
            level: MergeLevel::Zealous,
            marker_size: Some(7),
        },
    );

    Ok(ThreeWayOutcome::Merged(ThreeWay {
        path,
        content,
        stages: (conflicts > 0).then_some([Some(pre_id), Some(our_id), Some(post_id)]),
    }))
}

/// Whether a creation/rename target is already taken, and where. git's
/// `check_to_create`: under `--index`/`--cached` the index is consulted first, then
/// (unless `--cached`) the worktree; without index mode only the worktree.
enum Block {
    InIndex,
    InWorktree,
}

fn create_block(
    staged: &HashMap<String, Option<Vec<u8>>>,
    idx: Option<(&gix::Repository, &gix::index::File)>,
    cached: bool,
    new: &str,
) -> Option<Block> {
    // An in-run result (a previous patch in this same invocation) wins first.
    match staged.get(new) {
        Some(Some(_)) => {
            return Some(if idx.is_some() {
                Block::InIndex
            } else {
                Block::InWorktree
            })
        }
        Some(None) => return None, // deleted earlier this run: the path is free
        None => {}
    }
    // `check_to_create()` (apply.c): `if (S_ISDIR(nst.st_mode) || ok_if_exists) return 0;`
    // — a *directory* standing where the patch wants a file is not a create-block.
    // git lets the check pass and fails later, in `create_file()`, with
    // `unable to write file '<path>' mode <mode>: Is a directory`.
    let blocked_on_disk = || match std::fs::symlink_metadata(new) {
        Ok(meta) => !meta.is_dir(),
        Err(_) => false,
    };
    match idx {
        Some((_, index)) => {
            if index.entry_by_path(new.as_bytes().as_bstr()).is_some() {
                return Some(Block::InIndex);
            }
            if !cached && blocked_on_disk() {
                return Some(Block::InWorktree);
            }
            None
        }
        None if blocked_on_disk() => Some(Block::InWorktree),
        None => None,
    }
}

/// An empty stand-in for the in-flight results, for the reads that must not see them.
static EMPTY_STAGED: std::sync::LazyLock<HashMap<String, Option<Vec<u8>>>> =
    std::sync::LazyLock::new(HashMap::new);

/// The outcome of reading a patch's pre-image.
enum PreRead {
    Found(Vec<u8>),
    MissingWorktree,
    MissingIndex,
    Mismatch,
    /// `checkout_target()` (apply.c:3485) could not write the file back out:
    /// `error(_("cannot checkout %s"), ce->name)`.
    CannotCheckout,
}

/// `checkout_target()` (apply.c:3485). `check_preimage()` reaches it when
/// `--index` is on and `lstat()` of the pre-image path failed (apply.c:3878-3881):
/// the index entry is written back into the working tree so the patch has something
/// to apply to, and only then does `verify_index_match()` get a say.
///
/// It runs under `--check` too — `check_preimage()` is part of the check pass — so
/// `git apply --check --index` on a path whose file was deleted recreates the file
/// (and its leading directories) as a side effect, which stock does and this
/// reproduces.
fn checkout_target(path: &str, mode: u32, data: &[u8]) -> bool {
    let p = Path::new(path);
    if create_leading_dirs(p).is_err() {
        return false;
    }
    write_created(p, mode, data).is_ok()
}

/// Load a patch's pre-image, from an earlier in-run result if present, else from
/// the index blob (git's `load_patch_target` when `check_index`) or the worktree.
/// Under `--index` (not `--cached`) the worktree content is verified against the
/// index blob — git's `verify_index_match` — refusing on any divergence. A file that
/// is *absent* is not a divergence: git checks it out of the index first
/// ([`checkout_target`]), and so does this.
fn read_preimage(
    staged: &HashMap<String, Option<Vec<u8>>>,
    idx: Option<(&gix::Repository, &gix::index::File)>,
    cached: bool,
    old: &str,
    // `previous_patch()` (apply.c:3505) returns NULL outright for a rename or copy —
    // "git patches do not depend on the order" — so those read their pre-image from
    // the worktree or the index as it stands, never from an earlier patch's result.
    // A three-way swap-rename therefore fails on the middle name in git, and does
    // here too.
    is_rename: bool,
) -> PreRead {
    let staged = if is_rename { &EMPTY_STAGED } else { staged };
    if let Some(entry) = staged.get(old) {
        return match entry {
            Some(bytes) => PreRead::Found(bytes.clone()),
            None => PreRead::MissingWorktree,
        };
    }
    match idx {
        Some((repo, index)) => {
            let Some(ce) = index.entry_by_path(old.as_bytes().as_bstr()) else {
                return PreRead::MissingIndex;
            };
            let bytes = match repo.find_object(ce.id) {
                Ok(obj) => obj.data.clone(),
                Err(_) => return PreRead::MissingIndex,
            };
            if !cached {
                let empty = HashMap::new();
                match read_current(&empty, old) {
                    Some(wt) => {
                        match gix::objs::compute_hash(
                            repo.object_hash(),
                            gix::objs::Kind::Blob,
                            &wt,
                        ) {
                            Ok(h) if h == ce.id => {}
                            _ => return PreRead::Mismatch,
                        }
                    }
                    // `stat_ret < 0` in `check_preimage()`: the file is not there at
                    // all, so git checks it out of the index rather than refusing.
                    // `verify_index_match()` then trivially agrees, because the bytes
                    // just written *are* the index blob.
                    //
                    // A gitlink is left out of that: `checkout_entry()` would create
                    // the submodule *directory* and `verify_index_match()` only asks
                    // whether the path is one (apply.c:3525-3528), so writing the
                    // commit id out as a file would satisfy neither. An absent
                    // submodule keeps the refusal it had.
                    None if ce.mode.bits() & 0o170000 == 0o160000 => {
                        return PreRead::Mismatch;
                    }
                    None => {
                        if !checkout_target(old, ce.mode.bits(), &bytes) {
                            return PreRead::CannotCheckout;
                        }
                    }
                }
            }
            PreRead::Found(bytes)
        }
        None => match read_current(staged, old) {
            Some(bytes) => PreRead::Found(bytes),
            None => PreRead::MissingWorktree,
        },
    }
}

/// Map a patch's raw file mode to the canonical index entry mode git records
/// (`create_ce_mode`): symlink, gitlink, or a regular file normalised to
/// executable/non-executable.
fn to_index_mode(mode: u32) -> IndexMode {
    match mode & 0o170000 {
        0o120000 => IndexMode::SYMLINK,
        0o160000 => IndexMode::COMMIT,
        _ if mode & 0o111 != 0 => IndexMode::FILE_EXECUTABLE,
        _ => IndexMode::FILE,
    }
}

/// One file's worth of work, resolved during the check phase and replayed
/// verbatim during the write phase (git's `write_out_one_result`: remove the
/// pre-image path, then create the post-image path).
struct Op {
    name: String, // display name for the verbose `Applied patch <name> cleanly.`
    remove: Option<String>,
    prune_dirs: bool,
    create: Option<(String, u32, Vec<u8>)>,
    /// `0 < patch->is_new`, the condition `create_file()` tests before recording an
    /// intent-to-add entry for the path (apply.c:4685).
    is_new: bool,
    /// `frag->rejected` for each of the patch's fragments, in order — only filled
    /// under `--reject`, where a hunk that did not land is recorded instead of
    /// failing the patch. Empty means there is nothing for
    /// `write_out_one_reject()` to say beyond `Applied patch <name> cleanly.`.
    applied: Vec<bool>,
    /// The verbatim text of the fragments that were rejected, already in the order
    /// and shape `<name>.rej` wants them.
    rej_body: Vec<u8>,
}

/// A single file's patch: the extended header facts plus its hunks.
struct Patch {
    old_name: Option<String>, // None once normalised => creation
    new_name: Option<String>, // None once normalised => deletion
    old_mode: Option<u32>,    // pre-image mode, for the summary's `mode change` line
    new_mode: Option<u32>,
    is_new: bool,
    is_delete: bool,
    is_rename: bool,
    /// `patch->is_copy`: a `copy from`/`copy to` header pair. The post-image is
    /// created exactly as a rename's is, and the *source is left in place* — which
    /// is the only difference between the two on the write side.
    is_copy: bool,
    binary: bool,
    /// The `GIT binary patch` payloads, forward first and the reverse second when the
    /// patch carries one (`--binary` writes both). `None` for a `Binary files … differ`
    /// stub, which carries no data at all.
    binary_forward: Option<BinaryPayload>,
    binary_reverse: Option<BinaryPayload>,
    /// The two ids of the `index <old>..<new>` line, when it carried them in full.
    /// A binary patch is only applied when they are there: git needs them to check
    /// that the pre-image is the one the payload was made against.
    index_old: Option<String>,
    index_new: Option<String>,
    score: u32, // `similarity index N%`, for the summary's rename line
    /// `patch->is_toplevel_relative`: set by `parse_git_header()` (apply.c:1457) for
    /// a `diff --git` patch, whose names are already relative to the worktree root.
    /// A traditional `---`/`+++` diff leaves it clear (apply.c:1596) and its names
    /// are read as relative to the directory `git apply` was invoked from, so
    /// [`prefix_patch`] prepends the prefix to them.
    is_toplevel_relative: bool,
    hunks: Vec<Hunk>,
    added: usize,
    deleted: usize,
}

impl Patch {
    /// `patch->old_oid_prefix`: the id of the blob the patch was written against,
    /// which the 3-way merge uses as its common ancestor.
    ///
    /// `reverse_patches()` swaps the pair along with everything else it reverses.
    /// [`Patch::reverse`] leaves `index_old`/`index_new` in file order because the
    /// binary payload they are checked against is not swapped either, so under
    /// `-R` the pre-image id is the one written second.
    fn preimage_id(&self, reversed: bool) -> Option<&String> {
        if reversed {
            self.index_new.as_ref()
        } else {
            self.index_old.as_ref()
        }
    }

    /// `-R`: swap the two images, so the patch undoes itself.
    fn reverse(&mut self) {
        std::mem::swap(&mut self.old_name, &mut self.new_name);
        std::mem::swap(&mut self.is_new, &mut self.is_delete);
        std::mem::swap(&mut self.added, &mut self.deleted);
        // A reversal swaps the two sides' modes too, so a reversed creation's
        // `new file mode` becomes the deletion's `deleted file mode`, and a
        // reversed mode change inverts. Context lines are direction-neutral, so
        // `h.context` (used by --no-add) is left as is.
        std::mem::swap(&mut self.old_mode, &mut self.new_mode);
        for h in &mut self.hunks {
            std::mem::swap(&mut h.pre, &mut h.post);
            std::mem::swap(&mut h.pre_common, &mut h.post_common);
            std::mem::swap(&mut h.old_pos, &mut h.new_pos);
            // The stored index into `pre`/`post` stays right because the two images
            // just traded places; only which side a body line belongs to inverts.
            // This is what `parse_fragment()` expresses as the `apply_in_reverse`
            // tests around `check_old_for_crlf()`/`check_whitespace()`
            // (apply.c:1855-1869): under `-R` it is the `-` lines that get the
            // whitespace check and the `+` lines that can relax it.
            for (_, marker, _) in &mut h.body {
                *marker = match *marker {
                    b'-' => b'+',
                    b'+' => b'-',
                    m => m,
                };
            }
        }
    }
}

/// One `@@` fragment. `pre`/`post` hold whole lines *including* their trailing
/// newline (absent on a line marked `\ No newline at end of file`), matching how
/// git's `struct image` stores them so the EOF-newline distinction falls out of
/// plain byte comparison.
#[derive(Clone)]
struct Hunk {
    old_pos: usize,
    new_pos: usize,
    pre: Vec<Vec<u8>>,
    /// `LINE_COMMON` on the pre-image: which of `pre`'s lines are context rather
    /// than deletions, so a relaxed match can pair them with the post-image's
    /// context lines the way `update_pre_post_images()` does.
    pre_common: Vec<bool>,
    post: Vec<Vec<u8>>,
    /// `LINE_COMMON` on the post-image: which of `post`'s lines are context rather
    /// than additions.
    post_common: Vec<bool>,
    /// The fragment's body lines in the order `parse_fragment()` (apply.c:1802) walks
    /// them, which is the order both whitespace passes depend on: `(index into the
    /// concatenated input, the `' '`/`'-'`/`'+'` marker, index into `pre` for the
    /// first two and into `post` for the last)`.
    ///
    /// The input index is what the whitespace check reports against
    /// (`<patch>:<line>: …`); the *order* is what makes `check_old_for_crlf()`
    /// (apply.c:1716) observable, since it only relaxes the lines that come after it.
    body: Vec<(usize, u8, usize)>,
    context: Vec<Vec<u8>>, // the context lines only, spliced in for --no-add
    raw: Vec<u8>,          // the fragment's verbatim text (header + body) for *.rej
    trailing: usize,       // trailing context lines; 0 means the hunk must match at EOF
    leading: usize,        // leading context lines, for `-C<n>` context reduction
    /// `--inaccurate-eof` took the newline off the last line of both images
    /// (apply.c:3099-3106), so the pre-image's last line has to match the file's as
    /// a prefix rather than in full — which is how git's flat-buffer `memcmp`
    /// compares it once the recorded length has shrunk.
    eof_fudge: bool,
}

// ---------------------------------------------------------------------------
// hunk placement — port of apply.c:find_pos / match_fragment
// ---------------------------------------------------------------------------

/// Apply every hunk of `p` to `image` in order. On failure returns the index of
/// the failing hunk (the caller reads its `old_pos`/`pre` for git's
/// `patch failed: <path>:<n>` and verbose `while searching for:` diagnostics).
/// With `no_add`, the post-image drops the added lines, leaving only context.
fn apply_hunks(
    image: &mut Vec<Vec<u8>>,
    p: &Patch,
    unidiff_zero: bool,
    no_add: bool,
    // `-C<n>`: the fewest context lines a hunk may be reduced to before it is called
    // a failure. `None` keeps every context line, which is git's default.
    p_context: Option<usize>,
    // `state->ws_ignore_action == ignore_ws_change`.
    ignore_ws: bool,
    // `state->allow_overlap`: do not mark the lines a fragment wrote as `LINE_PATCHED`,
    // so a later fragment of the same patch may match against them.
    allow_overlap: bool,
    v: Verbosity,
    // `state->apply_with_reject`: a fragment that will not land is recorded and the
    // rest are still tried, instead of failing the whole patch at the first one.
    reject: bool,
    // The name a failure is reported against (`apply_fragments()`' `name`).
    label: &str,
) -> Result<Vec<bool>, usize> {
    let mut applied = Vec::with_capacity(p.hunks.len());
    // `img->line[].flag`, carrying `LINE_PATCHED` for every line an earlier fragment
    // of this patch has already written (apply.c:2969-2971). It keeps the second hunk
    // of a patch from matching against the first hunk's own output; `--allow-overlap`
    // is precisely "stop setting this".
    let mut patched: Vec<bool> = vec![false; image.len()];
    for (idx, h) in p.hunks.iter().enumerate() {
        let Some(placed) =
            place_with_context(image.as_slice(), &patched, h, unidiff_zero, p_context, ignore_ws)
        else {
            // apply.c:3369-3374 — the diagnostics come first either way; only what
            // happens next differs.
            if v.verbose {
                let pre: Vec<u8> = h.pre.concat();
                err(
                    v.quiet,
                    &format!("error: while searching for:\n{}", String::from_utf8_lossy(&pre)),
                );
            }
            err(v.quiet, &format!("error: patch failed: {label}:{}", h.old_pos));
            if !reject {
                return Err(idx);
            }
            applied.push(false);
            continue;
        };
        placed.report(idx + 1, v);
        let hunk = placed.hunk.as_ref().unwrap_or(h);
        let repl = replacement(image.as_slice(), placed.at, hunk, no_add, ignore_ws);
        let written = repl.len();
        image.splice(placed.at..placed.at + hunk.pre.len(), repl);
        patched.splice(
            placed.at..placed.at + hunk.pre.len(),
            std::iter::repeat_n(!allow_overlap, written),
        );
        applied.push(true);
    }
    Ok(applied)
}

/// `state->apply_verbosity` for this run.
fn verbosity(o: &Opts) -> Verbosity {
    Verbosity {
        verbose: o.verbose(),
        quiet: o.quiet(),
        reverse: o.reverse,
    }
}

/// `state->apply_verbosity`, for the two progress lines the placement loop prints.
/// `--reject` raises a normal run to verbose (apply.c:164-168), which is why
/// `Hunk #N succeeded …` shows up there without `-v`.
#[derive(Clone, Copy)]
struct Verbosity {
    /// `apply_verbosity > verbosity_normal`: `-v`, or `--reject`.
    verbose: bool,
    /// `apply_verbosity <= verbosity_silent`: `-q`, which mutes even the
    /// context-reduction warning.
    quiet: bool,
    /// `state->apply_in_reverse`, which flips the sign of a reported offset.
    reverse: bool,
}

/// Where a hunk landed, and what it took to get it there.
struct Placed {
    /// The image line the pre-image matched at.
    at: usize,
    /// The trimmed form that matched, when context had to be dropped for it.
    /// `None` means the hunk applied as written.
    hunk: Option<Hunk>,
    /// git's running `pos`: where the hunk was *expected*, which the reduction
    /// loop decrements once per leading context line it drops (apply.c:3162).
    expected: isize,
    /// The context counts left on the placed form, for the reduction warning.
    leading: usize,
    trailing: usize,
}

impl Placed {
    /// The two lines apply.c:3194-3213 prints once a fragment has landed.
    fn report(&self, nth: usize, v: Verbosity) {
        if v.verbose && self.at as isize != self.expected {
            let offset = self.at as isize - self.expected;
            let offset = if v.reverse { -offset } else { offset };
            eprintln!(
                "Hunk #{nth} succeeded at {} (offset {offset} {}).",
                self.at + 1,
                if offset.abs() == 1 { "line" } else { "lines" }
            );
        }
        if self.hunk.is_some() && !v.quiet {
            eprintln!(
                "Context reduced to ({}/{}) to apply fragment at {}",
                self.leading,
                self.trailing,
                self.at + 1
            );
        }
    }
}

/// The lines that replace the pre-image at `at`.
///
/// `update_pre_post_images()` (apply.c:2433), which `line_by_line_fuzzy_match()`
/// runs once a hunk has matched only under relaxed whitespace: the pre-image is
/// replaced by the bytes actually in the file, and every context line of the
/// post-image is re-taken from its counterpart there, in order. So a context line
/// keeps the file's whitespace rather than the patch's — only added lines come out
/// of the patch. When the two matched byte for byte this copies each line onto
/// itself, which is why it needs no separate "was the match fuzzy" flag.
fn replacement(
    image: &[Vec<u8>],
    at: usize,
    h: &Hunk,
    no_add: bool,
    ignore_ws: bool,
) -> Vec<Vec<u8>> {
    let source = if no_add { &h.context } else { &h.post };
    if !ignore_ws {
        return source.clone();
    }
    // The pre-image lines that are context, which the post-image's context lines
    // pair with one for one and in order (git's `LINE_COMMON` walk).
    let mut common = h
        .pre_common
        .iter()
        .enumerate()
        .filter(|(_, &c)| c)
        .map(|(j, _)| j);
    let mut out = Vec::with_capacity(source.len());
    for (k, line) in source.iter().enumerate() {
        // Under `--no-add` the replacement is the context lines alone, so every
        // one of them is common.
        if !no_add && !h.post_common.get(k).copied().unwrap_or(false) {
            out.push(line.clone());
            continue;
        }
        match common.next().and_then(|j| image.get(at + j)) {
            Some(file_line) => out.push(file_line.clone()),
            None => out.push(line.clone()),
        }
    }
    out
}

/// `apply_one_fragment()`'s placement loop (apply.c:3138-3170): try the hunk
/// where it says it goes, then — only when `-C<n>` allows it — first drop the
/// begin/end anchoring, and after that trim a context line off whichever end has
/// more of them, retrying each time down to that floor.
///
/// git reduces *both* ends in one pass when they are equal (`leading >= trailing`
/// takes the front, and the separate `trailing > leading` test then takes the back
/// off the already-shortened form), which is why the counts it reports can drop by
/// two at a time.
fn place_with_context(
    image: &[Vec<u8>],
    patched: &[bool],
    h: &Hunk,
    unidiff_zero: bool,
    p_context: Option<usize>,
    ignore_ws: bool,
) -> Option<Placed> {
    let mut cur = h.clone();
    // apply.c:3134 — `pos = frag->newpos ? (frag->newpos - 1) : 0`, which the
    // reduction loop then moves back one line for every leading context line it
    // drops. It can go negative, and `find_pos()` reads that as "start from the end
    // of the file".
    let mut expected = if h.new_pos == 0 { 0 } else { h.new_pos as isize - 1 };
    // "a hunk that is (oldpos <= 1) with or without leading context must match at
    // the beginning"; "a hunk without trailing lines must match at the end" — both
    // defeated by `--unidiff-zero`, which makes the absence of context
    // uninformative. Computed once, from the hunk as written, and dropped as a
    // whole on the first retry.
    let mut match_beginning = h.old_pos == 0 || (h.old_pos == 1 && !unidiff_zero);
    let mut match_end = !unidiff_zero && h.trailing == 0;
    // `state->p_context` is `UINT_MAX` by default, so the limit test below is
    // satisfied at once and no reduction happens unless `-C<n>` asked for one.
    let floor = p_context.unwrap_or(usize::MAX);
    loop {
        if let Some(at) = find_pos(
            image,
            patched,
            &cur.pre,
            expected,
            match_beginning,
            match_end,
            ignore_ws,
            cur.eof_fudge,
        ) {
            let (leading, trailing) = (cur.leading, cur.trailing);
            let reduced = leading != h.leading || trailing != h.trailing;
            return Some(Placed {
                at,
                hunk: reduced.then_some(cur),
                expected,
                leading,
                trailing,
            });
        }
        // "Am I at my context limits?"
        if cur.leading <= floor && cur.trailing <= floor {
            return None;
        }
        if match_beginning || match_end {
            match_beginning = false;
            match_end = false;
            continue;
        }
        if cur.leading >= cur.trailing {
            cur.leading -= 1;
            cur.pre.remove(0);
            cur.pre_common.remove(0);
            cur.post.remove(0);
            cur.post_common.remove(0);
            if !cur.context.is_empty() {
                cur.context.remove(0);
            }
            expected -= 1;
        }
        if cur.trailing > cur.leading {
            cur.trailing -= 1;
            cur.pre.pop();
            cur.pre_common.pop();
            cur.post.pop();
            cur.post_common.pop();
            cur.context.pop();
            // `image_remove_last_line()` takes the shortened line away with
            // everything else, so what is left ends in a newline again.
            cur.eof_fudge = false;
        }
    }
}

/// Locate `pre` in `image`, starting at `line` and walking outward one line at a
/// time, alternating backwards then forwards exactly as git does (so a patch
/// that could land in two places lands where git lands it).
fn find_pos(
    image: &[Vec<u8>],
    patched: &[bool],
    pre: &[Vec<u8>],
    start: isize,
    match_beginning: bool,
    match_end: bool,
    ignore_ws: bool,
    eof_fudge: bool,
) -> Option<usize> {
    let mut line = if match_beginning {
        0
    } else if match_end {
        image.len() as isize - pre.len() as isize
    } else {
        start
    };
    // apply.c:2847-2853 compares as `size_t`, so a negative line — which
    // `match_end` with an over-long pre-image, and the reduction loop's `pos--`,
    // both produce — wraps past the image and is clamped to its end.
    if line < 0 || line as usize > image.len() {
        line = image.len() as isize;
    }
    let line = line as usize;

    let (mut backwards, mut forwards, mut current) = (line, line, line);
    let mut i: usize = 0;
    loop {
        if matches_at(image, patched, pre, current, match_beginning, match_end, ignore_ws, eof_fudge) {
            return Some(current);
        }
        // Pick the next candidate: odd steps go backwards, even steps forwards,
        // skipping (and burning a step on) a direction that has run out.
        loop {
            if backwards == 0 && forwards == image.len() {
                return None;
            }
            if i % 2 == 1 {
                if backwards == 0 {
                    i += 1;
                    continue;
                }
                backwards -= 1;
                current = backwards;
            } else {
                if forwards == image.len() {
                    i += 1;
                    continue;
                }
                forwards += 1;
                current = forwards;
            }
            break;
        }
        i += 1;
    }
}

/// Whether `pre` sits in `image` at line `at`, honouring the anchoring flags.
///
/// `eof_fudge` is `--inaccurate-eof`: the pre-image's last line had its newline
/// taken off, and git compares the shortened buffer, so that line matches the
/// file's as a prefix.
fn matches_at(
    image: &[Vec<u8>],
    // `img->line[].flag & LINE_PATCHED` (apply.c:2650), one entry per image line.
    patched: &[bool],
    pre: &[Vec<u8>],
    at: usize,
    match_beginning: bool,
    match_end: bool,
    ignore_ws: bool,
    eof_fudge: bool,
) -> bool {
    if at + pre.len() > image.len() {
        return false;
    }
    // "Quick hash check" (apply.c:2648-2655): a line an earlier fragment of this same
    // patch already wrote is off limits, so a hunk cannot match text the patch itself
    // produced. `--allow-overlap` is the flag that stops the marking in the first
    // place (apply.c:2969), which is why nothing here consults it.
    if patched[at..at + pre.len()].iter().any(|&p| p) {
        return false;
    }
    if match_end && at + pre.len() != image.len() {
        return false;
    }
    if match_beginning && at != 0 {
        return false;
    }
    if eof_fudge && !pre.is_empty() {
        let last = pre.len() - 1;
        if image[at..at + last] == pre[..last]
            && image[at + last].starts_with(&pre[last])
        {
            return true;
        }
    } else if image[at..at + pre.len()] == *pre {
        return true;
    }
    // `match_fragment()` tries the byte-exact comparison first and only then, under
    // `--ignore-whitespace`, `line_by_line_fuzzy_match()`. Its trailing check that
    // whatever of the pre-image runs past EOF is blank cannot fire here: the
    // pre-image is only allowed to overrun the file under `--whitespace=fix`
    // (`correct_ws_error`), and the length test above has already ruled it out.
    ignore_ws
        && image[at..at + pre.len()]
            .iter()
            .zip(pre)
            .all(|(a, b)| fuzzy_matchlines(a, b))
}

/// C's `isspace()` in the C locale, which is what apply.c compares against — one
/// character wider than Rust's `is_ascii_whitespace` (vertical tab).
fn c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `fuzzy_matchlines()` (apply.c:2500): the two lines are equal once every run of
/// whitespace is collapsed — but a run may not vanish, so `a b` still does not
/// match `ab`. Line endings are ignored on both sides.
fn fuzzy_matchlines(s1: &[u8], s2: &[u8]) -> bool {
    let trim = |s: &[u8]| {
        let mut end = s.len();
        while end > 0 && (s[end - 1] == b'\r' || s[end - 1] == b'\n') {
            end -= 1;
        }
        end
    };
    let (e1, e2) = (trim(s1), trim(s2));
    let (mut i, mut j) = (0, 0);
    while i < e1 && j < e2 {
        if c_space(s1[i]) {
            if !c_space(s2[j]) {
                return false;
            }
            while i < e1 && c_space(s1[i]) {
                i += 1;
            }
            while j < e2 && c_space(s2[j]) {
                j += 1;
            }
        } else if s1[i] != s2[j] {
            return false;
        } else {
            i += 1;
            j += 1;
        }
    }
    // "If we reached the end on one side only, lines don't match."
    i == e1 && j == e2
}

// ---------------------------------------------------------------------------
// patch parsing
// ---------------------------------------------------------------------------

/// `error(_("corrupt patch at %s:%d"))` — every `return -1` out of apply.c's
/// `parse_fragment()` surfaces as this one message, naming the patch input and
/// the line the parser had reached, and leaves `git apply` exiting 128.
///
/// Carried as its own error type so the entry point can tell it from the
/// generic failures that share the parse path and reproduce git's wording and
/// exit code instead of the crate-wide `zvcs: apply: …` form.
#[derive(Debug)]
struct CorruptPatch(String);

impl std::fmt::Display for CorruptPatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "corrupt patch at {}", self.0)
    }
}

impl std::error::Error for CorruptPatch {}

/// A header diagnostic that already reads the way git prints it, reported through
/// `error()` and unwound to exit 128 exactly as a corrupt fragment is.
#[derive(Debug)]
struct HeaderError(String);

impl std::fmt::Display for HeaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HeaderError {}

/// `apply_state.patch_input_file` + `apply_state.linenr`.
///
/// git parses each `<patch>` argument on its own, resetting `linenr` per file,
/// so `corrupt patch at <file>:<line>` names the file the hunk came from and the
/// line within *that* file. The inputs are concatenated into one buffer here, so
/// this records the line each one started at and maps back.
struct InputSpans {
    /// `(name, index of the input's first line in the concatenated buffer)`,
    /// in the order the inputs were read.
    spans: Vec<(String, usize)>,
}

impl InputSpans {
    /// [`CorruptPatch`] for the (0-based) line `idx`, in the input that the
    /// (0-based) line `anchor` belongs to.
    ///
    /// The two differ when a fragment's body runs past the end of its own input:
    /// git, parsing one file at a time, simply runs out of bytes and reports the
    /// line one past that file's last, so the input is chosen by where the
    /// fragment *started* rather than by where the scan stopped.
    /// The `<input>:<line>` a (0-based) index in the concatenated buffer belongs to.
    fn location(&self, idx: usize) -> (String, usize) {
        let (name, start) = self
            .spans
            .iter()
            .rev()
            .find(|(_, start)| *start <= idx)
            .map(|(name, start)| (name.clone(), *start))
            .unwrap_or_else(|| ("<stdin>".to_string(), 0));
        (name, idx - start + 1)
    }

    fn corrupt_at(&self, anchor: usize, idx: usize) -> anyhow::Error {
        let (name, start) = self
            .spans
            .iter()
            .rev()
            .find(|(_, start)| *start <= anchor)
            .map(|(name, start)| (name.as_str(), *start))
            .unwrap_or(("<stdin>", 0));
        anyhow::Error::new(CorruptPatch(format!("{name}:{}", idx - start + 1)))
    }
}

/// Split `buf` into lines that keep their trailing newline; a final line without
/// one is kept as-is.
fn split_lines(buf: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, &b) in buf.iter().enumerate() {
        if b == b'\n' {
            out.push(&buf[start..=i]);
            start = i + 1;
        }
    }
    if start < buf.len() {
        out.push(&buf[start..]);
    }
    out
}

/// A line as text with its terminator removed, for header matching.
fn txt(line: &[u8]) -> String {
    let end = line.len() - usize::from(line.last() == Some(&b'\n'));
    String::from_utf8_lossy(&line[..end]).into_owned()
}

/// Scan the whole input for patch headers, skipping any surrounding prose
/// (commit messages, mail headers) as git does.
fn parse_patches(
    lines: &[&[u8]],
    strip: usize,
    // `state->p_value_known`: `-p<n>` was given, so no traditional patch may infer
    // its own value.
    strip_explicit: bool,
    // The invocation prefix, which `guess_p_value()` matches names against, and
    // `--directory=<root>` (slash-terminated), which it prepends first.
    prefix: &str,
    root: &str,
    recount: bool,
    spans: &InputSpans,
) -> Result<Vec<Patch>> {
    let mut out = Vec::new();
    // `state->p_value` / `state->p_value_known`: the running pair
    // `parse_traditional_patch()` fixes on the first traditional patch whose two
    // name lines agree, and every later patch in the same input then reuses.
    let mut strip = strip;
    let mut known = strip_explicit;
    let mut i = 0;
    while i < lines.len() {
        let l = txt(lines[i]);
        if l.starts_with("diff --git ") {
            let (p, next) = parse_one(lines, i, strip, true, recount, spans)?;
            i = next;
            out.push(p);
        } else if l.starts_with("--- ")
            && lines.get(i + 1).map(|n| txt(n).starts_with("+++ ")) == Some(true)
        {
            if !known {
                // `parse_traditional_patch()` (apply-lib.c:865): guess from both
                // sides, let a `/dev/null` side defer to the other, and adopt the
                // value only when the two agree.
                let p = guess_p_value(&txt(lines[i])[4..], root, prefix);
                let q = guess_p_value(&txt(lines[i + 1])[4..], root, prefix);
                let p = p.or(q);
                if let (Some(p), Some(q)) = (p, q) {
                    if p == q {
                        strip = p;
                        known = true;
                    }
                }
            }
            let (p, next) = parse_one(lines, i, strip, false, recount, spans)?;
            i = next;
            out.push(p);
        } else {
            i += 1;
        }
    }
    // "Empty patch cannot be applied if it is a text patch without metadata
    // change" (apply.c): a header with no fragments, no binary payload and nothing
    // `metadata_changes()` recognises leaves nothing to do, and git's list never
    // gets it — which is what turns a lone `diff --git a/x b/x` into
    // `No valid patches in input`.
    out.retain(|p| !p.hunks.is_empty() || p.binary || metadata_changes(p));
    Ok(out)
}

/// `metadata_changes()` (apply.c): whether a fragment-less patch still says
/// something — a rename, a copy, a creation, a deletion or a mode change.
fn metadata_changes(p: &Patch) -> bool {
    p.is_rename
        || p.is_copy
        || p.is_new
        || p.is_delete
        || matches!((p.old_mode, p.new_mode), (Some(a), Some(b)) if a != b)
}

/// Parse one file's patch beginning at `start`, returning it and the index of
/// the first line after it.
fn parse_one(
    lines: &[&[u8]],
    start: usize,
    strip: usize,
    git_style: bool,
    recount: bool,
    spans: &InputSpans,
) -> Result<(Patch, usize)> {
    let mut p = Patch {
        old_name: None,
        new_name: None,
        old_mode: None,
        new_mode: None,
        is_new: false,
        is_delete: false,
        is_rename: false,
        is_copy: false,
        binary: false,
        binary_forward: None,
        binary_reverse: None,
        index_old: None,
        index_new: None,
        score: 0,
        is_toplevel_relative: git_style,
        hunks: Vec::new(),
        added: 0,
        deleted: 0,
    };
    let mut i = start;
    // The `--- `/`+++ ` name lines of a traditional patch, kept raw: git resolves
    // the two together in `parse_traditional_patch()` rather than one at a time,
    // so the pair is only usable once both have been read.
    let (mut trad_old, mut trad_new) = (None, None);
    // `parse_chunk()`'s handover to `parse_binary()`: `(line the header parse
    // stopped at, first line after the binary section)`.
    let mut binary_stop: Option<(usize, usize)> = None;

    // `patch->def_name` (apply.c): the name off the `diff --git` line is only a
    // *fallback*. `parse_git_diff_header()` installs it after the extended-header
    // table has run, and only when neither `---` nor `+++` supplied a name — so a
    // header carrying one of the two and not the other is a parse error rather than
    // a patch to the `diff --git` name. Assigning both up front hid that.
    let mut def_name: Option<String> = None;
    if git_style {
        let header = txt(lines[i]);
        if let Some((a, _)) = git_header_names(&header["diff --git ".len()..], strip)? {
            def_name = Some(a);
        }
        i += 1;
    }

    // Extended headers, then the `---`/`+++` pair, in whatever order they appear.
    while i < lines.len() {
        let l = txt(lines[i]);
        // `parse_mode_line()` (apply.c): a mode `strtoul` cannot consume whole is
        // `error("invalid mode at %s:%d: %s")` over the rest of the line — newline
        // included, which is why the diagnostic is followed by a blank one — and
        // unwinds to `git apply`'s exit 128.
        let mode = |rest: &str| -> Result<u32> {
            octal(rest).map_err(|_| {
                let (file, line) = spans.location(i);
                anyhow::Error::new(HeaderError(format!("invalid mode at {file}:{line}: {rest}\n")))
            })
        };
        if let Some(rest) = l.strip_prefix("new file mode ") {
            p.is_new = true;
            p.new_mode = Some(mode(rest)?);
        } else if let Some(rest) = l.strip_prefix("deleted file mode ") {
            p.is_delete = true;
            p.old_mode = Some(mode(rest)?);
        } else if let Some(rest) = l.strip_prefix("new mode ") {
            p.new_mode = Some(mode(rest)?);
        } else if let Some(rest) = l.strip_prefix("old mode ") {
            // The pre-image mode drives the summary's `mode change` line.
            p.old_mode = Some(mode(rest)?);
        } else if let Some(rest) = l.strip_prefix("rename from ") {
            p.is_rename = true;
            p.old_name = rename_path(rest, strip)?;
        } else if let Some(rest) = l.strip_prefix("rename to ") {
            p.is_rename = true;
            p.new_name = rename_path(rest, strip)?;
        } else if let Some(rest) = l.strip_prefix("copy from ") {
            p.is_copy = true;
            p.old_name = rename_path(rest, strip)?;
        } else if let Some(rest) = l.strip_prefix("copy to ") {
            p.is_copy = true;
            p.new_name = rename_path(rest, strip)?;
        } else if let Some(rest) = l.strip_prefix("similarity index ") {
            // Drives the `(N%)` in the summary's rename line.
            p.score = rest.trim().trim_end_matches('%').parse().unwrap_or(0);
        } else if l.starts_with("dissimilarity index ") {
            // Rename/copy scoring; irrelevant to application.
        } else if let Some(rest) = l.strip_prefix("index ") {
            // `index <old>..<new> <mode>` carries the mode when it did not change;
            // git creates the result with it, so an executable file stays one.
            if let Some((_, m)) = rest.split_once(' ') {
                if p.new_mode.is_none() {
                    p.new_mode = Some(mode(m)?);
                }
            }
            // The ids themselves matter to a binary patch, which is only applied
            // when the line named them in full.
            let ids = rest.split(' ').next().unwrap_or("");
            if let Some((old, new)) = ids.split_once("..") {
                p.index_old = Some(old.to_string());
                p.index_new = Some(new.to_string());
            }
        } else if let Some(rest) = l.strip_prefix("--- ") {
            if git_style {
                p.old_name = header_path(rest, strip)?;
            } else {
                trad_old = Some(rest.to_string());
            }
        } else if let Some(rest) = l.strip_prefix("+++ ") {
            if git_style {
                p.new_name = header_path(rest, strip)?;
            } else {
                trad_new = Some(rest.to_string());
            }
        } else if l.starts_with("GIT binary patch") || l.starts_with("Binary files ") {
            p.binary = true;
            let stop = i;
            i += 1;
            // `parse_binary()`: the forward payload, then the reverse one when the
            // patch was written with `--binary`. A missing reverse half is not an
            // error; a *corrupt* one is, and so is a corrupt forward half — both
            // stop the parse with git's own `corrupt binary patch at <file>:<line>: `.
            let corrupt = |idx: usize| -> anyhow::Error {
                let (name, line) = spans.location(idx);
                anyhow::Error::new(HeaderError(format!(
                    "corrupt binary patch at {name}:{line}: "
                )))
            };
            match parse_binary_block(lines, i) {
                Err(at) => return Err(corrupt(at)),
                Ok(Some((forward, next))) => {
                    p.binary_forward = Some(forward);
                    i = next;
                    match parse_binary_block(lines, i) {
                        Err(at) => return Err(corrupt(at)),
                        Ok(Some((reverse, next))) => {
                            p.binary_reverse = Some(reverse);
                            i = next;
                        }
                        Ok(None) => {}
                    }
                }
                Ok(None) => {}
            }
            // Consume whatever is left of the section.
            while i < lines.len() {
                let n = txt(lines[i]);
                if n.starts_with("diff --git ") || n.starts_with("--- ") {
                    break;
                }
                i += 1;
            }
            binary_stop = Some((stop, i));
            break;
        } else {
            break;
        }
        i += 1;
    }

    // `parse_git_diff_header()`'s `done:` (apply.c:1425), which every exit from the
    // header table reaches: the line the parse stopped at is the one both
    // filename diagnostics report.
    let hdr_stop = binary_stop.map_or(i, |(stop, _)| stop);
    if !git_style {
        resolve_traditional(&mut p, trad_old.as_deref(), trad_new.as_deref(), strip)?;
    }
    // "Some things may not have the old name in the rest of the headers anywhere
    // (pure mode changes, or removing or adding empty files), so we get the default
    // name from the header."
    if p.old_name.is_none() && p.new_name.is_none() {
        if let Some(def) = &def_name {
            p.old_name = Some(def.clone());
            p.new_name = Some(def.clone());
        }
    }
    require_names(&p, git_style, strip, spans, if git_style { hdr_stop } else { start })?;
    // The second check `parse_git_diff_header()` makes once the fallback has had
    // its chance: a header that named one side and not the other, without saying
    // the other side is absent, is not a patch.
    if git_style
        && ((p.new_name.is_none() && !p.is_delete) || (p.old_name.is_none() && !p.is_new))
    {
        let (file, line) = spans.location(hdr_stop);
        return Err(anyhow::Error::new(HeaderError(format!(
            "git diff header lacks filename information at {file}:{line}"
        ))));
    }

    if let Some((_, next)) = binary_stop {
        return Ok((normalise(p)?, next));
    }

    while i < lines.len() && txt(lines[i]).starts_with("@@ ") {
        let (h, added, deleted, next) = parse_hunk(lines, i, recount, spans)?;
        p.added += added;
        p.deleted += deleted;
        p.hunks.push(h);
        i = next;
    }

    Ok((normalise(p)?, i))
}

/// `parse_traditional_patch()` (apply.c:856): the two name lines are resolved
/// together, not one at a time. A `/dev/null` on either side makes the patch a
/// creation or a deletion and the other line supplies the name; otherwise the
/// `+++` line is read with the `---` line's name as `find_name_common()`'s `def`,
/// and the single name that comes out is used for *both* sides. That is what lets
/// `-p<n>` over-strip one side without failing, and what makes `--- a/f.txt.orig`
/// / `+++ a/f.txt` a patch to `f.txt` rather than a rename.
fn resolve_traditional(
    p: &mut Patch,
    first: Option<&str>,
    second: Option<&str>,
    strip: usize,
) -> Result<()> {
    let (Some(first), Some(second)) = (first, second) else {
        return Ok(());
    };
    if is_dev_null(&first[..name_end(first)]) {
        p.is_new = true;
        p.new_name = header_path(second, strip)?;
    } else if is_dev_null(&second[..name_end(second)]) {
        p.is_delete = true;
        p.old_name = header_path(first, strip)?;
    } else {
        let def = header_path(first, strip)?;
        let name = match (header_path(second, strip)?, def) {
            // "Generally we prefer the shorter name, especially if the other one
            // is just a variation of that with something else tacked on to the
            // end (ie "file.orig" or "file~")."
            (Some(name), Some(def)) if def.len() < name.len() && name.starts_with(&def) => {
                Some(def)
            }
            (Some(name), _) => Some(name),
            // `find_name_common()` falls back to `def` when the second line
            // yields nothing.
            (None, def) => def,
        };
        p.old_name = name.clone();
        p.new_name = name;
    }
    Ok(())
}

/// The two "this header named no file" diagnostics: `parse_git_diff_header()`'s
/// `done:` block (apply.c:1425) for a `diff --git` header, and
/// `parse_traditional_patch()`'s tail (apply.c:904) for a `---`/`+++` pair. Both
/// carry `state->patch_input_file` and the line the parse was sitting on — the
/// header's last line for a git patch, the `---` line for a traditional one.
fn require_names(
    p: &Patch,
    git_style: bool,
    strip: usize,
    spans: &InputSpans,
    idx: usize,
) -> Result<()> {
    if p.old_name.is_some() || p.new_name.is_some() {
        return Ok(());
    }
    let (file, line) = spans.location(idx);
    let msg = if git_style {
        // `Q_()`: singular for one component only.
        let unit = if strip == 1 { "component" } else { "components" };
        format!(
            "git diff header lacks filename information when removing \
             {strip} leading pathname {unit} at {file}:{line}"
        )
    } else {
        format!("unable to find filename in patch at {file}:{line}")
    };
    Err(anyhow::Error::new(HeaderError(msg)))
}

/// Reconcile the creation/deletion flags with the two names, so that exactly one
/// side is `None` for a creation or deletion.
fn normalise(mut p: Patch) -> Result<Patch> {
    if p.old_name.is_none() && p.new_name.is_none() {
        crate::git_fatal!("corrupt patch: no file name in the header");
    }
    if p.old_name.is_none() {
        p.is_new = true;
    }
    if p.new_name.is_none() {
        p.is_delete = true;
    }
    if p.is_new {
        p.old_name = None;
    }
    if p.is_delete {
        p.new_name = None;
    }
    Ok(p)
}

/// Parse an `@@ -a,b +c,d @@` fragment and its body.
///
/// `recount` is `--recount`: the counts in the header are not trusted, so the
/// body runs until the first line that is not a body line instead of until the
/// header's counts are exhausted, and a mismatch is not an error.
fn parse_hunk(
    lines: &[&[u8]],
    start: usize,
    recount: bool,
    spans: &InputSpans,
) -> Result<(Hunk, usize, usize, usize)> {
    let header = txt(lines[start]);
    // `parse_fragment_header()` failing is `parse_fragment()` returning -1 while
    // `state->linenr` still points at the `@@` line.
    let (old_pos, mut old_rem, new_pos, mut new_rem) =
        hunk_range(&header).ok_or_else(|| spans.corrupt_at(start, start))?;

    let mut h = Hunk {
        old_pos,
        new_pos,
        pre: Vec::new(),
        pre_common: Vec::new(),
        post: Vec::new(),
        post_common: Vec::new(),
        body: Vec::new(),
        context: Vec::new(),
        raw: Vec::new(),
        trailing: 0,
        leading: 0,
        eof_fudge: false,
    };
    let (mut added, mut deleted) = (0usize, 0usize);
    let mut last = Side::None;
    let mut i = start + 1;

    while i < lines.len() {
        let raw = lines[i];
        // `\ No newline at end of file` retracts the newline from the line just
        // read, on whichever image(s) that line joined.
        if raw.first() == Some(&b'\\') {
            match last {
                Side::Context => {
                    drop_newline(h.pre.last_mut());
                    drop_newline(h.post.last_mut());
                }
                Side::Pre => drop_newline(h.pre.last_mut()),
                Side::Post => drop_newline(h.post.last_mut()),
                Side::None => {}
            }
            i += 1;
            continue;
        }
        if !recount && old_rem == 0 && new_rem == 0 {
            break;
        }
        // A context line whose single leading space was stripped in transit is
        // still a context line; git accepts the bare newline.
        let (marker, body): (u8, &[u8]) = match raw.first() {
            Some(&b'\n') | None => (b' ', &b"\n"[..]),
            Some(&c) if c == b' ' || c == b'+' || c == b'-' => (c, &raw[1..]),
            _ => break,
        };
        match marker {
            b' ' => {
                if added == 0 && deleted == 0 {
                    h.leading += 1;
                }
                h.body.push((i, b' ', h.pre.len()));
                h.pre.push(body.to_vec());
                h.pre_common.push(true);
                h.post.push(body.to_vec());
                h.post_common.push(true);
                h.context.push(body.to_vec());
                h.trailing += 1;
                last = Side::Context;
                old_rem = old_rem.saturating_sub(1);
                new_rem = new_rem.saturating_sub(1);
            }
            b'-' => {
                h.body.push((i, b'-', h.pre.len()));
                h.pre.push(body.to_vec());
                h.pre_common.push(false);
                h.trailing = 0;
                deleted += 1;
                last = Side::Pre;
                old_rem = old_rem.saturating_sub(1);
            }
            _ => {
                h.body.push((i, b'+', h.post.len()));
                h.post.push(body.to_vec());
                h.post_common.push(false);
                h.trailing = 0;
                added += 1;
                last = Side::Post;
                new_rem = new_rem.saturating_sub(1);
            }
        }
        i += 1;
    }

    // `if (oldlines || newlines) return -1;` — the body ran out (or hit a line
    // that is not a body line) before the header's counts were satisfied.
    // `state->linenr` has been advanced past every line consumed, so the line
    // reported is the one that stopped the scan, or the first line past the
    // input when it simply ended.
    if !recount && (old_rem != 0 || new_rem != 0) {
        return Err(spans.corrupt_at(start, i));
    }
    // `if (!patch->recount && !deleted && !added) return -1;` — a fragment that
    // is nothing but context changes nothing, so git calls the patch corrupt
    // rather than silently applying a no-op. `--recount` exempts it: the counts
    // are then derived from the body, and an all-context body is how a hunk
    // whose `+`/`-` lines were mangled in transit still reaches `recount_diff`.
    if !recount && added == 0 && deleted == 0 {
        return Err(spans.corrupt_at(start, i));
    }
    // The fragment's verbatim bytes (header through the last consumed body line),
    // re-emitted unchanged into a *.rej file when the hunk is rejected.
    for line in &lines[start..i] {
        h.raw.extend_from_slice(line);
    }
    Ok((h, added, deleted, i))
}

/// Which image(s) the most recent body line joined, for the `\ No newline` rule.
enum Side {
    None,
    Context,
    Pre,
    Post,
}

fn drop_newline(line: Option<&mut Vec<u8>>) {
    if let Some(l) = line {
        if l.last() == Some(&b'\n') {
            l.pop();
        }
    }
}

/// `@@ -a[,b] +c[,d] @@ [section]` → `(a, b, c, d)`.
fn hunk_range(header: &str) -> Option<(usize, usize, usize, usize)> {
    let rest = header.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let new = rest.split_once(" @@")?.0;
    let (os, oc) = one_range(old)?;
    let (ns, nc) = one_range(new)?;
    Some((os, oc, ns, nc))
}

fn one_range(s: &str) -> Option<(usize, usize)> {
    match s.split_once(',') {
        Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

// ---------------------------------------------------------------------------
// path handling
// ---------------------------------------------------------------------------

/// A `---`/`+++` header path: the name as [`name_end`] delimits it (traditional
/// diffs append a timestamp after a tab), `/dev/null` meaning "this side does not
/// exist".
fn header_path(rest: &str, strip: usize) -> Result<Option<String>> {
    let name = &rest[..name_end(rest)];
    if is_dev_null(name) {
        return Ok(None);
    }
    Ok(strip_path(&unquote(name)?, strip)?.map(|n| squash_slash(&n)))
}

/// A `rename from`/`rename to` name: `find_name()` with `terminate == 0` and one
/// fewer leading component than `-p<n>` asks for, because the name here has no
/// `a/`/`b/` prefix on it (apply.c:1073).
fn rename_path(rest: &str, strip: usize) -> Result<Option<String>> {
    let name = &rest[..name_end_no_term(rest)];
    Ok(strip_path(&unquote(name)?, strip.saturating_sub(1))?.map(|n| squash_slash(&n)))
}

/// `git_isspace()` — git compiles against `sane-ctype.h`, whose `isspace()` is the
/// four bytes flagged `GIT_SPACE` in `sane_ctype[]` (ctype.c:21-23): space, `\t`,
/// `\n`, `\r`. `\v` and `\f` are `GIT_CNTRL` only, so they are *not* whitespace to
/// any of apply.c's scans, and Rust's Unicode-aware `char::is_whitespace` would
/// wrongly accept them (plus NBSP and friends).
fn git_isspace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// Where `find_name_common()`'s scan (apply.c:666-678) stops under `TERM_TAB`, which
/// is what `gitdiff_verify_name()` (apply.c:936) and `find_name_traditional()`
/// (apply.c:737) both pass: the first `isspace()` byte that `name_terminate()`
/// (apply.c:437) does not exempt. Under `TERM_TAB` only a plain space is exempt, so a
/// tab, a newline **or a carriage return** ends the name.
///
/// The `\r` arm is what makes `git am --keep-cr` of a CRLF patch behave: every header
/// line then ends `…\r\n`, and without this the path would come out as `f.txt\r` and
/// miss both the index and the working tree.
fn name_end(s: &str) -> usize {
    s.bytes()
        .position(|b| b != b' ' && git_isspace(b))
        .unwrap_or(s.len())
}

/// The same scan under `terminate == 0`, which is what `gitdiff_renamesrc()`/
/// `gitdiff_renamedst()` pass (apply.c:1073, :1083): `name_terminate()` exempts both
/// a space and a tab, so a `rename from`/`rename to` name may contain either and ends
/// only at a newline or a carriage return.
fn name_end_no_term(s: &str) -> usize {
    s.bytes()
        .position(|b| b == b'\n' || b == b'\r')
        .unwrap_or(s.len())
}

/// `squash_slash()` (apply.c:448): collapse runs of `/` into one. Every name that
/// comes back from `find_name_common()`/`find_name_gnu()` goes through it, which is
/// why `--- a/sub//g.txt` patches `sub/g.txt`.
///
/// `git_header_name()` deliberately does *not* call it — its result only ever becomes
/// `patch->def_name`, so a pure mode change on `a/sub//g.txt b/sub//g.txt` keeps the
/// doubled slash and is refused as `invalid path 'sub//g.txt'` by both git and this
/// port.
fn squash_slash(name: &str) -> String {
    if !name.contains("//") {
        return name.to_string();
    }
    let mut out = String::with_capacity(name.len());
    let mut prev_slash = false;
    for c in name.chars() {
        if c == '/' && prev_slash {
            continue;
        }
        prev_slash = c == '/';
        out.push(c);
    }
    out
}

/// `is_dev_null()` (apply.c:429): the name is `/dev/null` followed by an `isspace()`
/// byte. The name has already had its terminator taken off here, so "followed by
/// nothing" stands in for git's "followed by the `\n` that ends the line".
fn is_dev_null(name: &str) -> bool {
    match name.strip_prefix("/dev/null") {
        Some(rest) => rest.is_empty() || rest.bytes().next().is_some_and(git_isspace),
        None => false,
    }
}

/// `guess_p_value()` (apply-lib.c:747): the `-p<n>` a *traditional* (non-`diff
/// --git`) patch implies, or `None` when the name says nothing.
///
/// `parse_traditional_patch()` runs it on both name lines of the first such patch
/// and, when the two agree, fixes `p_value` for the rest of the input. This is what
/// lets `diff -u old new > p` apply with no `-p0`: a name with no slash at all can
/// only be meant whole, so the answer is 0.
///
/// `nameline` is the text after `--- `/`+++ `, and the name is read with `p_value`
/// 0 — the whole thing, timestamp trimmed and unquoted, with `--directory=<root>`
/// already in front of it, because `find_name_common()` prepends `state->root`
/// before the guess ever looks for a slash. That is why `--directory=X` changes the
/// answer for a one-component name: `X/s.txt` has a directory part and `s.txt`
/// does not.
fn guess_p_value(nameline: &str, root: &str, prefix: &str) -> Option<usize> {
    let name = header_path(nameline, 0).ok().flatten()?;
    let name = if root.is_empty() { name } else { format!("{root}{name}") };
    let Some(slash) = name.find('/') else {
        // No directory part: the name is already relative to the worktree root.
        return Some(0);
    };
    if prefix.is_empty() {
        return None;
    }
    // "Does it begin with `a/$our-prefix` and such?  Then this is very likely to
    // apply to our directory."
    let slashes = prefix.matches('/').count();
    if name.starts_with(prefix) {
        return Some(slashes);
    }
    if name[slash + 1..].starts_with(prefix) {
        return Some(slashes + 1);
    }
    None
}

/// Both names off a `diff --git a/x b/y` line.
///
/// Quoted forms are unquoted; otherwise we take git's rule of accepting a split
/// only when the two halves are the same path after stripping, which is the case
/// that matters here — a header with no `---`/`+++` pair is a pure mode change,
/// where both sides name the same file.
fn git_header_names(rest: &str, strip: usize) -> Result<Option<(String, String)>> {
    // `rest` is reindexed at original offsets (`rest[..=end]`, `rest[end + 2..]`),
    // so rebasing onto a stripped slice would not be behavior-identical.
    #[allow(clippy::manual_strip)]
    if rest.starts_with('"') {
        if let Some(end) = rest[1..].find('"').map(|i| i + 1) {
            let (Some(a), Some(b)) = (
                strip_path(&unquote(&rest[..=end])?, strip)?,
                strip_path(&unquote(rest[end + 2..].trim())?, strip)?,
            ) else {
                return Ok(None);
            };
            return Ok(Some((a, b)));
        }
        return Ok(None);
    }
    for (idx, _) in rest.match_indices(' ') {
        let (Ok(Some(a)), Ok(Some(b))) = (
            strip_path(&rest.as_bytes()[..idx], strip),
            strip_path(&rest.as_bytes()[idx + 1..], strip),
        ) else {
            continue;
        };
        if a == b {
            return Ok(Some((a, b)));
        }
    }
    Ok(None)
}

/// Drop `n` leading slash-separated components, as `-p<n>` asks.
///
/// `None` is `find_name_common()` (apply.c:654) returning NULL: the name ran out
/// of components before `-p<n>` was satisfied (`start` never set), or nothing was
/// left after them (`len == 0`). git does not treat that as an error where it
/// happens — the name is simply absent, and whoever wanted one says so later,
/// which is why the diagnostic can name the header's line.
fn strip_path(name: &[u8], n: usize) -> Result<Option<String>> {
    let mut s: &[u8] = name;
    for _ in 0..n {
        match s.iter().position(|&b| b == b'/') {
            Some(i) => s = &s[i + 1..],
            None => return Ok(None),
        }
    }
    if s.is_empty() {
        return Ok(None);
    }
    let out = String::from_utf8(s.to_vec())
        .map_err(|_| anyhow::anyhow!("non-UTF-8 paths in patches are not supported"))?;
    Ok(Some(out))
}

/// `--directory=<root>`: git's `prefix_one()` — prepend `root` to every patch
/// path, after `-p<n>` has done its stripping. A `/dev/null` side is `None` here
/// (a creation's pre-image, a deletion's post-image) and stays that way.
///
/// Nothing is validated here, exactly as in git: a root that pushes a path out of
/// the working tree (`--directory=/tmp`) produces the joined name and lets
/// [`check_unsafe_path`] rule on it later, per patch.
fn prefix_names(p: &mut Patch, root: &str) {
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return;
    }
    for n in [&mut p.old_name, &mut p.new_name].into_iter().flatten() {
        *n = format!("{root}/{n}");
    }
}

/// `normalize_path_copy_len()` (path.c:1121), the part that runs on POSIX:
/// resolve `.` and `..` components textually and squeeze runs of `/`. `None` is
/// git's `-1` — a relative path that climbs above where it started, or an
/// absolute one that climbs above `/`, which is the only way
/// `apply_option_parse_directory()` can fail.
///
/// The Windows arms of the C (`offset_1st_component()` past a drive letter or a
/// `//server/share` prefix, backslash separators) are not reproduced; on POSIX
/// `offset_1st_component()` is just the leading `/`.
fn normalize_path(src: &str) -> Option<String> {
    let b = src.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0usize;
    // The absolute path's leading `/` is copied through and becomes the floor
    // `..` may not climb past (`dst0` in the C).
    if b.first() == Some(&b'/') {
        out.push(b'/');
        i = 1;
    }
    let floor = out.len();
    while b.get(i) == Some(&b'/') {
        i += 1;
    }

    loop {
        // A component beginning with `.` may be one of the four special forms.
        let mut up = false;
        if b.get(i) == Some(&b'.') {
            match (b.get(i + 1), b.get(i + 2)) {
                // (1) "." and ends: ignore and terminate.
                (None, _) => i += 1,
                // (2) "./": ignore, eat the slash and continue.
                (Some(b'/'), _) => {
                    i += 2;
                    while b.get(i) == Some(&b'/') {
                        i += 1;
                    }
                    continue;
                }
                // (3) ".." and ends: strip one and terminate.
                (Some(b'.'), None) => {
                    i += 2;
                    up = true;
                }
                // (4) "../": strip one, eat the slash and continue.
                (Some(b'.'), Some(b'/')) => {
                    i += 3;
                    while b.get(i) == Some(&b'/') {
                        i += 1;
                    }
                    up = true;
                }
                _ => {}
            }
        }
        if up {
            // The C steps off the trailing '/' first and only then compares
            // against `dst0`, so a `..` with nothing above it is the failure.
            if out.len() <= floor {
                return None;
            }
            out.pop();
            while out.len() > floor && out.last() != Some(&b'/') {
                out.pop();
            }
            continue;
        }
        // Copy up to the next '/', then eat every '/' that follows it.
        while let Some(&c) = b.get(i) {
            if c == b'/' {
                break;
            }
            out.push(c);
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        out.push(b'/');
        while b.get(i) == Some(&b'/') {
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `verify_dotfile()` (read-cache.c:944): rule on a path component that begins
/// with `.`, with the leading dot already consumed. `false` means the whole path
/// is invalid.
fn verify_dotfile(rest: &[u8], is_symlink: bool) -> bool {
    // "." on its own, and "./", are not allowed.
    match rest.first() {
        None | Some(b'/') => return false,
        _ => {}
    }
    match rest[0] {
        // ".git" followed by NUL or a slash, matched case-insensitively even when
        // `ignore_case` is off — ".GIT" is outlawed everywhere out of caution.
        b'g' | b'G' => {
            if !matches!(rest.get(1), Some(b'i' | b'I')) {
                return true;
            }
            if !matches!(rest.get(2), Some(b't' | b'T')) {
                return true;
            }
            if matches!(rest.get(3), None | Some(b'/')) {
                return false;
            }
            // A symlink may not be named ".gitmodules" either.
            if is_symlink {
                let tail = &rest[3..];
                if tail.len() >= 7 && tail[..7].eq_ignore_ascii_case(b"modules") {
                    return !matches!(tail.get(7), None | Some(b'/'));
                }
            }
            true
        }
        // ".." followed by NUL or a slash.
        b'.' => !matches!(rest.get(1), None | Some(b'/')),
        _ => true,
    }
}

/// `verify_path()` (read-cache.c:839): may this path be an index entry at all?
/// The rule runs at the start of the path and again after every `/`, so it rules
/// out an empty name, a leading or doubled or trailing separator, and any `.`,
/// `..` or `.git` component.
///
/// Not reproduced: the `protect_hfs`/`protect_ntfs` arms, which also reject the
/// Unicode- and 8.3-obfuscated spellings of `.git` (`is_hfs_dotgit()`,
/// `is_ntfs_dotgit()`), and the Windows-only `has_dos_drive_prefix()` /
/// `is_valid_path()` gates — on POSIX the latter two are compiled out
/// (git-compat-util.h:224-229, :265).
fn verify_path(path: &str, mode: u32) -> bool {
    let is_symlink = mode & 0o170000 == 0o120000;
    let b = path.as_bytes();
    let mut i = 0usize;
    loop {
        match b.get(i) {
            // The end of the string here is an empty path or a trailing '/'.
            None => return false,
            // A leading '/', or a doubled one.
            Some(b'/') => return false,
            Some(b'.') if !verify_dotfile(&b[i + 1..], is_symlink) => return false,
            _ => {}
        }
        i += 1;
        // Scan to the next separator; the checks above then run on what follows it.
        match b[i..].iter().position(|&c| c == b'/') {
            None => return true,
            Some(off) => i += off + 1,
        }
    }
}

/// `build_fake_ancestor()` (apply.c:4243): write, to the file `--build-fake-ancestor`
/// names, an index holding one entry per patch that is not a creation — the path's
/// pre-image, taken from the blob its `index <old>..` line points at.
///
/// The three ways git finds that blob, in its order: the `index` line's id (however
/// abbreviated) resolved against the object store; failing that, and only for a patch
/// that adds and deletes no line, the path's current index entry (`get_current_oid`,
/// a mode-only change); failing that, the run is over. Submodule (gitlink) pre-images
/// are not read from a `Subproject commit` body here, so a gitlink patch takes the
/// same route as any other.
///
/// git's entries carry the stat `refresh_cache_entry()` fills in; these are zeroed,
/// which no reader of a fake ancestor consults — `git am -3` uses it as a tree.
fn build_fake_ancestor(patches: &[Patch], path: &str, quiet: bool) -> Result<bool> {
    let repo = crate::setup::discover()?;
    let mut result = gix::index::File::from_state(
        gix::index::State::new(repo.object_hash()),
        std::path::PathBuf::from(path),
    );
    // `get_current_oid()`'s source, read once and only if a mode-only change asks.
    let current = || repo.open_index().ok();
    for p in patches {
        if p.is_new {
            continue;
        }
        let name = p.old_name.as_deref().or(p.new_name.as_deref()).unwrap_or_default();
        let id = match p.index_old.as_deref().and_then(|hex| blob_by_prefix(&repo, hex)) {
            Some(id) => id,
            None if p.added == 0 && p.deleted == 0 => {
                match current()
                    .and_then(|idx| idx.entry_by_path(name.as_bytes().as_bstr()).map(|e| e.id))
                {
                    Some(id) => id,
                    None => {
                        err(
                            quiet,
                            &format!("error: mode change for {name}, which is not in current HEAD"),
                        );
                        return Ok(false);
                    }
                }
            }
            None => {
                err(
                    quiet,
                    &format!("error: sha1 information is lacking or useless ({name})."),
                );
                return Ok(false);
            }
        };
        // `make_cache_entry()` runs the name past `verify_path()` too.
        let mode = p.old_mode.or(p.new_mode).unwrap_or(0o100644);
        if !verify_path(name, mode) {
            err(quiet, &format!("error: invalid path '{name}'"));
            err(quiet, &format!("error: make_cache_entry failed for path '{name}'"));
            return Ok(false);
        }
        result.dangerously_push_entry(
            Stat::default(),
            id,
            Flags::empty(),
            to_index_mode(mode),
            BString::from(name.as_bytes().to_vec()).as_ref(),
        );
    }
    result.sort_entries();
    result.write(gix::index::write::Options::default())?;
    Ok(true)
}

/// `repo_get_oid_blob()` for the `index` line's id: git writes it abbreviated, so
/// this resolves a prefix, and only a blob answers.
fn blob_by_prefix(repo: &gix::Repository, hex: &str) -> Option<ObjectId> {
    let prefix = gix::hash::Prefix::from_hex(hex).ok()?;
    let id = match repo.objects.lookup_prefix(prefix, None).ok()?? {
        Ok(id) => id,
        // Ambiguous — git's `get_oid` would refuse it too.
        Err(()) => return None,
    };
    let obj = repo.try_find_object(id).ok()??;
    (obj.kind == gix::object::Kind::Blob).then_some(id)
}

/// `check_unsafe_path()` (apply.c:4036): the per-patch gate `--unsafe-paths`
/// waives. Returns the offending name, which the caller reports as git does —
/// `error: invalid path '<name>'`, and then exit 128 for the whole run.
///
/// Which names are examined is git's own selection: a deletion is judged on the
/// path it removes, a creation on the path it makes, and everything else on both.
fn check_unsafe_path(p: &Patch) -> Option<&str> {
    let old_name = if p.is_delete || !p.is_new {
        p.old_name.as_deref()
    } else {
        None
    };
    let new_name = if p.is_delete { None } else { p.new_name.as_deref() };
    if let Some(old) = old_name {
        if !verify_path(old, p.old_mode.unwrap_or(0)) {
            return Some(old);
        }
    }
    if let Some(new) = new_name {
        if !verify_path(new, p.new_mode.unwrap_or(0)) {
            return Some(new);
        }
    }
    None
}

/// `prefix_patch()` (apply.c:2191): a patch that is not already root-relative has
/// the invocation prefix prepended to both of its names, exactly as `prefix_one()`
/// does. A `/dev/null` side is `None` here and stays that way.
fn prefix_patch(p: &mut Patch, prefix: &str) {
    if p.is_toplevel_relative {
        return;
    }
    for n in [&mut p.old_name, &mut p.new_name].into_iter().flatten() {
        *n = format!("{prefix}{n}");
    }
}

/// `setup_git_directory()`'s two results, in one step: chdir to the top of the
/// worktree and return the slash-terminated path of the directory the command was
/// invoked from, relative to that top. Empty when already at the top, when the
/// repository is bare, or when there is no repository at all — the three cases where
/// git leaves `state->prefix` NULL and every path in the patch is taken as given.
fn worktree_prefix() -> Result<String> {
    let Ok(repo) = crate::setup::discover() else {
        return Ok(String::new());
    };
    let Some(workdir) = repo.workdir() else {
        return Ok(String::new());
    };
    let root = workdir.canonicalize()?;
    let here = std::env::current_dir()?.canonicalize()?;
    let Ok(rel) = here.strip_prefix(&root) else {
        return Ok(String::new());
    };
    if rel.as_os_str().is_empty() {
        return Ok(String::new());
    }
    // Every path this port compares the prefix against is a slash-joined patch
    // header name, so the prefix is built the same way rather than through the
    // platform separator.
    let mut parts: Vec<String> = Vec::new();
    for c in rel.components() {
        parts.push(c.as_os_str().to_string_lossy().into_owned());
    }
    std::env::set_current_dir(&root)?;
    Ok(format!("{}/", parts.join("/")))
}

/// Undo git's C-style quoting when a header path is wrapped in double quotes.
fn unquote(s: &str) -> Result<Vec<u8>> {
    let b = s.as_bytes();
    if b.len() < 2 || b[0] != b'"' || b[b.len() - 1] != b'"' {
        return Ok(b.to_vec());
    }
    let inner = &b[1..b.len() - 1];
    let mut out = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        if inner[i] != b'\\' {
            out.push(inner[i]);
            i += 1;
            continue;
        }
        i += 1;
        let Some(&c) = inner.get(i) else {
            crate::git_fatal!("corrupt quoted path {s:?}");
        };
        i += 1;
        match c {
            b'a' => out.push(0x07),
            b'b' => out.push(0x08),
            b't' => out.push(b'\t'),
            b'n' => out.push(b'\n'),
            b'v' => out.push(0x0b),
            b'f' => out.push(0x0c),
            b'r' => out.push(b'\r'),
            b'"' | b'\\' => out.push(c),
            b'0'..=b'7' => {
                let mut v = u32::from(c - b'0');
                for _ in 0..2 {
                    match inner.get(i) {
                        Some(&d) if (b'0'..=b'7').contains(&d) => {
                            v = v * 8 + u32::from(d - b'0');
                            i += 1;
                        }
                        _ => break,
                    }
                }
                out.push(v as u8);
            }
            _ => crate::git_fatal!("corrupt quoted path {s:?}"),
        }
    }
    Ok(out)
}

/// `quote_c_style()`: the name verbatim unless some byte needs escaping, in which
/// case the whole name double-quoted with C escapes. The table and the
/// `core.quotePath` flag it reads live in [`crate::quote`], shared with every
/// other verb that prints a path.
fn quote_path(path: &str) -> String {
    crate::quote::quoted_name_string(path.as_bytes())
}

fn octal(s: &str) -> Result<u32> {
    u32::from_str_radix(s.trim(), 8).map_err(|_| anyhow::anyhow!("corrupt file mode {s:?}"))
}

// ---------------------------------------------------------------------------
// output and filesystem
// ---------------------------------------------------------------------------

/// `--numstat`: `<added>\t<deleted>\t<path>`, `-\t-\t` for binary patches, the
/// post-image path (pre-image for a deletion), quoted unless `-z`.
fn render_numstat(patches: &[Patch], nul: bool) -> String {
    let mut out = String::new();
    for p in patches {
        if p.binary {
            out.push_str("-\t-\t");
        } else {
            out.push_str(&format!("{}\t{}\t", p.added, p.deleted));
        }
        let name = p
            .new_name
            .as_deref()
            .or(p.old_name.as_deref())
            .unwrap_or_default();
        if nul {
            out.push_str(name);
            out.push('\0');
        } else {
            out.push_str(&quote_path(name));
            out.push('\n');
        }
    }
    out
}

// ---------------------------------------------------------------------------
// --stat / --summary — port of apply.c:show_stats / summary_patch_list
// ---------------------------------------------------------------------------

/// `--stat`: git's scaled diffstat graph, one line per patch plus a summary
/// tail. A direct port of apply.c's `show_stats` / `stat_patch_list`: the name
/// column is `min(max quoted-name length, 50)` wide, the `+`/`-` graph is scaled
/// so the widest change fills `70 - name_column` columns (or is drawn 1:1 when it
/// already fits), and each line's decimal count is the file's added+deleted total.
fn render_stat(patches: &[Patch]) -> String {
    let mut names: Vec<String> = Vec::with_capacity(patches.len());
    let (mut adds, mut dels, mut max_len, mut max_change) = (0usize, 0usize, 0usize, 0usize);
    for p in patches {
        let raw = p.new_name.as_deref().or(p.old_name.as_deref()).unwrap_or("");
        let q = quote_path(raw);
        // `patch_stats()` (apply.c) widens `max_len` with *both* names, while
        // `show_stats()` prints only the post-image one — so a rename or a copy
        // sizes the column by whichever of its two names is longer.
        for name in [p.old_name.as_deref(), p.new_name.as_deref()].into_iter().flatten() {
            max_len = max_len.max(quote_path(name).len());
        }
        max_change = max_change.max(p.added + p.deleted);
        adds += p.added;
        dels += p.deleted;
        names.push(q);
    }
    let m = max_len.min(50);
    let graph_max = if m + max_change > 70 { 70 - m } else { max_change };

    let mut out = String::new();
    for (p, q) in patches.iter().zip(names.iter()) {
        let display = if q.len() > m { truncate_name(q, m) } else { q.clone() };
        if p.binary {
            out.push_str(&format!(" {display:<m$} |  Bin\n"));
            continue;
        }
        out.push_str(&format!(" {display:<m$} |"));
        let (add, del) = scale_graph(p.added, p.deleted, graph_max, max_change);
        out.push_str(&format!(
            "{:5} {}{}\n",
            p.added + p.deleted,
            "+".repeat(add),
            "-".repeat(del)
        ));
    }
    out.push_str(&stat_summary_line(patches.len(), adds, dels));
    out
}

/// Scale a hunk's add/delete counts into graph columns (apply.c's rounding:
/// `(n * max + max_change/2) / max_change`, with `del` taking the remainder).
fn scale_graph(add: usize, del: usize, graph_max: usize, max_change: usize) -> (usize, usize) {
    if max_change == 0 {
        return (0, 0);
    }
    let total = ((add + del) * graph_max + max_change / 2) / max_change;
    let a = (add * graph_max + max_change / 2) / max_change;
    (a, total - a)
}

/// Truncate an over-long stat name to the column width, keeping a trailing path
/// component and prefixing `...` (apply.c's `strchr` from `len + 3 - max`).
fn truncate_name(q: &str, m: usize) -> String {
    let bytes = q.as_bytes();
    let start = q.len() + 3 - m;
    let cut = bytes[start..]
        .iter()
        .position(|&b| b == b'/')
        .map(|i| start + i)
        .unwrap_or(start);
    format!("...{}", &q[cut..])
}

/// The `--stat` tail: `N files changed, X insertions(+), Y deletions(-)`, with
/// git's singular/plural forms and the clause-omission rules from diff.c's
/// `print_stat_summary`.
fn stat_summary_line(files: usize, ins: usize, del: usize) -> String {
    if files == 0 {
        return " 0 files changed\n".to_string();
    }
    let mut s = format!(" {} {} changed", files, if files == 1 { "file" } else { "files" });
    if ins > 0 || del == 0 {
        s.push_str(&format!(
            ", {} {}(+)",
            ins,
            if ins == 1 { "insertion" } else { "insertions" }
        ));
    }
    if del > 0 || ins == 0 {
        s.push_str(&format!(
            ", {} {}(-)",
            del,
            if del == 1 { "deletion" } else { "deletions" }
        ));
    }
    s.push('\n');
    s
}

/// `--summary`: git's `summary_patch_list` — one line per patch that creates,
/// deletes, renames, or changes the mode of a file (pure content edits print
/// nothing).
fn render_summary(patches: &[Patch]) -> String {
    let mut out = String::new();
    for p in patches {
        if p.is_rename || p.is_copy {
            out.push_str(&rename_line(p));
        } else if p.is_new {
            out.push_str(&format!(
                " create mode {:06o} {}\n",
                p.new_mode.unwrap_or(0),
                p.new_name.as_deref().unwrap_or("")
            ));
        } else if p.is_delete {
            out.push_str(&format!(
                " delete mode {:06o} {}\n",
                p.old_mode.unwrap_or(0),
                p.old_name.as_deref().unwrap_or("")
            ));
        } else if let (Some(om), Some(nm)) = (p.old_mode, p.new_mode) {
            if om != nm {
                out.push_str(&format!(
                    " mode change {:06o} => {:06o} {}\n",
                    om,
                    nm,
                    p.new_name.as_deref().unwrap_or("")
                ));
            }
        }
    }
    out
}

/// apply.c's `show_rename_copy`: strip the common leading *directory* prefix (whole
/// `foo/` components only, no suffix folding) and render `dir/{old => new}` when a
/// prefix was found, else `old => new`.
fn rename_line(p: &Patch) -> String {
    let verb = if p.is_copy { "copy" } else { "rename" };
    let old = p.old_name.as_deref().unwrap_or("");
    let new = p.new_name.as_deref().unwrap_or("");
    let (ob, nb) = (old.as_bytes(), new.as_bytes());
    let mut pfx = 0usize;
    loop {
        let so = ob[pfx..].iter().position(|&b| b == b'/');
        let sn = nb[pfx..].iter().position(|&b| b == b'/');
        match (so, sn) {
            (Some(a), Some(b)) if a == b && ob[pfx..pfx + a] == nb[pfx..pfx + b] => {
                pfx += a + 1;
            }
            _ => break,
        }
    }
    if pfx > 0 {
        format!(
            " {verb} {}{{{} => {}}} ({}%)\n",
            &old[..pfx],
            &old[pfx..],
            &new[pfx..],
            p.score
        )
    } else {
        format!(" {verb} {old} => {new} ({}%)\n", p.score)
    }
}

// ---------------------------------------------------------------------------
// --include / --exclude — port of apply.c:use_patch + wildmatch (flags 0)
// ---------------------------------------------------------------------------

/// Whether a patch survives the `--include`/`--exclude` rule list: the first rule
/// whose glob matches the patch's post-image name decides (its include/exclude
/// sense); with no match, a path is kept unless any `--include` rule exists.
fn use_patch(p: &Patch, prefix: &str, limits: &[(bool, String)], has_include: bool) -> bool {
    let name = p.new_name.as_deref().or(p.old_name.as_deref()).unwrap_or("");
    // "Paths outside are not touched regardless of `--include`" (apply.c:2218): the
    // path must live strictly *below* the directory `git apply` was invoked from.
    if !prefix.is_empty() {
        match name.strip_prefix(prefix) {
            Some(rest) if !rest.is_empty() => {}
            _ => return false,
        }
    }
    for (is_include, pat) in limits {
        if wildmatch0(pat.as_bytes(), name.as_bytes()) {
            return *is_include;
        }
    }
    !has_include
}

/// `wildmatch(pattern, text, 0)`: `*` matches any run *including* `/`, `?` a single
/// byte, `[...]` a bracket set (with `!`/`^` negation and `a-z` ranges), and `\`
/// escapes the next byte. POSIX `[:class:]` names are not handled (unused here).
fn wildmatch0(pat: &[u8], text: &[u8]) -> bool {
    match pat.first() {
        None => text.is_empty(),
        Some(b'*') => {
            if wildmatch0(&pat[1..], text) {
                return true;
            }
            match text.split_first() {
                Some((_, trest)) => wildmatch0(pat, trest),
                None => false,
            }
        }
        Some(b'?') => match text.split_first() {
            Some((_, trest)) => wildmatch0(&pat[1..], trest),
            None => false,
        },
        Some(b'[') => match text.split_first() {
            Some((&c, trest)) => match match_class(pat, c) {
                Some((true, np)) => wildmatch0(&pat[np..], trest),
                Some((false, _)) => false,
                None => c == b'[' && wildmatch0(&pat[1..], trest),
            },
            None => false,
        },
        Some(b'\\') if pat.len() >= 2 => match text.split_first() {
            Some((&c, trest)) if c == pat[1] => wildmatch0(&pat[2..], trest),
            _ => false,
        },
        Some(&pc) => match text.split_first() {
            Some((&c, trest)) if c == pc => wildmatch0(&pat[1..], trest),
            _ => false,
        },
    }
}

/// Match one `[...]` bracket expression against byte `c`. Returns
/// `(matched, index just past the ']')`, or `None` if the class is unterminated
/// (so the caller can treat `[` as a literal).
fn match_class(pat: &[u8], c: u8) -> Option<(bool, usize)> {
    let mut i = 1;
    let negated = matches!(pat.get(i), Some(&b'!') | Some(&b'^'));
    if negated {
        i += 1;
    }
    let start = i;
    let mut matched = false;
    loop {
        match pat.get(i) {
            None => return None,
            Some(&b']') if i > start => {
                i += 1;
                break;
            }
            Some(&ch) => {
                let is_range = pat.get(i + 1) == Some(&b'-')
                    && pat.get(i + 2).is_some_and(|&d| d != b']');
                if is_range {
                    if ch <= c && c <= pat[i + 2] {
                        matched = true;
                    }
                    i += 3;
                } else {
                    if ch == c {
                        matched = true;
                    }
                    i += 1;
                }
            }
        }
    }
    Some((matched ^ negated, i))
}

/// The current bytes of `path`, preferring the result an earlier patch in this
/// same run produced. `None` means the path does not exist.
fn read_current(staged: &HashMap<String, Option<Vec<u8>>>, path: &str) -> Option<Vec<u8>> {
    if let Some(entry) = staged.get(path) {
        return entry.clone();
    }
    let meta = std::fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() {
        // A symlink's blob content is its target, with no trailing newline.
        return Some(
            std::fs::read_link(path)
                .ok()?
                .into_os_string()
                .into_string()
                .ok()?
                .into_bytes(),
        );
    }
    std::fs::read(path).ok()
}

fn create_leading_dirs(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

/// Create `path` fresh with `mode`, as git's `try_create_file` does: symlinks via
/// `symlink(2)`, everything else opened `O_CREAT|O_EXCL` with 0777 or 0666 so the
/// process umask decides the final permissions.
#[cfg(unix)]
fn write_created(path: &Path, mode: u32, data: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    if mode & 0o170000 == 0o120000 {
        let target = String::from_utf8_lossy(data).into_owned();
        std::os::unix::fs::symlink(&target, path)?;
        return Ok(());
    }
    let perm = if mode & 0o100 != 0 { 0o777 } else { 0o666 };
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(perm)
        .open(path)?;
    f.write_all(data)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_created(path: &Path, _mode: u32, data: &[u8]) -> Result<()> {
    std::fs::write(path, data)?;
    Ok(())
}

/// `create_one_file()` (apply.c:4549). The plain `O_CREAT|O_EXCL` create is only the
/// first attempt; git has two recoveries under it, and without them a perfectly
/// ordinary input fails:
///
/// * `ENOENT` — a leading directory is missing, so create them and retry
///   (apply.c:4594). [`create_leading_dirs`] runs first here, which folds that in.
/// * `EEXIST` — something is already at the path. git first tries `rmdir()` in case a
///   *directory* is in the way (apply.c:4604-4611), then writes the content to
///   `<path>~<pid>` and `rename()`s it over the top (apply.c:4613-4632).
///
/// The `EEXIST` arm is reached by anything that writes the same path twice in one
/// run — two patches in one input against the same file, which is what `git
/// format-patch`-style stacked patches look like once concatenated. Without it the
/// second write returns raw `File exists (os error 17)` and the run dies mid-write.
fn create_one_file(path: &Path, mode: u32, data: &[u8]) -> Result<()> {
    create_leading_dirs(path)?;
    let Err(first) = write_created(path, mode, data) else {
        return Ok(());
    };
    let kind = first.downcast_ref::<std::io::Error>().map(std::io::Error::kind);
    // `if (errno == EEXIST || errno == EACCES)`: "We may be trying to create a file
    // where a directory used to be." The C then rewrites `errno` to `EEXIST` whenever
    // `lstat()` finds anything at the path that is not a directory, or is one it could
    // remove, and only that rewritten `EEXIST` reaches the temporary-name loop.
    //
    // The one shape not reproduced is `EACCES` on a path that already exists as a
    // non-directory, which the C would route into that loop. `O_CREAT|O_EXCL` reports
    // `EEXIST` before it checks permissions, so the kernel cannot hand us that pair;
    // a real `EACCES` here means an unwritable parent, where `lstat()` also fails and
    // the C falls through to the error exactly as this does.
    // `errno` as the C carries it into `if (errno == EEXIST)`: the rewrite only
    // happens when `lstat()` finds a non-directory, or a directory `rmdir()` could
    // remove. A directory `rmdir()` refuses leaves *its* errno in place, which is
    // what makes `git apply` over a non-empty directory report `Directory not
    // empty` instead of looping over temporary names.
    let mut reason = first
        .downcast_ref::<std::io::Error>()
        .map_or_else(|| first.to_string(), io_msg);
    let mut eexist = kind == Some(std::io::ErrorKind::AlreadyExists);
    let mut was_dir = false;
    if matches!(
        kind,
        Some(std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::PermissionDenied)
    ) {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => match std::fs::remove_dir(path) {
                Ok(()) => {
                    eexist = true;
                    was_dir = true;
                }
                Err(e) => {
                    eexist = false;
                    reason = io_msg(&e);
                }
            },
            Ok(_) => eexist = true,
            Err(_) => {}
        }
    }
    if was_dir {
        if write_created(path, mode, data).is_ok() {
            return Ok(());
        }
    }
    if !eexist {
        bail!(
            "unable to write file '{}' mode {mode:o}: {reason}",
            path.to_string_lossy()
        );
    }
    // `mkpathdup("%s~%u", path, nr)`, starting at the pid and counting up until a
    // name is free, then renamed over the target.
    let mut nr = std::process::id();
    for _ in 0..1000 {
        let tmp = path.with_file_name(format!(
            "{}~{nr}",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
        match write_created(&tmp, mode, data) {
            Ok(()) => {
                if std::fs::rename(&tmp, path).is_ok() {
                    return Ok(());
                }
                let _ = std::fs::remove_file(&tmp);
                break;
            }
            Err(e) if is_already_exists(&e) => nr = nr.wrapping_add(1),
            Err(_) => break,
        }
    }
    bail!(
        "unable to write file '{}' mode {mode:o}: {reason}",
        path.to_string_lossy()
    );
}

/// `errno == EEXIST` on the temporary-name loop's create, which is the only thing
/// that makes it try the next number (apply.c:4627). `write_created()` wraps
/// `std::io::Error` in `anyhow`, so the kind has to be recovered rather than matched
/// on directly.
fn is_already_exists(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>().map(std::io::Error::kind)
        == Some(std::io::ErrorKind::AlreadyExists)
}

/// After removing a file, drop the directories it emptied, exactly as git's
/// `remove_path` does. Stops at the first non-empty (or non-removable) parent.
fn prune_empty_parents(path: &Path) {
    let mut dir: PathBuf = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => return,
    };
    while std::fs::remove_dir(&dir).is_ok() {
        match dir.parent() {
            Some(p) if !p.as_os_str().is_empty() => dir = p.to_path_buf(),
            _ => break,
        }
    }
}

/// An io error's message without Rust's ` (os error N)` suffix, so our stderr
/// reads like git's `strerror`-based output.
fn io_msg(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error ") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}

// ---------------------------------------------------------------------------
// whitespace checking — apply.c's ws_check path
// ---------------------------------------------------------------------------

/// One line the whitespace check will look at: `(index into the concatenated input,
/// index of the hunk, index into that hunk's `post`, the `patch->ws_rule` in force
/// when `parse_fragment()` reached it)`.
type WsTarget = (usize, usize, usize, u32);

/// `check_old_for_crlf()` (apply.c:1716): a context or removed line that ends `\r\n`
/// means the *pre-image* is a CRLF file, so the `\r` the patch adds back on every
/// line is not a whitespace error.
fn ends_with_crlf(line: &[u8]) -> bool {
    line.ends_with(b"\r\n")
}

/// Walk one patch's fragments the way `parse_fragment()` (apply.c:1802-1881) does,
/// carrying `patch->ws_rule` along, and collect every line the check applies to.
///
/// The order is load-bearing: `check_old_for_crlf()` only ORs `WS_CR_AT_EOL` in as it
/// meets a CRLF pre-image line, so a `+` line *ahead* of the first such line is still
/// checked under the strict rule and is reported as `trailing whitespace`. `-R` has
/// already inverted the markers ([`Patch::reverse`]), which is how the C's
/// `apply_in_reverse` tests are expressed here.
///
/// `patch->ws_rule` is per patch, not per fragment, so a CRLF line in one hunk
/// relaxes the hunks after it too.
fn ws_targets(p: &Patch, base: u32) -> Vec<WsTarget> {
    let mut rule = base;
    let mut out = Vec::new();
    for (hunk_idx, h) in p.hunks.iter().enumerate() {
        for &(input_idx, marker, idx) in &h.body {
            if marker == b'+' {
                out.push((input_idx, hunk_idx, idx, rule));
            } else if h.pre.get(idx).is_some_and(|l| ends_with_crlf(l)) {
                rule |= super::diff_color::WS_CR_AT_EOL;
            }
        }
    }
    out
}

/// Report the whitespace errors every added line carries, as `apply.c`'s
/// `check_whitespace()` does: one `<patch>:<line>: <error>.` line followed by the
/// offending text, the first five only, then the count.
///
/// Returns the number of offending lines.
///
/// `nowarn` produces no output *and* no count in git: `parse_fragment()` skips
/// `check_whitespace()` entirely under `nowarn_ws_error` (apply.c:1867-1869), so
/// `state->whitespace_error` stays 0 and `apply_all_patches()`'s whole summary block
/// is skipped with it. The count is still returned here for symmetry; [`ws_summary`]
/// is what declines to say anything about it.
fn report_whitespace(
    patches: &[Patch],
    spans: &InputSpans,
    rule: u32,
    action: &WsAction,
    quiet: bool,
) -> usize {
    // `squelch_whitespace_errors`: git prints the first five and summarises the rest.
    const SQUELCH: usize = 5;
    let mut errors = 0usize;
    let mut printed = 0usize;
    let silent = matches!(action, WsAction::Silent);
    for p in patches {
        for (input_idx, hunk_idx, post_idx, rule) in ws_targets(p, rule) {
            let Some(line) = p.hunks[hunk_idx].post.get(post_idx) else {
                continue;
            };
            let result = super::diff_files::ws_check(line, rule);
            if result == 0 {
                continue;
            }
            errors += 1;
            if silent || printed >= SQUELCH {
                continue;
            }
            printed += 1;
            let (file, no) = spans.location(input_idx);
            let what = super::diff_files::whitespace_error_string(result);
            err(quiet, &format!("{file}:{no}: {what}."));
            let body = line.strip_suffix(b"\n").unwrap_or(line);
            err(quiet, &String::from_utf8_lossy(body));
        }
    }
    errors
}

/// `apply_all_patches()`'s tail (apply.c:5141-5171): the counts, once every input
/// file has been through `apply_patch()`.
///
/// Where this sits matters as much as what it says. The block is past the `end:`
/// label's `goto`s, so a run whose patch did not apply prints **none** of it — and
/// it is past `write_out_results()`, so under `--reject` it follows
/// `Applied patch <name> cleanly.` rather than preceding it. Both were measured
/// against stock through `git am`, where `apply` is invoked with `--index`.
///
/// `state->squelch_whitespace_errors` is 5, so the "squelched" line reports the
/// offenders past the fifth that `check_whitespace()` did not print.
fn ws_summary(
    errors: usize,
    action: &WsAction,
    apply: bool,
    applied_after_fixing: usize,
    quiet: bool,
) {
    const SQUELCH: usize = 5;
    if errors == 0 || matches!(action, WsAction::Silent | WsAction::Invalid) {
        return;
    }
    if errors > SQUELCH {
        err(
            quiet,
            &format!(
                "warning: squelched {} whitespace {}",
                errors - SQUELCH,
                plural_errors(errors - SQUELCH)
            ),
        );
    }
    let n = errors;
    match action {
        WsAction::Error => err(
            quiet,
            &format!(
                "error: {n} {} whitespace errors.",
                if n == 1 { "line adds" } else { "lines add" }
            ),
        ),
        // `if (state->applied_after_fixing_ws && state->apply)` picks the first
        // wording, and counts *that* rather than the error total; anything else
        // falls through to the plain count. A `--whitespace=fix` run that only
        // dropped a CR before the newline reports no fix at all, because
        // `ws_fix_copy()` strips that byte without setting its `fixed` flag.
        WsAction::Fix if apply && applied_after_fixing > 0 => {
            let n = applied_after_fixing;
            err(
                quiet,
                &format!(
                    "warning: {n} {} after fixing whitespace errors.",
                    if n == 1 { "line applied" } else { "lines applied" }
                ),
            )
        }
        _ => err(
            quiet,
            &format!(
                "warning: {n} {} whitespace errors.",
                if n == 1 { "line adds" } else { "lines add" }
            ),
        ),
    }
}

/// `Q_("whitespace error", "whitespace errors", n)`.
fn plural_errors(n: usize) -> &'static str {
    if n == 1 {
        "error"
    } else {
        "errors"
    }
}

// ---------------------------------------------------------------------------
// binary patches — apply.c's `GIT binary patch` payload
// ---------------------------------------------------------------------------

/// One `GIT binary patch` fragment: how to turn the pre-image into the post-image.
#[derive(Clone)]
enum BinaryPayload {
    /// `literal <size>`: the inflated bytes are the whole post-image.
    Literal(Vec<u8>),
    /// `delta <size>`: the inflated bytes are a git delta against the pre-image.
    Delta(Vec<u8>),
}

impl BinaryPayload {
    /// The post-image this payload produces from `base`.
    fn rebuild(&self, base: &[u8]) -> Option<Vec<u8>> {
        match self {
            BinaryPayload::Literal(data) => Some(data.clone()),
            BinaryPayload::Delta(delta) => apply_git_delta(base, delta),
        }
    }
}

/// `patch_delta()` (patch-delta.c): a size header for each side, then copy-from-base
/// and insert-literal instructions. `None` when the delta does not describe `base`.
fn apply_git_delta(base: &[u8], delta: &[u8]) -> Option<Vec<u8>> {
    let mut pos = 0usize;
    let mut varint = |pos: &mut usize| -> Option<usize> {
        let mut value = 0usize;
        let mut shift = 0u32;
        loop {
            let byte = *delta.get(*pos)?;
            *pos += 1;
            value |= ((byte & 0x7f) as usize) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
            shift += 7;
        }
    };
    if varint(&mut pos)? != base.len() {
        return None;
    }
    let target_size = varint(&mut pos)?;
    let mut out: Vec<u8> = Vec::with_capacity(target_size);
    while pos < delta.len() {
        let op = delta[pos];
        pos += 1;
        if op & 0x80 != 0 {
            // Copy: the low bits say which offset/size bytes are present.
            let mut offset = 0usize;
            let mut size = 0usize;
            for (bit, shift) in [(0x01, 0), (0x02, 8), (0x04, 16), (0x08, 24)] {
                if op & bit != 0 {
                    offset |= (*delta.get(pos)? as usize) << shift;
                    pos += 1;
                }
            }
            for (bit, shift) in [(0x10, 0), (0x20, 8), (0x40, 16)] {
                if op & bit != 0 {
                    size |= (*delta.get(pos)? as usize) << shift;
                    pos += 1;
                }
            }
            if size == 0 {
                size = 0x10000;
            }
            out.extend_from_slice(base.get(offset..offset.checked_add(size)?)?);
        } else if op != 0 {
            let len = op as usize;
            out.extend_from_slice(delta.get(pos..pos + len)?);
            pos += len;
        } else {
            // A zero opcode is reserved and git refuses it.
            return None;
        }
    }
    (out.len() == target_size).then_some(out)
}

/// Read one `literal <n>`/`delta <n>` block: the header line, then base85 lines
/// until a blank one.
///
/// `Ok(None)` is `parse_binary_hunk()` answering "there is no hunk here" — the
/// line is neither `literal` nor `delta`, which is how the *reverse* half is
/// found to be absent. `Err(<0-based line>)` is its `goto corrupt`, which is a
/// different answer entirely: "Not having reverse hunk is not an error, but
/// having a corrupt reverse hunk is" (`parse_binary()`, apply.c).
///
/// The block is terminated by a blank line and by nothing else. git's loop reads
/// `llen = linelen(buffer, size)` and only `llen == 1` breaks it, so a base85
/// run that reaches the end of the patch — or a line that is not base85 at all —
/// falls through to `goto corrupt` rather than ending the block. Treating either
/// as a terminator accepts a truncated payload that stock refuses: dropping the
/// single blank line after a `literal 0` reverse block is a one-byte edit, and
/// `git am` on the result is `error: corrupt binary patch …` at exit 128 while
/// this reader used to commit the patch.
fn parse_binary_block(
    lines: &[&[u8]],
    mut i: usize,
) -> std::result::Result<Option<(BinaryPayload, usize)>, usize> {
    let Some(head_line) = lines.get(i) else {
        return Ok(None);
    };
    let head = String::from_utf8_lossy(head_line).trim_end().to_string();
    let (kind, size) = match head.split_once(' ') {
        Some(("literal", n)) => match n.parse::<usize>() {
            Ok(n) => ("literal", n),
            Err(_) => return Ok(None),
        },
        Some(("delta", n)) => match n.parse::<usize>() {
            Ok(n) => ("delta", n),
            Err(_) => return Ok(None),
        },
        _ => return Ok(None),
    };
    i += 1;
    let mut encoded: Vec<u8> = Vec::new();
    loop {
        // Running out of input is `linelen()` returning 0, which is neither the
        // blank line nor a well-formed base85 line: `goto corrupt`.
        let Some(line) = lines.get(i) else {
            return Err(i);
        };
        let body = line.strip_suffix(b"\n").unwrap_or(line);
        if body.is_empty() {
            i += 1;
            break;
        }
        // The first byte is the length this line encodes, in git's `A`..`Z`/`a`..`z`
        // counting; a line outside that range is corrupt.
        let len = match body[0] {
            c @ b'A'..=b'Z' => (c - b'A') as usize + 1,
            c @ b'a'..=b'z' => (c - b'a') as usize + 27,
            _ => return Err(i),
        };
        match super::binary_patch::decode_base85(&body[1..], len) {
            Some(decoded) => encoded.extend_from_slice(&decoded),
            None => return Err(i),
        }
        i += 1;
    }
    // The payload is deflated, exactly as `emit_binary_diff_body()` wrote it.
    let mut inflate = gix::zlib::Inflate::default();
    let mut out = vec![0u8; size];
    let Ok((_status, _consumed, written)) = inflate.once(&encoded, out.as_mut_slice()) else {
        return Err(i);
    };
    if written != size {
        return Err(i);
    }
    Ok(Some((
        match kind {
            "literal" => BinaryPayload::Literal(out),
            _ => BinaryPayload::Delta(out),
        },
        i,
    )))
}

/// `apply_binary()` (apply.c:3276): rebuild a binary file's post-image, refusing
/// unless the `index` line named both ids in full and the pre-image is the one the
/// payload was made against.
///
/// `reverse` is `state->apply_in_reverse`. [`Patch::reverse`] has already swapped the
/// names and modes by the time this runs, but it leaves `index_old`/`index_new` in
/// file order, so the swap `reverse_patches()` performs on the two oid prefixes
/// (apply.c:2340) — and the matching switch to the *reverse* payload
/// (apply.c:3245-3251) — happen here instead.
fn rebuild_binary(p: &Patch, pre: &[u8], reverse: bool) -> std::result::Result<Vec<u8>, String> {
    // `apply_binary()`'s own name, which prefers the pre-image side.
    let name = p
        .old_name
        .clone()
        .or_else(|| p.new_name.clone())
        .unwrap_or_default();
    let hexsz = gix::hash::Kind::Sha1.len_in_hex();
    let (Some(old_id), Some(new_id)) = (
        p.preimage_id(reverse),
        if reverse {
            p.index_old.as_ref()
        } else {
            p.index_new.as_ref()
        },
    ) else {
        return Err(format!(
            "cannot apply binary patch to '{name}' without full index line"
        ));
    };
    if old_id.len() != hexsz || new_id.len() != hexsz {
        return Err(format!(
            "cannot apply binary patch to '{name}' without full index line"
        ));
    }

    // `apply_binary()` (apply.c:3295-3313): a patch with a pre-image side has to meet
    // contents that hash to the id it names — and the id git reports back is the one
    // it just computed from those contents, not the one the patch asked for. A patch
    // with no pre-image side (a creation) instead requires the target to be empty,
    // and that is decided by `patch->old_name`, never by the bytes.
    let have = blob_hex(pre);
    if p.old_name.is_none() {
        if !pre.is_empty() {
            return Err(format!(
                "the patch applies to an empty '{name}' but it is not empty"
            ));
        }
    } else if have != *old_id {
        return Err(format!(
            "the patch applies to '{name}' ({have}), which does not match the current contents."
        ));
    }

    // A null post-image id is a deletion; the payload describes nothing (apply.c:3316).
    if new_id.bytes().all(|b| b == b'0') {
        return Ok(Vec::new());
    }

    // "We already have the postimage" (apply.c:3321): when the object store can hand
    // over the result there is no need for the payload at all — which is how a
    // reverse-apply of a patch that carries no reverse hunk still works, as long as
    // the blob it rebuilds is one the repository already holds.
    if let Some(result) = read_blob_by_hex(new_id) {
        return Ok(result);
    }

    // `apply_binary_fragment()` (apply.c:3230), whose own name prefers the post-image
    // side, picks the second payload when reversing.
    let frag_name = p
        .new_name
        .clone()
        .or_else(|| p.old_name.clone())
        .unwrap_or_default();
    let payload = if reverse {
        let Some(payload) = &p.binary_reverse else {
            if p.binary_forward.is_none() {
                return Err(format!("missing binary patch data for '{frag_name}'"));
            }
            return Err(format!(
                "cannot reverse-apply a binary patch without the reverse hunk to '{frag_name}'"
            ));
        };
        payload
    } else {
        let Some(payload) = &p.binary_forward else {
            return Err(format!("missing binary patch data for '{frag_name}'"));
        };
        payload
    };

    let Some(result) = payload.rebuild(pre) else {
        return Err(format!("binary patch does not apply to '{name}'"));
    };
    let got = blob_hex(&result);
    if got != *new_id {
        return Err(format!(
            "binary patch to '{name}' creates incorrect result (expecting {new_id}, got {got})"
        ));
    }
    Ok(result)
}

/// The blob `hex` names, when the repository this run stands in already holds it.
/// `None` outside a repository, which is what `odb_has_object()` reports there.
fn read_blob_by_hex(hex: &str) -> Option<Vec<u8>> {
    let id = gix::ObjectId::from_hex(hex.as_bytes()).ok()?;
    let repo = crate::setup::discover().ok()?;
    let obj = repo.try_find_object(id).ok()??;
    (obj.kind == gix::object::Kind::Blob).then(|| obj.data.clone())
}

/// The blob id of `data`, which is what both ends of a binary patch are checked against.
fn blob_hex(data: &[u8]) -> String {
    gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Blob, data)
        .map(|id| id.to_hex().to_string())
        .unwrap_or_default()
}

/// `ws_fix_copy()` for git's default whitespace rule: strip the trailing whitespace a
/// line ends with, and drop the spaces that sit in front of a tab in its indent.
///
/// Only the default rule set is fixed here. `indent-with-non-tab` and `tab-in-indent`
/// reshape the indent in ways this has not been verified against, so a repository that
/// configures them keeps the deferred `--whitespace=fix` refusal rather than getting a
/// guess (see [`ws_fix_supported`]).
fn ws_fix_default(line: &[u8], rule: u32) -> (Vec<u8>, bool) {
    // `ws_fix_copy()`'s prologue, in its own order: the newline comes off first,
    // then a `\r` in front of it — which is put back only under `cr-at-eol` and,
    // crucially, does **not** count as a fix — and only then does the `isspace()`
    // strip run and set `fixed`. That is why `git apply --whitespace=fix` over a
    // patch whose added line ends CRLF removes the CR and still reports "N lines
    // add whitespace errors" rather than "applied after fixing".
    let mut fixed = false;
    let (mut body, terminator): (&[u8], &[u8]) = match line.strip_suffix(b"\n") {
        Some(rest) => (rest, b"\n"),
        None => (line, b""),
    };
    let mut cr_tail: &[u8] = b"";
    if let Some(rest) = body.strip_suffix(b"\r") {
        body = rest;
        if rule & super::diff_color::WS_CR_AT_EOL != 0 {
            cr_tail = b"\r";
        }
    }
    // `blank-at-eol`: everything after the last non-blank goes. C's `isspace`, so
    // the vertical tab and form feed count with space and tab.
    let end = body
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(0, |i| i + 1);
    if end != body.len() {
        fixed = true;
    }
    let body = &body[..end];

    // `space-before-tab`: inside the indent, a run of spaces followed by a tab is the
    // violation, and the fix is to drop the spaces.
    let indent_end = body
        .iter()
        .position(|b| !matches!(b, b' ' | b'\t'))
        .unwrap_or(body.len());
    let mut out: Vec<u8> = Vec::with_capacity(line.len());
    let mut i = 0usize;
    while i < indent_end {
        if body[i] == b'\t' {
            out.push(b'\t');
            i += 1;
            continue;
        }
        let run_end = body[i..indent_end]
            .iter()
            .position(|b| *b != b' ')
            .map_or(indent_end, |n| i + n);
        // Kept unless a tab follows the run, which is what makes it a violation.
        if body.get(run_end) != Some(&b'\t') {
            out.extend_from_slice(&body[i..run_end]);
        }
        i = run_end;
    }
    // A shortened indent is `need_fix_leading_space`, which does set `fixed`.
    if out.len() != indent_end {
        fixed = true;
    }
    out.extend_from_slice(&body[indent_end..]);
    out.extend_from_slice(cr_tail);
    out.extend_from_slice(terminator);
    (out, fixed)
}

/// Whether [`ws_fix_default`] describes what `rule` asks for: git's default set, with
/// any tab width (the width only matters to the rules this does not fix).
fn ws_fix_supported(rule: u32) -> bool {
    use super::diff_color::{WS_BLANK_AT_EOF, WS_BLANK_AT_EOL, WS_SPACE_BEFORE_TAB};
    const FIXABLE: u32 = WS_BLANK_AT_EOL | WS_BLANK_AT_EOF | WS_SPACE_BEFORE_TAB;
    // Ignore the low six bits, which carry the tab width rather than a rule.
    (rule & !0x3f) == FIXABLE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(name: &str, toplevel_relative: bool) -> Patch {
        Patch {
            old_name: Some(name.to_string()),
            new_name: Some(name.to_string()),
            old_mode: None,
            new_mode: None,
            is_new: false,
            is_delete: false,
            is_rename: false,
            is_copy: false,
            binary: false,
            binary_forward: None,
            binary_reverse: None,
            index_old: None,
            index_new: None,
            score: 0,
            is_toplevel_relative: toplevel_relative,
            hunks: Vec::new(),
            added: 0,
            deleted: 0,
        }
    }

    /// `use_patch()`'s prefix gate (apply.c:2219): run from `sub/`, only paths
    /// strictly below `sub/` are touched. Verified against git 2.55.0 — `git apply`
    /// from a subdirectory applies the in-tree half of a whole-tree patch and
    /// silently skips the rest, exit 0.
    #[test]
    fn the_invocation_prefix_drops_paths_outside_it() {
        let keep = patch("sub/s.txt", true);
        let outside = patch("f.txt", true);
        let sibling = patch("subsidiary/x.txt", true);
        // The prefix directory itself has an empty remainder and is not a path.
        let bare = patch("sub/", true);
        assert!(use_patch(&keep, "sub/", &[], false));
        assert!(!use_patch(&outside, "sub/", &[], false));
        assert!(!use_patch(&sibling, "sub/", &[], false));
        assert!(!use_patch(&bare, "sub/", &[], false));
        // With no prefix (invoked at the top) every path is in scope.
        assert!(use_patch(&outside, "", &[], false));
    }

    /// "Paths outside are not touched regardless of `--include`" — the prefix test
    /// runs before the rule list, so an include naming an out-of-prefix path still
    /// loses. Matching happens on the whole root-relative name, which is why
    /// `--include=deep/*` from `sub/` matches nothing.
    #[test]
    fn the_prefix_outranks_an_explicit_include() {
        let outside = patch("f.txt", true);
        let inside = patch("sub/deep/t.txt", true);
        let rules = vec![(true, "f.txt".to_string())];
        assert!(!use_patch(&outside, "sub/", &rules, true));
        let rules = vec![(true, "deep/*".to_string())];
        assert!(!use_patch(&inside, "sub/", &rules, true));
        let rules = vec![(true, "sub/deep/*".to_string())];
        assert!(use_patch(&inside, "sub/", &rules, true));
    }

    /// `guess_p_value()` (apply-lib.c:747), the inference that lets a plain
    /// `diff -u old new` patch apply with no `-p0`. Verified against git 2.55.0:
    /// `git apply --stat` over `--- s.txt`/`+++ s.txt` prints ` s.txt | 2 +-` from
    /// the worktree root, from a subdirectory, and outside a repository entirely.
    #[test]
    fn a_name_with_no_directory_part_infers_p0() {
        assert_eq!(guess_p_value("s.txt", "", ""), Some(0));
        assert_eq!(guess_p_value("s.txt", "", "sub/"), Some(0));
        // A `/dev/null` side says nothing; the caller falls back to the other one.
        assert_eq!(guess_p_value("/dev/null", "", ""), None);
        // A trailing timestamp is not part of the name.
        assert_eq!(guess_p_value("s.txt\t2005-04-07 22:13:13", "", ""), Some(0));
    }

    /// With a directory part the guess only speaks when the name embeds the
    /// invocation prefix, so an ordinary `a/`-prefixed patch keeps the default
    /// `-p1` — which is why nothing about existing patches changes.
    #[test]
    fn a_name_with_a_directory_part_needs_the_prefix_to_match() {
        assert_eq!(guess_p_value("a/s.txt", "", ""), None);
        assert_eq!(guess_p_value("a/s.txt", "", "sub/"), None);
        // `sub/s.txt` from `sub/`: the name starts with the prefix, so strip its
        // own depth (one slash → 0 components before it).
        assert_eq!(guess_p_value("sub/s.txt", "", "sub/"), Some(1));
        // `a/sub/s.txt` from `sub/`: the prefix begins after the first component.
        assert_eq!(guess_p_value("a/sub/s.txt", "", "sub/"), Some(2));
        // `--directory=X` is prepended before the slash test, so a one-component
        // name stops looking like one and the guess declines.
        assert_eq!(guess_p_value("s.txt", "X/", "sub/"), None);
        assert_eq!(guess_p_value("s.txt", "X/", ""), None);
    }

    /// `prefix_patch()` (apply.c:2191): a `diff --git` patch is already relative to
    /// the worktree root and keeps its names; a traditional `---`/`+++` diff was
    /// written relative to the invocation directory and gains the prefix.
    #[test]
    fn only_a_traditional_patch_gains_the_prefix() {
        let mut git_style = patch("s.txt", true);
        prefix_patch(&mut git_style, "sub/");
        assert_eq!(git_style.old_name.as_deref(), Some("s.txt"));
        assert_eq!(git_style.new_name.as_deref(), Some("s.txt"));

        let mut traditional = patch("s.txt", false);
        prefix_patch(&mut traditional, "sub/");
        assert_eq!(traditional.old_name.as_deref(), Some("sub/s.txt"));
        assert_eq!(traditional.new_name.as_deref(), Some("sub/s.txt"));
    }

    /// The `HeaderError` text `parse_patches()` fails with, or a panic naming what
    /// happened instead.
    fn header_error(lines: &[&[u8]], strip: usize, spans: &InputSpans) -> String {
        match parse_patches(lines, strip, true, "", "", false, spans) {
            Ok(p) => panic!("expected a header diagnostic, parsed {} patch(es)", p.len()),
            Err(e) => match e.downcast_ref::<HeaderError>() {
                Some(h) => h.to_string(),
                None => panic!("expected a header diagnostic, got: {e}"),
            },
        }
    }

    /// The two shapes `-p<n>` over-strip produces, which git reports with the input
    /// file and the line the header parse stopped at. Measured against git 2.55.0:
    /// a traditional patch names its `---` line, a `diff --git` header names the
    /// line the header ended on (the `@@`, or one past the last line when the patch
    /// has no body), and the component count is singular only at one.
    #[test]
    fn over_stripping_reports_the_headers_own_line() {
        let spans = InputSpans { spans: vec![("p.patch".to_string(), 0)] };
        let trad = concat!(
            "--- a/sub/deep/f.txt\n",
            "+++ b/sub/deep/f.txt\n",
            "@@ -1,1 +1,1 @@\n",
            "-one\n",
            "+two\n",
        );
        let lines = split_lines(trad.as_bytes());
        let err = header_error(&lines, 9, &spans);
        assert_eq!(
            err,
            "unable to find filename in patch at p.patch:1"
        );

        let git_style = concat!(
            "diff --git a/sub/deep/f.txt b/sub/deep/f.txt\n",
            "index 1234567..89abcde 100644\n",
            "--- a/sub/deep/f.txt\n",
            "+++ b/sub/deep/f.txt\n",
            "@@ -1,1 +1,1 @@\n",
            "-one\n",
            "+two\n",
        );
        let lines = split_lines(git_style.as_bytes());
        let err = header_error(&lines, 9, &spans);
        assert_eq!(
            err,
            "git diff header lacks filename information when removing 9 leading \
             pathname components at p.patch:5"
        );

        // A pure mode change has no `@@` at all, so the parse runs off the end and
        // git reports the line one past the last.
        let mode_only = "diff --git x y\nold mode 100644\nnew mode 100755\n";
        let lines = split_lines(mode_only.as_bytes());
        let err = header_error(&lines, 1, &spans);
        assert_eq!(
            err,
            "git diff header lacks filename information when removing 1 leading \
             pathname component at p.patch:4"
        );
    }

    /// `parse_traditional_patch()` reads the `+++` line with the `---` line's name as
    /// `find_name_common()`'s `def`, so one side may over-strip without failing and
    /// both sides end up with the single name that came out. Measured against git
    /// 2.55.0: `-p2` on `--- a/f.txt` / `+++ b/deep/f.txt` patches `f.txt`.
    #[test]
    fn a_traditional_patch_resolves_both_names_together() {
        let spans = InputSpans { spans: vec![("p.patch".to_string(), 0)] };
        let text = concat!(
            "--- a/f.txt\n",
            "+++ b/deep/f.txt\n",
            "@@ -1,1 +1,1 @@\n",
            "-one\n",
            "+two\n",
        );
        let lines = split_lines(text.as_bytes());
        let patches = parse_patches(&lines, 2, true, "", "", false, &spans).unwrap();
        assert_eq!(patches[0].old_name.as_deref(), Some("f.txt"));
        assert_eq!(patches[0].new_name.as_deref(), Some("f.txt"));

        // "Generally we prefer the shorter name": `f.txt.orig` vs `f.txt` is a patch
        // to `f.txt`, not a rename.
        let orig = concat!(
            "--- a/f.txt\n",
            "+++ b/f.txt.orig\n",
            "@@ -1,1 +1,1 @@\n",
            "-one\n",
            "+two\n",
        );
        let lines = split_lines(orig.as_bytes());
        let patches = parse_patches(&lines, 1, true, "", "", false, &spans).unwrap();
        assert_eq!(patches[0].old_name.as_deref(), Some("f.txt"));
        assert_eq!(patches[0].new_name.as_deref(), Some("f.txt"));
    }

    /// `fuzzy_matchlines()` (apply.c:2500): whitespace runs may differ in width but
    /// may not appear or disappear, and line endings do not count on either side.
    #[test]
    fn fuzzy_matching_collapses_runs_but_not_their_absence() {
        assert!(fuzzy_matchlines(b"\tbeta   gamma\n", b"    beta gamma\n"));
        assert!(fuzzy_matchlines(b"a b\r\n", b"a\tb"));
        assert!(fuzzy_matchlines(b"same\n", b"same\n"));
        // A run that vanishes is a different line, and so is one that appears only
        // on one side.
        assert!(!fuzzy_matchlines(b"a b\n", b"ab\n"));
        assert!(!fuzzy_matchlines(b"  indented\n", b"indented\n"));
        assert!(!fuzzy_matchlines(b"one\n", b"two\n"));
        // Trailing whitespace is a run the other side does not have.
        assert!(!fuzzy_matchlines(b"trail \n", b"trail\n"));
    }

    /// `update_pre_post_images()`: a hunk that only matched under relaxed whitespace
    /// takes its context lines from the file, not from the patch, so the file's own
    /// indentation survives and only added lines come out of the patch.
    #[test]
    fn a_relaxed_match_keeps_the_files_whitespace_on_context_lines() {
        let image: Vec<Vec<u8>> = vec![
            b"\tone\n".to_vec(),
            b"  two\n".to_vec(),
            b"\tthree\n".to_vec(),
        ];
        let h = Hunk {
            old_pos: 1,
            new_pos: 1,
            pre: vec![b" one\n".to_vec(), b"\ttwo\n".to_vec(), b" three\n".to_vec()],
            pre_common: vec![true, false, true],
            post: vec![b" one\n".to_vec(), b"NEW\n".to_vec(), b" three\n".to_vec()],
            post_common: vec![true, false, true],
            body: vec![(0, b' ', 0), (0, b'-', 1), (0, b'+', 1), (0, b' ', 2)],
            context: vec![b" one\n".to_vec(), b" three\n".to_vec()],
            raw: Vec::new(),
            trailing: 1,
            leading: 1,
            eof_fudge: false,
        };
        assert_eq!(
            place_with_context(&image, &[false; 3], &h, false, None, false).map(|p| p.at),
            None,
            "byte-exact matching still rejects it"
        );
        assert_eq!(
            place_with_context(&image, &[false; 3], &h, false, None, true).map(|p| p.at),
            Some(0)
        );
        assert_eq!(
            replacement(&image, 0, &h, false, true),
            vec![b"\tone\n".to_vec(), b"NEW\n".to_vec(), b"\tthree\n".to_vec()]
        );
        // Without the flag the replacement is the patch's own text.
        assert_eq!(replacement(&image, 0, &h, false, false), h.post);
    }
}
