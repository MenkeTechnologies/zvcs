//! Differential cases for the four verbs that move paths between the working
//! tree and the index: `add`, `rm`, `mv` and `clean`.
//!
//! `corpus.rs` and [`crate::corpus::worktree_index`] already carry the flag
//! breadth for these verbs — every short option, the obvious refusals, and the
//! `./`-relative pathspec forms that regressed once. What they do not carry is
//! the *input state* those flags decide about. Their cases run on
//! [`Shape::Linear`], [`Shape::Dirty`], [`Shape::AwkwardPaths`],
//! [`Shape::Submodule`] and [`Shape::Conflicted`], and in that set:
//!
//! * no path is sparse-excluded, so `--sparse` and the `advice.updateSparsePath`
//!   refusal that guards it are unmeasured on all four verbs;
//! * no path is *ignored*, so `add`'s `-f` gate is measured only through its own
//!   success path, and `clean -x`/`-X` see exactly one ignore rule
//!   ([`Shape::Stashed`]'s one-line `.gitignore`) anywhere in the corpus;
//! * no untracked *directory* exists, so `clean -d` never had a directory to
//!   recurse into and `-fd` scored the same as `-f`;
//! * no untracked nested repository exists, so `-ff` scored the same as `-f` —
//!   the whole difference between the two is a directory git refuses to descend
//!   into once and removes twice;
//! * no combining mark reaches argv, so the precompose pass every one of these
//!   verbs runs its pathspec through is unmeasured.
//!
//! So this module is organised by *shape* first and flag second, and leans on
//! the four fixtures the existing add/rm/mv/clean corpus never opens:
//! [`Shape::Attributes`] (five ignore-rule sources and two untracked
//! directories), [`Shape::Sparse`] (two skip-worktree entries and an untracked
//! file inside the excluded cone), [`Shape::Worktree`] (an untracked nested
//! repository) and [`Shape::DecomposedPaths`] (an NFD name on disk and in argv).
//!
//! # What the probes see
//!
//! `ls-files --stage` is the instrument for `add`/`rm`/`mv`: it prints mode,
//! object id and stage number, so a `--chmod=+x` that only rewrote the mode, an
//! `add` that resolved a stage 1/2/3 triple to stage 0, and an `rm --cached`
//! that dropped an entry while leaving the file are three post-states that
//! stdout cannot tell apart (`add` and `mv` are silent on success).
//! `status --porcelain=v1 -uall` is the instrument for `clean`: it is what
//! proves a removal took exactly the paths stock git took. Most `clean` cases
//! here come in an `-n`/`-f` pair, because the listing and the removal are
//! separate code paths in `builtin/clean.c` and a port can print the right list
//! and delete the wrong set.
//!
//! # Fixture constraints these cases work around
//!
//! * A case is one argv against a pristine copy, so nothing can be staged,
//!   ignored or excluded first. Every case needing an ignored path uses
//!   [`Shape::Attributes`], every case needing a sparse-excluded one uses
//!   [`Shape::Sparse`], and every case needing an untracked nested repository
//!   uses [`Shape::Worktree`] or [`Shape::BehindRemote`].
//! * `add -i`/`add -p` and `clean -i` drive a terminal dialogue, so they are
//!   left to the existing `clean -i` EOF case; scripting them through
//!   [`Case::with_stdin`] would measure the interactive loop's parser rather
//!   than the verb.
//! * There is no `mv.*` configuration to reach: `git help -c` on git 2.55.0
//!   lists no key under that prefix and `git mv -h` offers only `-v -f -n -k
//!   --sparse`. A case naming an invented key would measure nothing, so the
//!   directory half of `mv` is reached through real directory arguments.
//! * [`Shape::Dirty`] deletes `src/lib.rs` but keeps the directory, which is
//!   what makes `.in_dir("src")` legal there — the fixture detail the
//!   whole-tree-vs-subtree group depends on.
//!
//! Every invocation below was run against stock git 2.55.0 in a copy of the
//! fixture before it was written down; the comments record what stock did.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    add_update_modes(out);
    add_from_subdirectory(out);
    add_chmod_and_refresh(out);
    add_ignored_paths(out);
    add_sparse(out);
    add_pathspec_magic(out);
    add_pathspec_from_file(out);
    add_edit_and_renormalize(out);
    add_submodule_and_conflicts(out);
    add_decomposed(out);
    rm_sparse(out);
    rm_submodule(out);
    rm_awkward_and_recursive(out);
    rm_refusals(out);
    rm_pathspec_from_file(out);
    rm_from_subdirectory(out);
    mv_destinations(out);
    mv_refusals(out);
    mv_state_shapes(out);
    clean_ignore_rules(out);
    clean_force_levels(out);
    clean_scoped(out);
    clean_shapes(out);
}

