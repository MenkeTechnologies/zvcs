//! Differential corpus cases for the plumbing_objects subsystem.
//!
//! Populated per-command; every case here is compared against stock git for
//! stdout, exit code and post-command repository state.
//!
//! # Standing limitation: the runner gives every case an empty stdin
//!
//! `runner.rs` spawns both sides with `.stdin(Stdio::null())`, and `Case` has no
//! field for input bytes. Seven commands in this group take their real payload
//! on stdin — `mktag`, `mktree`, `unpack-objects`, `get-tar-commit-id`,
//! `show-index`, `stripspace`, `patch-id` — plus the `--stdin` / `--stdin-paths`
//! modes of `hash-object`, `index-pack` and `pack-objects`. For those, what is
//! measured here is the *empty-input* path only: argument parsing, the
//! zero-object result (`mktree` printing the empty tree, `pack-objects` writing
//! an empty pack), and the early-EOF error. Their parsing, fsck and streaming
//! logic is unreachable from this corpus and stays unmeasured until the harness
//! grows a stdin channel. Contriving stdin-free substitutes would report
//! coverage that does not exist, so it is not done.
//!
//! Two further exclusions, both deliberate:
//!   * `<cmd> -h` usage text is not asserted. It lands on stdout, but the runner
//!     documents error prose as outside the compatibility surface, and a usage
//!     block is prose.
//!   * `verify-pack` / `show-index` / `index-pack` cannot be pointed at a real
//!     pack: no fixture shape contains one, a case runs exactly one command, and
//!     fixture construction is owned elsewhere. They are covered on their error
//!     paths (missing file, non-pack file, bad flag) only.

use crate::corpus::read_only;
use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    hash_object(out);
    write_tree(out);
    read_tree(out);
    commit_tree(out);
    tree_and_tag_builders(out);
    pack_plumbing(out);
    text_and_env(out);
}

/// `hash-object`: the object-id oracle every other command is judged against.
fn hash_object(out: &mut Vec<Case>) {
    // README.md is modified-but-unstaged in Dirty, so the shape sweep is not
    // five copies of one answer.
    read_only("hash-object", &["hash-object", "README.md"], out);

    for args in [
        &["hash-object", "-t", "blob", "README.md"][..],
        &["hash-object", "-w", "README.md"][..],
        &["hash-object", "-w", "--no-filters", "README.md"][..],
        &["hash-object", "--path=other.txt", "README.md"][..],
        &["hash-object", "--no-filters", "-t", "blob", "-w", "src/lib.rs"][..],
        &["hash-object", "README.md", "src/lib.rs"][..],
        // `-t` off the default: only `--literally` lets a blob be labelled as
        // another type, so the pair brackets the fsck gate.
        &["hash-object", "-t", "tree", "--literally", "README.md"][..],
        &["hash-object", "-t", "tag", "README.md"][..],
        // stdin modes: empty input, so this asserts the empty-blob id and the
        // no-paths-read exit only.
        &["hash-object", "--stdin"][..],
        &["hash-object", "-w", "--stdin"][..],
        &["hash-object", "-t", "commit", "--literally", "--stdin"][..],
        &["hash-object", "--stdin-paths"][..],
        &["hash-object", "-w", "--stdin-paths"][..],
    ] {
        out.push(Case::new("hash-object", args, Shape::Linear));
    }

    out.push(Case::new("hash-object", &["hash-object", "-w", "untracked.txt"], Shape::Dirty));

    // Path handling: bytes that break naive quoting must not change the id.
    for args in [
        &["hash-object", "with space.txt"][..],
        &["hash-object", "üñïçødé.txt"][..],
        &["hash-object", "-w", "quote\"name.txt"][..],
        &["hash-object", "nested/deep/path.txt"][..],
    ] {
        out.push(Case::new("hash-object", args, Shape::AwkwardPaths));
    }

    // Error paths.
    for args in [
        &["hash-object"][..],
        &["hash-object", "no-such-file.txt"][..],
        &["hash-object", "-t", "bogus", "README.md"][..],
        &["hash-object", "--literally", "-t", "bogus", "README.md"][..],
        &["hash-object", "--path=x", "no-such-file.txt"][..],
    ] {
        out.push(Case::new("hash-object", args, Shape::Linear));
    }
}

