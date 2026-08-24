//! Differential corpus cases for `tag`, `describe`, `verify-tag`, `verify-commit`
//! and `mktag` — the five verbs that read and write *tag* objects and refs.
//!
//! What this corpus measures, and the fixture facts every case leans on:
//!
//! * [`Shape::Branched`] is the only shape carrying tags at all, and it carries
//!   one of each kind: `v0.1.0` is **lightweight** (`refs/tags/v0.1.0` points
//!   straight at commit `5915d79…`) and `v0.2.0` is **annotated** (the ref points
//!   at tag object `d7277ea…`, whose target is that same commit). That pairing is
//!   what makes the two halves of nearly every flag here separable: `%(objecttype)`
//!   answers `commit` for one and `tag` for the other, `tag -v` refuses the first
//!   and parses the second, `describe` without `--tags` can only see the second,
//!   and `--points-at v0.2.0` matches the tag *object* rather than the commit.
//! * `main` sits exactly on both tags and `feature` is one commit past them, so
//!   `describe HEAD` is the zero-distance form and `describe feature` is the
//!   `v0.2.0-1-g07e86d1` form. Every `--abbrev` case therefore names `feature`:
//!   at zero distance git prints the bare tag name and the abbreviation width is
//!   unobservable, which is exactly how a port that ignores `--abbrev` passes.
//! * No other shape has a tag, so `describe` on [`Shape::Linear`],
//!   [`Shape::Merged`], [`Shape::Octopus`], [`Shape::Detached`] and
//!   [`Shape::Dirty`] reaches the `--always`/`--all`/no-names paths and nothing
//!   else. That is not a gap being papered over — those *are* the paths a port
//!   gets wrong, because `--all` falls back through refs while `--always` falls
//!   back to a raw abbreviated id, and on [`Shape::Detached`] both fire at once
//!   (`HEAD` is at `edfab1b…`, which no ref names, so `--all --always` prints the
//!   id even though `--all` was asked for).
//! * Tag objects are byte-reproducible across both sides because `env::harden`
//!   pins the tagger identity and `GIT_COMMITTER_DATE`. That is what makes the
//!   creating cases worth running at all: `probe_state` compares
//!   `cat-file --batch-check --batch-all-objects`, so a tag object written with a
//!   wrong header order, a wrong tagger line or a mis-cleaned message has a
//!   different oid and is caught even though `tag` printed nothing.
//! * `for-each-ref %(objecttype)` in the same probe is what separates "created a
//!   lightweight tag" from "created an annotated one" — the two are
//!   indistinguishable on stdout, since a successful `git tag` is silent.
//!
//! Determinism, case by case:
//!
//! * `GIT_EDITOR` is `true` under `env::harden`, which makes the *empty message*
//!   path reachable and deterministic rather than a hang: `tag -a <name>` with no
//!   `-m`/`-F` opens the editor, gets an unchanged (comment-only) buffer back and
//!   dies with `no tag message?` (builtin/tag.c, `create_tag()` →
//!   `write_tag_body()` / the `cleanup_mode` check). `--edit` beside a `-m` is the
//!   same mechanism from the other direction: the editor exits 0 without touching
//!   the buffer, so the supplied message survives.
//! * Relative date formats read the wall clock and are absent. `%(taggerdate:unix)`,
//!   `%(taggerdate:short)` and `%(creatordate:unix)` are pinned by the fixture's
//!   committer date and are used instead.
//! * `--sort=creatordate` versus `--sort=taggerdate` is a real distinction here
//!   even though both tags carry the same timestamp: a lightweight tag has *no*
//!   tagger, so `%(taggerdate:unix)` is empty for `v0.1.0` and `%(creatordate:unix)`
//!   is not (`creatordate` falls back to the tagged commit's committer date). A
//!   port that aliases the two atoms prints the same two lines for both cases.
//!
//! **Signing is unreachable, stated rather than pretended around.** There is no
//! gpg key anywhere in the harness and `env::harden` gives the child a fresh
//! `HOME`, so no case here can produce a *signed* object and none claims to.
//! `verify-tag`/`verify-commit` are therefore measured on the unsigned verdict —
//! exit 1, `error: no signature found`, with the object still parsed and printed
//! under `-v` — which is the path that runs without ever invoking gpg. The one
//! case that does reach the signing code (`tag -s` under
//! `gpg.program=/nonexistent-gpg`) reaches it only to fail the `exec`, and it is
//! deliberately **not** `strict`: the `cannot exec` line is written by the forked
//! child onto a shared stderr, so its interleaving with git's own `error:` lines
//! is a scheduling artifact rather than a property of the implementation. Its
//! exit code, its (empty) stdout and its post-command state are still compared.
//!
//! `mktag` bodies name object ids that exist in the fixture — the initial commit
//! `edfab1b…` and its tree `e0e1a77…`, both present in every shape built on the
//! common base — so the success path actually writes an object. The one body
//! naming the empty blob's constant id `e69de29…` is an *error* case on purpose
//! and is labelled as one below: no fixture contains an empty file, so that
//! object is absent from every object store and git stops at
//! `could not read tagged object` before fsck ever runs.