/// The update modes, and the two spellings that subtract from them.
///
/// `-A`, `-u` and a bare pathspec differ only in which of {modified, deleted,
/// untracked} they pick up, and [`Shape::Dirty`] carries one of each plus a
/// path already staged. A port that treats `-u` as `-A` stages `untracked.txt`
/// too and the index probe sees it; a port that ignores `--ignore-removal`
/// stages the `src/lib.rs` deletion where stock does not.
///
/// `-N` is the one a naive `status` reader cannot see: under `add -A -N` stock
/// stages the *deletion* of `src/lib.rs` outright while leaving `README.md`'s
/// modification unstaged and `untracked.txt` at intent-to-add, so the pair
/// `-A` / `-A -N` is what separates the ADD_CACHE_INTENT pass in
/// `builtin/add.c` from a real add.
fn add_update_modes(out: &mut Vec<Case>) {
    out.push(Case::new("add", &["add", "-u"], Shape::Dirty));
    out.push(Case::new("add", &["add", "-A", "-n"], Shape::Dirty));
    out.push(Case::new("add", &["add", "--ignore-removal", "."], Shape::Dirty));
    out.push(Case::new("add", &["add", "-A", "-N"], Shape::Dirty));
    out.push(Case::new("add", &["add", "-N", "untracked.txt"], Shape::Dirty));
}

/// The same modes run from `src/`, which is where they stop agreeing.
///
/// Since git 2.0 a bare `add -u` and `add -A` are whole-tree regardless of the
/// working directory, while `add .` is not: from `src/`, stock stages
/// `README.md` under `-u` and leaves it alone under `.`. A port that applies
/// the cwd prefix to every mode, or to none, passes at the top level and fails
/// here — the only place the corpus can tell.
fn add_from_subdirectory(out: &mut Vec<Case>) {
    for args in [&["add", "-u"][..], &["add", "-A"], &["add", "."], &["add", "-n", "."]] {
        out.push(Case::new("add", args, Shape::Dirty).in_dir("src"));
    }
}

/// `--chmod` and `--refresh`: the two modes that write an index entry without
/// reading the file's content.
///
/// `--chmod=+x` on a clean [`Shape::Linear`] leaves stock reporting
/// `MM README.md` — the index entry's mode moved to 100755 while the file on
/// disk stayed 644, so the path is staged-modified and worktree-modified at
/// once. `core.fileMode=false` does not suppress it: the flag writes the mode
/// directly rather than reading it off the filesystem, so a port that routes
/// `--chmod` through the same stat path as everything else changes nothing here
/// and the index probe sees the missing 100755.
///
/// Bare `--refresh` with no pathspec is the `advice.addEmptyPathspec` path:
/// stock exits **0** after printing `Nothing specified, nothing added.` and two
/// hints on stderr, which is why it is [`Case::strict`].
fn add_chmod_and_refresh(out: &mut Vec<Case>) {
    out.push(Case::new("add", &["add", "--chmod=+x", "README.md"], Shape::Linear));
    out.push(
        Case::new("add", &["add", "--chmod=+x", "README.md"], Shape::Linear)
            .with_config(&[("core.fileMode", "false")]),
    );
    // `--chmod` validates its argument before touching the index.
    out.push(Case::strict("add", &["add", "--chmod=bogus", "README.md"], Shape::Linear));

    out.push(Case::new("add", &["add", "--refresh", "."], Shape::Dirty));
    out.push(Case::strict("add", &["add", "--refresh"], Shape::Dirty));

    // `--ignore-missing` is only legal with `--dry-run`, and turns a fatal
    // unmatched pathspec into a silent skip that still reports its neighbours.
    out.push(Case::new(
        "add",
        &["add", "--ignore-missing", "--dry-run", "nosuch.txt", "untracked.txt"],
        Shape::Dirty,
    ));
}