/// `write-tree`: index → tree. Shape decides the answer, so it sweeps wide.
fn write_tree(out: &mut Vec<Case>) {
    read_only("write-tree", &["write-tree"], out);

    // Conflicted must refuse (unmerged entries); Submodule must emit a gitlink
    // entry; AwkwardPaths exercises name sorting over non-ASCII bytes.
    out.push(Case::new("write-tree", &["write-tree"], Shape::Conflicted));
    out.push(Case::new("write-tree", &["write-tree"], Shape::AwkwardPaths));
    out.push(Case::new("write-tree", &["write-tree"], Shape::Submodule));

    for args in [
        &["write-tree", "--missing-ok"][..],
        &["write-tree", "--prefix=src"][..],
        &["write-tree", "--prefix=src/"][..],
    ] {
        out.push(Case::new("write-tree", args, Shape::Linear));
    }

    out.push(Case::new("write-tree", &["write-tree", "--prefix=nested"], Shape::AwkwardPaths));
    out.push(Case::new("write-tree", &["write-tree", "--prefix=nested/deep"], Shape::AwkwardPaths));
    out.push(Case::new("write-tree", &["write-tree", "--prefix=sub"], Shape::Submodule));

    // Error paths.
    out.push(Case::new("write-tree", &["write-tree", "--prefix=nosuch"], Shape::Linear));
    out.push(Case::new("write-tree", &["write-tree", "--bogus"], Shape::Linear));
}