use crate::fixture::Shape;
use crate::runner::Case;

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    tag_create(out);
    tag_list(out);
    describe(out);
    verify(out);
    mktag(out);
    configured(out);
}

/// A tag message delivered on stdin for `tag -F -`.
///
/// Two paragraphs, because the blank line is what `--cleanup` and the
/// subject/body split in `%(contents:subject)` key on — a one-line message would
/// score a port that never separates them as correct.
const TAG_MESSAGE_STDIN: &[u8] = b"from stdin body\n\nsecond paragraph\n";

/// A well-formed tag over the fixture's initial commit `edfab1b…`.
const MKTAG_COMMIT: &[u8] = b"object edfab1b71619a22120a8da1a3d85d68e0200290a\n\
type commit\n\
tag parity-commit-tag\n\
tagger zvcs parity <parity@example.invalid> 1700000000 +0000\n\
\n\
tagging the initial commit\n";

/// A well-formed tag over the fixture's initial *tree* `e0e1a77…`.
///
/// A tag may name any object type; a port that only ever peels to a commit
/// rejects this one.
const MKTAG_TREE: &[u8] = b"object e0e1a776261f58b1c8741e3747adde42edd1a859\n\
type tree\n\
tag parity-tree-tag\n\
tagger zvcs parity <parity@example.invalid> 1700000000 +0000\n\
\n\
tagging the initial tree\n";

/// Headers, blank line, and nothing after it. The message is legitimately empty
/// and the object is still valid — a port that requires a body writes a
/// different oid or refuses.
const MKTAG_EMPTY_BODY: &[u8] = b"object edfab1b71619a22120a8da1a3d85d68e0200290a\n\
type commit\n\
tag parity-no-body\n\
tagger zvcs parity <parity@example.invalid> 1700000000 +0000\n";

/// The same tag whose message does not end in a newline. `mktag` stores the
/// input verbatim, so the missing byte moves the oid; a port that "helpfully"
/// terminates the message writes the wrong object.
const MKTAG_NO_TRAILING_NEWLINE: &[u8] = b"object edfab1b71619a22120a8da1a3d85d68e0200290a\n\
type commit\n\
tag parity-no-trailing-nl\n\
tagger zvcs parity <parity@example.invalid> 1700000000 +0000\n\
\n\
no trailing newline";

/// A tag name `check_refname_format` rejects, so `--no-strict` demotes the fsck
/// verdict to a warning and writes the object anyway.
const MKTAG_BAD_NAME: &[u8] = b"object edfab1b71619a22120a8da1a3d85d68e0200290a\n\
type commit\n\
tag bad..name\n\
tagger zvcs parity <parity@example.invalid> 1700000000 +0000\n\
\n\
bad ref name\n";

/// `type blob` over an object that is a commit: caught by the *type* check in
/// builtin/mktag.c (`verify_object_in_tag`), not by fsck.
const MKTAG_WRONG_TYPE: &[u8] = b"object edfab1b71619a22120a8da1a3d85d68e0200290a\n\
type blob\n\
tag wrong-type\n\
tagger zvcs parity <parity@example.invalid> 1700000000 +0000\n\
\n\
mismatch\n";

/// No `tagger` line at all: fsck's `missingTaggerEntry`.
const MKTAG_NO_TAGGER: &[u8] = b"object edfab1b71619a22120a8da1a3d85d68e0200290a\n\
type commit\n\
tag parity-no-tagger\n\
\n\
missing tagger\n";

/// A `tagger` line with no email: fsck's `missingEmail`. Distinct from the
/// missing-line case above, and a port that only checks for the line's presence
/// accepts it.
const MKTAG_BAD_TAGGER: &[u8] = b"object edfab1b71619a22120a8da1a3d85d68e0200290a\n\
type commit\n\
tag parity-bad-tagger\n\
tagger nobody\n\
\n\
malformed tagger\n";