/// The `-f` gate, against the one shape that has ignore rules to enforce.
///
/// [`Shape::Attributes`] is the only fixture where `add` can be told no: a root
/// `.gitignore` with a negation (`!important.log`) and an anchored rule
/// (`/notes.tmp`), a nested `sub/.gitignore`, and `.git/info/exclude`. Stock
/// exits **1** — not 128 — and prints the path list plus the
/// `advice.addIgnoredFile` hint, so each refusal is [`Case::strict`]: the exit
/// code alone does not separate "refused" from "matched nothing". Stock also
/// prints the pathspec with any trailing slash *stripped* (`build`, not
/// `build/`), which only the byte comparison catches.
///
/// `important.log` is the control: matched by `*.log`, un-matched by
/// `!important.log`, so a port whose ignore engine stops at the first matching
/// rule refuses a path stock adds. `add -f -A` is the other control — it has to
/// descend *into* the two ignored directories (`build/`,
/// `sub/deep-ignored/`), which a walker that prunes ignored directories before
/// consulting `-f` never does.
fn add_ignored_paths(out: &mut Vec<Case>) {
    out.push(Case::strict("add", &["add", "logs/debug.log"], Shape::Attributes));
    out.push(Case::strict("add", &["add", "build/"], Shape::Attributes));
    out.push(Case::strict("add", &["add", "excluded-by-info.txt"], Shape::Attributes));
    out.push(Case::strict("add", &["add", "sub/local-scratch.txt"], Shape::Attributes));

    out.push(Case::new("add", &["add", "important.log"], Shape::Attributes));
    out.push(Case::new("add", &["add", "-A"], Shape::Attributes));
    out.push(Case::new("add", &["add", "-f", "-A"], Shape::Attributes));
}

/// `--sparse`, and the refusal standing in front of it.
///
/// [`Shape::Sparse`] keeps `outside/drop.txt` and `outside/nested/deep.txt` as
/// skip-worktree index entries with no file on disk, plus an untracked
/// `outside/stray.txt` inside the excluded cone. Without `--sparse` stock
/// refuses to update any path outside the cone, exits **1**, and names the
/// paths with the `advice.updateSparsePath` hint — including for `add -A`,
/// which is the case a port is most likely to miss, because there the refusal
/// is triggered by a path the user never typed.
///
/// `add --sparse outside/drop.txt` is the opposite corner: the index entry
/// exists but the file does not, so stock exits 128 with
/// `pathspec ... did not match any files` and adds nothing.
fn add_sparse(out: &mut Vec<Case>) {
    out.push(Case::strict("add", &["add", "outside/drop.txt"], Shape::Sparse));
    out.push(Case::strict("add", &["add", "-A"], Shape::Sparse));
    out.push(Case::strict("add", &["add", "--sparse", "outside/drop.txt"], Shape::Sparse));

    out.push(Case::new("add", &["add", "--sparse", "."], Shape::Sparse));
    out.push(Case::new("add", &["add", "--sparse", "outside/stray.txt"], Shape::Sparse));
}

/// The pathspec magic forms, on a shape where each selects a different subset.
///
/// [`Shape::Dirty`] has a modified `README.md`, a deleted `src/lib.rs` and an
/// untracked `untracked.txt`, so `:(exclude)src/*` and `:!untracked.txt` drop
/// visibly different halves of `.` — `-n` makes the selection itself the
/// stdout, which is what makes these cheap and sharp. `:/` from `src/` is the
/// top-of-tree escape: it must reach `README.md`, which a prefix-joining
/// implementation cannot.
fn add_pathspec_magic(out: &mut Vec<Case>) {
    for args in [
        &["add", "-n", ":(exclude)src/*", "."][..],
        &["add", "-n", ":!untracked.txt", "."],
        &["add", "-n", ":(glob)**/*.txt"],
        &["add", "-n", ":(top,exclude)src/"],
    ] {
        out.push(Case::new("add", args, Shape::Dirty));
    }
    out.push(Case::new("add", &["add", "-n", ":/"], Shape::Dirty).in_dir("src"));
    out.push(Case::new("add", &["add", "-n", ":(glob)**/*.txt"], Shape::AwkwardPaths));
}

/// `--pathspec-from-file=-`, both separators.
///
/// The pathspec set arrives on stdin instead of argv, so this is the one place
/// where a path containing a space has to survive a *parser* rather than an
/// `execve` boundary: the newline form C-quotes such a path
/// (`"with space.txt"`), the NUL form does not and must not. A port that
/// dequotes unconditionally selects the wrong path on [`Shape::AwkwardPaths`],
/// which the index probe sees.
fn add_pathspec_from_file(out: &mut Vec<Case>) {
    out.push(Case::with_stdin(
        "add",
        &["add", "--pathspec-from-file=-"],
        Shape::Dirty,
        b"untracked.txt\nREADME.md\n",
    ));
    out.push(Case::with_stdin(
        "add",
        &["add", "--pathspec-from-file=-", "--pathspec-file-nul"],
        Shape::Dirty,
        b"untracked.txt\0README.md\0",
    ));
    out.push(Case::with_stdin(
        "add",
        &["add", "--pathspec-from-file=-"],
        Shape::AwkwardPaths,
        b"\"with space.txt\"\n",
    ));
    // An unmatched pathspec read from stdin is still fatal.
    out.push(Case::with_stdin(
        "add",
        &["add", "--pathspec-from-file=-"],
        Shape::Dirty,
        b"nosuch.txt\n",
    ));
}