/// `read-tree`: tree → index, and the two- and three-tree merges built on it.
///
/// The mutating half of this module. Every case rewrites the index, so the
/// post-state probe carries at least as much signal as stdout, which `read-tree`
/// barely uses.
fn read_tree(out: &mut Vec<Case>) {
    // --- one tree: the plain load, and the flags that modify it ---
    for args in [
        &["read-tree", "HEAD"][..],
        &["read-tree", "HEAD^{tree}"][..],
        &["read-tree", "--empty"][..],
        &["read-tree", "-m", "HEAD"][..],
        &["read-tree", "-m", "--empty"][..],
        &["read-tree", "--reset", "HEAD"][..],
        &["read-tree", "-u", "HEAD"][..],
        &["read-tree", "--reset", "-u", "HEAD"][..],
        &["read-tree", "--reset", "-u", "--empty"][..],
        &["read-tree", "-i", "-m", "HEAD"][..],
        &["read-tree", "-v", "HEAD"][..],
        &["read-tree", "--prefix=sub/", "HEAD"][..],
        &["read-tree", "--prefix=src", "HEAD"][..],
        &["read-tree", "--no-sparse-checkout", "HEAD"][..],
        &["read-tree", "--index-output=alt-index", "HEAD"][..],
        &["read-tree", "--exclude-per-directory=.gitignore", "-m", "-u", "HEAD"][..],
    ] {
        out.push(Case::new("read-tree", args, Shape::Linear));
    }

    // Named refs, tags and peeled tags all have to resolve to the same tree.
    for args in [
        &["read-tree", "feature"][..],
        &["read-tree", "v0.2.0"][..],
        &["read-tree", "v0.1.0^{tree}"][..],
        &["read-tree", "--reset", "-u", "HEAD^"][..],
        &["read-tree", "-u", "--reset", "feature"][..],
    ] {
        out.push(Case::new("read-tree", args, Shape::Branched));
    }

    // Dirty is where the index/worktree safety checks live: `-m` must refuse to
    // discard work, `--reset` must be allowed to, and `-u` must reconcile the
    // worktree with the loaded tree.
    for args in [
        &["read-tree", "HEAD"][..],
        &["read-tree", "-m", "HEAD"][..],
        &["read-tree", "-m", "-u", "HEAD"][..],
        &["read-tree", "-u", "HEAD"][..],
        &["read-tree", "--reset", "HEAD"][..],
        &["read-tree", "--reset", "-u", "HEAD"][..],
        &["read-tree", "--empty"][..],
        &["read-tree", "--reset", "-u", "--empty"][..],
    ] {
        out.push(Case::new("read-tree", args, Shape::Dirty));
    }

    out.push(Case::new("read-tree", &["read-tree", "HEAD"], Shape::Detached));
    out.push(Case::new("read-tree", &["read-tree", "--reset", "-u", "main"], Shape::Detached));
    out.push(Case::new("read-tree", &["read-tree", "--reset", "-u", "HEAD^"], Shape::Merged));

    // An index carrying stage 1/2/3 entries: loading a tree must collapse them.
    out.push(Case::new("read-tree", &["read-tree", "HEAD"], Shape::Conflicted));
    out.push(Case::new("read-tree", &["read-tree", "-m", "HEAD"], Shape::Conflicted));
    out.push(Case::new("read-tree", &["read-tree", "--reset", "HEAD"], Shape::Conflicted));

    out.push(Case::new("read-tree", &["read-tree", "HEAD"], Shape::AwkwardPaths));
    out.push(Case::new("read-tree", &["read-tree", "--reset", "-u", "HEAD"], Shape::AwkwardPaths));
    out.push(Case::new("read-tree", &["read-tree", "--prefix=x/", "HEAD"], Shape::AwkwardPaths));
    out.push(Case::new("read-tree", &["read-tree", "HEAD"], Shape::Submodule));
    out.push(Case::new("read-tree", &["read-tree", "--prefix=z/", "HEAD"], Shape::Submodule));

    // --- two and three trees: the merge machinery `git merge` sits on ---
    for args in [
        &["read-tree", "-m", "HEAD", "feature"][..],
        &["read-tree", "-m", "-u", "HEAD", "feature"][..],
        &["read-tree", "-m", "HEAD^", "HEAD", "feature"][..],
        &["read-tree", "--aggressive", "-m", "HEAD", "HEAD", "feature"][..],
        &["read-tree", "--trivial", "-m", "HEAD^", "HEAD", "feature"][..],
    ] {
        out.push(Case::new("read-tree", args, Shape::Branched));
    }
    out.push(Case::new("read-tree", &["read-tree", "-m", "HEAD^", "HEAD", "HEAD^2"], Shape::Merged));

    // Error paths.
    for args in [
        &["read-tree"][..],
        &["read-tree", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"][..],
        // Two trees without -m is not a merge request; git rejects it.
        &["read-tree", "HEAD", "HEAD"][..],
        &["read-tree", "--bogus", "HEAD"][..],
    ] {
        out.push(Case::new("read-tree", args, Shape::Linear));
    }
}

/// `commit-tree`: the only way to mint a commit without touching the index.
///
/// Deterministic because `env::harden` pins author and committer identity *and*
/// both dates, so the resulting commit id is a pure function of the arguments.
fn commit_tree(out: &mut Vec<Case>) {
    for args in [
        &["commit-tree", "HEAD^{tree}", "-m", "msg"][..],
        &["commit-tree", "-m", "msg", "HEAD^{tree}"][..],
        // No -m and empty stdin: an empty commit message, not an error.
        &["commit-tree", "HEAD^{tree}"][..],
        &["commit-tree", "-p", "HEAD", "-m", "child", "HEAD^{tree}"][..],
        // Repeated -m becomes paragraphs; -F reads the message from a file.
        &["commit-tree", "-m", "a", "-m", "b", "HEAD^{tree}"][..],
        &["commit-tree", "-F", "README.md", "HEAD^{tree}"][..],
        // A commit-ish where a tree is wanted: git peels it, so this must too.
        &["commit-tree", "HEAD", "-m", "x"][..],
    ] {
        out.push(Case::new("commit-tree", args, Shape::Linear));
    }

    out.push(Case::new(
        "commit-tree",
        &["commit-tree", "-p", "HEAD", "-p", "feature", "-m", "merge", "HEAD^{tree}"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "commit-tree",
        &["commit-tree", "v0.2.0^{tree}", "-m", "tagtree"],
        Shape::Branched,
    ));

    // Error paths.
    for args in [
        &["commit-tree"][..],
        &["commit-tree", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef", "-m", "x"][..],
        &["commit-tree", "HEAD^{tree}", "-m", "msg", "--bogus"][..],
    ] {
        out.push(Case::new("commit-tree", args, Shape::Linear));
    }
}

/// `mktree`, `mktag`, `unpack-file`: small object constructors and extractors.
///
/// `mktree` and `mktag` read their definition from stdin, so only the empty
/// input is reachable — for `mktree` that is still the meaningful assertion that
/// no entries yields the canonical empty tree, and for `mktag` that a truncated
/// tag fails fsck.
fn tree_and_tag_builders(out: &mut Vec<Case>) {
    for args in [
        &["mktree"][..],
        &["mktree", "-z"][..],
        &["mktree", "--missing"][..],
        &["mktree", "--batch"][..],
        &["mktree", "-z", "--missing", "--batch"][..],
        &["mktree", "--bogus"][..],
    ] {
        out.push(Case::new("mktree", args, Shape::Linear));
    }

    for args in [
        &["mktag"][..],
        &["mktag", "--strict"][..],
        &["mktag", "--no-strict"][..],
        // mktag takes no operands.
        &["mktag", "extra"][..],
    ] {
        out.push(Case::new("mktag", args, Shape::Linear));
    }

    // `unpack-file` names its output file randomly, so the success case can only
    // ever be scored Nondeterministic — stock cannot reproduce its own stdout.
    // Kept anyway: it still proves zvcs neither crashes nor fails on the path,
    // and the argument form is deterministic even though the output is not.
    out.push(Case::new("unpack-file", &["unpack-file", "HEAD:README.md"], Shape::Linear));

    // Error paths, all deterministic.
    for args in [
        &["unpack-file"][..],
        &["unpack-file", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"][..],
        // A tree and a commit-ish are both the wrong object type here.
        &["unpack-file", "HEAD^{tree}"][..],
        &["unpack-file", "README.md"][..],
    ] {
        out.push(Case::new("unpack-file", args, Shape::Linear));
    }
}

/// Pack plumbing: `pack-objects`, `index-pack`, `unpack-objects`,
/// `verify-pack`, `show-index`, `prune-packed`.
///
/// `pack-objects` cases that actually pack something compare pack **bytes** —
/// via stdout directly, or via the pack checksum it prints when writing to disk.
/// That is a real difference and is left visible on purpose; `runner.rs` relaxes
/// the *storage* probe to counts precisely because the vendored gitoxide emits a
/// valid but differently-ordered, differently-compressed pack, and nothing
/// relaxes stdout.
fn pack_plumbing(out: &mut Vec<Case>) {
    // Empty object list on stdin: an empty pack, which both sides can agree on.
    for args in [
        &["pack-objects", "--stdout"][..],
        &["pack-objects", "--stdout", "-q"][..],
        &["pack-objects", "--stdout", "--non-empty"][..],
        &["pack-objects", "--stdout", "--window=0"][..],
        &["pack-objects", "--revs", "--stdout"][..],
        &["pack-objects", ".git/objects/pack/parity"][..],
        &["pack-objects"][..],
        &["pack-objects", "--bogus", "--stdout"][..],
    ] {
        out.push(Case::new("pack-objects", args, Shape::Linear));
    }
    // `--all --revs` supplies the object list without stdin, so these are the
    // only cases in the module that pack real content.
    for args in [
        &["pack-objects", "--all", "--revs", "--stdout"][..],
        &["pack-objects", "--all", "--revs", "--stdout", "--delta-base-offset"][..],
        &["pack-objects", "--all", "--revs", ".git/objects/pack/parity"][..],
    ] {
        out.push(Case::new("pack-objects", args, Shape::Linear));
    }
    out.push(Case::new(
        "pack-objects",
        &["pack-objects", "--all", "--revs", "--include-tag", "--stdout"],
        Shape::Branched,
    ));
    out.push(Case::new(
        "pack-objects",
        &["pack-objects", "--all", "--revs", "--stdout"],
        Shape::Merged,
    ));

    // index-pack: no readable pack exists in any fixture, so every case is a
    // rejection — a name that is not `*.pack`, a missing file, or an empty
    // stream. Agreeing on *how* they are rejected is the whole assertion.
    for args in [
        &["index-pack"][..],
        &["index-pack", "--stdin"][..],
        &["index-pack", "--stdin", "--fix-thin"][..],
        &["index-pack", "nope.pack"][..],
        &["index-pack", "--verify", "nope.pack"][..],
        &["index-pack", "README.md"][..],
        &["index-pack", "--keep", "README.md"][..],
        &["index-pack", "--strict", "README.md"][..],
        // `-o` bypasses the name check, so the file is actually read.
        &["index-pack", "-o", "out.idx", "README.md"][..],
    ] {
        out.push(Case::new("index-pack", args, Shape::Linear));
    }

    // unpack-objects reads the pack from stdin: empty-stream path only.
    for args in [
        &["unpack-objects"][..],
        &["unpack-objects", "-n"][..],
        &["unpack-objects", "-q"][..],
        &["unpack-objects", "-r"][..],
        &["unpack-objects", "--strict"][..],
        &["unpack-objects", "--max-input-size=10"][..],
        &["unpack-objects", "--bogus"][..],
    ] {
        out.push(Case::new("unpack-objects", args, Shape::Linear));
    }

    // verify-pack takes a path, but no fixture has an idx to point it at.
    for args in [
        &["verify-pack"][..],
        &["verify-pack", ".git/objects/pack/nope.idx"][..],
        &["verify-pack", "-s", ".git/objects/pack/nope.idx"][..],
        // An existing file that is not an index: the malformed-input path.
        &["verify-pack", "README.md"][..],
        &["verify-pack", "-v", "README.md"][..],
        &["verify-pack", "--object-format=sha1", "README.md"][..],
        &["verify-pack", "--bogus", "README.md"][..],
    ] {
        out.push(Case::new("verify-pack", args, Shape::Linear));
    }

    // show-index reads the idx from stdin only; empty input is a header error.
    for args in [
        &["show-index"][..],
        &["show-index", "--object-format=sha1"][..],
        &["show-index", "--object-format=bogus"][..],
        &["show-index", "extra"][..],
        &["show-index", "--bogus"][..],
    ] {
        out.push(Case::new("show-index", args, Shape::Linear));
    }

    // prune-packed on a repo with no pack must be a no-op that deletes nothing;
    // the state probe is the real assertion here, not the empty stdout.
    for args in [
        &["prune-packed", "--dry-run"][..],
    ] {
        out.push(Case::new("prune-packed", args, Shape::Linear));
    }
}

/// `stripspace`, `patch-id`, `get-tar-commit-id`, `var`.
///
/// The first three are stdin filters, so only their empty-input and
/// argument-rejection behavior is reachable. `var` takes no input at all and is
/// fully covered.
fn text_and_env(out: &mut Vec<Case>) {
    for args in [
        &["stripspace"][..],
        &["stripspace", "-s"][..],
        &["stripspace", "--strip-comments"][..],
        &["stripspace", "-c"][..],
        &["stripspace", "--comment-lines"][..],
        // -s and -c are mutually exclusive.
        &["stripspace", "-s", "-c"][..],
        &["stripspace", "--bogus"][..],
        &["stripspace", "extra-arg"][..],
    ] {
        out.push(Case::new("stripspace", args, Shape::Linear));
    }

    for args in [
        &["patch-id"][..],
        &["patch-id", "--stable"][..],
        &["patch-id", "--unstable"][..],
        &["patch-id", "--verbatim"][..],
        &["patch-id", "--bogus"][..],
        &["patch-id", "extra"][..],
    ] {
        out.push(Case::new("patch-id", args, Shape::Linear));
    }

    for args in [
        &["get-tar-commit-id"][..],
        &["get-tar-commit-id", "extra"][..],
        &["get-tar-commit-id", "--bogus"][..],
    ] {
        out.push(Case::new("get-tar-commit-id", args, Shape::Linear));
    }

    // `var` is deterministic only because `env::harden` pins identity, dates and
    // the editor; without that the ident vars would carry the wall clock.
    for args in [
        &["var", "GIT_AUTHOR_IDENT"][..],
        &["var", "GIT_COMMITTER_IDENT"][..],
        &["var", "GIT_EDITOR"][..],
        &["var", "GIT_SEQUENCE_EDITOR"][..],
        &["var", "GIT_PAGER"][..],
        &["var", "GIT_DEFAULT_BRANCH"][..],
        &["var", "GIT_ATTR_SYSTEM"][..],
        &["var", "GIT_ATTR_GLOBAL"][..],
        &["var", "GIT_CONFIG_SYSTEM"][..],
        &["var", "GIT_CONFIG_GLOBAL"][..],
        &["var", "GIT_SHELL_PATH"][..],
        &["var", "-l"][..],
        // Error paths: an unknown name, no name, and more than one name.
        &["var"][..],
        &["var", "NOT_A_VARIABLE"][..],
        &["var", "GIT_AUTHOR_IDENT", "GIT_COMMITTER_IDENT"][..],
    ] {
        out.push(Case::new("var", args, Shape::Linear));
    }
    // GIT_DEFAULT_BRANCH is answered from config, not from HEAD, so a detached
    // HEAD must not change it.
    out.push(Case::new("var", &["var", "GIT_DEFAULT_BRANCH"], Shape::Detached));
    out.push(Case::new("var", &["var", "GIT_AUTHOR_IDENT"], Shape::Branched));
}