/// An `object` line whose value is not a hex oid: fsck's `badObjectSha1`, which
/// fires before the object store is ever consulted.
const MKTAG_BAD_OID: &[u8] = b"object notahexoid\n\
type blob\n\
tag parity-bad-oid\n\
tagger zvcs parity <parity@example.invalid> 1700000000 +0000\n\
\n\
bad oid\n";

/// No `object` line: fsck's `missingObject`.
const MKTAG_NO_OBJECT: &[u8] = b"type commit\n\
tag parity-no-object\n\
tagger zvcs parity <parity@example.invalid> 1700000000 +0000\n\
\n\
no object line\n";

/// A well-formed tag naming the empty blob's constant id, which no fixture
/// contains. Stops at `could not read tagged object` — the existence check, a
/// step earlier than every other rejection above.
const MKTAG_ABSENT_OBJECT: &[u8] = b"object e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\n\
type blob\n\
tag parity-absent-object\n\
tagger zvcs parity <parity@example.invalid> 1700000000 +0000\n\
\n\
tagging an object that is not here\n";

// ---------------------------------------------------------------------------
// tag: creation, deletion, verification
// ---------------------------------------------------------------------------

/// Creating and deleting tags.
///
/// A successful `git tag` prints nothing, so every case in this group is scored
/// almost entirely by `probe_state`: `for-each-ref` proves the ref landed with
/// the right target *and* the right object type, and
/// `cat-file --batch-check --batch-all-objects` proves the annotated cases wrote
/// a tag object whose bytes hash to the same id on both sides. A port that
/// creates a lightweight ref where git creates an annotated tag, or that emits
/// the tag headers in a different order, is invisible on stdout and caught here.
fn tag_create(out: &mut Vec<Case>) {
    let b = |args: &[&str], out: &mut Vec<Case>| out.push(Case::new("tag", args, Shape::Branched));

    // The three ways to ask for an annotated tag. `-m` alone implies `-a`
    // (builtin/tag.c sets `annotate = 1` when a message is given), which is the
    // rule a port most often drops — it produces a *lightweight* ref and loses
    // the message with no diagnostic at all.
    b(&["tag", "vfresh"], out);
    b(&["tag", "-a", "vann", "-m", "annotated body"], out);
    b(&["tag", "-m", "implied annotate", "vimp"], out);

    // GIT_EDITOR is `true`, so `-a` with no message gets an unchanged buffer
    // back and dies. Strict: the refusal text and the exit code are the whole
    // observable result.
    out.push(Case::strict("tag", &["tag", "-a", "vnoedit"], Shape::Branched));
    // The same editor, invoked *beside* a message: it exits without writing, so
    // the `-m` text survives and the tag is created normally.
    b(&["tag", "--edit", "-m", "edited", "vedit"], out);

    // `-F -` reads the message from stdin. The payload has a blank line in it,
    // so the resulting tag object separates subject from body — and `-F` implies
    // `-a` the same way `-m` does.
    out.push(Case::with_stdin("tag", &["tag", "-F", "-", "vfile"], Shape::Branched, TAG_MESSAGE_STDIN));

    // Message cleanup: `verbatim` keeps the `#` line that the default mode
    // (`whitespace`, used for every `-m` message above) strips. A different
    // message body is a different tag object id, so the object probe measures it
    // even though the command prints nothing.
    b(&["tag", "-a", "vverbatim", "-m", "msg\n# comment\n", "--cleanup=verbatim"], out);
    // A trailer is appended by the same interpret-trailers machinery `commit`
    // uses; it lands in the tag object's message and moves its id.
    b(&["tag", "-a", "vtrailer", "-m", "body", "--trailer", "Acked-by=Someone"], out);

    // `-f` on an existing tag prints `Updated tag 'x' (was <abbrev>)` — the one
    // creating form that has stdout, and it carries an abbreviated id whose
    // width a port can get wrong.
    b(&["tag", "-f", "v0.1.0", "HEAD~1"], out);
    // Without `-f`, both the lightweight and the annotated form refuse.
    out.push(Case::strict("tag", &["tag", "v0.1.0"], Shape::Branched));

    // Deletion. Several names in one call must report each one with the id it
    // had — for the annotated tag that is the *tag object* id, not the commit's.
    b(&["tag", "-d", "v0.1.0", "v0.2.0"], out);
    // Missing name: `error:` and exit 1, not `fatal:` and 128.
    out.push(Case::strict("tag", &["tag", "-d", "nosuchtag"], Shape::Branched));

    // A name `check_refname_format` rejects, refused before any ref is written.
    // (`tag.lock` and `HEAD` are rejected by the same function with the same
    // wording, so one representative is enough here.)
    out.push(Case::strict("tag", &["tag", "bad..name"], Shape::Branched));

    // The commit-ish operand, in every form a caller writes it. Naming the
    // annotated tag creates a ref at the *tag object* (git does not peel here),
    // `HEAD^{}` peels explicitly, `HEAD^{tree}` proves a tag may name a tree, and
    // the abbreviated id has to be resolved before the ref is written.
    b(&["tag", "from-tag", "v0.2.0"], out);
    b(&["tag", "from-peeled", "HEAD^{}"], out);
    b(&["tag", "from-tree", "HEAD^{tree}"], out);
    b(&["tag", "from-abbrev", "5915d79"], out);

    // `--create-reflog` is accepted and does not change stdout; the probe does
    // not read reflogs, so this pins the flag's parse and the ref it still has
    // to write. Recorded as a known probe limit rather than a measured fact.
    b(&["tag", "--create-reflog", "vreflog"], out);

    // Creating from a subdirectory: the ref is repository-scoped, so the cwd
    // must not leak into the name or the lookup.
    out.push(Case::new("tag", &["tag", "vsub"], Shape::Branched).in_dir("src"));

    // `-v`: a lightweight tag is not a tag object, so verification cannot start;
    // an annotated one parses, prints its body on stdout, and then fails for
    // want of a signature. Both strict — the refusal is the result.
    out.push(Case::strict("tag", &["tag", "-v", "v0.1.0"], Shape::Branched));
    out.push(Case::strict("tag", &["tag", "-v", "v0.2.0"], Shape::Branched));

    // Other shapes. `HEAD^2` and `HEAD^3` name parents no linear history has, so
    // the operand has to go through real rev parsing; and Detached has no branch
    // at all, which is where a port that writes the tag through the current
    // branch breaks.
    out.push(Case::new("tag", &["tag", "-a", "vmerge", "-m", "second parent", "HEAD^2"], Shape::Merged));
    out.push(Case::new("tag", &["tag", "-a", "voct", "-m", "third parent", "HEAD^3"], Shape::Octopus));
    out.push(Case::new("tag", &["tag", "vdetach"], Shape::Detached));
}