/// `-e` and `--renormalize`: the two modes that rewrite content on the way in.
///
/// `-e` builds a patch, hands it to `$GIT_EDITOR` and applies what comes back.
/// The hermetic environment pins the editor to `true`, so the patch returns
/// unmodified and the mode reduces to "stage exactly this diff" — except on
/// [`Shape::Linear`], where there is no diff and stock exits 128 with
/// `empty patch. aborted` before opening anything.
///
/// `--renormalize` re-runs the clean filter over every tracked path.
/// [`Shape::Whitespace`] tracks a file that was committed with CRLF and carries
/// a whitespace-only unstaged edit; [`Shape::Attributes`] carries `* text=auto`
/// plus a nested `eol=crlf` override, which is the precedence a single
/// `.gitattributes` cannot express.
fn add_edit_and_renormalize(out: &mut Vec<Case>) {
    out.push(Case::new("add", &["add", "-e", "README.md"], Shape::Dirty));
    out.push(Case::strict("add", &["add", "-e", "README.md"], Shape::Linear));

    out.push(Case::new("add", &["add", "--renormalize", "."], Shape::Whitespace));
    out.push(Case::new("add", &["add", "--renormalize", "-A"], Shape::Attributes));
}

/// A gitlink, and an unmerged path.
///
/// `add sub` on [`Shape::Submodule`] re-reads the submodule's HEAD into the
/// gitlink entry; `add sub/mod.txt` is the refusal that keeps a parent
/// repository from swallowing a submodule's contents — stock exits 128 with
/// `Pathspec 'sub/mod.txt' is in submodule 'sub'` and writes nothing.
///
/// On [`Shape::Conflicted`], `add` is how a conflict is resolved: the stage
/// 1/2/3 triple collapses to one stage 0 entry, and `add -u` does it as
/// readily as a named path. Only `ls-files --stage` shows the difference —
/// `status` reports the path either way — and an index left holding stage 2 and
/// 3 entries is one `write-tree` cannot read, which the interop probe reports
/// as well.
fn add_submodule_and_conflicts(out: &mut Vec<Case>) {
    out.push(Case::new("add", &["add", "sub"], Shape::Submodule));
    out.push(Case::strict("add", &["add", "sub/mod.txt"], Shape::Submodule));

    out.push(Case::new("add", &["add", "conflict.txt"], Shape::Conflicted));
    out.push(Case::new("add", &["add", "-u"], Shape::Conflicted));
}

/// A combining mark in argv and on disk.
///
/// [`Shape::DecomposedPaths`] writes `e` + U+0301 to the filesystem; on macOS
/// git composes it before it reaches the index, so the tracked entry is
/// `\303\251.txt` while the directory entry is `e\314\201.txt`. Both spellings
/// therefore have to select the same path from argv, and
/// `core.precomposeunicode=false` has to make them stop: with the conversion
/// off, stock's `add -A` stages the composed path's edit *and* inserts the
/// decomposed names as separate entries. That is a five-entry index no other
/// case in the corpus can produce.
///
/// On Linux both sides leave the bytes alone and agree on the decomposed
/// answer, exactly as they agree on the composed one on macOS.
fn add_decomposed(out: &mut Vec<Case>) {
    out.push(Case::new("add", &["add", "-A"], Shape::DecomposedPaths));
    out.push(Case::new("add", &["add", "--", "\u{e9}-new.txt"], Shape::DecomposedPaths));
    out.push(Case::new("add", &["add", "--", "e\u{301}-new.txt"], Shape::DecomposedPaths));
    out.push(
        Case::new("add", &["add", "-A"], Shape::DecomposedPaths)
            .with_config(&[("core.precomposeunicode", "false")]),
    );
}

/// `rm --sparse`, and the refusal standing in front of it.
///
/// The same gate as `add`'s, reached through `builtin/rm.c`'s own sparse check:
/// without `--sparse`, removing a skip-worktree entry exits 1 with the
/// `advice.updateSparsePath` list, and with it the entry goes — while the file
/// it names never existed on disk, so the removal is index-only no matter what
/// `--cached` says. `rm --cached inside/keep.txt` is the control from inside the
/// cone: it drops the index entry and leaves the file, so the path appears
/// twice in `status` (staged deletion, then untracked).
fn rm_sparse(out: &mut Vec<Case>) {
    out.push(Case::strict("rm", &["rm", "outside/drop.txt"], Shape::Sparse));

    out.push(Case::new("rm", &["rm", "--sparse", "outside/drop.txt"], Shape::Sparse));
    out.push(Case::new("rm", &["rm", "-r", "--sparse", "outside"], Shape::Sparse));
    out.push(Case::new("rm", &["rm", "--cached", "inside/keep.txt"], Shape::Sparse));
    out.push(Case::new("rm", &["rm", "-r", "inside"], Shape::Sparse));
}