// ---------------------------------------------------------------------------
// tag: listing
// ---------------------------------------------------------------------------

/// Listing, filtering, sorting and formatting.
///
/// All on [`Shape::Branched`] unless noted, because it is the only shape with a
/// tag to list and — decisively — the only one with *both* kinds, so every atom
/// below has a lightweight row and an annotated row to disagree about.
fn tag_list(out: &mut Vec<Case>) {
    let b = |args: &[&str], out: &mut Vec<Case>| out.push(Case::new("tag", args, Shape::Branched));

    // Patterns. `-l` matches with `wildmatch` against the *short* name: two
    // literal patterns OR together, `*` behaves as in a shell glob, and matching
    // is case-sensitive until `--ignore-case` says otherwise.
    b(&["tag", "-l", "v0.1.0", "v0.2.0"], out);
    b(&["tag", "-l", "*.0"], out);
    b(&["tag", "-l", "V0.*"], out);
    b(&["tag", "-l", "--ignore-case", "V0.*"], out);

    // Sorting. `refname` is the default, so `-refname` is the form that proves
    // the sort runs at all; `version:refname` is the version-aware comparator
    // rather than a string compare (its reversed form is reached from config
    // below); and `creatordate` and `taggerdate` are *not* the same key even
    // though both tags carry the same pinned timestamp, because a lightweight
    // tag has no tagger at all.
    b(&["tag", "--sort=-refname"], out);
    b(&["tag", "--sort=version:refname"], out);
    // The two date keys with their values printed, which is what shows that
    // `taggerdate` is empty for the lightweight tag and `creatordate` is not.
    b(&["tag", "--sort=creatordate", "--format=%(creatordate:unix) %(refname:short)"], out);
    b(&["tag", "--sort=taggerdate", "--format=%(taggerdate:unix) %(refname:short)"], out);
    out.push(Case::strict("tag", &["tag", "--sort=nosuchfield"], Shape::Branched));

    // `-n`: the annotation column, which is the only listing mode that reads a
    // message at all. The lightweight tag has no tag message, so git falls back
    // to the tagged *commit's* subject for it and to the tag's own for the
    // annotated one — two different sources in two adjacent lines.
    b(&["tag", "-n"], out);

    // `--format`, one group of atoms per case so a failure names the atom.
    // `%(objecttype)` and `%(*objectname)` are the two that separate the
    // lightweight row from the annotated one outright: the first prints
    // commit/tag, the second is empty for a ref that needs no peeling.
    b(&["tag", "--format=%(refname:short) %(objecttype) %(contents:subject)"], out);
    b(&["tag", "--format=%(taggername) %(taggeremail) %(taggerdate:short)"], out);
    b(&["tag", "--format=%(objectname) %(*objectname)"], out);
    // `%(if)` over `%(taggername)` is the formatter's own answer to "is this tag
    // annotated"; the `%(else)` arm is what a port that treats an absent atom as
    // an error never reaches.
    b(&["tag", "--format=%(if)%(taggername)%(then)A %(refname:short)%(else)L %(refname:short)%(end)"], out);
    // The same format with an empty result for the lightweight row: without
    // `--omit-empty` git prints a blank line for it, with it prints nothing.
    b(&["tag", "--omit-empty", "--format=%(if)%(taggername)%(then)%(refname:short)%(end)"], out);
    // Reachability filters. `HEAD~1` is an ancestor of both tags, `feature` is a
    // sibling of neither, so the four flags split the tag set two different ways
    // and a port that implements `--no-contains` as "not --contains" over the
    // wrong base agrees on one and not the other.
    b(&["tag", "--contains", "HEAD~1"], out);
    b(&["tag", "--contains", "feature"], out);
    b(&["tag", "--no-contains", "HEAD~1"], out);
    b(&["tag", "--merged", "feature"], out);
    b(&["tag", "--no-merged", "feature"], out);

    // `--points-at` compares the ref's *own* object id, before peeling: `HEAD`
    // matches both tags because the lightweight ref and the annotated tag's
    // target are the same commit, while `v0.2.0` resolves to the tag object and
    // so matches only itself.
    b(&["tag", "--points-at", "HEAD"], out);
    b(&["tag", "--points-at", "v0.2.0"], out);
    // A bad object name to `--points-at` is a parse-time `error:` with exit 129
    // — `--merged` answers the same input with `fatal:` and 128, so a port that
    // routes every bad rev through one helper gets one of the two wrong.
    out.push(Case::strict("tag", &["tag", "--points-at", "nosuchrev"], Shape::Branched));
    // A merge's second parent, which only a shape with a merge can name.
    out.push(Case::new("tag", &["tag", "--points-at", "HEAD^2"], Shape::Merged));

    // Columns. With TERM=dumb and no tty, `--column` still packs the list onto
    // one line because the option was asked for explicitly.
    b(&["tag", "--column"], out);
    b(&["tag", "--no-column"], out);

    // Flags that interact: `--format` overrides `-n` entirely rather than adding
    // to it, so the annotation column has to disappear.
    b(&["tag", "-l", "-n", "--format=%(refname)"], out);
}