/// A gitlink through `rm`, which has to touch `.gitmodules` too.
///
/// `rm sub` removes the gitlink *and* stages an edit to `.gitmodules`;
/// `rm --cached sub` removes only the gitlink, leaves the submodule's working
/// tree behind as an untracked directory, and does not touch `.gitmodules`. The
/// two differ in one flag and produce different index contents, so a port that
/// reads `--cached` as "skip the unlink" and nothing else fails on the
/// `.gitmodules` entry alone.
fn rm_submodule(out: &mut Vec<Case>) {
    out.push(Case::new("rm", &["rm", "sub"], Shape::Submodule));
    out.push(Case::new("rm", &["rm", "--cached", "sub"], Shape::Submodule));
    out.push(Case::new("rm", &["rm", "-r", "--cached", "."], Shape::Submodule));
    out.push(Case::strict("rm", &["rm", "-f", "sub/mod.txt"], Shape::Submodule));
}

/// Recursion, and the pathspec forms that choose what to recurse over.
///
/// `rm nested` without `-r` is the guard rail: stock exits 128 with
/// `not removing 'nested' recursively without -r` and leaves the index
/// untouched. `rm -r --cached .` is the opposite extreme — it empties the index
/// of a shape whose paths need quoting, so every path appears twice in `status`
/// (staged deletion, then untracked) and a quoting mistake shows up on both
/// halves.
fn rm_awkward_and_recursive(out: &mut Vec<Case>) {
    out.push(Case::strict("rm", &["rm", "nested"], Shape::AwkwardPaths));

    out.push(Case::new("rm", &["rm", "-r", "--cached", "."], Shape::AwkwardPaths));
    out.push(Case::new("rm", &["rm", ":(glob)**/*.txt"], Shape::AwkwardPaths));
    out.push(Case::new("rm", &["rm", ":(icase)WITH SPACE.TXT"], Shape::AwkwardPaths));
    out.push(Case::new("rm", &["rm", "-f", "--", "\u{e9}.txt"], Shape::DecomposedPaths));
    out.push(Case::new("rm", &["rm", "-r", "--cached", "logs"], Shape::Attributes));
}

/// The refusals, and the flags that lift them.
///
/// `rm -r .` on [`Shape::Dirty`] hits *both* refusal classes in one invocation
/// — `staged.txt` has changes staged in the index, `README.md` has local
/// modifications — and stock prints them as two `error:` blocks, each followed
/// by its own `(use --cached to keep the file, or -f to force removal)` line,
/// then exits 1 having removed nothing. A port that collapses the classes into
/// one message, drops the remedy line, or reports only the first path it hit is
/// visible here and nowhere else. `-n` does not soften it: the dry run runs the
/// same checks and refuses identically.
///
/// `rm -f untracked.txt` is the third class: `-f` overrides the safety checks,
/// not the requirement that the path be tracked, so stock still exits 128 with
/// `did not match any files`.
///
/// On [`Shape::Conflicted`] there is nothing to refuse over and `rm -r .`
/// succeeds — but stock prints `rm 'conflict.txt'` **twice**, once per surviving
/// unmerged stage it removes, which is the kind of detail only a byte
/// comparison against a real conflict can pin.
fn rm_refusals(out: &mut Vec<Case>) {
    out.push(Case::strict("rm", &["rm", "-r", "."], Shape::Dirty));
    out.push(Case::strict("rm", &["rm", "-n", "-r", "."], Shape::Dirty));
    out.push(Case::strict("rm", &["rm", "-f", "untracked.txt"], Shape::Dirty));
    out.push(Case::strict("rm", &["rm", "-r", "."], Shape::Conflicted));

    out.push(Case::new("rm", &["rm", "-f", "-r", "."], Shape::Dirty));
    out.push(Case::new("rm", &["rm", "src/lib.rs"], Shape::Dirty));
    out.push(Case::new("rm", &["rm", "-n", "-r", "."], Shape::Conflicted));
    out.push(Case::new("rm", &["rm", "--cached", "conflict.txt"], Shape::Conflicted));
}