// ---------------------------------------------------------------------------
// describe
// ---------------------------------------------------------------------------

/// `describe`: naming a commit after the nearest tag that reaches it.
///
/// Every case that wants a non-zero distance names `feature`. `HEAD` sits *on*
/// both tags, so it prints a bare tag name with no id at all — which is exactly
/// the shape in which `--abbrev`, `--long` and the `-<n>-g` suffix are
/// unobservable, and exactly how a port with no abbreviation logic passes.
fn describe(out: &mut Vec<Case>) {
    let b = |args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("describe", args, Shape::Branched))
    };

    // The default only considers annotated tags; `--tags` admits lightweight
    // ones too. Both name `v0.2.0` here because it is the annotated one and both
    // sit on the same commit — the pair matters together with the `--exclude`
    // cases below, which is where the two answers finally differ.
    b(&["describe", "feature"], out);
    b(&["describe", "--tags", "feature"], out);
    // `--long` forces the `-<n>-g<id>` suffix unconditionally rather than only
    // when the distance is non-zero.
    b(&["describe", "--long", "feature"], out);

    // Abbreviation width. `--abbrev=0` suppresses the suffix entirely (a
    // different code path from a narrow id); 1 is below `MINIMUM_ABBREV` and is
    // clamped *up* to 4; 40 is the full id; 41 is past the end of one and is
    // clamped down rather than rejected. The remaining documented width, 4, is
    // reached through `core.abbrev` below, which is the half a port that reads
    // only the flag misses.
    b(&["describe", "--abbrev=0", "feature"], out);
    b(&["describe", "--abbrev=1", "feature"], out);
    b(&["describe", "--abbrev=40", "feature"], out);
    b(&["describe", "--abbrev=41", "feature"], out);
    // `--long` and `--abbrev=0` are mutually exclusive: one demands the suffix,
    // the other forbids it.
    out.push(Case::strict("describe", &["describe", "--tags", "--long", "--abbrev=0", "feature"], Shape::Branched));

    // `--all` widens the candidate set from tags to *every* ref, so `feature`
    // describes itself as `heads/feature` rather than as a distance past a tag.
    b(&["describe", "--all", "feature"], out);

    // `--exact-match` and `--candidates=0` are the same request spelled two ways
    // (builtin/describe.c maps the former onto `max_candidates = 0`): a name or
    // nothing, never a distance. `HEAD` has one, `feature` does not, so the pair
    // covers both verdicts.
    b(&["describe", "--exact-match"], out);
    out.push(Case::strict("describe", &["describe", "--candidates=0", "feature"], Shape::Branched));

    // `--contains` inverts the search: it names the *earliest* tag that contains
    // the commit and renders the offset with `~`, through name-rev rather than
    // through the describe walk. `HEAD~1` is two commits below `v0.1.0`, and
    // `v0.1.0` — the lightweight tag — is what `--contains` picks even though
    // plain describe would pick the annotated `v0.2.0`.
    b(&["describe", "--contains"], out);
    b(&["describe", "--contains", "HEAD~1"], out);

    // Candidate filtering. `--match` and `--exclude` both act on the short tag
    // name; without `--tags` they can only ever narrow the annotated set, which
    // is why excluding `v0.2.*` leaves nothing and produces the two-line
    // "there were unannotated tags: try --tags" advice, while the same exclusion
    // with `--tags` finds `v0.1.0`.
    b(&["describe", "--match", "v0.*", "feature"], out);
    b(&["describe", "--tags", "--match", "v0.1.*", "feature"], out);
    b(&["describe", "--tags", "--exclude", "v0.2.*", "feature"], out);
    out.push(Case::strict("describe", &["describe", "--exclude", "v0.2.*", "feature"], Shape::Branched));

    // `--first-parent` restricts the walk. Branched has no merge, so this pins
    // that the flag is accepted and does not change a linear answer; the
    // *interesting* half — a merge whose two sides carry different tags — is not
    // constructible, since no shape with a merge has a tag and a case cannot
    // create one first. Recorded rather than faked.
    b(&["describe", "--first-parent", "feature"], out);

    // Blobs. Since 2.16 describe has a separate path for them
    // (builtin/describe.c `describe_blob()`): it walks history for a commit that
    // introduced the blob and prints `<describe of that commit>:<path>`.
    // `src/lib.rs` was last written by the tagged commit, so it names the tag;
    // `README.md` dates from the initial commit, which no tag reaches, so it
    // needs `--always` and prints a raw id.
    b(&["describe", "HEAD:src/lib.rs"], out);
    b(&["describe", "--always", "HEAD:README.md"], out);
    // A tree is neither, and is refused by name rather than silently described.
    out.push(Case::strict("describe", &["describe", "HEAD^{tree}"], Shape::Branched));

    // ---- shapes with no tags at all ----
    // The fallback ladder. Without a tag, plain describe dies; `--all` drops to a
    // ref name; `--always` drops to an abbreviated id. They are three different
    // fallbacks and a port that implements one as the other is only visible on a
    // shape that has no tag to hide behind.
    out.push(Case::strict("describe", &["describe"], Shape::Linear));
    out.push(Case::new("describe", &["describe", "--all", "--long"], Shape::Merged));
    out.push(Case::new("describe", &["describe", "--all", "--long"], Shape::Octopus));
    // Detached HEAD is at a commit no ref names, so `--all` finds nothing and
    // `--always` has to catch it — the one shape where both fallbacks fire in
    // one invocation.
    out.push(Case::new("describe", &["describe", "--all", "--always"], Shape::Detached));

    // ---- dirty marking ----
    // `--dirty` appends a mark when the worktree differs from HEAD, and its
    // argument replaces the default `-dirty`. `--broken` is the same idea for a
    // repository whose diff *fails*; on a merely dirty tree it still reports
    // `-dirty`, which is the distinction a port collapses.
    out.push(Case::new("describe", &["describe", "--always", "--dirty"], Shape::Dirty));
    out.push(Case::new("describe", &["describe", "--always", "--dirty=-modified"], Shape::Dirty));
    out.push(Case::new("describe", &["describe", "--always", "--broken"], Shape::Dirty));
}

// ---------------------------------------------------------------------------
// verify-tag / verify-commit
// ---------------------------------------------------------------------------

/// The unsigned verdict, in the forms the base corpus does not reach.
///
/// `corpus/info_attrs.rs` already covers the single-name calls non-strictly.
/// What is added here is the multi-name behaviour — git keeps going after a
/// failure and reports each name, exiting 1 once at the end — plus `--format`
/// atoms and the wrong-object-type refusals, all strict where the refusal text
/// *is* the result.
fn verify(out: &mut Vec<Case>) {
    // `--format` on an unsigned tag produces no output at all: the format is
    // rendered only after verification succeeds. A port that formats first and
    // checks afterwards prints a line git does not.
    out.push(Case::new(
        "verify-tag",
        &["verify-tag", "--format=%(objectname) %(objecttype) %(tag) %(taggername)", "v0.2.0"],
        Shape::Branched,
    ));
    // Two names, the first of which is a lightweight tag: the run must not stop
    // at the first refusal, and the second name's body must still be printed
    // under `-v`.
    out.push(Case::strict("verify-tag", &["verify-tag", "-v", "v0.1.0", "v0.2.0"], Shape::Branched));
    out.push(Case::strict("verify-tag", &["verify-tag", "--raw", "v0.1.0", "v0.2.0"], Shape::Branched));
    // A tree: a third object type, with its own message wording.
    out.push(Case::strict("verify-tag", &["verify-tag", "HEAD^{tree}"], Shape::Branched));

    // verify-commit over several revs, and over an object that is a tag.
    out.push(Case::new("verify-commit", &["verify-commit", "--raw", "HEAD", "HEAD~1"], Shape::Branched));
    out.push(Case::strict("verify-commit", &["verify-commit", "-v", "v0.2.0"], Shape::Branched));
    // Shapes the base corpus's `verify-commit` cases do not reach: a merge
    // commit has extra parent headers to parse before the signature is looked
    // for, and an octopus has three of them.
    out.push(Case::new("verify-commit", &["verify-commit", "--raw", "HEAD"], Shape::Merged));
    out.push(Case::new("verify-commit", &["verify-commit", "HEAD"], Shape::Octopus));
}