/// `rm --pathspec-from-file=-`, both separators.
///
/// The same quoting fork as `add`'s, on the verb where getting it wrong deletes
/// a file. `misc_commands.rs` already records that
/// `rm --no-pathspec-from-file=x README.md` once removed `README.md` outright,
/// so this option's parser is a fixed defect class rather than a hypothetical
/// one.
fn rm_pathspec_from_file(out: &mut Vec<Case>) {
    out.push(Case::with_stdin(
        "rm",
        &["rm", "--pathspec-from-file=-"],
        Shape::AwkwardPaths,
        b"\"with space.txt\"\n",
    ));
    out.push(Case::with_stdin(
        "rm",
        &["rm", "--pathspec-from-file=-", "--pathspec-file-nul"],
        Shape::AwkwardPaths,
        b"with space.txt\0quote\"name.txt\0",
    ));
    out.push(Case::with_stdin(
        "rm",
        &["rm", "-n", "--pathspec-from-file=-"],
        Shape::Linear,
        b"README.md\nsrc/lib.rs\n",
    ));
}

/// `rm` with a working directory below the top level.
///
/// Two things have to happen at once and they are separate code: the pathspec
/// resolves against the prefix, and the report prints the *full* path — stock
/// says `rm 'src/lib.rs'`, not `rm 'lib.rs'`. `src/lib.rs` is also the only
/// tracked file in `src/`, so removing it empties the directory the command is
/// running in, which git then removes as a now-empty leading directory. Any
/// step after that runs with a deleted cwd.
fn rm_from_subdirectory(out: &mut Vec<Case>) {
    out.push(Case::new("rm", &["rm", "-r", "."], Shape::Linear).in_dir("src"));
    out.push(Case::new("rm", &["rm", "lib.rs"], Shape::Linear).in_dir("src"));
    out.push(Case::new("rm", &["rm", "-n", "-r", ":/"], Shape::Linear).in_dir("src"));
}

/// Where a `mv` destination can land.
///
/// Four kinds of destination, each handled differently by stock: an existing
/// directory (the source moves *into* it), a missing directory component
/// (`fatal: renaming 'README.md' failed: No such file or directory` — git does
/// not create the parent), a directory-to-directory rename, and a case-only
/// rename. The last is the macOS-specific corner: on a case-insensitive
/// filesystem `mv README.md readme.md` succeeds and stock records
/// `R README.md -> readme.md`, so a port that compares the two names for
/// equality, or that asks whether the destination exists before asking whether
/// it *is* the source, refuses a rename git performs.
///
/// `mv -v src src2` is the multi-line one: stock reports the directory and then
/// every entry it carried (`Renaming src to src2`, then
/// `Renaming src/lib.rs to src2/lib.rs`).
///
/// `mv orig moved` on [`Shape::Renamed`] moves a directory *into* an existing
/// one rather than renaming it, giving `moved/orig/...` — the nesting a port
/// that treats an existing destination directory as an overwrite target loses.
fn mv_destinations(out: &mut Vec<Case>) {
    out.push(Case::new("mv", &["mv", "-v", "src", "src2"], Shape::Linear));
    out.push(Case::new("mv", &["mv", "README.md", "readme.md"], Shape::Linear));
    out.push(Case::new("mv", &["mv", "-k", "README.md", "src/lib.rs"], Shape::Linear));
    out.push(Case::new("mv", &["mv", "-n", "-f", "README.md", "src/lib.rs"], Shape::Linear));

    out.push(Case::new("mv", &["mv", "orig", "moved"], Shape::Renamed));
    out.push(Case::new("mv", &["mv", "moved/alpha.txt", "orig/"], Shape::Renamed));

    out.push(Case::new("mv", &["mv", "src/tabs.rs", "logs/tabs.rs"], Shape::Attributes));
    out.push(Case::new("mv", &["mv", "README.md", "src/lib.rs", "nested/deep"], Shape::AwkwardPaths));
    out.push(Case::new("mv", &["mv", "lib.rs", "../moved.rs"], Shape::AwkwardPaths).in_dir("src"));
    out.push(Case::new("mv", &["mv", "--", "\u{e9}.txt", "plain.txt"], Shape::DecomposedPaths));
}