// ---------------------------------------------------------------------------
// mktag
// ---------------------------------------------------------------------------

/// `mktag`: a tag object built from stdin, with fsck as the gate.
///
/// The success cases are worth more than their stdout: the printed oid is a hash
/// of the exact bytes git stored, so a port that reorders headers, rewrites the
/// tagger line or normalizes the message prints a different id *and* leaves a
/// different object behind for `cat-file --batch-all-objects` to find.
///
/// The rejections are deliberately one per gate, in the order builtin/mktag.c
/// applies them: `badObjectSha1` (the `object` line does not parse),
/// `missingObject` / `missingTaggerEntry` / `missingEmail` (fsck on the header
/// block), the object-existence check, and finally the type check — a port that
/// collapses them into one "invalid tag" answer agrees on the exit code and on
/// nothing else.
fn mktag(out: &mut Vec<Case>) {
    let s = |body: &'static [u8], out: &mut Vec<Case>| {
        out.push(Case::with_stdin("mktag", &["mktag"], Shape::Linear, body))
    };
    let strict = |args: &'static [&'static str], body: &'static [u8], out: &mut Vec<Case>| {
        // `compare_stderr` is a public field, so a strict stdin case is a
        // struct update rather than a constructor the runner does not have.
        out.push(Case { compare_stderr: true, ..Case::with_stdin("mktag", args, Shape::Linear, body) })
    };

    s(MKTAG_COMMIT, out);
    s(MKTAG_TREE, out);
    s(MKTAG_EMPTY_BODY, out);
    s(MKTAG_NO_TRAILING_NEWLINE, out);
    // `--no-strict` demotes fsck to warnings, so an unwritable-as-a-ref name
    // still yields an object.
    s(MKTAG_BAD_NAME, out);
    out.push(Case::with_stdin("mktag", &["mktag", "--no-strict"], Shape::Linear, MKTAG_BAD_NAME));

    strict(&["mktag"], MKTAG_WRONG_TYPE, out);
    strict(&["mktag"], MKTAG_NO_TAGGER, out);
    strict(&["mktag"], MKTAG_BAD_TAGGER, out);
    strict(&["mktag"], MKTAG_BAD_OID, out);
    strict(&["mktag"], MKTAG_NO_OBJECT, out);
    strict(&["mktag"], MKTAG_ABSENT_OBJECT, out);
}