/// The four things `mv` refuses, each with its own message.
///
/// `builtin/mv.c` reports refusals as `fatal: <reason>, source=<a>,
/// destination=<b>`, and the reason carries the whole diagnostic value:
/// `bad source` for a tracked path whose file is gone, `destination already
/// exists` for a collision, `conflicted` for an unmerged path, and
/// `can not move directory into itself` for a self-move. A port that answers
/// all four with one message agrees on the exit code and fails the byte
/// comparison, which is why every case here is [`Case::strict`].
///
/// `-f` does not lift the conflicted refusal — verified against stock, which
/// prints the same `fatal: conflicted` and exits 128 either way. The sparse
/// refusal is not an `mv.c` message at all: it is the shared
/// `advice.updateSparsePath` block at exit **1**, so a port that reports it as
/// a `bad source` fatal differs in both text and exit code.
fn mv_refusals(out: &mut Vec<Case>) {
    out.push(Case::strict("mv", &["mv", "README.md", "nodir/README.md"], Shape::Linear));
    out.push(Case::strict("mv", &["mv", "README.md", "README.md"], Shape::Linear));
    out.push(Case::strict("mv", &["mv", "src", "README.md"], Shape::Linear));
    out.push(Case::strict("mv", &["mv", "src/lib.rs", "README2.md"], Shape::Dirty));
    out.push(Case::strict("mv", &["mv", "conflict.txt", "other.txt"], Shape::Conflicted));
    out.push(Case::strict("mv", &["mv", "-f", "conflict.txt", "other.txt"], Shape::Conflicted));
    out.push(Case::strict("mv", &["mv", "sub", "nested/sub"], Shape::Submodule));
    out.push(Case::strict("mv", &["mv", "outside/drop.txt", "inside/drop.txt"], Shape::Sparse));
}

/// `mv` over index state the base corpus does not carry.
///
/// A path with *staged* content moves without complaint: stock rewrites the
/// index entry under the new name and keeps the staged blob, so `staged.txt`
/// becomes `A renamed.txt` rather than reverting to HEAD. A sparse-excluded
/// source needs `--sparse`, and then moves a path with no file on disk at all —
/// the index entry is rewritten and the destination stays absent from the
/// worktree, which only `ls-files --stage` shows.
fn mv_state_shapes(out: &mut Vec<Case>) {
    out.push(Case::new("mv", &["mv", "staged.txt", "renamed.txt"], Shape::Dirty));
    out.push(Case::new("mv", &["mv", "-k", "src/lib.rs", "moved.rs"], Shape::Dirty));

    out.push(Case::new("mv", &["mv", "--sparse", "outside/drop.txt", "inside/drop.txt"], Shape::Sparse));
    out.push(Case::new("mv", &["mv", "root.txt", "inside/root.txt"], Shape::Sparse));

    out.push(Case::new("mv", &["mv", "counter.txt", "count.txt"], Shape::Stashed));
    out.push(Case::new("mv", &["mv", "notes.txt", "notes.md"], Shape::Stashed));
}

/// `clean` against five sources of ignore rules at once.
///
/// This is the group the corpus was missing outright. [`Shape::Attributes`]
/// carries a root `.gitignore` (`*.log`, `!important.log`, `build/`,
/// `/notes.tmp`, `**/deep-ignored/`, `*.o`), a nested `sub/.gitignore`
/// (`!*.log`, `local-*`) and `.git/info/exclude`, over eight untracked paths —
/// two of them directories. So `-d`, `-x` and `-X` each select a different set:
///
/// | flags | selects                                              |
/// |-------|------------------------------------------------------|
/// | `-d`  | `important.log`, `tracked-looking.txt`               |
/// | `-dX` | the six ignored paths only                           |
/// | `-dx` | all eight                                            |
///
/// `important.log` is the discriminator: ignored by `*.log`, un-ignored by
/// `!important.log`, so it belongs to the `-d` set and *not* to the `-X` set.
/// An engine that returns on the first matching rule puts it in the wrong one
/// of the three and every column above changes.
fn clean_ignore_rules(out: &mut Vec<Case>) {
    for args in [
        &["clean", "-nd"][..],
        &["clean", "-ndx"],
        &["clean", "-ndX"],
        &["clean", "-fd"],
        &["clean", "-fdx"],
        &["clean", "-fdX"],
        &["clean", "-fdxq"],
    ] {
        out.push(Case::new("clean", args, Shape::Attributes));
    }

    // `-e` adds a sixth exclude source on top of the five, and repeats.
    out.push(Case::new("clean", &["clean", "-ndx", "-e", "*.log"], Shape::Attributes));
    out.push(Case::new("clean", &["clean", "-ndx", "-e", "build", "-e", "*.tmp"], Shape::Attributes));

    // `core.excludesFile` is the seventh. Pointing it at a tracked file is the
    // cheapest way to give it real content deterministically.
    out.push(
        Case::new("clean", &["clean", "-ndx"], Shape::Attributes)
            .with_config(&[("core.excludesFile", ".gitattributes")]),
    );

    // `-x` and `-X` are mutually exclusive; stock rejects the pair up front.
    out.push(Case::strict("clean", &["clean", "-ndxX"], Shape::Attributes));
}

/// `-f`, `-ff`, and `clean.requireForce`.
///
/// The two force levels differ over exactly one thing: a directory that is
/// itself a git repository. [`Shape::Worktree`] has one — `wt/`, a linked
/// worktree hidden from `status` by `.git/info/exclude` — and stock's behaviour
/// there is the sharpest three-way split in this file:
///
/// * `clean -ndx` prints **nothing**, and `clean -fdx` removes nothing;
/// * `clean -nffdx` prints `Would remove wt/`;
/// * `clean -ffdx` removes it.
///
/// So a port that ignores the second `-f` passes the first line and destroys
/// nothing, and a port that treats `-ff` as `-f` fails only the third. Both are
/// one-flag mistakes every other shape hides, because every other shape's
/// untracked directories are ordinary ones.
///
/// [`Shape::BehindRemote`]'s `.remote.git` is the control: it is a *bare*
/// repository, and `builtin/clean.c` asks whether a directory is a **non-bare**
/// one before skipping it, so a single `-f` removes this without complaint. A
/// port that answers "is this a repository" instead of "is this a non-bare
/// repository" refuses to clean it at every force level.
///
/// `clean.requireForce` is the refusal itself. Unset, stock exits 128 with
/// `clean.requireForce is true and -f not given: refusing to clean`; set to
/// false, the same argv deletes. A config key whose only observable effect is
/// whether files survive.
fn clean_force_levels(out: &mut Vec<Case>) {
    for args in [
        &["clean", "-ndx"][..],
        &["clean", "-nffdx"],
        &["clean", "-fdx"],
        &["clean", "-ffdx"],
    ] {
        out.push(Case::new("clean", args, Shape::Worktree));
    }
    out.push(Case::new("clean", &["clean", "-ndx"], Shape::BehindRemote));
    out.push(Case::new("clean", &["clean", "-fdx"], Shape::BehindRemote));

    out.push(Case::strict("clean", &["clean"], Shape::Attributes));
    out.push(Case::strict("clean", &["clean", "-d"], Shape::Attributes));
    out.push(
        Case::new("clean", &["clean", "-d"], Shape::Attributes)
            .with_config(&[("clean.requireForce", "false")]),
    );
    out.push(
        Case::new("clean", &["clean", "-dx"], Shape::Attributes)
            .with_config(&[("clean.requireForce", "false")]),
    );
}

/// Restricting `clean` to a subtree, by pathspec and by working directory.
///
/// Both restrictions exist and they are not the same code. A pathspec filters
/// the directory walk's results; a working directory below the top level *also*
/// changes what the paths are printed relative to — stock run from `sub/` says
/// `Removing deep-ignored/`, not `Removing sub/deep-ignored/`. A port that
/// resolves the prefix for matching but prints full paths passes the pathspec
/// cases and fails these.
fn clean_scoped(out: &mut Vec<Case>) {
    out.push(Case::new("clean", &["clean", "-fdx", "--", "sub"], Shape::Attributes));
    out.push(Case::new("clean", &["clean", "-ndx", "--", ":(glob)**/*.log"], Shape::Attributes));
    out.push(Case::new("clean", &["clean", "-ndx", "--", ":!*.log"], Shape::Attributes));

    out.push(Case::new("clean", &["clean", "-fdx"], Shape::Attributes).in_dir("sub"));
    out.push(Case::new("clean", &["clean", "-ndx", ":/"], Shape::Attributes).in_dir("sub"));
    out.push(Case::new("clean", &["clean", "-nd"], Shape::Dirty).in_dir("src"));
    out.push(Case::new("clean", &["clean", "-fd"], Shape::Dirty).in_dir("src"));
}

/// `clean` on the remaining shapes whose untracked set is structurally
/// different.
///
/// [`Shape::Sparse`]'s `outside/stray.txt` sits inside a directory the cone
/// excludes, so the walk has to descend into a directory holding no tracked
/// file — where a sparse-aware walk can skip too much. [`Shape::Stashed`] pairs
/// an ignored file with an untracked one under a one-line `.gitignore`, the
/// minimal `-x` discriminator. [`Shape::DecomposedPaths`] is the only shape
/// where the path `clean` prints needs both C-quoting and a precompose
/// decision.
fn clean_shapes(out: &mut Vec<Case>) {
    out.push(Case::new("clean", &["clean", "-nd"], Shape::Sparse));
    out.push(Case::new("clean", &["clean", "-fd"], Shape::Sparse));
    out.push(Case::new("clean", &["clean", "-fdx"], Shape::Sparse));

    out.push(Case::new("clean", &["clean", "-ndx"], Shape::Stashed));
    out.push(Case::new("clean", &["clean", "-fdx"], Shape::Stashed));
    out.push(Case::new("clean", &["clean", "-fdX"], Shape::Stashed));

    out.push(Case::new("clean", &["clean", "-nd"], Shape::DecomposedPaths));
    out.push(Case::new("clean", &["clean", "-fd"], Shape::DecomposedPaths));
}