// ---------------------------------------------------------------------------
// configuration
// ---------------------------------------------------------------------------

/// The settings these verbs read.
///
/// Each pair is chosen so the setting *changes the answer* on this fixture: a
/// case that merely proves a key is accepted would score a port that parses the
/// config and ignores it as correct.
fn configured(out: &mut Vec<Case>) {
    let tag = |cfg: &[(&str, &str)], args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("tag", args, Shape::Branched).with_config(cfg))
    };
    let desc = |cfg: &[(&str, &str)], args: &[&str], out: &mut Vec<Case>| {
        out.push(Case::new("describe", args, Shape::Branched).with_config(cfg))
    };

    // `versionsort.suffix` is an ordered list: a refname carrying an earlier
    // suffix sorts before one carrying a later suffix, whatever the version
    // compare would otherwise say. Naming `.2.0` first and `.1.0` second
    // therefore *reverses* the natural order, so this proves the list is read in
    // order and applied — not merely accepted.
    tag(
        &[("versionsort.suffix", ".2.0"), ("versionsort.suffix", ".1.0")],
        &["tag", "--sort=version:refname"],
        out,
    );
    // The deprecated spelling of the same key, which git still honours.
    tag(&[("versionsort.prereleaseSuffix", ".2.0")], &["tag", "--sort=version:refname"], out);

    // `tag.sort` supplies the default sort key when `--sort` is absent. The
    // base corpus already covers `-refname`; the version comparator is a
    // different code path reached through the same key.
    tag(&[("tag.sort", "-version:refname")], &["tag"], out);

    // Column layout from config rather than from the command line: `column.tag`
    // has to override the "no tty, so no columns" default in one direction and
    // the explicit `--column` in the other.
    tag(&[("column.tag", "always")], &["tag"], out);
    tag(&[("column.tag", "never")], &["tag"], out);

    // `tag.gpgSign=false` must leave an annotated tag unsigned — and, since the
    // tag object's bytes are compared, must not perturb it either.
    tag(&[("tag.gpgSign", "false")], &["tag", "-a", "vsig", "-m", "msg"], out);
    // `tag.forceSignAnnotated` signs a tag that became annotated *implicitly*
    // (a `-m` with no `-a`); with gpg pointed at nothing, that turns into a
    // deterministic refusal. Not strict: the `cannot exec` line comes from the
    // forked child on a shared stderr, so its position among git's own lines is
    // a scheduling artifact. Exit code, stdout and post-command state are still
    // compared, and they are what distinguishes "tried to sign and failed" from
    // "ignored the setting and wrote a tag".
    tag(
        &[("tag.forceSignAnnotated", "true"), ("gpg.program", "/nonexistent-gpg")],
        &["tag", "-m", "forced sign", "vfsa"],
        out,
    );
    // The same failure asked for explicitly with `-s`.
    tag(&[("gpg.program", "/nonexistent-gpg")], &["tag", "-s", "vsigned", "-m", "msg"], out);
    // And the read side: an unsigned tag never reaches gpg at all, so a bogus
    // `gpg.program` must not change the verdict. A port that shells out before
    // looking for a signature block fails here and nowhere else.
    out.push(
        Case::new("verify-tag", &["verify-tag", "v0.2.0"], Shape::Branched)
            .with_config(&[("gpg.program", "/nonexistent-gpg")]),
    );

    // `core.abbrev` sets the width of every id describe prints, and it is the
    // only route to the documented width 4 that no `--abbrev` case above reaches.
    // At distance zero no id is printed at all, so this names `feature`; a port
    // that hard-codes 7 passes every other describe case in this file.
    desc(&[("core.abbrev", "4")], &["describe", "feature"], out);
    // A value that is not a number: the config parser's own diagnostic, which
    // names the key and the unit rule.
    out.push(
        Case::strict("describe", &["describe", "feature"], Shape::Branched)
            .with_config(&[("core.abbrev", "nonsense")]),
    );
}
