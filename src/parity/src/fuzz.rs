//! Combinatorial flag fuzzing with deterministic seeding and shrinking.
//!
//! The corpus covers what a human thought to check. This covers what nobody
//! thought to check: flag combinations, argument orderings, and rev-spec forms
//! that a real caller will eventually produce.
//!
//! Determinism is a hard requirement — a parity failure nobody can reproduce is
//! not actionable. Every case is a pure function of `(seed, index)`, so a failing
//! run replays exactly from the seed printed in its report.
//!
//! # What a configuration draw costs
//!
//! Configuration used to be one dimension with one delivery mechanism: a key, a
//! value, and `-c`. It is now two — *which keys* and *which scope each one comes
//! from* — and the scope half means some draws write files into the fixture. That
//! is the only new cost in this file, and it is worth stating what it is before
//! reading the sampler that spends it.
//!
//! **The price.** A draw that lands on a file scope costs one `open`/`write`/
//! `close` of a few hundred bytes, per side, per case, into a directory tree the
//! runner has just created and deletes when the case ends. At most five such
//! writes — one per file scope — and none at all for a draw that stays on `-c`
//! or on `GIT_CONFIG_KEY_<n>`, which is most of them. No extra child process, no
//! extra fixture template, no extra comparison, and not one more case in the
//! parity denominator: a scoped case is the same single invocation it was, read
//! under a premise that was put in place before it ran.
//!
//! **Why this is the cheap shape.** The two alternatives are both more
//! expensive, and the first is also wrong:
//!
//!  * *Install each key by running `git config --file …` first.* One child
//!    process per key per side, and on the zvcs side it is the **implementation
//!    under test** writing the premise. A port with a broken config writer would
//!    corrupt its own premise, and the case would then be measuring the writer
//!    twice instead of measuring the key once — which is the one thing a
//!    differential harness must never do. Writing the bytes from Rust means both
//!    sides start from a file this crate produced, byte for byte.
//!  * *A fixture template per scope.* Every shape crossed with the scope
//!    combinations, built at start-up and hashed, so that a four-line text file
//!    could exist. The premise is smaller than the machinery.
//!
//! The two sides live at different roots, which is the problem [`Case::cwd`] and
//! [`crate::runner::REPO_PLACEHOLDER`] already solve for directories and
//! environment values; the file scopes solve it the same way, by naming a path
//! *relative to the side's own fixture root* and letting the runner resolve it
//! per side ([`crate::runner::scope_file`]). A case names a scope and never a
//! path, which is also what keeps the two synthetic scopes compatible with the
//! hardened environment — see [`crate::runner::ConfigScope`].

use crate::fixture::Shape;
use crate::runner::{Case, ConfigEntry, ConfigScope, Sequence};

/// xorshift64*. Chosen for being reproducible and dependency-free rather than
/// statistically excellent — case selection does not need cryptographic quality.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        // A zero state is absorbing for xorshift; remap it.
        Self(if seed == 0 { 0x9E3779B97F4A7C15 } else { seed })
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }

    fn chance(&mut self, num: u64, denom: u64) -> bool {
        self.next() % denom < num
    }

    /// A count biased toward the low end but with a real tail: most draws are
    /// small, yet `max` still comes up often enough to exercise deep stacking.
    fn count_upto(&mut self, max: usize) -> usize {
        // Two rolls, take the min — triangular, tail toward 0, but the full
        // range is reachable. Deep combinations stay rare without being absent.
        let a = self.below(max + 1);
        let b = self.below(max + 1);
        a.min(b).max(if self.chance(1, 6) { max } else { 0 })
    }
}

/// What a subcommand accepts, as a grammar the generator samples from.
pub struct Grammar {
    pub cmd: &'static str,
    /// Flags safe to combine freely.
    pub flags: &'static [&'static str],
    /// Positional arguments — revs, paths, or refs depending on the command.
    pub positionals: &'static [&'static str],
    /// Shapes this command is meaningful against.
    pub shapes: &'static [Shape],
}

/// The shapes a rev-resolving command is drawn against.
///
/// `BehindRemote` is here for one reason and it is not topology: it is the only
/// shape whose `main` has an upstream, and without it the whole `@{upstream}`
/// half of git's rev grammar — `@{u}`, `@{push}`, `main@{upstream}`,
/// `origin/main`, `refs/remotes/origin/main` — could only ever be measured on
/// its refusal (`fatal: no upstream configured for branch 'main'`, verified
/// against stock 2.55.0 on every other shape here). A port that resolves the
/// spelling but reads the wrong `branch.<name>.merge` looks correct until one
/// case resolves it for real. The template already exists for the round trips
/// and generated grammars, so the shape costs no fixture and no case.
///
/// The four below it are here for the opposite reason: each is a *history* that
/// answers a question the five above cannot be asked at all. Every one of those
/// descends from the one `initial` commit, has at most one merge base for any
/// pair of tips, carries each patch once, and is read straight out of its
/// objects — so four answers a walk can give were unreachable however the argv
/// was drawn.
///
///  * `Unrelated` — three roots in one repository. Verified against stock 2.55.0
///    on this shape: `rev-list --max-parents=0 --all` prints **three** ids where
///    every shape above prints one, `merge alien-clash` is `fatal: refusing to
///    merge unrelated histories` (rc 128), and the same merge under
///    `--allow-unrelated-histories` is an add/add conflict on `README.md` whose
///    index holds stages 2 and 3 and **no stage 1** — the only conflict this
///    harness can produce with no base to diff against.
///  * `CrissCross` — two incomparable merge bases. Verified: `merge-base --all
///    cc-left cc-right` prints two ids (`0a24ba32…`, `27e7a991…`) where every
///    other shape prints one, and after `merge cc-right` (rc 1, `CONFLICT
///    (content): Merge conflict in clash.txt`) stage 1 is a blob no commit holds
///    — `cat-file -p :1:clash.txt` starts `<<<<<<<<< Temporary merge branch 1`.
///    `--ancestry-path`, `--simplify-merges` and `--topo-order` are engines
///    whose interesting input is exactly this graph.
///  * `Cherry` — one patch id on both sides of a fork. `--cherry-mark` and
///    `--cherry-pick` are already in the `log` and `rev-list` flag pools and
///    could only ever print `+`, `<` and `>`; the `=` class needs a duplicated
///    patch and no other shape has one.
///  * `CommitGraph` — the same commits read through
///    `.git/objects/info/commit-graph` rather than through their objects, with
///    one commit deliberately left outside the file so a walk has to mix
///    graph-supplied generation numbers with computed ones. Verified:
///    `commit-graph verify` exits 0 on the shape as built, and
///    `-c core.commitGraph=false log --oneline` walks the same history the other
///    way — which is why `core.commitGraph` is in [`CONFIG_KEYS`], and why that
///    key would have been worth nothing until this shape was drawn.
///
/// The four below *those* are the same argument again, made by four shapes that
/// change what a walk is walking rather than what the history is: two substitute
/// objects underneath it, one truncates it, and one leaves objects out of it.
/// Each was measured on stock 2.55.0 on the shape itself before it was added.
///
///  * `NotesReplace` — `refs/replace/*` present before the case runs. Verified:
///    `log --oneline` prints `0dc1e64 notes: replacement for commit 1` where
///    `--no-replace-objects log --oneline` prints `0dc1e64 notes: commit 1` over
///    the same four ids, and `cat-file -p HEAD:README.md` answers
///    `# replaced readme` against the flag's `# fixture`. The substitution is
///    invisible to a port that writes `refs/replace/*` and never reads it, and a
///    corpus agent established that is exactly what the port does — which no
///    *writing* case can show, because the write itself is correct. It also
///    widens the ref store every reader here enumerates: `rev-parse --all`
///    prints six ids on this shape (three `refs/notes/*`, two `refs/replace/*`,
///    `main`) where `Linear` prints one.
///  * `TagChain` — a tag object pointing at a tag object pointing at a tag
///    object, plus tags on a blob and on a tree. It reaches a walk through
///    `--all`/`--tags`/`--decorate` without any case having to name a tag:
///    verified, `log --oneline --decorate --all` prints
///    `7e36e3a (tag: outermost, tag: outer, tag: light-to-tag, tag: inner)` —
///    four decorations on one commit, three of which are only found by peeling
///    a tag whose target is another tag — `rev-list --tags` reaches the commit
///    through the chain, and `rev-list --all --objects` lists five tag objects
///    by name and carries `blobtag`'s blob and `treetag`'s tree behind them.
///    (`cat-file --batch-all-objects --batch-check` on this shape counts
///    5 blob / 4 commit / 5 tag / 5 tree.) Every other shape's tags peel in
///    one step, so an implementation that peels once scored the same as one that
///    peels to the end.
///  * `Shallow` — a real `.git/shallow`, and the parents named in it genuinely
///    absent from the object store. Verified: `log --oneline` prints **two**
///    commits and stops at the graft (rc 0) where the same history unshallowed
///    prints six, `rev-list --count --all` is `4`, and
///    `rev-parse --is-shallow-repository` — a flag already in `rev-parse`'s pool
///    that answers `false` on every other shape in this file — answers `true`.
///    A walk that does not consult the graft list has to fail on a parent it
///    cannot read, which is a whole class of answer no complete repository can
///    ask for.
///  * `Promisor` — objects missing on purpose, with a peer that can supply them.
///    `rev-list`'s pool already carries `--missing=print`, `--missing=allow-any`,
///    `--filter=`, `--filter-provided-objects` and
///    `--exclude-promisor-objects`, and until this shape every one of them was
///    argument parsing over a store with nothing absent. Verified:
///    `rev-list --missing=print --objects --all` prints three `?`-prefixed ids
///    (`?64055193…`, `?0880af1b…`, `?7eefafca…`) and `status --porcelain` is
///    empty, which is the pairing that separates a partial clone from damage.
///
/// Nothing here can reach the network and nothing here can block. Both shapes
/// with a peer name it `./.remote.git` *inside the fixture* (`remote.origin.url`
/// verified to read exactly that), so a lazy fetch is a local path: timed
/// against stock 2.55.0, `log -p --oneline` on `Promisor` fetches all three
/// absent blobs and returns in 0.67s wall.
///
/// `Symlinks` and `Damaged` are deliberately **not** here. Neither changes what
/// a walk answers — one differs in a file mode and one in a broken ref and a
/// corrupt loose object — so both are drawn by [`STORE_SHAPES`] instead, which
/// is the pool for the readers that answer about the stores rather than about
/// the history.
const REV_SHAPES: &[Shape] = &[
    Shape::Linear,
    Shape::Branched,
    Shape::Merged,
    Shape::Detached,
    Shape::BehindRemote,
    Shape::Unrelated,
    Shape::CrissCross,
    Shape::Cherry,
    Shape::CommitGraph,
    Shape::NotesReplace,
    Shape::TagChain,
    Shape::Shallow,
    Shape::Promisor,
];
/// [`REV_SHAPES`] plus the two shapes whose *stores* are unusual rather than
/// whose history is, for the three readers whose subject is a store.
///
/// `rev-parse` resolves names against the ref store, `cat-file` answers about
/// the object store, and `ls-tree` reads a tree out of it. Those three are the
/// only place `Damaged` and `Symlinks` pay, and both would be waste anywhere
/// else — which is the same judgement [`ALL_SHAPES`] documents, applied to two
/// shapes rather than to a pool.
///
///  * `Symlinks` is the only shape whose trees carry mode `120000` and the empty
///    blob. Verified against stock 2.55.0: `ls-tree -r --format='%(objectmode)
///    %(path)'` prints seven `120000` entries and two `e69de29b…` blobs here and
///    none on any other shape, and `cat-file --batch-check --follow-symlinks`
///    reaches all four of its answers — a resolved blob, `dangling`, `symlink`
///    with the out-of-tree target, and resolution through a symlinked directory
///    — from [`P_SYMLINK_SPECS`]. That flag is in `cat-file`'s pool already and
///    had nothing to follow on any shape above: the same four request lines
///    answer `missing` four times on `Branched`, with and without the flag.
///  * `Damaged` answers differently rather than merely refusing, which is what
///    earns it a pool at all. Verified on this shape: `rev-parse --verify
///    refs/heads/dangling` **succeeds** and prints
///    `deadbeefdeadbeefdeadbeefdeadbeefdeadbeef` (rc 0) while `show-ref` dies
///    `fatal: git show-ref: bad ref refs/heads/dangling` (rc 128) and
///    `rev-parse --verify refs/heads/broken-symref` warns `ignoring dangling
///    symref` and dies `fatal: Needed a single revision`; `rev-parse --all`
///    prints the dangling id beside the real one; `cat-file
///    --batch-all-objects --batch-check` lists the corrupt object as `missing`
///    and exits **0** while `log --oneline --all` on the same repository is
///    `fatal: bad object refs/heads/dangling` (rc 128).
///
/// `Damaged` goes no further than this, on purpose. A repository with a broken
/// ref makes `--all` fatal for every walking verb and a corrupt object makes
/// every full read of the store fatal, so putting it in [`REV_SHAPES`] or
/// [`ALL_SHAPES`] would buy one refusal, repeated across the ten grammars those
/// two pools cover, at the price of a share of every one of their budgets.
/// Where a damaged store is the *subject* rather than the obstacle is
/// `gc`/`prune`/`fsck`, and those are
/// generated grammars whose shape lists this file does not own — so the walk
/// [`STOPPERS`] gains for `gc` is how a generated case reaches one.
///
/// Written out rather than derived because `&[Shape]` cannot be concatenated in
/// a const; `store_shapes_extends_rev_shapes` fails `cargo test` if the two ever
/// drift.
const STORE_SHAPES: &[Shape] = &[
    Shape::Linear,
    Shape::Branched,
    Shape::Merged,
    Shape::Detached,
    Shape::BehindRemote,
    Shape::Unrelated,
    Shape::CrissCross,
    Shape::Cherry,
    Shape::CommitGraph,
    Shape::NotesReplace,
    Shape::TagChain,
    Shape::Shallow,
    Shape::Promisor,
    Shape::Symlinks,
    Shape::Damaged,
];

/// The shapes [`STORE_SHAPES`] carries that [`REV_SHAPES`] does not, named once
/// so `store_shapes_extends_rev_shapes` can assert the containment *and* the
/// size without a hard-coded count that a future addition would silently make
/// meaningless.
const STORE_ONLY_SHAPES: &[Shape] = &[Shape::Symlinks, Shape::Damaged];
/// The shapes a command with no particular topology requirement is drawn
/// against. Not `Shape::ALL`: several shapes exist for one verb apiece (a
/// submodule, a sparse checkout, a decomposed path) and drawing every command
/// against them buys repetition rather than coverage.
///
/// `Hooked` earns its place because the *presence* of a hook changes what many
/// verbs do, and nothing else here has one — a commit from a subdirectory of a
/// repository with any hook at all used to fail outright, and no generated case
/// could reach that combination.
///
/// `Symlinks` earns its place the same way, one layer down: the commands drawn
/// against this pool are the index and worktree readers — `status`, `ls-files`,
/// `diff` — and every one of them had only ever been asked about regular files.
/// Mode `120000` is not a rendering detail to them; it decides what the
/// directory walk stats, what a content diff can even mean, and what `--eol`
/// reports. Verified against stock 2.55.0 on the shape: `ls-files --stage`
/// prints seven `120000` entries, `status --porcelain` reports the retargeted
/// symlink as ` M link-wt` and the untracked one as `?? stray-link`, `diff --
/// link-wt` renders a one-line hunk with `\ No newline at end of file` on both
/// sides and a `120000` index line, and one `ls-files --eol` reaches three
/// different answers on this shape alone — `i/lf w/lf` for `README.md`,
/// `i/none w/none` for the zero-byte `empty.txt`, and two empty fields for
/// `link-to-file`. Both of the last two are new here: no other fixture writes an
/// empty file either.
///
/// The rest of the six are absent for the reasons [`REV_SHAPES`] and
/// [`STORE_SHAPES`] give: four of them differ in their history or in how it is
/// read, which says nothing to a porcelain looking at one commit's worth of
/// index, and `Damaged` would spend a share of seven grammars' budgets on the
/// same refusal.
const ALL_SHAPES: &[Shape] = &[
    Shape::Linear,
    Shape::Branched,
    Shape::Merged,
    Shape::Dirty,
    Shape::Conflicted,
    Shape::Detached,
    Shape::AwkwardPaths,
    Shape::Hooked,
    Shape::Symlinks,
];

/// [`ALL_SHAPES`] plus the three shapes whose **index** is in a state no other
/// shape's is, for the three readers whose subject is the index and the worktree
/// beside it: `status`, `diff` and `ls-files`.
///
/// The same judgement [`STORE_SHAPES`] documents, applied one layer down. None
/// of the three says anything to a walking verb or to a ref-filter front end —
/// they all descend from the same history everything else does — so membership
/// in [`REV_SHAPES`] or in [`ALL_SHAPES`] would spend seven grammars' budgets on
/// a repetition. What each one changes, verified against stock 2.55.0 on the
/// shape:
///
///  * `IntentToAdd` — the third state between tracked and untracked.
///    `status --short` prints ` A ita-new.txt` and ` A sub/ita-nested.txt`
///    against `A  staged.txt` for a real staged add and `AM both.txt` for one
///    that was edited after staging, and ` D ita-gone.txt` for an entry whose
///    blob is the empty one and whose file is gone. `diff --name-status` reports
///    the two intent-to-add paths as **additions in the worktree** (`A
///    ita-new.txt`) while `diff --cached --name-status` hides them and names
///    only `both.txt` and `staged.txt` — which is precisely what
///    `--ita-visible-in-index`/`--ita-invisible-in-index` flip, and both flags
///    were argument parsing until this shape. The bit itself never appears in a
///    listing: `ls-files --stage -v ita-new.txt` is `H 100644 e69de29b… 0` for
///    the path, the same line an ordinary staged add of an empty file prints,
///    so `status` and `diff` are the only readers in this harness that can say
///    an entry is an intent rather than a record.
///  * `PendingRename` — the `2` record, half of `--porcelain=v2`'s grammar and
///    never once produced by this corpus. Verified:
///    `status --porcelain=v2` prints `2 R. … R100 pure-renamed.txt\tpure.txt`,
///    `2 RM … R100 near-renamed.txt\tnear.txt` (a rename in the index column and
///    a modification in the worktree column at once), `2 R. … R60
///    far-renamed.txt\tfar.txt` and `2 .R … R100 wt-renamed.txt\twt.txt` — a
///    rename that is not staged at all. Five pairs at four similarity indices is
///    what makes `--find-renames=<n>`, `--no-renames` and `status.renames` sort
///    something instead of agreeing with each other.
///  * `Rerere` — a merge in progress whose worktree holds the **recorded
///    resolution** while the index still holds stages 1/2/3. `diff` renders it
///    `diff --cc other.txt` with `++resolved other`, which is a combined diff
///    whose result side matches neither parent and carries no conflict markers.
///    [`Shape::Conflicted`] can only ever produce the marker form, so `--cc` and
///    `--combined-all-paths` had one input. Its `status` answer (`UU`, `AA`) is
///    the same one `Conflicted` gives and is not why it is here; what every draw
///    against it also buys is free, because `runner`'s op-state probe reads
///    `MERGE_RR` and no other shape has one.
const INDEX_SHAPES: &[Shape] = &[
    Shape::Linear,
    Shape::Branched,
    Shape::Merged,
    Shape::Dirty,
    Shape::Conflicted,
    Shape::Detached,
    Shape::AwkwardPaths,
    Shape::Hooked,
    Shape::Symlinks,
    Shape::IntentToAdd,
    Shape::PendingRename,
    Shape::Rerere,
];

/// [`ALL_SHAPES`] plus the one shape that has notes **before the case runs**.
///
/// Every `notes` subcommand that is not `add` reads a store, and on every other
/// shape that store is empty — so `list`, `show`, `merge`, `prune`, `copy`,
/// `get-ref` and the whole `--ref=` dimension were being measured on one answer
/// ("no note found") that says nothing about which ref was selected. Verified
/// against stock 2.55.0 on [`Shape::NotesReplace`]: `notes list` prints two
/// `<note> <annotated>` pairs, and `notes merge other` — the third ref, which
/// annotates the same commit as the default one with different text — is rc 1
/// with `CONFLICT (add/add): Merge conflict in notes for object 7b6d7d59…` and
/// parks `NOTES_MERGE_REF`/`NOTES_MERGE_WORKTREE`, a state `probe_op_state`
/// reads by name and that no other shape can reach.
const NOTES_SHAPES: &[Shape] = &[
    Shape::Linear,
    Shape::Branched,
    Shape::Merged,
    Shape::Dirty,
    Shape::Conflicted,
    Shape::Detached,
    Shape::AwkwardPaths,
    Shape::Hooked,
    Shape::Symlinks,
    Shape::NotesReplace,
];

/// Rev-specs worth throwing at anything that resolves one. Includes forms that
/// *should* fail, because agreeing on rejection is also parity, and the hard
/// forms git's own `rev-parse` grammar allows: peels, ranges, reflog walks,
/// `:path` object specs, `:/text` searches, and raw oids.
const REVS: &[&str] = &[
    "HEAD", "HEAD^", "HEAD^^", "HEAD^2", "HEAD~1", "HEAD~2", "HEAD~3",
    "HEAD^0", "HEAD^{}", "HEAD^{tree}", "HEAD^{commit}", "HEAD^{tag}",
    "main", "@", "@~1", "@{-1}", "HEAD@{0}", "HEAD@{1}", "HEAD@{now}",
    "main..HEAD", "main...HEAD", "HEAD~2..HEAD", "^HEAD",
    "HEAD:README.md", ":/fixture", ":0:src/lib.rs", "refs/heads/main",
    "0000000000000000000000000000000000000000", "deadbeef",
    "does-not-exist", "@{999}", "HEAD~999", "",
    // The parent-set notations. Each expands to a *set* rather than to one
    // commit — `revision.c:handle_revision_arg` turns `^@` into every parent,
    // `^!` into the commit with its parents negated, `^-` into
    // `<rev> ^<rev>^1` — and a port that treats them as suffixes on a single
    // resolution prints one id where stock prints two. Verified against stock
    // 2.55.0: `HEAD^!` prints the commit then `^<parent>`, `HEAD^@` prints the
    // parent alone, and `HEAD^-` matches `HEAD^-1`.
    "HEAD^@", "HEAD^!", "HEAD^-", "HEAD^-1",
    // Ranges with an omitted endpoint. `..HEAD`, `HEAD..` and `...HEAD` are all
    // legal and all default the missing side to `HEAD`; a range parser that
    // splits on `..` and resolves both halves rejects every one of them.
    "..HEAD", "HEAD..", "...HEAD",
    // Upstream-relative forms. Fatal on every shape but `BehindRemote`, which is
    // exactly why that shape is in [`REV_SHAPES`] — see its comment.
    "@{u}", "@{push}", "main@{upstream}", "origin/main", "refs/remotes/origin/main",
    // The DWIM table (`refs.c:ref_rev_parse_rules`) spelled at its three depths:
    // fully qualified, namespace-relative, and short. A port with a two-entry
    // table resolves `refs/tags/v0.1.0` and `v0.1.0` and misses `tags/v0.2.0`.
    "refs/tags/v0.1.0", "heads/main", "tags/v0.2.0",
    // `feature` is a branch on `Branched` and *also* a tag for the duration of
    // the `tag-shadows-branch` round trip, which is the only place in the
    // harness where one name resolves through two rules. Verified against stock
    // 2.55.0 with both refs present: `rev-parse feature` warns
    // `refname 'feature' is ambiguous.` and answers with the **tag**, not the
    // branch — `refs/tags/%.*s` precedes `refs/heads/%.*s` in
    // `refs.c:ref_rev_parse_rules`, so the answer is the tagged commit
    // (`add two`) while `refs/heads/feature` points a commit further on. A port
    // that walks the table in the order a reader would guess answers with the
    // branch and looks right on every other case in this pool.
    "feature",
    // Peeling to a type the object is not, and peeling a tag to a tree rather
    // than to its commit — two different arms of `peel_to_type`. Verified:
    // `v0.1.0^{tree}` yields the tree, `HEAD^{blob}` dies with
    // `expected blob type, but the object dereferences to tree type`.
    "v0.1.0^{tree}", "HEAD^{blob}",
    // `:/text` searches every ref; `<rev>^{/text}` searches only from a rev.
    // Same syntax, two different walks, and a miss is a third outcome.
    "main^{/two}", ":/nomatchhere",
    // The two broken refs [`Shape::Damaged`] carries, spelled fully qualified so
    // the DWIM table is not what is being measured. They resolve on that shape
    // and nowhere else, which is the same trade `@{u}` above makes for
    // `BehindRemote` — and the answers are not one answer. Verified against
    // stock 2.55.0 on `Damaged`: `rev-parse refs/heads/dangling` prints
    // `deadbeef…` and exits **0** with no object behind it, while
    // `rev-parse --verify refs/heads/broken-symref` warns
    // `ignoring dangling symref refs/heads/broken-symref` and dies
    // `fatal: Needed a single revision` (rc 128). `cat-file -t` refuses both and
    // in two different ways: `refs/heads/dangling` is
    // `fatal: git cat-file: could not get object info` and
    // `refs/heads/broken-symref` is the same warning followed by
    // `fatal: Not a valid object name refs/heads/broken-symref`. On `Branched`
    // both names are the ordinary `fatal: ambiguous argument …` (rc 128), which
    // is a fourth answer again. A port that treats "the ref does not lead to an
    // object" as one condition gets at least one of the four wrong.
    "refs/heads/dangling", "refs/heads/broken-symref",
    // `:path` with no stage number is stage 0; `:1:path` names a stage that only
    // a conflicted index has, and stock answers the miss with a *hint* naming
    // `:0:` rather than with the generic ambiguous-argument text.
    ":README.md", ":1:README.md",
    // Abbreviated object names are deliberately **not** sampled here beyond the
    // `deadbeef` above, and the reason is worth stating so the next widening
    // does not re-add them. An abbreviation only measures `get_short_oid` if it
    // is a prefix of an object the repository *has*, and a static pool cannot
    // name one: every fixture oid is a function of the fixture and changes the
    // moment `fixture::build` changes by a byte. The empty blob is the one id
    // that is a hash-function constant rather than a fixture value, but no shape
    // stores it — no fixture writes an empty file — so it does not resolve
    // either. Verified against stock 2.55.0 on `Branched`: `rev-parse` answers
    // `dea`, `dead`, `e69de29`, `e69de29bb2d1` and the `deadbeef` already in
    // this pool with byte-identical `fatal: ambiguous argument …` text, and
    // `cat-file -t` answers all five with `fatal: Not a valid object name …`.
    // Four more spellings of one refusal is four more draws that cannot
    // disagree; `core.abbrev` and `core.disambiguate` are reached from the
    // `abbrev-disambiguate` group against revs that do resolve.
];

/// Path arguments including magic pathspecs, which have their own parser in git
/// and are a rich source of divergence.
const PATHS: &[&str] = &[
    "README.md", "src/lib.rs", "src", "src/", ".", "./README.md", "..",
    "*.md", "**/*.rs", "no/such/path",
    ":(glob)**/*.rs", ":(icase)readme.md", ":!src", ":(exclude)*.md",
    ":(top)README.md", ":(attr:text)", "with space.txt", "üñïçødé.txt",
    // `literal` is the magic that *disables* the glob the same string would
    // otherwise be: `:(literal)*.md` matches a file actually named `*.md` and
    // therefore nothing, while the bare `*.md` above matches. A port that
    // parses the magic and then globs anyway passes every case in this pool
    // except this one.
    ":(literal)*.md",
    // Combined magic. `pathspec.c` parses the long form as a comma list and
    // applies every flag; a parser that takes the first word and stops gets
    // `:(glob,icase)` right by accident and `:(exclude,icase)` inverted.
    // Verified against stock 2.55.0: `:(glob,icase)**/*.RS` matches
    // `src/lib.rs` and `:(exclude,icase)*.MD` leaves `README.md` out.
    ":(glob,icase)**/*.RS", ":(exclude,icase)*.MD", ":(top,glob)src/*.rs",
    // The short spelling of `:(top)`. `:/` is the whole tree and `:/src` is a
    // path from the top — the same two-character prefix that starts a `:/text`
    // *rev*, which is why a shared scanner gets one of the two wrong.
    ":/", ":/src",
    // The attribute magic beyond the plain `:(attr:text)` above: a negated
    // attribute and a valued one, which are two more branches of
    // `parse_attr_spec` and both match nothing here — so they measure the
    // parse rather than the fixture.
    ":(attr:!text)", ":(attr:diff=rust)",
    // Magic that is not. Stock dies with
    // `fatal: Invalid pathspec magic 'bogus' in ':(bogus)x'`; an empty exclude
    // is legal and selects nothing.
    ":(bogus)x", ":(exclude)",
    // Paths that look like options. Without a `--` separator stock's
    // parse-options rejects these before any pathspec parser sees them —
    // `error: unknown switch \`R'` (rc 129) for the short form and
    // `error: unknown option \`README.md'` for the long — and with one they are
    // ordinary paths that match nothing. The `--` is injected on a quarter of
    // draws, so both sides of that boundary are reached.
    "-README.md", "--README.md",
    // A path that needs quoting on output, and the decomposed spelling of a
    // path that `Shape::DecomposedPaths` tracks (`fixture::NFD_TRACKED`). The
    // precomposed sibling is written out separately: which of the two matches
    // is the whole content of `core.precomposeUnicode`, and a pool holding one
    // form measures the setting never.
    "quote\"name.txt", "e\u{301}.txt", "\u{e9}.txt",
    // Path spellings git normalises before matching: a doubled separator and a
    // `..` that climbs back inside the tree.
    "src//lib.rs", "./src/../README.md",
    // Three paths [`Shape::Symlinks`] tracks and no other shape has, named for
    // the same reason `üñïçødé.txt` and `é.txt` are named above: a pathspec that
    // matches only on one shape is how that shape's index entries get asked
    // about individually rather than through a `.` that sweeps everything.
    // `link-wt` is the retargeted symlink (` M link-wt`, and a `diff` hunk whose
    // both sides end `\ No newline at end of file`), `link-broken` points at a
    // target that does not exist so the directory walk has to stat something
    // absent, and `empty.txt` is the zero-byte blob. Verified against stock
    // 2.55.0: all three are ordinary matches on `Symlinks` and none of them
    // exists elsewhere — `ls-files --error-unmatch link-to-file` on `Branched`
    // is `error: pathspec 'link-to-file' did not match any file(s) known to git`
    // with rc 1, which is the exit-code-moving refusal that pool entry is for.
    "link-wt", "link-broken", "empty.txt",
];

/// Replacement values for `--flag=value` mutation: empty, boundary, overflow,
/// and garbage. A parser that only ever saw well-formed values in the corpus
/// meets malformed ones here.
const VALUES: &[&str] = &[
    "", "0", "1", "-1", "999999999", "99999999999999999999999999",
    "abc", "true", "false", "v1", "=", "%H%n", "\t", "0x10",
    // The number spellings `strtol` accepts and a hand-written parser does not,
    // and the one it rejects. Verified against stock 2.55.0 on `core.abbrev`,
    // which is the same integer parser every `--flag=<n>` reaches from the
    // other side: `" 4"` is **4** (leading whitespace is skipped), `"4 "` is
    // **fatal** (`invalid unit`), `"+4"` and `"04"` are 4, `"0x10"` is 16, and
    // `"4k"` is 4096 — the `k`/`m`/`g` suffix is not a size-key privilege, it
    // applies to every integer git parses from configuration. The flag parser
    // disagrees with all of that (`rev-parse --short=4k` is 4, and so are
    // `--short=" 4"`, `--short=+4` and `--short=0x10`), and the disagreement is
    // the point: one pool feeding both sides is what makes a port that shares
    // one parser between them visible.
    //
    // Demonstrated at 4 rather than at 1 because `core.abbrev` has a *second*
    // check under the parse: 1 parses fine and is then refused with
    // `error: abbrev length out of range: 1`, which says nothing about the
    // spelling. The pool below is spelled at 1 anyway — as a `--flag=` value it
    // meets whichever range each flag has, and [`CONFIG_EDGE_VALUES`] is where
    // the same spellings meet `core.abbrev`'s range check on purpose.
    " 1", "1 ", "+1", "01", "1k", "2m", "-0",
];

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Every spelling git accepts for a boolean, plus none of the ones it does not.
///
/// `config.c:git_parse_maybe_bool_text` takes `true`/`false`, `yes`/`no`,
/// `on`/`off`, `one`/`zero`, and any integer — and a port that only recognises
/// `true`/`false` reads `off` as *set*, which is the inverted-flag class of bug
/// this list exists to find. Spelled out rather than reduced to `true`/`false`
/// because the spelling is the thing under test.
const BOOLS: &[&str] = &["true", "false", "yes", "no", "on", "off", "1", "0"];

/// Real git configuration keys, each paired with the values that are meaningful
/// *for that key*.
///
/// Keys, not a grammar: `core.abbrev` takes a width, `merge.conflictStyle` takes
/// one of three names, and crossing every key with every value would spend the
/// whole budget on combinations git rejects at parse time before any behaviour
/// runs. Each key's own list is where the *behaviour* differences live; the
/// generic [`CONFIG_EDGE_VALUES`] pool is mixed in on a minority of draws so the
/// malformed path is reached too.
///
/// Invalid values are deliberately present in both lists. A key that makes
/// stock git die must make zvcs die identically — that is a pass, and excluding
/// it would leave the port's config *validation* unmeasured, which is exactly
/// the half a port skips first.
const CONFIG_KEYS: &[(&str, &[&str])] = &[
    // core.*: the settings that change what a repository even looks like.
    ("core.abbrev", &["4", "7", "12", "40", "auto", "no", "0", "1", "64"]),
    ("core.autocrlf", &["true", "false", "input"]),
    ("core.eol", &["lf", "crlf", "native"]),
    ("core.ignorecase", BOOLS),
    ("core.precomposeUnicode", BOOLS),
    ("core.quotePath", BOOLS),
    ("core.logAllRefUpdates", &["true", "false", "always"]),
    ("core.safecrlf", &["true", "false", "warn"]),
    ("core.symlinks", BOOLS),
    // `core.bare=true` over a worktree is one of the deaths worth agreeing on:
    // `setup.c` rejects the pair rather than honouring it.
    ("core.bare", BOOLS),
    ("core.fileMode", BOOLS),
    // The one key in this pool that chooses between two *implementations* of the
    // same answer rather than changing the answer: with a graph file present,
    // `true` reads generation numbers out of `.git/objects/info/commit-graph`
    // and `false` walks the commit objects, and the two must agree. It is worth
    // a key now and was worth nothing before, because until `Shape::CommitGraph`
    // entered [`REV_SHAPES`] no fixture had the file. Verified against stock
    // 2.55.0 on that shape: `-c core.commitGraph=false log --oneline -3` prints
    // the same three lines the default does, `commit-graph verify` exits 0, and
    // `-c core.commitGraph=bogus` is
    // `fatal: bad boolean config value 'bogus' for 'core.commitgraph'` (rc 128)
    // — the lower-cased key name in the message being its own small parity
    // question.
    ("core.commitGraph", BOOLS),
    // diff.*: rename detection and hunk shape, none of which any case could set.
    ("diff.renames", &["true", "false", "copies", "copy"]),
    ("diff.renameLimit", &["0", "1", "2", "1000", "-1", "99999999999999999999999999"]),
    ("diff.algorithm", &["myers", "minimal", "patience", "histogram", "default"]),
    ("diff.context", &["0", "1", "3", "10", "-1"]),
    ("diff.mnemonicPrefix", BOOLS),
    ("diff.noprefix", BOOLS),
    ("diff.relative", &["true", "false", "src/", "no/such/"]),
    // log.*: the defaults every `log`/`show`/`whatchanged` invocation inherits.
    ("log.abbrevCommit", BOOLS),
    (
        "log.date",
        &[
            "relative", "local", "iso", "iso-strict", "rfc", "short", "raw", "human", "unix",
            "default", "format:%Y-%m-%d", "bogus",
        ],
    ),
    ("log.decorate", &["short", "full", "auto", "no", "true", "false"]),
    ("log.follow", BOOLS),
    ("log.showSignature", BOOLS),
    // status.*: what the most-read porcelain in git decides to print.
    ("status.showUntrackedFiles", &["no", "normal", "all", "bogus"]),
    ("status.short", BOOLS),
    ("status.branch", BOOLS),
    ("status.relativePaths", BOOLS),
    ("status.renames", &["true", "false", "copies"]),
    // `color.ui` is pinned off by `NO_COLOR` in the hardened environment, so
    // `always` here is the one way a case can ask for escape sequences at all.
    ("color.ui", &["auto", "always", "never", "true", "false"]),
    ("grep.patternType", &["basic", "extended", "fixed", "perl", "default", "bogus"]),
    ("grep.lineNumber", BOOLS),
    // Ref ordering, including the `-` prefix and the `version:` sort git parses
    // out of the same string.
    ("tag.sort", &["refname", "-refname", "version:refname", "taggerdate", "bogus"]),
    ("branch.sort", &["refname", "-refname", "committerdate", "bogus"]),
    ("versionsort.suffix", &["-pre", "-rc", "", "-"]),
    ("push.default", &["nothing", "matching", "simple", "upstream", "current", "tracking", "bogus"]),
    ("merge.conflictStyle", &["merge", "diff3", "zdiff3", "bogus"]),
    ("blame.date", &["iso", "short", "raw", "relative", "bogus"]),
    // `pretty.<name>` defines a format `--pretty=<name>` then resolves, so this
    // one key reaches the whole placeholder language from the config side.
    ("pretty.custom", &["%H", "%h %s", "format:%an", "tformat:%H", "%(bogus)"]),
    // `commit.cleanup` is the one setting that changes the bytes of a commit
    // *object* rather than of an output line, so a wrong reading survives into
    // the state probe and into every later read. Verified against stock 2.55.0
    // on a `-m` message holding a `#` line, a blank run and a scissors marker:
    // `strip` removes the `#` line and collapses the blank run, `whitespace`
    // keeps the `#` line and collapses the run, `verbatim` keeps both, and
    // `bogus` is `fatal: Invalid cleanup mode bogus` with rc 128.
    //
    // `scissors` is in the pool for what it does **not** do: with `-m` (and with
    // `-F`) it is byte-for-byte `default`, marker and all — the cut only happens
    // for a message an editor produced, which `env::harden` pins to `true` so no
    // case can reach it. A port that applies the cut wherever the mode is set
    // truncates a message stock keeps, and that difference is in the commit
    // object rather than in a printed line.
    ("commit.cleanup", &["strip", "whitespace", "verbatim", "scissors", "default", "bogus"]),
    // The `git_config_pathname` vocabulary, which nothing else in this pool
    // has: `~`, `~user`, `%(prefix)/` and a plain relative path are four
    // different expansions before the file is even opened, and `~nosuchuser/x`
    // is `fatal: failed to expand user dir in: '~nosuchuser/x'` (verified).
    ("core.attributesFile", &[".gitattributes", "no-such", "~/x", "~nosuchuser/x", "%(prefix)/x", ""]),
    // The arm that accumulates *every* bad token into one non-fatal warning
    // block instead of dying on the first — verified: `diff.dirstat=bogus,alsobogus`
    // prints two `Unknown dirstat parameter` lines under one `warning:` header
    // and still exits 0.
    ("diff.dirstat", &["changes", "lines", "files", "cumulative", "10", "files,10", "bogus,alsobogus"]),
    // Unknown value only warns and keeps going, which is the opposite of what
    // most enum keys here do — verified: `blame.coloring=bogus` prints
    // `warning: invalid value for 'blame.coloring': 'bogus'` and exits 0.
    ("blame.coloring", &["repeatedLines", "highlightRecent", "none", "bogus"]),
    // Any value at all is legal *including none*, and the value becomes a MIME
    // boundary in the output — verified: `format.attach=BOUND` yields
    // `Content-Type: multipart/mixed; boundary="------------BOUND"`.
    ("format.attach", &["BOUND", "true", "false", ""]),
    // A name in `advice.c`'s table is parsed as a boolean and a name outside it
    // is not parsed at all. Verified: `advice.statusHints=bogus` is
    // `fatal: bad boolean config value 'bogus' for 'advice.statushints'` while
    // `advice.noSuchAdvice=bogus` exits 0 — the same value, two outcomes,
    // decided by a table a port has to have transcribed.
    ("advice.statusHints", BOOLS),
];

/// Values thrown at *any* key, whatever it expects: empty, whitespace, garbage,
/// overflow, and the enum names that belong to some other key.
///
/// This is where the parse-failure paths live. A key's own list exercises what
/// the setting does; this one exercises what happens when it cannot.
const CONFIG_EDGE_VALUES: &[&str] = &[
    "", " ", "auto", "abc", "-1", "0", "1", "999999999", "99999999999999999999999999",
    "true", "false", "yes", "no", "on", "off", "none", "\t", "=", "%H",
    // `git_parse_int`'s own branches, which nothing above reaches: the four
    // spellings it accepts and the one it does not. Every entry below was run
    // through stock 2.55.0 as `-c core.abbrev=<v> rev-parse --short HEAD`:
    //
    //   `" 1"`   parses to 1, then `error: abbrev length out of range: 1` +
    //            `fatal: unable to parse 'core.abbrev' from command-line config`
    //   `"1 "`   `fatal: bad numeric config value '1 ' for 'core.abbrev':
    //            invalid unit` — the trailing space is a unit suffix, not blank
    //   `"+1"`   same as `" 1"`: parses to 1, then out of range
    //   `"0x10"` parses to 16 → a 16-character id, so a leading `0x` *is* hex
    //   `"1k"`   parses to 1024, clamped to the full 40-character id
    //   `"-0"`   `error: abbrev length out of range: 0`
    //
    // The two-line refusal for the in-range-syntax-out-of-range-value cases is
    // the point: the *parse* succeeded and a second check rejected the number,
    // which is a different failure from the one-line `bad numeric config value`
    // that `"1 "` gets. A port that reaches for `str::parse::<i64>()` gets
    // `" 1"`, `"+1"`, `"0x10"` and `"1k"` wrong in four different directions and
    // `"1 "` right by accident. Spelled at 1 rather than at a legal abbreviation
    // length so both checks are crossed by one pool.
    " 1", "1 ", "+1", "0x10", "1k", "-0",
    // Enum words that are legal *somewhere*. The pool already carried `none` and
    // `auto`; these four are the rest of the vocabulary git spreads across keys,
    // and the point is that one word reaches five different outcomes depending on
    // which key it lands on. Every row measured with `-c <key>=always status
    // --porcelain` against stock 2.55.0:
    //
    //   core.autocrlf     fatal: bad boolean config value 'always' (128)
    //   core.ignorecase   the same refusal
    //   core.abbrev       fatal: bad numeric config value 'always': invalid unit
    //   status.showUntrackedFiles  error: Invalid untracked files mode 'always'
    //                              + fatal: unable to parse … (128)
    //   core.logAllRefUpdates      rc 0 — `always` is one of its three values
    //   core.eol                   rc 0 — accepted and ignored, though `lf`,
    //                              `crlf` and `native` are the documented set
    //
    // `input` is the same experiment from the other direction: legal for
    // `core.autocrlf` and `fatal: bad boolean config value 'input' for
    // 'core.safecrlf'` one key over. A port with one shared enum table accepts
    // every row, and the two rc-0 rows are what stop that from being visible with
    // a single draw.
    "always", "input", "warn", "all",
];

// ---------------------------------------------------------------------------
// Configuration: interacting key sets
// ---------------------------------------------------------------------------

/// Shared value pools for the key sets below, so a key that takes a colour and
/// a key that takes an expiry do not both end up drawing from one generic list.
///
/// Each is spelled with the forms git's own parser distinguishes rather than
/// with representatives: `never`/`always`/`auto` are three *different* branches
/// of `git_config_colorbool`, and a pool holding only `true`/`false` would
/// measure the bool branch three times and the colour branches never.
const COLORBOOL: &[&str] = &["auto", "always", "never", "true", "false", "no", "bogus", ""];
/// Colour specifications, including the ones that are not.
const COLOR: &[&str] =
    &["red", "bold blue", "normal", "reverse", "black green", "#ff0000", "12", "256", "bogus", ""];
/// Integers, including the two that are not integers and the one that overflows.
const INT: &[&str] =
    &["0", "1", "2", "3", "-1", "7", "40", "1000", "99999999999999999999999999", "abc", ""];
/// Expiry dates. `never` and `now` are special-cased before `approxidate` runs,
/// and `all`/`false`/`bogus` are rejected — three different outcomes from one
/// parser.
const EXPIRY: &[&str] =
    &["never", "now", "2.weeks.ago", "1.day.ago", "all", "bogus", "", "false", "3.months.ago"];
/// Byte sizes, which git parses with a `k`/`m`/`g` suffix.
const SIZE: &[&str] = &["0", "1", "16k", "2m", "1g", "512", "-1", "bogus", ""];
/// File names, as `git_config_pathname` sees them.
///
/// Not a generic string pool: this parser expands `~`, `~user` and
/// `%(prefix)/` before anything opens the file, and each expansion has its own
/// failure. Verified against stock 2.55.0 — `~nosuchuser/x` is
/// `fatal: failed to expand user dir in: '~nosuchuser/x'` on every one of the
/// keys that reads this pool (`core.attributesFile`, `core.excludesFile`,
/// `commit.template`, `blame.ignoreRevsFile`, `diff.orderFile`,
/// `format.signatureFile`), because the expansion happens before anything looks
/// at the name. A pool of plain relative names would measure the open six times
/// and the expansion never.
///
/// `no-such` is the entry that separates those six keys from each other rather
/// than joining them, and it does not behave uniformly — verified:
/// `blame.ignoreRevsFile` is `fatal: could not open object name list: no-such`,
/// `commit.template` is `fatal: could not read 'no-such': No such file or
/// directory`, `diff.orderFile` is `fatal: failed to read orderfile 'no-such':
/// …`, `format.signatureFile` is `fatal: unable to read signature file
/// 'no-such': …`, while `core.attributesFile` and `core.excludesFile` accept a
/// missing file in silence and exit 0. Four distinct refusals and one silent
/// acceptance from one string is the whole reason it is in the pool.
///
/// Relative names only: the two sides live at different roots, and
/// `config_pool_is_well_formed` rejects a leading `/` for exactly that reason.
const PATHNAME: &[&str] =
    &[".gitattributes", "no-such", "src", "~/x", "~nosuchuser/x", "%(prefix)/x", ""];

/// A set of configuration keys that only matter **together**.
///
/// The single-key sampler could never reach an interaction, and interactions are
/// where a port's configuration handling actually breaks: reading `diff.renames`
/// correctly and ignoring `diff.renameLimit` produces the right answer on every
/// case that sets one of them. Each group below is a set of keys that one
/// decision reads, so drawing two or three of them at once is what puts the
/// decision under test rather than the parse.
///
/// Two sources fed this table, and both are cited per group: git's own
/// documented interactions, and — more usefully — which keys each config
/// callback in `src/extensions/src/{default_config,diff_config,status_config,
/// log_config,cmd_config}.rs` reads *in one arm or under one guard*. The second
/// source names interactions no documentation states: `status_config.rs:208-225`
/// parses `diff.renameLimit` only while `st.rename_limit` is still `-1`, so a
/// preceding `status.renameLimit` makes a later garbage `diff.renameLimit`
/// silently acceptable — a two-key fact that no one-key draw can express.
struct ConfigGroup {
    /// Slug for the test that asserts the table is well formed. Not rendered
    /// into a case id: the id names the keys that were actually drawn, which is
    /// what a reader needs, and a group name would be one more token that does
    /// not narrow anything — which is also why the field is read only by
    /// `cargo test`, and says so rather than being silently warned about.
    #[allow(dead_code)]
    name: &'static str,
    keys: &'static [(&'static str, &'static [&'static str])],
}

/// The interacting sets.
///
/// A key may appear in more than one group; that is not duplication, it is the
/// point. `core.abbrev` interacts with `log.abbrevCommit` in one decision and
/// with `core.disambiguate` in another, and a table that listed it once would
/// have to pick which of the two interactions to leave unmeasured.
const CONFIG_GROUPS: &[ConfigGroup] = &[
    // `core.eol` is only consulted for a file `core.autocrlf` did not already
    // decide, and `core.safecrlf` decides whether an irreversible conversion is
    // a warning or a refusal — three keys, one conversion. The port validates
    // all three independently and leaves `core.eol`'s arm empty
    // (`default_config.rs:350`, `:355`, `:364`), so any disagreement here is a
    // disagreement about the conversion rather than about the parse.
    ConfigGroup {
        name: "crlf",
        keys: &[
            ("core.autocrlf", &["true", "false", "input"]),
            ("core.eol", &["lf", "crlf", "native", "bogus"]),
            ("core.safecrlf", &["true", "false", "warn"]),
            ("core.checkRoundtripEncoding", &["SHIFT-JIS", "UTF-16", "bogus", ""]),
            ("core.filemode", BOOLS),
        ],
    },
    // The four prefix keys are read as four independent strings/bools
    // (`diff_config.rs:189-201`) and resolved against each other only when a
    // diff is actually rendered: `diff.noprefix` wins over an explicit
    // `diff.srcPrefix`, and `diff.mnemonicPrefix` replaces both with a letter
    // that depends on which side of the comparison a file is. `format.noprefix`
    // is the fifth, with its own refusal path (`log_config.rs:164-173`).
    ConfigGroup {
        name: "diff-prefix",
        keys: &[
            ("diff.noprefix", BOOLS),
            ("diff.srcPrefix", &["a/", "src/", "", "x", "a b/"]),
            ("diff.dstPrefix", &["b/", "dst/", "", "y", "c d/"]),
            ("diff.mnemonicPrefix", BOOLS),
            ("format.noprefix", BOOLS),
            ("diff.relative", &["true", "false", "src/", "no/such/"]),
        ],
    },
    // `color.ui` decides whether the per-command keys are consulted at all, and
    // a per-command key decides whether its slots are. The slot tables return
    // *before* the value is parsed for a slot they do not know
    // (`diff_config.rs:283-289`, `status_config.rs:229-235`), so a bad colour is
    // only reachable by pairing a real slot with it — and `color.grep`'s table is
    // the exception that rejects the unknown slot itself
    // (`cmd_config.rs:183-189`). Both halves need two keys to reach.
    ConfigGroup {
        name: "color-cascade",
        keys: &[
            ("color.ui", COLORBOOL),
            ("color.diff", COLORBOOL),
            ("diff.color", COLORBOOL),
            ("color.status", COLORBOOL),
            ("color.branch", COLORBOOL),
            ("color.grep", COLORBOOL),
            ("color.diff.meta", COLOR),
            ("color.diff.frag", COLOR),
            ("color.diff.whitespace", COLOR),
            ("color.status.header", COLOR),
            ("color.status.bogusSlot", COLOR),
            ("color.grep.filename", COLOR),
            ("color.grep.bogusSlot", COLOR),
            ("color.decorate.branch", COLOR),
            ("color.advice.hint", COLOR),
        ],
    },
    // `--short` output has no branch header unless `status.branch` asks for one,
    // and `status.aheadBehind` decides whether that header costs a revision walk.
    // The port reads all four out of one bool arm (`status_config.rs:166-173`),
    // which is exactly why a one-key draw cannot tell a port that honours the
    // combination from one that honours each key alone.
    ConfigGroup {
        name: "status-render",
        keys: &[
            ("status.short", BOOLS),
            ("status.branch", BOOLS),
            ("status.aheadBehind", BOOLS),
            ("status.showStash", BOOLS),
            ("status.showUntrackedFiles", &["no", "normal", "all", "1", "off", "bogus"]),
            ("status.relativePaths", BOOLS),
            ("status.displayCommentPrefix", BOOLS),
            ("status.submoduleSummary", &["true", "false", "0", "5", "bogus"]),
        ],
    },
    // The `-1` guards: `diff.renames` and `diff.renameLimit` are read only while
    // the status defaults are still unset, so a `status.*` spelling drawn first
    // changes what a later `diff.*` spelling does — including making an
    // otherwise-fatal value silently acceptable (`status_config.rs:208-225`).
    ConfigGroup {
        name: "rename-detection",
        keys: &[
            ("diff.renames", &["true", "false", "copies", "copy", ""]),
            ("status.renames", &["true", "false", "copies", "copy", ""]),
            ("diff.renameLimit", INT),
            ("status.renameLimit", INT),
            ("merge.renames", BOOLS),
            ("merge.renameLimit", INT),
        ],
    },
    // `push.default=simple`/`upstream` are *defined* in terms of
    // `branch.<name>.merge` and `branch.<name>.remote`, and `remote.pushDefault`
    // overrides the branch's own remote. `push.default` alone can only ever be
    // measured against a branch with no upstream configured.
    ConfigGroup {
        name: "push-upstream",
        keys: &[
            (
                "push.default",
                &["nothing", "matching", "simple", "upstream", "current", "tracking", "bogus"],
            ),
            ("branch.main.merge", &["refs/heads/main", "refs/heads/other", "main", ""]),
            ("branch.main.remote", &["origin", "gen", ".", ""]),
            ("branch.main.pushRemote", &["origin", "gen", ""]),
            ("remote.pushDefault", &["origin", "gen", ""]),
            ("push.autoSetupRemote", BOOLS),
            ("branch.autoSetupMerge", &["true", "false", "always", "inherit", "simple"]),
        ],
    },
    // `feature.manyFiles` is not a setting, it is a *bundle*: it turns on
    // `index.skipHash`, `index.version=4` and `core.untrackedCache`, and an
    // explicit key must beat the bundle. That is only observable when both are
    // set, and the direction it must win in is the whole content of the feature.
    ConfigGroup {
        name: "index-features",
        keys: &[
            ("feature.manyFiles", BOOLS),
            ("feature.experimental", BOOLS),
            ("index.skipHash", BOOLS),
            ("index.version", &["2", "3", "4", "0", "5", "bogus"]),
            ("index.recordEndOfIndexEntries", BOOLS),
            ("index.recordOffsetTable", BOOLS),
            ("core.untrackedCache", &["true", "false", "keep", "bogus"]),
            ("core.fsmonitor", BOOLS),
        ],
    },
    // `gc.auto` decides whether the automatic run happens at all;
    // `gc.autoPackLimit` decides what it does when it does; `gc.autoDetach`
    // decides whether the caller waits for it. The port reads them as one flat
    // sequence (`cmd_config.rs:458-472`) with no key gating another, which is
    // itself worth comparing against git, where `gc.auto=0` makes the rest inert.
    ConfigGroup {
        name: "gc-auto",
        keys: &[
            ("gc.auto", INT),
            ("gc.autoPackLimit", INT),
            ("gc.autoDetach", BOOLS),
            ("gc.bigPackThreshold", SIZE),
            ("gc.cruftPacks", BOOLS),
            ("gc.maxCruftSize", SIZE),
            ("gc.aggressiveWindow", INT),
            ("gc.aggressiveDepth", INT),
            ("gc.packRefs", &["true", "false", "notbare", "bogus"]),
        ],
    },
    // Two expiry vocabularies in one command. `gc.reflogExpireUnreachable` is
    // only consulted once `gc.reflogExpire` has resolved
    // (`cmd_config.rs:453-455`), and the prune trio uses a *different* rule from
    // the reflog pair — `approxidate` strictly in the past
    // (`cmd_config.rs:479-482`, `:555-564`) — so `all` is legal to one and
    // rejected by the other.
    ConfigGroup {
        name: "gc-expiry",
        keys: &[
            ("gc.reflogExpire", EXPIRY),
            ("gc.reflogExpireUnreachable", EXPIRY),
            ("gc.pruneExpire", EXPIRY),
            ("gc.worktreePruneExpire", EXPIRY),
            ("gc.logExpiry", EXPIRY),
        ],
    },
    // `branch.<name>.rebase` overrides `pull.rebase` for one branch, and
    // `pull.ff=only` contradicts both. A single key can only ever measure the
    // fallback.
    ConfigGroup {
        name: "pull-rebase",
        keys: &[
            ("pull.rebase", &["true", "false", "merges", "interactive", "bogus"]),
            ("branch.main.rebase", &["true", "false", "merges", "interactive", "bogus"]),
            ("pull.ff", &["true", "false", "only", "bogus"]),
            ("merge.ff", &["true", "false", "only", "bogus"]),
            ("pull.twohead", &["ort", "recursive", "bogus"]),
            ("rebase.autoStash", BOOLS),
            ("merge.autoStash", BOOLS),
        ],
    },
    // `rerere.enabled` decides whether a conflict is *recorded*, and
    // `merge.conflictStyle` decides what the recorded markers look like, so the
    // two together decide what `.git/rr-cache` ends up containing — which
    // `runner::probe_rr_cache` compares. `rerere.autoUpdate` then decides whether
    // the resolution is staged, which changes the index the next step reads.
    ConfigGroup {
        name: "merge-conflict",
        keys: &[
            ("merge.conflictStyle", &["merge", "diff3", "zdiff3", "bogus"]),
            ("rerere.enabled", BOOLS),
            ("rerere.autoUpdate", BOOLS),
            ("merge.ff", &["true", "false", "only", "bogus"]),
            ("merge.verbosity", INT),
            ("merge.log", &["true", "false", "20", "bogus"]),
            ("merge.branchdesc", BOOLS),
        ],
    },
    // Every id in a `log` line is abbreviated to the same width, and the width is
    // `core.abbrev` — but only when `log.abbrevCommit` asked for abbreviation at
    // all. `log.decorate` and `log.date` then decide what else is on the line.
    // The port validates `log.decorate` nowhere (`log_config.rs:209`, an empty
    // arm) and `log.date` as a bare string (`:203`), so both are only measurable
    // through what the command prints.
    ConfigGroup {
        name: "log-render",
        keys: &[
            ("log.abbrevCommit", BOOLS),
            ("core.abbrev", &["4", "7", "12", "40", "auto", "no", "0", "1", "64"]),
            ("log.decorate", &["short", "full", "auto", "no", "true", "false", "bogus"]),
            (
                "log.date",
                &["relative", "local", "iso", "iso-strict", "short", "raw", "human", "unix",
                  "default", "format:%Y-%m-%d", "bogus"],
            ),
            ("log.follow", BOOLS),
            ("log.showRoot", BOOLS),
            ("log.mailmap", BOOLS),
            (
                "log.diffMerges",
                &["off", "none", "on", "first-parent", "separate", "combined", "dense-combined",
                  "remerge", "bogus"],
            ),
        ],
    },
    // One shared range check with three entry points (`default_config.rs:559-566`):
    // `-1` is exempt, `0..=9` is legal, everything else is fatal — and the message
    // names whichever key was written, so a port that normalises them to one key
    // reports the wrong one.
    ConfigGroup {
        name: "zlib-levels",
        keys: &[
            ("core.compression", &["-1", "0", "1", "9", "10", "-2", "bogus", ""]),
            ("core.looseCompression", &["-1", "0", "1", "9", "10", "-2", "bogus", ""]),
            ("pack.compression", &["-1", "0", "1", "9", "10", "-2", "bogus", ""]),
        ],
    },
    // `diff.colorMoved` accepts the whole boolean grammar before its seven mode
    // names (`diff_config.rs:405-423`), and `diff.colorMovedWS`'s second error
    // fires only when `allow-indentation-change` co-occurs with a whitespace
    // token (`:464-470`) — a combination no single token can reach.
    ConfigGroup {
        name: "diff-moved",
        keys: &[
            (
                "diff.colorMoved",
                &["no", "plain", "blocks", "zebra", "default", "dimmed-zebra", "true", "false",
                  "bogus"],
            ),
            (
                "diff.colorMovedWS",
                &["no", "ignore-space-change", "ignore-space-at-eol", "ignore-all-space",
                  "allow-indentation-change", "allow-indentation-change,ignore-all-space", ""],
            ),
            (
                "diff.wsErrorHighlight",
                &["none", "default", "all", "new", "old", "context", "new,old", "bogus"],
            ),
            ("diff.indentHeuristic", BOOLS),
            ("diff.algorithm", &["myers", "minimal", "patience", "histogram", "default", "bogus"]),
            ("diff.context", &["0", "1", "3", "10", "-1"]),
            ("diff.interHunkContext", &["0", "1", "3", "-1"]),
        ],
    },
    // `core.whitespace` is one key whose *tokens* contradict each other — only
    // the `tab-in-indent` + `indent-with-non-tab` pair is fatal
    // (`default_config.rs:475-491`) — and `apply.whitespace` decides what `apply`
    // does about the rules `core.whitespace` set.
    ConfigGroup {
        name: "whitespace-rules",
        keys: &[
            (
                "core.whitespace",
                &["trailing-space", "space-before-tab", "indent-with-non-tab", "tab-in-indent",
                  "tab-in-indent,indent-with-non-tab", "tabwidth=4", "tabwidth=0", "tabwidth=99",
                  "-trailing-space", "blank-at-eof", "bogus"],
            ),
            ("apply.whitespace", &["nowarn", "warn", "fix", "error", "error-all", "bogus"]),
            ("apply.ignoreWhitespace", &["no", "change", "bogus"]),
            ("core.autocrlf", &["true", "false", "input"]),
        ],
    },
    // `submodule.active` is a pathspec and `submodule.<name>.active` is a bool
    // that overrides it for one submodule; `.gitmodules` supplies `path` and
    // `url` and `.git/config` overrides both. This is the group the
    // [`ConfigScope::Modules`] scope exists for — every key here is one git will
    // read out of `.gitmodules`, and no other key is.
    ConfigGroup {
        name: "submodule",
        keys: &[
            ("submodule.sub.path", &["sub", "other", "", "sub/"]),
            ("submodule.sub.url", &["./sub", "../sub", "", "https://example.invalid/x"]),
            ("submodule.sub.active", BOOLS),
            ("submodule.sub.ignore", &["all", "dirty", "untracked", "none", "bogus"]),
            ("submodule.sub.update", &["checkout", "rebase", "merge", "none", "bogus"]),
            ("submodule.sub.branch", &["main", ".", "", "bogus"]),
            ("submodule.active", &["sub", ":(glob)**", ".", "", "no/such"]),
            ("submodule.recurse", BOOLS),
            ("submodule.fetchJobs", INT),
        ],
    },
    // `diff.submodule` and `status.submoduleSummary` decide how a submodule's
    // change is *rendered*, and `diff.ignoreSubmodules` decides whether it is
    // rendered at all. The port dies on a bad `diff.ignoreSubmodules` and only
    // warns on a bad `diff.submodule` (`diff_config.rs:223-240`), and the warning
    // is latched process-wide (`:105-116`), so the pair's outcome depends on
    // which one the command reads first.
    ConfigGroup {
        name: "submodule-render",
        keys: &[
            ("diff.ignoreSubmodules", &["all", "untracked", "dirty", "none", "bogus"]),
            ("diff.submodule", &["log", "short", "diff", "bogus"]),
            ("status.submoduleSummary", &["true", "false", "0", "5", "bogus"]),
            ("submodule.recurse", BOOLS),
            ("fetch.recurseSubmodules", &["true", "false", "on-demand", "bogus"]),
        ],
    },
    // `fetch.prune` and `fetch.pruneTags` are separate switches over one refspec
    // walk, and `fetch.parallel`/`fetch.recurseSubmodules` decide how many
    // processes do it. `fetch.output` then decides whether any of it is printed.
    ConfigGroup {
        name: "fetch",
        keys: &[
            ("fetch.all", BOOLS),
            ("fetch.prune", BOOLS),
            ("fetch.pruneTags", BOOLS),
            ("fetch.showForcedUpdates", BOOLS),
            ("fetch.recurseSubmodules", &["true", "false", "on-demand", "bogus"]),
            ("fetch.parallel", INT),
            ("fetch.output", &["full", "compact", "bogus"]),
            ("remote.origin.prune", BOOLS),
            ("remote.origin.tagOpt", &["--tags", "--no-tags", "bogus"]),
        ],
    },
    // `grep.patternType` and `grep.extendedRegexp` are two spellings of one
    // choice with a documented precedence between them, and the rest decide what
    // a matching line looks like.
    ConfigGroup {
        name: "grep",
        keys: &[
            ("grep.patternType", &["default", "basic", "extended", "fixed", "perl", "bogus"]),
            ("grep.extendedRegexp", BOOLS),
            ("grep.lineNumber", BOOLS),
            ("grep.column", BOOLS),
            ("grep.fullName", BOOLS),
            ("grep.threads", INT),
            ("grep.fallbackToNoIndex", BOOLS),
        ],
    },
    // `core.abbrev` sets the width and `core.disambiguate` decides which objects
    // a short id is allowed to name, so the two together decide whether a given
    // prefix resolves at all. `core.checkStat` is in the same callback and shares
    // its fall-through (`default_config.rs:284-292`).
    ConfigGroup {
        name: "abbrev-disambiguate",
        keys: &[
            ("core.abbrev", &["4", "7", "12", "40", "auto", "no", "false", "0", "1", "64", "true"]),
            (
                "core.disambiguate",
                &["none", "commit", "committish", "tree", "treeish", "blob", "bogus"],
            ),
            ("log.abbrevCommit", BOOLS),
            ("core.checkStat", &["default", "minimal", "bogus"]),
        ],
    },
    // Path interpretation: whether a name is folded, decomposed, quoted or
    // refused. `core.precomposeUnicode` and `core.protectHFS` disagree about the
    // same byte sequences, and `core.quotePath` decides whether the disagreement
    // is even visible in the output.
    ConfigGroup {
        name: "path-handling",
        keys: &[
            ("core.ignoreCase", BOOLS),
            ("core.precomposeUnicode", BOOLS),
            ("core.protectHFS", BOOLS),
            ("core.protectNTFS", BOOLS),
            ("core.quotePath", BOOLS),
            ("core.symlinks", BOOLS),
            ("core.fileMode", BOOLS),
        ],
    },
    // Two spellings of one setting, each in its own arm. A port that normalises
    // them to one key reports the wrong name in the error, and a port that
    // implements only one silently ignores the other — neither is visible unless
    // both spellings are drawn.
    ConfigGroup {
        name: "alias-spellings",
        keys: &[
            ("diff.suppressBlankEmpty", BOOLS),
            ("diff.suppress-blank-empty", BOOLS),
            ("pager.color", BOOLS),
            ("color.pager", BOOLS),
            ("core.commentChar", &["#", ";", "auto", "", "ab", "\n"]),
            ("core.commentString", &["#", ";", "auto", "", "--"]),
            ("repack.writeBitmaps", BOOLS),
            ("pack.writeBitmaps", BOOLS),
            ("diff.color", COLORBOOL),
            ("color.diff", COLORBOOL),
            // The hyphenated spelling git kept for compatibility, and the camel
            // one that replaced it. Verified against stock 2.55.0 that both are
            // live and that each names **itself**: `-c add.ignoreErrors=bogus add
            // README.md` is `fatal: bad boolean config value 'bogus' for
            // 'add.ignoreerrors'` and `-c add.ignore-errors=bogus` is the same
            // sentence ending in `'add.ignore-errors'`. A port that folds the two
            // into one key reports the wrong name, and one that knows only the
            // camel spelling ignores a setting git honours.
            //
            // The hyphenated one is **absent from `git help -c`** — 1005 lines,
            // and `add.ignoreErrors` is the only `add.ignore*` in it — so it is
            // also a key a port built from that listing cannot know it needs.
            // This is `diff.textconv`'s test run in reverse: there, absence from
            // the listing meant the key was not real; here the same absence sits
            // beside a measured refusal, and the refusal is what decides.
            ("add.ignoreErrors", BOOLS),
            ("add.ignore-errors", BOOLS),
            // The same shape one section over, and this pair is an *integer*
            // rather than a boolean: `-c merge.summary=bogus merge <branch>` is
            // `fatal: bad numeric config value 'bogus' for 'merge.summary':
            // invalid unit` and `merge.log` gives the identical sentence under
            // its own name (verified on [`Shape::MergeableDirty`]). `merge.log`
            // is already drawn by `merge-conflict`; the deprecated spelling is
            // drawn nowhere else, is likewise absent from `git help -c`, and is
            // what a port is likeliest not to have.
            ("merge.summary", &["true", "false", "20", "bogus"]),
        ],
    },
    // The repository's own format, which decides whether the rest of the
    // configuration is even read: an unknown `extensions.*` at format version 1
    // makes git refuse the repository outright, and `core.bare` over a worktree
    // is a refusal `setup.c` takes before any command runs.
    ConfigGroup {
        name: "repo-format",
        keys: &[
            ("core.repositoryFormatVersion", &["0", "1", "2", "bogus", ""]),
            ("extensions.worktreeConfig", BOOLS),
            ("extensions.objectFormat", &["sha1", "sha256", "bogus"]),
            ("extensions.refStorage", &["files", "reftable", "bogus"]),
            ("extensions.noSuchExtension", BOOLS),
            ("core.bare", BOOLS),
            ("core.logAllRefUpdates", &["true", "false", "always"]),
            // The key whose whole behaviour is a function of the **scope** it
            // arrives from, which is what makes it belong here rather than in the
            // flat pool. `setup.c` reads the repository's own configuration
            // before it knows which verb is running and never looks at the
            // command line for this one. Verified against stock 2.55.0 on
            // [`Shape::Linear`]:
            //
            //   -c core.worktree=src status                rc 0, ignored outright
            //   [core] worktree = src   in .git/config     fatal: cannot chdir to
            //                                              'src': No such file …
            //   [core] worktree = ..    in .git/config     rc 0, and
            //                                              rev-parse --show-toplevel
            //                                              is the fixture root
            //   [core] worktree = . + [core] bare = true   warning: core.bare and
            //                                              core.worktree do not
            //                                              make sense
            //                                              fatal: unable to set up
            //                                              work tree using invalid
            //                                              config
            //
            // The last row is why it is in *this* group: the refusal needs
            // `core.bare` beside it and needs both of them in a file, so it is
            // reachable only by a two-key draw that also lands in a file scope —
            // and [`sample_scope`] biases a draw toward one scope precisely so
            // that happens. The relative value is resolved against `$GIT_DIR`,
            // not the worktree root, which is why `..` is the spelling that works
            // and `src` is the one that fails.
            ("core.worktree", &["..", ".", "src", "no-such", ""]),
        ],
    },
    // Ref ordering and the `versionsort.suffix` list `version:` sorting consults,
    // which is inert unless a `version:` sort is actually asked for.
    ConfigGroup {
        name: "ref-ordering",
        keys: &[
            ("tag.sort", &["refname", "-refname", "version:refname", "taggerdate", "bogus"]),
            ("branch.sort", &["refname", "-refname", "committerdate", "bogus"]),
            ("versionsort.suffix", &["-pre", "-rc", "", "-"]),
            ("versionsort.prereleaseSuffix", &["-pre", "-rc", ""]),
            ("column.ui", &["never", "always", "auto", "column", "row", "plain", "bogus"]),
            ("column.branch", &["never", "always", "auto", "bogus"]),
            ("column.tag", &["never", "always", "auto", "bogus"]),
            // `column.status` overrides `column.ui` for one verb, under the same
            // guard that reads it (`status_config.rs:143-158`), and each key
            // names *itself* in its refusal. Verified against stock 2.55.0: a
            // bogus value is two lines, `error: unsupported option 'bogus'` then
            // `error: invalid column.status mode bogus` — with `column.ui` and
            // `column.branch` printing their own names in the second line the
            // same way. A port that normalises the pair to one key reports the
            // wrong one of the two.
            ("column.status", &["never", "always", "auto", "bogus"]),
        ],
    },
    // What a `-m` message becomes once it is a commit object. `commit.cleanup`
    // decides whether comment lines are stripped and
    // `core.commentChar`/`core.commentString` decide what a comment line *is*,
    // so the pair decides the bytes that get hashed — a difference that outlives
    // the invocation and is visible to `runner::probe_state`, not merely to the
    // line the command printed. Verified against stock 2.55.0: with
    // `commit.cleanup=strip` and `core.commentChar=';'` a `; semi` line is
    // stripped from the message while `# hash` survives, and with
    // `core.commentString='//'` a `// slash` line is the one that goes.
    // `commit.status`/`commit.verbose` decide what else the template holds, and
    // `status.displayCommentPrefix` decides whether that block is commented at
    // all — which is the same comment character again, read by a second reader.
    ConfigGroup {
        name: "commit-message",
        keys: &[
            ("commit.cleanup", &["strip", "whitespace", "verbatim", "scissors", "default", "bogus"]),
            ("core.commentChar", &["#", ";", "auto", "", "ab", "\n"]),
            // `//` is the obvious second multi-character marker and is *not*
            // here: `config_pool_is_well_formed` rejects a leading `/` because a
            // value is written into argv unsubstituted and a literal root would
            // name one side's copy to both. `--` is the same shape and passes.
            ("core.commentString", &["#", ";", "auto", "", "--", "REM"]),
            ("commit.status", BOOLS),
            ("commit.verbose", &["true", "false", "0", "1", "2", "bogus"]),
            ("commit.gpgSign", BOOLS),
            ("commit.template", PATHNAME),
            ("status.displayCommentPrefix", BOOLS),
        ],
    },
    // `blame`'s output is decided by three keys that are each inert without
    // another. `blame.markIgnoredLines`/`markUnblamableLines` mark only lines
    // attributed to a revision `blame.ignoreRevsFile` named, so neither mark can
    // appear without the file; and `blame.coloring` selects *which* of the two
    // `color.blame.*` slots is consulted.
    //
    // Verified against stock 2.55.0, and not the way the obvious reading
    // predicts: `blame.coloring` alone is what turns the paint on, and it
    // ignores `color.ui` entirely. `blame.coloring=highlightRecent` puts
    // `\e[34m` on every line with `color.ui` unset, still does so under
    // `color.ui=never`, and takes the slot's colour when
    // `color.blame.highlightRecent=red` is also set (`\e[31m`); dropping
    // `blame.coloring` is the only one of the three that removes the escapes.
    // So the pair that matters is coloring+slot — the slot is unobservable
    // without the coloring key, and `color.ui` is a decoy in this decision that
    // a port implementing the usual `want_color` gate would honour.
    //
    // The ignore-revs pool reaches three outcomes rather than one: a missing
    // file is `fatal: could not open object name list: no-such`, a file that
    // exists but holds something else is
    // `fatal: invalid object name: ref: refs/heads/main` (verified with
    // `.git/HEAD`), and the empty value is accepted and ignored.
    ConfigGroup {
        name: "blame-render",
        keys: &[
            ("blame.ignoreRevsFile", &["no-such", ".git/HEAD", "", ".gitignore"]),
            ("blame.markIgnoredLines", BOOLS),
            ("blame.markUnblamableLines", BOOLS),
            ("blame.coloring", &["repeatedLines", "highlightRecent", "none", "bogus"]),
            ("color.blame.repeatedLines", COLOR),
            ("color.blame.highlightRecent", &["red", "red,1.month.ago,blue", "bogus", ""]),
            ("color.ui", COLORBOOL),
            ("blame.showEmail", BOOLS),
            ("blame.showRoot", BOOLS),
            ("blame.blankBoundary", BOOLS),
            ("blame.date", &["iso", "short", "raw", "relative", "bogus"]),
        ],
    },
    // The one config pair in this table that changes an **exit code** by itself.
    // `diff.external` hands the comparison to another program, and
    // `diff.trustExitCode` decides whether that program's non-zero exit means
    // "differences" or "the driver died". Verified against stock 2.55.0:
    // `-c diff.external=false diff <a> <b>` is `fatal: external diff died,
    // stopping at …` with rc 128, and adding `-c diff.trustExitCode=true` makes
    // the same invocation exit 0 — and 1 under `--exit-code`. One key alone can
    // only ever measure the death.
    //
    // `true`/`false` are shell builtins, so the drivers named here exist on
    // every machine and produce no output; `no-such-driver` is the third
    // outcome, where the spawn itself fails.
    ConfigGroup {
        name: "diff-driver",
        keys: &[
            ("diff.external", &["true", "false", "no-such-driver", ""]),
            ("diff.trustExitCode", BOOLS),
            ("diff.autoRefreshIndex", BOOLS),
            ("diff.wordRegex", &["[a-z]+", ".", "[", "", "bogus("]),
            ("diff.orderFile", PATHNAME),
            // `diff.textconv` is *not* here. It reads like a sibling of
            // `diff.trustExitCode` and is not a key at all — git spells it
            // `diff.<driver>.textconv`, per-driver. Verified against stock
            // 2.55.0: it is absent from `git help -c`'s 1005-line list, and
            // `-c diff.textconv=bogus diff HEAD~1` prints the diff and exits 0
            // where every real boolean key in this table dies on `bogus`. A key
            // git does not know is a draw that cannot disagree.
        ],
    },
    // Two integers dividing one fixed width. `diff.statNameWidth` is the budget
    // for the path and `diff.statGraphWidth` the budget for the bar, and the
    // renderer resolves them against each other and against the terminal width —
    // so a port that clamps them independently prints the right line for either
    // key alone and the wrong one for both. Verified against stock 2.55.0:
    // `diff.statNameWidth=5` turns ` src/lib.rs | 1 +` into ` ...rs | 1 +` —
    // the path is truncated from the left to the budget and the elision is
    // `...`, not `…` — and a non-numeric value is
    // `fatal: bad numeric config value 'bogus' for 'diff.statnamewidth':
    // invalid unit`.
    ConfigGroup {
        name: "diff-stat",
        keys: &[
            ("diff.statNameWidth", INT),
            ("diff.statGraphWidth", INT),
            ("diff.dirstat", &["changes", "lines", "files", "cumulative", "10", "files,10", "bogus,alsobogus"]),
            ("diff.context", &["0", "1", "3", "10", "-1"]),
            ("diff.relative", &["true", "false", "src/", "no/such/"]),
        ],
    },
    // Four keys that are **fatal under `log` and accepted under `format-patch`**,
    // because `git_format_config` claims them before the diff callback can
    // reject them (`log_config.rs:105`, one arm, four keys, empty body).
    // Verified against stock 2.55.0 on the same repository:
    // `-c diff.noprefix=bogus log -1` is `fatal: bad boolean config value
    // 'bogus' for 'diff.noprefix'` with rc 128, while
    // `-c diff.noprefix=bogus format-patch --stdout -1` prints the patch and
    // exits 0 — and `color.ui=bogus` behaves the same way.
    //
    // `format.noprefix` is the replacement spelling that stayed validated, and
    // its refusal carries two `hint:` lines nothing else here prints. A port
    // that validates configuration once, before dispatch, gets every one of
    // these six backwards for one of the two verbs.
    ConfigGroup {
        name: "format-shortcircuit",
        keys: &[
            ("format.noprefix", BOOLS),
            ("diff.noprefix", BOOLS),
            ("diff.color", COLORBOOL),
            ("color.diff", COLORBOOL),
            ("color.ui", COLORBOOL),
            ("diff.submodule", &["log", "short", "diff", "bogus"]),
        ],
    },
    // The mail `format-patch` writes. Three interactions, each needing two keys:
    // `format.signatureFile` and `format.signature` name the same trailer from a
    // file and from a literal (verified: the literal wins whichever order the
    // two are read in, and a missing file is `fatal: unable to read signature
    // file 'no-such'`); `format.forceInBodyFrom` adds a second in-body `From:`
    // line only once `format.from` has rewritten the header one (verified);
    // and `format.coverFromDescription` is consulted only where
    // `format.coverLetter` produced a cover to take a description from — while
    // being the key in this set that dies on a bad value
    // (`fatal: bogus: invalid cover from description mode`).
    //
    // `format.thread` is deliberately absent: it emits a `Message-ID` built from
    // the wall clock, which would make every case that drew it nondeterministic
    // on both sides for a reason that has nothing to do with the port.
    ConfigGroup {
        name: "format-patch",
        keys: &[
            ("format.coverLetter", &["auto", "true", "false", "bogus"]),
            (
                "format.coverFromDescription",
                &["default", "none", "message", "subject", "auto", "bogus"],
            ),
            ("format.numbered", &["auto", "true", "false", "bogus"]),
            ("format.subjectPrefix", &["RFC", "", "PATCH v2"]),
            // No embedded newline. A `-c` value goes into the case id verbatim,
            // and an id that spans two lines stops being one record to
            // `scripts/split_failures.pl` and to the report — a multi-line
            // signature is not worth a triage surface that cannot parse it.
            ("format.signature", &["SIG", "", "-- dashes"]),
            ("format.signatureFile", PATHNAME),
            ("format.filenameMaxLength", INT),
            ("format.attach", &["BOUND", "true", "false", ""]),
            ("format.from", &["true", "false", "Bot <bot@example.invalid>"]),
            ("format.forceInBodyFrom", BOOLS),
            ("format.mboxrd", BOOLS),
        ],
    },
    // Sparse checkout is three keys and one worktree. `core.sparseCheckout`
    // decides whether the skip-worktree bits are honoured at all,
    // `core.sparseCheckoutCone` decides which pattern dialect
    // `.git/info/sparse-checkout` is read in, and
    // `sparse.expectFilesOutsideOfPatterns` decides whether a file that is
    // present but outside the patterns gets its bit cleared. Only the first is
    // meaningful alone; the other two are inert without it, which is precisely
    // what a one-key draw cannot show — and the outcome is a *worktree*, so it
    // lands in the state probe rather than in a line of output.
    ConfigGroup {
        name: "sparse-checkout",
        keys: &[
            ("core.sparseCheckout", BOOLS),
            ("core.sparseCheckoutCone", BOOLS),
            ("sparse.expectFilesOutsideOfPatterns", BOOLS),
            ("index.sparse", BOOLS),
            ("core.untrackedCache", &["true", "false", "keep", "bogus"]),
        ],
    },
    // Whether a hint is printed, and what colour it is if so. Two facts here,
    // and both need two keys. A name inside `advice.c`'s table is parsed as a
    // boolean and a name outside it is not parsed at all — verified against
    // stock 2.55.0: `advice.statusHints=bogus` is `fatal: bad boolean config
    // value 'bogus' for 'advice.statushints'` while `advice.noSuchAdvice=bogus`
    // exits 0 — so the *table* is the thing under test and only a pair of draws
    // straddles it. And `advice.<slot>=false` suppresses the hint entirely,
    // which makes whatever `color.advice.<slot>` says unobservable, so the
    // colour keys are only measurable against a slot that is still on.
    //
    // `GIT_ADVICE` in [`ENV_VARS`] is the third switch over the same decision
    // and is drawn independently, so the two can disagree.
    ConfigGroup {
        name: "advice",
        keys: &[
            ("advice.statusHints", BOOLS),
            ("advice.statusUoption", BOOLS),
            ("advice.detachedHead", BOOLS),
            ("advice.noSuchAdvice", BOOLS),
            ("color.advice", COLORBOOL),
            ("color.advice.hint", COLOR),
            ("color.advice.reset", COLOR),
            ("color.advice.bogusSlot", COLOR),
        ],
    },
    // What counts as an unchanged file. `core.checkStat=minimal` narrows the
    // set of stat fields compared, `core.trustCtime=false` removes another field
    // from that same comparison, `core.ignoreStat=true` suppresses it outright
    // and `core.fileMode` decides whether the mode bit is part of it — four keys
    // feeding one predicate in `read-cache.c`. A port that honours each key on
    // its own gets every single-key case right and still refreshes the wrong
    // entries when two of them apply, which shows up as a different `.git/index`
    // rather than as a different line.
    ConfigGroup {
        name: "stat-cache",
        keys: &[
            ("core.checkStat", &["default", "minimal", "bogus"]),
            ("core.trustCtime", BOOLS),
            ("core.ignoreStat", BOOLS),
            ("core.fileMode", BOOLS),
            ("core.fsmonitor", BOOLS),
            ("diff.autoRefreshIndex", BOOLS),
        ],
    },
    // One parser, seven entry points. Every key here is read through
    // `git_config_pathname`, which expands `~`, `~user` and `%(prefix)/` before
    // anything opens the named file, and every one of them fails identically on
    // an expansion it cannot do — verified against stock 2.55.0:
    // `-c core.attributesFile='~nosuchuser/x' check-attr text -- README.md` is
    // `fatal: failed to expand user dir in: '~nosuchuser/x'`, and the same value
    // on `blame.ignoreRevsFile` or `commit.template` dies the same way. Which
    // key dies first is decided by configuration order rather than by the order
    // the command would have consulted them, so drawing two of them together is
    // what puts the *ordering* under test; drawing one measures the expansion
    // once and the ordering never.
    //
    // `attr.tree` is the odd member and is here for that: it names a tree rather
    // than a file and *overrides* the source `core.attributesFile` contributes
    // to, so the pair decides which attributes exist at all.
    ConfigGroup {
        name: "config-pathnames",
        keys: &[
            ("core.attributesFile", PATHNAME),
            ("core.excludesFile", PATHNAME),
            ("attr.tree", &["HEAD", "HEAD^{tree}", "no-such-tree", ""]),
            ("commit.template", PATHNAME),
            ("blame.ignoreRevsFile", PATHNAME),
            ("diff.orderFile", PATHNAME),
            ("format.signatureFile", PATHNAME),
            // The eighth reader of that parser, and the only one whose *hit* is
            // an exit code rather than a rendering. Verified against stock 2.55.0
            // on [`Shape::HooksFail`], whose `pre-commit` refuses:
            // `commit -m x --allow-empty` is rc 1 and prints `pre-commit
            // refuses`, `-c core.hooksPath=no-such` on the same invocation
            // **commits** at rc 0, and `-c core.hooksPath=.git/hooks` — the same
            // directory named the long way — refuses again. The expansion failure
            // is shared with the rest of the group: `~nosuchuser/x` is
            // `fatal: failed to expand user dir in: '~nosuchuser/x'` here too,
            // and it happens before any hook is looked for.
            ("core.hooksPath", &[".git/hooks", "no-such", "~/x", "~nosuchuser/x", "%(prefix)/x", ""]),
        ],
    },
    // The decoration line, which four keys and a colour table decide between
    // them. `log.decorate` says whether any decoration is printed at all,
    // `log.initialDecorationSet` and `log.excludeDecoration` say which refs
    // survive into it, `color.decorate.<slot>` says what colour each surviving
    // kind is, and `color.ui` says whether the colour is emitted. Every one of
    // them is inert without `log.decorate`, which is exactly what a single-key
    // draw cannot show.
    //
    // Three facts, each verified against stock 2.55.0 on [`Shape::Branched`]:
    //
    //  * **The slot table decides whether the value is parsed at all.**
    //    `-c color.decorate.branch=bogus log --oneline -1` is
    //    `error: invalid color value: bogus` + `fatal: unable to parse
    //    'color.decorate.branch' from command-line config` at rc 128, while
    //    `-c color.decorate.bogusSlot=bogus` on the same invocation prints the
    //    commit and exits 0 — `parse_decorate_color_config` returns before it
    //    reads the value for a slot outside `color_decorate_slots[]`
    //    (`log_config.rs:225-240`). `format-patch` does **not** short-circuit
    //    this one (verified: same fatal), unlike the five keys in
    //    `format-shortcircuit`.
    //  * **`log.excludeDecoration` is multi-valued.** Two entries both apply:
    //    with `refs/tags/*` alone the line is `(HEAD -> main)`, and with
    //    `refs/heads/*` added it is `(HEAD)`. Every other key in this table is
    //    last-wins, so a port that stores this one in a `String` reads the second
    //    draw and drops the first — a difference only a repeated key can show,
    //    and repetition is something [`sample_config`] already does.
    //  * **`log.graphColors` refuses without dying.** `-c log.graphColors=bogus
    //    log --graph` prints `error: invalid color value: bogus` *and*
    //    `warning: ignored invalid color 'bogus' in log.graphColors` and still
    //    exits **0** — the same `invalid color value` text that is fatal for
    //    `color.decorate.branch`, from the same parser, with the opposite
    //    outcome. With `red,blue` and `color.ui=always` the second graph column
    //    is `\e[34m` where the default paints `\e[32m`, so the setting is
    //    measurable in stdout and not only in an exit code.
    ConfigGroup {
        name: "decoration",
        keys: &[
            ("log.decorate", &["short", "full", "auto", "no", "true", "false", "bogus"]),
            ("log.excludeDecoration", &["refs/tags/*", "refs/heads/*", "bogus", ""]),
            ("log.initialDecorationSet", &["all", "short", "bogus", ""]),
            ("log.graphColors", &["red,blue", "bogus", "red", ""]),
            ("color.decorate.branch", COLOR),
            ("color.decorate.tag", COLOR),
            ("color.decorate.HEAD", COLOR),
            ("color.decorate.grafted", COLOR),
            ("color.decorate.bogusSlot", COLOR),
            ("color.ui", COLORBOOL),
        ],
    },
    // Keys whose value is parsed **at the moment the code that needs it runs**,
    // beside keys from the same section that are parsed while the file is read.
    // One value, one repository, and whether it is fatal depends on the verb —
    // which is the half of configuration handling a port that validates once,
    // eagerly, gets wrong in both directions at the same time.
    //
    // Every row verified against stock 2.55.0 on [`Shape::Dirty`] with the value
    // `bogus` delivered by `-c`:
    //
    // ```text
    // key                        inert under          fatal under
    // core.bigFileThreshold      hash-object (rc 0)   add (128, invalid unit)
    // core.filesRefLockTimeout   status      (rc 0)   update-ref (128)
    // core.packedRefsTimeout     status      (rc 0)   pack-refs (128)
    // core.splitIndex            add         (rc 0)   — never read
    // core.deltaBaseCacheLimit   —                    status (128)
    // core.packedGitLimit        —                    status (128)
    // core.preloadIndex          —                    status (128)
    // core.multiPackIndex        —                    status (128)
    // core.warnAmbiguousRefs     —                    status (128)
    // core.maxTreeDepth          —                    status (128)
    // ```
    //
    // So the interaction is an *ordering* one, the same shape `config-pathnames`
    // has: drawing a lazy key with an eager one means the eager refusal happens
    // first and the lazy key is never reached, and drawing two lazy ones means
    // the verb decides which of them speaks. A port that answers each key
    // correctly on its own still has to agree about which refusal wins.
    //
    // `core.lockfilePid` is here because it is the newest reader in the set and
    // the one with a *filesystem* consequence rather than a printed one: with it
    // on, git writes a `<resource>~pid.lock` companion beside every lock it takes
    // and unlinks it before the lock is persisted, so a step that dies between
    // the two leaves a file behind for `probe_state` to find. Verified:
    // `-c core.lockfilePid=bogus status` is `fatal: bad boolean config value
    // 'bogus' for 'core.lockfilepid'`, and `=true` exits 0 leaving nothing.
    ConfigGroup {
        name: "lazy-validation",
        keys: &[
            ("core.bigFileThreshold", SIZE),
            ("core.filesRefLockTimeout", INT),
            ("core.packedRefsTimeout", INT),
            ("core.splitIndex", BOOLS),
            ("core.deltaBaseCacheLimit", SIZE),
            ("core.packedGitLimit", SIZE),
            ("core.packedGitWindowSize", SIZE),
            ("core.preloadIndex", BOOLS),
            ("core.multiPackIndex", BOOLS),
            ("core.warnAmbiguousRefs", BOOLS),
            ("core.maxTreeDepth", INT),
            ("core.lockfilePid", BOOLS),
        ],
    },
    // The cruft-pack and multi-pack-index settings `gc` and `repack` divide
    // between them, and the only group in this table whose members reach **three
    // different exit codes** from one value.
    //
    // `repack`'s cruft knobs are not parsed as configuration at all: they are
    // spliced into an option list and handed to a child `repack`, so their
    // refusal is a *usage* error from the other side of a fork. Verified against
    // stock 2.55.0 on [`Shape::Packed`], all with `bogus`:
    //
    // ```text
    // repack.cruftWindow     repack -d          rc 0    (never reached)
    // repack.cruftWindow     repack --cruft -d  rc 129  error: option `window'
    //                                                   expects an integer value…
    // repack.cruftDepth      gc                 rc 128  the same error, then
    //                                                   fatal: failed to run repack
    // repack.midxSplitFactor repack -d          rc 128  bad numeric config value
    // repack.packKeptObjects repack -d          rc 128  bad boolean config value
    // gc.cruftPacks          gc                 rc 128  bad boolean config value
    // gc.repackFilter        repack -d          rc 0    (a string, unvalidated)
    // ```
    //
    // 0, 128 and 129 for the same word, decided by which verb ran and whether a
    // cruft pack was being written. A port that validates these where it
    // validates the rest of `[repack]` turns the 129 into a 128 and the 0 into
    // either — and the 129 is the one no other group in this file can produce,
    // because it comes from a child's `parse-options` rather than from a config
    // callback. Every key here was added to `cmd_config.rs` by the config-chain
    // work, and none of them was drawn by any generated case before this group.
    ConfigGroup {
        name: "repack-cruft",
        keys: &[
            ("repack.cruftWindow", INT),
            ("repack.cruftWindowMemory", SIZE),
            ("repack.cruftDepth", INT),
            ("repack.cruftThreads", INT),
            ("repack.midxMustContainCruft", BOOLS),
            ("repack.midxSplitFactor", INT),
            ("repack.midxNewLayerThreshold", SIZE),
            ("repack.packKeptObjects", BOOLS),
            ("repack.useDeltaIslands", BOOLS),
            ("repack.updateServerInfo", BOOLS),
            ("repack.useDeltaBaseOffset", BOOLS),
            ("gc.cruftPacks", BOOLS),
            ("gc.maxCruftSize", SIZE),
            ("gc.repackFilter", &["blob:none", "blob:limit=1k", "bogus", ""]),
            ("gc.repackFilterTo", &["gen-filtered.pack", "no/such/dir/x", ""]),
            ("pack.deltaCacheSize", SIZE),
        ],
    },
    // Which notes ref is read, which are displayed, and what happens to them when
    // a commit is rewritten. Three keys, three *severities*, and each severity is
    // only reachable through the reader that owns it — verified against stock
    // 2.55.0 on [`Shape::NotesReplace`], which carries three notes refs holding
    // different text for `HEAD`:
    //
    // ```text
    // core.notesRef=refs/notes/other  notes show HEAD  prints the *other* note
    // core.notesRef=bogus             notes show HEAD  fatal: refusing to show
    //                                 notes in bogus (outside of refs/notes/)
    // core.notesRef=bogus             status           rc 0
    // notes.displayRef=refs/notes/*   log --notes      three notes, concatenated
    // notes.displayRef=bogus          log --notes      warning: notes ref bogus
    //                                 is invalid — and the default note anyway,
    //                                 rc 0
    // notes.displayRef=bogus          notes list       rc 0, silent
    // notes.rewriteMode=bogus         commit --amend   error: Bad
    //                                 notes.rewriteMode value: 'bogus' — and the
    //                                 commit is **made**, rc 0
    // notes.rewrite.amend=bogus       commit --amend   fatal: bad boolean config
    //                                 value…, rc 128
    // notes.mergeStrategy=bogus       notes merge      error + fatal, rc 128
    // ```
    //
    // Fatal, warning, non-fatal `error:` and silence, from four keys of one
    // family — and the `notes.rewriteMode` row is the one worth the group on its
    // own, because a port that treats a bad value there as fatal refuses a commit
    // git makes. `core.notesRef` is also the key that decides which ref every
    // other row is about, so a wrong reading of it makes the rest look right.
    ConfigGroup {
        name: "notes-refs",
        keys: &[
            ("core.notesRef", &["refs/notes/commits", "refs/notes/other", "bogus", ""]),
            ("notes.displayRef", &["refs/notes/*", "refs/notes/other", "bogus", ""]),
            ("notes.rewriteMode", &["overwrite", "concatenate", "cat_sort_uniq", "ignore", "bogus"]),
            ("notes.rewrite.amend", BOOLS),
            ("notes.rewrite.rebase", BOOLS),
            ("notes.rewriteRef", &["refs/notes/commits", "refs/notes/*", "bogus", ""]),
            ("notes.mergeStrategy", &["manual", "ours", "theirs", "union", "bogus"]),
            ("format.notes", &["true", "false", "refs/notes/other", "bogus"]),
        ],
    },
];

/// Lines written verbatim into a scope's file that git's parser **rejects**,
/// each verified against stock 2.55.0 to produce
/// `fatal: bad config line <n> in file <path>`.
///
/// Unreachable any other way. `-c` is handed a key and a value that have already
/// been split, so no `-c` can express a section header that never closes, a
/// value whose quote never closes, or a `]` with nothing in front of it — and
/// none of those produce a *line number*, which is the diagnostic a file has and
/// a command line does not.
///
/// The bad-escape entry carries its own `[core]` header because a `\q` is only a
/// bad escape once the line is being read as a value; appended to a file with no
/// section it would fail earlier, with a different message, and the case would
/// be measuring a different refusal than the one it names.
const CONFIG_BAD_LINES: &[&str] = &[
    "garbage line",
    "[core",
    "[bad section]",
    "]",
    "[]",
    "[core]]",
    "= 1",
    "x = \"unterminated",
    "[core \"a\" b]",
    "[core]\n\tabbrev = \"bad\\qescape\"",
];

/// Lines that are *legal* and that `-c` still cannot produce.
///
/// The malformed pool above measures the refusal; this one measures the parser
/// on inputs it accepts and that no other scope can deliver: a key with no value
/// at all (which is boolean **true**, while `-c key=` is the empty string — the
/// asymmetry `default_config.rs:823-830` implements), a trailing comment, a
/// backslash continuation that joins two lines into one value, a section and a
/// key on one line, and case folding of both the section and the key.
///
/// Each was checked against stock 2.55.0: the continuation yields `45` from
/// `4\`+`5`, the folded spelling yields `4`, and the valueless key yields the
/// empty rendering `--get` gives a true boolean.
const CONFIG_ODD_LINES: &[&str] = &[
    "[core]\n\tabbrev",
    "[core]\n\tabbrev = 4 # comment",
    "[core]\n\tabbrev = 4 ; comment",
    "# comment only",
    "; comment only",
    "[core]\n\tabbrev = 4\\\n5",
    "[CORE]\n\tABBREV = 4",
    "[core] abbrev = 4",
    "[branch \"a.b\"]\n\tmerge = refs/heads/main",
    "\n",
];

/// Keys written with **no value at all**, which is a fifth thing only a file can
/// say.
///
/// `-c key=` and `key` on its own line are different inputs: the first delivers
/// the empty string and the second delivers `NULL`, and git's callbacks branch on
/// which one arrived. [`crate::runner::ConfigEntry::set`] always renders
/// `key=value`, so no keyed draw in this file can produce the second — a raw line
/// is the only route, and until now the only valueless line in the pools was
/// `[core]\n\tabbrev`, which measures the `NULL` branch of exactly one key.
///
/// The reason it is worth its own pool rather than six more entries in
/// [`CONFIG_ODD_LINES`] is that "no value" is not one outcome. Each line below
/// was run against stock 2.55.0 appended to `.git/config`, and they reach five
/// different renderings of the same condition:
///
/// ```text
/// [core] quotePath     status         rc 0 — git_config_bool(NULL) is *true*
/// [format] headers     format-patch   fatal: format.headers without value
/// [format] headers     log            rc 0 — the arm is only in git_format_config
/// [diff] context       status         fatal: bad numeric config value '' for
///                                     'diff.context' in file .git/config:
///                                     invalid unit
/// [log] diffMerges     log            error: missing value for 'log.diffmerges'
///                                     fatal: bad config variable
///                                     'log.diffmerges' in file '.git/config' at
///                                     line 10
/// [core] worktree      status         error: missing value for 'core.worktree'
///                                     fatal: bad config line 10 in file
///                                     .git/config
/// ```
///
/// Four fatal sentences and one silent acceptance, and the `format.headers` pair
/// is the whole shape this pool exists for: one key, one value, two verbs, and
/// the refusal lives in the callback `format-patch` installs and `log` does not
/// (`log_config.rs:88-93`). A port that validates configuration once, before
/// dispatch, cannot get both rows right.
///
/// `core.worktree` is the odd one and is here for it: its refusal is *setup*'s
/// rather than the config reader's, so it names a line and not a variable, and it
/// happens before the verb is even reached.
///
/// `[gc]\n\tpruneExpire` is the sixth and shares `log.diffMerges`'s rendering —
/// verified, `gc --auto` gives `error: missing value for 'gc.pruneexpire'` and
/// the same `bad config variable … at line 10` fatal — reached through a
/// different callback (`cmd_config.rs`'s gc chain rather than the log one), which
/// is the half of the pair a port can get right in one place and wrong in the
/// other.
const CONFIG_VALUELESS_LINES: &[&str] = &[
    "[core]\n\tquotePath",
    "[format]\n\theaders",
    "[diff]\n\tcontext",
    "[log]\n\tdiffMerges",
    "[core]\n\tworktree",
    "[gc]\n\tpruneExpire",
];

/// The scopes a given key may be delivered from.
///
/// `.gitmodules` is not a general configuration file: `submodule-config.c` reads
/// `submodule.*` out of it and nothing else, so putting `core.abbrev` there
/// would write a file git parses and then discards — a case that looks like it
/// set a key and set nothing. Every other key is deliverable from every scope,
/// which is what makes the scope the variable under test rather than a property
/// of the key.
fn scopes_for(key: &str) -> &'static [ConfigScope] {
    if key.starts_with("submodule.") {
        ConfigScope::ALL
    } else {
        // Every scope git layers, which for a key that is not a submodule key is
        // every scope there is: `ConfigScope::ALL` differs from this only by
        // `Modules`, which is the one this branch exists to exclude.
        ConfigScope::ORDERED
    }
}

// ---------------------------------------------------------------------------
// Environment
// ---------------------------------------------------------------------------

/// The `GIT_*` variables [`crate::env::harden`] deliberately leaves unset, with
/// the values worth setting them to.
///
/// `harden` starts from `env_clear`, so every one of these is guaranteed absent
/// on both sides unless a case sets it — which is what makes sampling one purely
/// additive and keeps the two runs symmetric. None of them may be a pin;
/// [`sampled_env_vars_are_never_pinned`] asserts that against `env::is_pinned`
/// rather than trusting this list to stay correct, and `apply_case_env` asserts
/// it again per case.
///
/// Path-shaped values are written with [`crate::runner::REPO_PLACEHOLDER`], never
/// as a literal absolute path: the two sides run against copies at different
/// roots, and a literal would name one side's repository to both.
const ENV_VARS: &[(&str, &[&str])] = &[
    // Discovery: which repository, and which worktree, before anything else.
    ("GIT_DIR", &[".git", "{repo}/.git", ".", "no-such-dir"]),
    ("GIT_WORK_TREE", &[".", "{repo}", "src", "no-such-dir"]),
    ("GIT_CEILING_DIRECTORIES", &["{repo}", "{repo}/src", "{repo}/no-such-dir", ""]),
    // Ref and object storage redirection.
    ("GIT_NAMESPACE", &["ns", "a/b", "", "refs/heads"]),
    ("GIT_INDEX_FILE", &["{repo}/.git/index", "{repo}/.git/no-such-index", ".git/index"]),
    ("GIT_OBJECT_DIRECTORY", &["{repo}/.git/objects", "{repo}/.git/no-such-objects"]),
    // Pathspec interpretation. Git reads these as "set to anything non-empty",
    // so `0` and `false` are *on* — the inverted-flag trap, from the environment
    // side this time.
    ("GIT_LITERAL_PATHSPECS", &["1", "0", "", "true", "false"]),
    ("GIT_ICASE_PATHSPECS", &["1", "0", "", "true", "false"]),
    ("GIT_GLOB_PATHSPECS", &["1", "0", "", "true", "false"]),
    ("GIT_NOGLOB_PATHSPECS", &["1", "0", "", "true", "false"]),
    // Advice, attributes, replacement, locking, and flush behaviour.
    ("GIT_ADVICE", &["0", "1", "false", "true"]),
    ("GIT_ATTR_NOSYSTEM", &["1", "0"]),
    ("GIT_NO_REPLACE_OBJECTS", &["1", "0"]),
    ("GIT_OPTIONAL_LOCKS", &["0", "1", "bogus"]),
    ("GIT_FLUSH", &["0", "1"]),
    // The reflog message itself. Nothing else in the harness can set it, and the
    // reflog is where this port has historically been most wrong — the first run
    // of the curated sequence corpus found eight defects and five were reflog
    // messages. `GIT_REFLOG_ACTION` replaces the verb's own prefix wholesale, so
    // a port that hard-codes `"commit: "` writes a line stock does not. Verified
    // against stock 2.55.0 by committing a message of `z` under each value:
    // `gen action` gives `HEAD@{0}: gen action: z`, and the empty string gives
    // `HEAD@{0}: : z` — an empty action still gets its separator.
    ("GIT_REFLOG_ACTION", &["gen action", "", "checkout", "rebase (pick)"]),
    // Which ref the notes machinery reads and writes. `log` decorates from it,
    // `notes` writes to it, and a port that resolves the default in one place
    // and the variable in the other disagrees only when the two differ.
    ("GIT_NOTES_REF", &["refs/notes/commits", "refs/notes/gen", "bogus", ""]),
    // Where attributes come from, as the environment spelling of
    // `--attr-source`. Both the option and the variable are drawn, so a port
    // that implements one of the two is caught by whichever it skipped.
    // Verified: an unreadable source — including the *empty* value — is
    // `fatal: bad --attr-source or GIT_ATTR_SOURCE`, which is a refusal taken
    // before any attribute is looked up.
    ("GIT_ATTR_SOURCE", &["HEAD", "HEAD^{tree}", "does-not-exist", ""]),
    // The second half of object lookup. `GIT_OBJECT_DIRECTORY` above replaces
    // the store; this one *adds* to it, and a missing entry is reported
    // (`error: object directory … does not exist; check
    // .git/objects/info/alternates`) without failing the command — a
    // warn-and-continue path nothing else reaches.
    (
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        &["{repo}/.git/objects", "{repo}/.git/no-such-objects", ""],
    ),
    // The linked-worktree split: which directory holds the refs and config that
    // are shared, as opposed to the per-worktree `HEAD` and index. A port that
    // treats the git dir as one directory resolves every case on every other
    // shape and none once this points somewhere else.
    ("GIT_COMMON_DIR", &["{repo}/.git", "{repo}/.git/no-such", ".git"]),
    // The on-disk format of the file every command rewrites. Versions 2, 3 and 4
    // have different entry encodings — v4 prefix-compresses path names — and an
    // unusable value warns rather than refusing: verified against stock 2.55.0,
    // `0`, `9` and `bogus` each print
    // `warning: GIT_INDEX_VERSION set, but the value is invalid.` followed by
    // `Using version 3` and exit 0, so the *warning text on stderr* is what a
    // case drawing one of them compares. `3` is the other surprise: asking for
    // it writes a v2 index (`update-index --show-index-version` answers 2),
    // because v3 is only chosen when an entry needs an extended flag.
    //
    // The version itself is **not** in the state probe — `runner::probe_state`
    // reads the index through `ls-files --stage`, whose three lines are the same
    // whichever version wrote them — so a port that writes v2 for `4` is caught
    // only by a case that asks for the version back or by the interop probe
    // failing to read the file at all.
    ("GIT_INDEX_VERSION", &["2", "3", "4", "0", "9", "bogus"]),
    // Where `replace` refs live. The empty value is the interesting one and is
    // verified: stock takes it to mean *every* ref is a replacement and prints
    // `warning: bad replace ref name: refs/heads/main` for each — a whole-ref-
    // namespace reinterpretation from one empty string.
    //
    // The trailing slashes are load-bearing. Verified against stock 2.55.0:
    // `GIT_REPLACE_REF_BASE=refs/replace git log -1` aborts with
    // `BUG: refs.c:1900: ref pattern must end in a trailing slash when trimming`
    // and rc 134 — stock kills itself on a value that is a prefix without its
    // separator, so a case spelling one would compare a SIGABRT against whatever
    // the port does and measure git's own assertion rather than the port. With
    // the slash both spellings exit 0.
    ("GIT_REPLACE_REF_BASE", &["refs/replace/", "refs/gen-replace/", ""]),
    // How much a merge says about what it did. Deterministic (the levels select
    // fixed message sets, not timings) and orthogonal to `merge.verbosity` in
    // the `merge-conflict` group, so the two can be drawn against each other.
    ("GIT_MERGE_VERBOSITY", &["0", "1", "2", "5", "bogus"]),
    // Whether a broken or unreadable ref aborts the iteration or is skipped.
    // Nothing in the fixtures is broken, so this measures the *decision* rather
    // than a repair — which is the half a port implements as a constant.
    ("GIT_REF_PARANOIA", &["0", "1"]),
];

// ---------------------------------------------------------------------------
// Global options
// ---------------------------------------------------------------------------

/// The options `git.c:handle_options` parses before it dispatches a verb.
///
/// Each entry is one whole option including its argument, so the shrinker can
/// drop `-C src` without leaving `src` behind as a positional.
///
/// `--list-cmds=main` is here even though it is not an option any porcelain
/// caller writes: it terminates argument handling and prints a list instead of
/// running the subcommand at all, which is a path nothing else in the harness
/// reaches, and it is a known gap in the port that no case could previously
/// catch.
const GLOBAL_OPTIONS: &[&[&str]] = &[
    &["--no-pager"],
    &["-P"],
    &["--no-advice"],
    &["--no-optional-locks"],
    &["--no-replace-objects"],
    &["--literal-pathspecs"],
    &["--icase-pathspecs"],
    &["--glob-pathspecs"],
    &["--noglob-pathspecs"],
    &["--namespace=ns"],
    &["--namespace=a/b"],
    &["--namespace="],
    &["-C", "src"],
    &["-C", "."],
    &["-C", "no-such-dir"],
    &["--git-dir=.git"],
    &["--work-tree=."],
    &["--attr-source=HEAD"],
    &["--attr-source=does-not-exist"],
    &["--list-cmds=main"],
    // The *other* half of the pager switch. `--no-pager` and `-P` are already
    // here; `-p` and `--paginate` take the opposite branch of the same code, and
    // that branch is the one that has been wrong — `pager: give the host its
    // stdout back` is a commit in this repository's history. `GIT_PAGER=cat` is
    // pinned by `env::harden`, so the pager runs and the output stays
    // deterministic instead of the option being untestable.
    &["-p"],
    &["--paginate"],
    // `setup.c` refuses the pair rather than honouring it. Verified against
    // stock 2.55.0 over a worktree: `git --bare status --porcelain` is
    // `fatal: not a git repository: '<path>'` with rc 128 — a refusal taken
    // before the subcommand exists, which `core.bare=true` in the `repo-format`
    // group reaches from the configuration side and nothing reached from here.
    &["--bare"],
    &["--no-lazy-fetch"],
    // The one delivery mechanism for a setting that is neither a file, a `-c`,
    // nor `GIT_CONFIG_KEY_<n>`: a key whose *value* is the name of a variable.
    // `env::harden` clears the environment, so the named variable is guaranteed
    // absent and the outcome is deterministic — verified:
    // `fatal: missing environment variable 'PARITY_UNSET' for configuration
    // 'core.abbrev'`, rc 128.
    &["--config-env=core.abbrev=PARITY_UNSET"],
    // An option git *used* to parse. Verified: stock 2.55.0 answers
    // `unknown option: --super-prefix=x/` with rc 129 and the usage block, so a
    // port that kept the option alive for compatibility is caught by the one
    // case that spells it.
    &["--super-prefix=x/"],
    // `--list-cmds` takes a *group* name, and the group decides both the list
    // and whether there is one. `main` is already here; `parseopt` is a
    // different list, and `bogus` is
    // `fatal: unsupported command listing type 'bogus'` with rc 128.
    &["--list-cmds=parseopt"],
    &["--list-cmds=bogus"],
    // Discovery pointed somewhere that is not a repository, from both spellings.
    &["--git-dir=no-such"],
    &["--work-tree=src"],
    // `-C` is repeatable and each hop is resolved against the previous one, so
    // this pair lands back at the fixture root by a route that is not the root.
    // Kept as one entry because the shrinker drops entries whole: half a chain
    // is a different case, not a smaller one.
    &["-C", "src", "-C", ".."],
    // Terminates option handling and prints instead of dispatching, like
    // `--list-cmds`. Safe to compare: the port answers `git version 2.55.0`,
    // the same string stock does.
    &["--version"],
];

// ---------------------------------------------------------------------------
// Working directory
// ---------------------------------------------------------------------------

/// Directories every shape contains, because `git init` and the base commit
/// create them: the git dir and four of its subdirectories, plus the one tracked
/// subdirectory the base fixture writes (`src/lib.rs`).
///
/// `Shape::Dirty` deletes `src/lib.rs` but not `src/`, and `fixture::copy_tree`
/// recreates empty directories, so the tracked subdirectory survives in every
/// shape. The runner would create a missing directory on both sides anyway; the
/// point of listing only what exists is that a directory git *finds* is a
/// different discovery situation from one that was conjured for the case.
const COMMON_DIRS: &[&str] =
    &[".git", ".git/refs", ".git/refs/heads", ".git/objects", ".git/info", ".git/hooks", "src"];

/// Directories a particular shape adds, read off `fixture::build`.
///
/// These are the layouts that make discovery interesting and that no other shape
/// can express: the `.git`-file indirection of a submodule checkout, the
/// per-worktree admin directory of a linked worktree, and a bare repository.
fn shape_dirs(shape: Shape) -> &'static [&'static str] {
    match shape {
        Shape::Worktree => &["wt", ".git/worktrees", ".git/worktrees/wt"],
        Shape::Submodule => &["sub", ".git/modules", ".git/modules/sub"],
        Shape::BehindRemote => &[".remote.git", ".remote.git/refs", ".remote.git/objects"],
        Shape::AwkwardPaths => &["nested", "nested/deep"],
        Shape::Attributes => &["docs", "vendor", "logs", "assets", "sub"],
        Shape::NoIndexTrees => &["ni", "ni/da", "ni/db", "ni/addonly_a", "ni/delonly_a"],
        // `outside/` survives only because an untracked file is written into it
        // after the cone is applied; `outside/nested` does not.
        Shape::Sparse => &["inside", "inside/nested", "outside"],
        // The combination that mattered: a hook present AND a working directory
        // below the top level. Committing from here with any hook at all used to
        // exit 1 with `No such file or directory`, and with no shape_dirs entry a
        // generated case could only ever have reached it by drawing one of the
        // COMMON_DIRS.
        Shape::Hooked => &["sub"],
        // A real directory and a **symlink to it**, which is the one discovery
        // question no other shape can pose: what a command answers about where
        // it is when the path it was started in is not the path it is at.
        // Verified against stock 2.55.0 on this shape,
        // `rev-parse --show-toplevel --show-prefix --show-cdup` run from each:
        // the two answers are byte-identical — the worktree root, `dir/`, `../`
        // — so git resolves the cwd rather than reporting the way it got there.
        // A port that takes its prefix from the logical path answers
        // `link-to-dir/` and is right about everything else.
        Shape::Symlinks => &["dir", "link-to-dir"],
        // Three linked worktrees instead of one, and the third is the point:
        // `.git/worktrees/wt-gone` is registered administrative state whose
        // checkout no longer exists, so a case run from inside it is asking what
        // a command answers when the *other* end of the pair it is standing in
        // is gone. `wt-gone` itself is deliberately absent from this list for
        // the reason the doc comment above gives — it does not exist, and a
        // directory the runner conjured is a different discovery situation from
        // one git found.
        Shape::WorktreeLocked => &[
            "wt",
            "wt-open",
            ".git/worktrees",
            ".git/worktrees/wt",
            ".git/worktrees/wt-open",
            ".git/worktrees/wt-gone",
        ],
        // The subdirectory each of these tracks a path below, so the prefix half
        // of a discovery answer is exercised on the shape whose index is the
        // reason it is drawn at all: `sub/ita-nested.txt` is an intent-to-add
        // entry below the top level, and `pkg/deep-renamed.txt` is a staged
        // rename below it.
        Shape::IntentToAdd => &["sub"],
        Shape::PendingRename => &["pkg"],
        // `.git/rr-cache`, which only this shape has: a directory *inside* the
        // git dir holding the preimage/postimage pairs, so a case drawn here is
        // both inside `.git` and inside a store no other shape carries.
        Shape::Rerere => &[".git/rr-cache"],
        // The three shapes that keep a bare peer inside the fixture, spelled the
        // same way [`Shape::BehindRemote`] is. Standing in a bare repository is
        // the discovery situation that separates `--is-bare-repository`,
        // `--show-toplevel` and `--show-cdup` from their answers everywhere
        // else, and each of these three peers differs from `BehindRemote`'s in
        // what it holds — hooks, the shallow clone's source, the promisor's.
        Shape::HooksFail => {
            &[".remote.git", ".remote.git/refs", ".remote.git/objects", ".remote.git/hooks"]
        }
        Shape::Shallow | Shape::Promisor => {
            &[".remote.git", ".remote.git/refs", ".remote.git/objects"]
        }
        _ => &[],
    }
}

// ---------------------------------------------------------------------------
// stdin
// ---------------------------------------------------------------------------

/// A tree entry naming the empty blob, whose id is a constant of the hash
/// function rather than of any fixture — so `mktree` gets a *valid* entry
/// without the case having to read an object id off disk at run time.
const P_TREE_ENTRY: &[u8] = b"100644 blob e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\tREADME.md\n";
/// The same shape with an id that is not one.
const P_TREE_ENTRY_BAD: &[u8] = b"100644 blob notanoid\tREADME.md\n";
/// A commit message with a trailer block, for `interpret-trailers`.
const P_TRAILERS: &[u8] = b"subject line\n\nbody text\n\nSigned-off-by: A U Thor <author@example.invalid>\n";
/// A well-formed unified diff against a path the base fixture tracks, for
/// `patch-id`, `apply` and `am`.
const P_PATCH: &[u8] = b"diff --git a/README.md b/README.md\nindex 0000000..1111111 100644\n--- a/README.md\n+++ b/README.md\n@@ -1 +1,2 @@\n # fixture\n+added line\n";
/// The same patch cut off mid-hunk.
const P_PATCH_TRUNCATED: &[u8] = b"diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1 +1,2 @@\n # fixt";
/// Ref transactions for `update-ref --stdin`.
const P_REF_UPDATES: &[u8] = b"create refs/heads/parity-fuzz HEAD\n";
/// The same, where the second command must fail — an implementation without a
/// real transaction leaves the first ref behind.
const P_REF_UPDATES_FAIL: &[u8] = b"create refs/heads/parity-fuzz HEAD\ncreate refs/heads/main HEAD\n";
/// Revisions and object names, for `cat-file --batch`, `rev-list --stdin` and
/// `diff-tree --stdin`. Includes the null oid, which resolves to nothing.
const P_OIDS: &[u8] = b"HEAD\nHEAD^{tree}\n0000000000000000000000000000000000000000\n";
/// Paths, for `check-ignore --stdin`, `check-attr --stdin` and
/// `update-index --stdin`.
const P_PATHS: &[u8] = b"README.md\nsrc/lib.rs\nno/such/path\n";
/// The same paths NUL-separated, which is what every `-z` mode expects and what
/// a reader that splits on newline gets wrong.
const P_PATHS_NUL: &[u8] = b"README.md\0src/lib.rs\0no/such/path\0";
/// The same paths with CRLF line endings.
const P_PATHS_CRLF: &[u8] = b"README.md\r\nsrc/lib.rs\r\n";
/// One path with no trailing newline at all — the last-line-without-EOL case
/// that a line reader drops.
const P_PATH_NO_EOL: &[u8] = b"README.md";
/// An index-info line, for `update-index --index-info`.
const P_INDEX_INFO: &[u8] = b"100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\t0\tREADME.md\n";
/// Nothing at all: immediate EOF, which is a different input from closed stdin
/// for anything that distinguishes "no payload" from "no pipe".
const P_EMPTY: &[u8] = b"";
/// Bytes that are not text, including embedded NULs and invalid UTF-8.
const P_BINARY: &[u8] = b"\x00\x01\x02\xff\xfe\n\x00garbage\x00\n";
/// A message with everything `stripspace` exists to remove: trailing blanks, a
/// run of empty lines, a leading blank run, and a comment line.
///
/// The payloads above are all *clean* — `P_TRAILERS` has no trailing whitespace,
/// no blank run and no comment — so `stripspace`'s entire job was measured only
/// by its ability to copy bytes through. Verified against stock 2.55.0, this one
/// input reaches three different answers from the same command: bare
/// `stripspace` collapses the blank run and keeps `# comment`, `-s` removes the
/// comment as well, and `-c` prefixes every line including the blank ones.
const P_STRIPSPACE_MESSY: &[u8] = b"stripme  \n\n\n\n# comment\n\ttabbed  \n\n\n";
/// A trailer with no blank line in front of it.
///
/// `P_TRAILERS` has a proper block, so `interpret-trailers` only ever had to
/// append to one. Here the last line *looks* like a trailer while the message
/// has no block at all, which is the case `trailer.c` decides by scanning
/// backwards — and stock 2.55.0 inserts the blank line the message was missing.
const P_TRAILERS_NO_BLOCK: &[u8] = b"subject only\nSigned-off-by: A <a@example.invalid>\n";
/// A patch that creates a file rather than editing one.
///
/// `P_PATCH` edits a tracked path, so `apply`'s creation path — `/dev/null` on
/// the old side, a mode line, no preimage to match — was unreachable. Verified:
/// `apply --check` accepts this against every shape, because it depends on the
/// absence of a path rather than on the content of one.
const P_PATCH_CREATE: &[u8] = b"diff --git a/new.txt b/new.txt\nnew file mode 100644\nindex 0000000..3b18e51\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hello world\n";
/// The same edit as [`P_PATCH`] with CRLF line endings in the hunk.
///
/// The payload whose acceptance depends on a *flag in the same argv*: verified
/// against stock 2.55.0, `apply --check` on this exits **1** with
/// `patch does not apply`, and `apply --check --ignore-whitespace` exits **0**.
/// Nothing else in this pool makes the sampled flags decide whether the input is
/// valid, which is what makes it worth a payload of its own.
const P_PATCH_CRLF: &[u8] = b"diff --git a/README.md b/README.md\nindex 0000000..1111111 100644\n--- a/README.md\n+++ b/README.md\n@@ -1 +1,2 @@\r\n # fixture\r\n+added\r\n";
/// Ref transactions in the `-z` dialect: `create SP <ref> NUL <value> NUL`.
///
/// A different parser from [`P_REF_UPDATES`], not a different payload — the
/// newline dialect splits on whitespace and dequotes, the NUL dialect does
/// neither. A port with one reader and a flag passes every case that draws the
/// first and none that draws this. Verified: creates the ref under
/// `update-ref --stdin -z`.
const P_REF_UPDATES_NUL: &[u8] = b"create refs/heads/parity-fuzz-z\0HEAD\0";
/// The transaction verbs of `update-ref --stdin`.
///
/// `start`/`prepare`/`commit` are a state machine layered on top of the command
/// vocabulary, and `option no-deref` changes what the update in between means.
/// They also *print*: stock 2.55.0 answers `start: ok`, `prepare: ok`,
/// `commit: ok` on stdout, so the machine is compared rather than only its
/// effect. Nothing else reaches these verbs at all.
const P_REF_TRANSACTION: &[u8] =
    b"option no-deref\nstart\ncreate refs/heads/parity-fuzz-tx HEAD\nprepare\ncommit\n";
/// A `cat-file --batch-command` request stream.
///
/// `--batch` reads *object names*; `--batch-command` reads **commands** —
/// `info`, `contents`, `flush` — and feeding the first dialect to the second
/// mode measures a rejection rather than the parser. This payload also depends
/// on a sampled flag, the other way round from [`P_PATCH_CRLF`]: verified
/// against stock 2.55.0, `flush` without `--buffer` is
/// `fatal: flush is only for --buffer mode` with rc 128, and with `--buffer` the
/// same stream succeeds.
const P_BATCH_COMMANDS: &[u8] = b"info HEAD\ncontents HEAD\nflush\n";
/// An `--index-info` line that **removes** a path: mode 0 and the null oid.
///
/// [`P_INDEX_INFO`] adds an entry, so the deletion half of the same reader was
/// unmeasured. Verified: this drops `README.md` from the index, which the state
/// probe's `ls-files --stage` reports.
const P_INDEX_INFO_DELETE: &[u8] =
    b"0 0000000000000000000000000000000000000000\tREADME.md\n";
/// A tag object body that is well formed and names an object the repository does
/// not have.
///
/// `mktree` has had a shaped payload since this pool existed; `mktag` never did,
/// so it drew from the whole pool and met a real tag header once in fifteen
/// draws. The empty blob's id is a hash-function constant rather than a fixture
/// value, so this reaches the *object lookup* after the header parse succeeded:
/// stock 2.55.0 answers `fatal: could not read tagged object
/// 'e69de29bb2d1d6434b8b29ae775ad8c2e48c5391'` with rc 128.
const P_MKTAG: &[u8] = b"object e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\ntype blob\ntag gen\ntagger zvcs parity <parity@example.invalid> 1700000000 +0000\n\nmsg\n";
/// The same four headers in the wrong order.
///
/// The other half of `mktag`: the fsck check that runs *before* any lookup.
/// Verified — `error: tag input does not pass fsck: missingObject: invalid
/// format - expected 'object' line`, then
/// `fatal: tag on stdin did not pass our strict fsck check`, rc 128. A port that
/// parses headers by name rather than by position accepts this.
const P_MKTAG_BAD_ORDER: &[u8] = b"type blob\nobject e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\ntag gen\ntagger zvcs parity <parity@example.invalid> 1700000000 +0000\n\nmsg\n";
/// Four `<tree-ish>:<path>` requests that name symlinks, for `cat-file
/// --batch-check --follow-symlinks`.
///
/// The payload that makes a *flag already in the pool* do something. Every
/// request line here is a path in [`Shape::Symlinks`] and nothing but a `missing`
/// on every other shape, and the four are one per answer `--follow-symlinks`
/// has. Verified against stock 2.55.0 on `Symlinks`, with the four lines in this
/// order:
///
///     --batch-check                  four blobs, the symlink blobs themselves
///     --batch-check --follow-symlinks
///         9741694d… blob 10          link-to-file, resolved to README.md
///         dangling 16 / HEAD:link-broken
///         symlink 14 / ../outside.txt
///         2ceb84c5… blob 15          reached *through* the symlinked directory
///
/// and on `Branched` all four lines answer `missing` with rc 0 whether or not
/// the flag is given — which is what the flag measured before this shape was in
/// [`STORE_SHAPES`]. No object id here is a fixture value: the paths are
/// literals and the ids above are what the reader prints, not what it is asked.
const P_SYMLINK_SPECS: &[u8] =
    b"HEAD:link-to-file\nHEAD:link-broken\nHEAD:link-escape\nHEAD:dir/link-up\n";

/// A complete `fast-import` stream: one blob, one root commit on
/// `refs/heads/gen-fi`, terminated by `done`.
///
/// `fast-import` is the one command in this harness whose *entire* input is a
/// language, and until this payload existed it was handed nothing at all — it
/// was not in [`STDIN_ALWAYS`] and has no `--stdin` flag, so every generated
/// `fast-import` case read EOF, imported no commands and exited 0. Its argv
/// parser was measured and its stream parser was not.
///
/// Every field is a literal so the result is a function of the bytes and not of
/// the repository: the commit has no `from`, so it is a root commit whose id
/// does not depend on the fixture's history, and both identities carry the
/// timestamp inline rather than inheriting `env::harden`'s pins. Verified
/// against stock 2.55.0 by importing it into two independently built
/// repositories: `rev-parse refs/heads/gen-fi` answered
/// `c53a4fafd7058d39286733184a6a6fa0bc3aef81` in both. The statistics block
/// (`Alloc'd objects`, `Memory total`) is written to **stderr**, which generated
/// cases do not compare, so the machine-dependent half of the output is not in
/// the comparison.
const P_FAST_IMPORT: &[u8] = b"blob\nmark :1\ndata 12\ngen import\n\ncommit refs/heads/gen-fi\nmark :2\nauthor zvcs parity <parity@example.invalid> 1700000000 +0000\ncommitter zvcs parity <parity@example.invalid> 1700000000 +0000\ndata 11\ngen commit\nM 100644 :1 gen.txt\n\ndone\n";
/// The same stream with a dangling mark.
///
/// The reject path, reached *after* the command parser has accepted every line:
/// stock 2.55.0 answers `fatal: mark :99 not declared` and dumps a crash report
/// to `.git/fast_import_crash_<pid>` — a filename carrying the process id, which
/// is why it matters that `runner::probe_op_state` names the state files it
/// reads instead of walking `.git`, and why this payload is safe to draw even
/// though the two sides write differently-named reports.
const P_FAST_IMPORT_BAD: &[u8] = b"commit refs/heads/gen-fi-bad\ncommitter zvcs parity <parity@example.invalid> 1700000000 +0000\ndata 3\nbad\nM 100644 :99 f.txt\n\ndone\n";

/// Every payload, as one pool. Sampling draws an **index** into this table
/// rather than generating bytes, so a case's input is a compile-time literal and
/// the case replays byte-for-byte from its seed.
const STDIN_PAYLOADS: &[&[u8]] = &[
    P_TREE_ENTRY,
    P_TREE_ENTRY_BAD,
    P_TRAILERS,
    P_PATCH,
    P_PATCH_TRUNCATED,
    P_REF_UPDATES,
    P_REF_UPDATES_FAIL,
    P_OIDS,
    P_PATHS,
    P_PATHS_NUL,
    P_PATHS_CRLF,
    P_PATH_NO_EOL,
    P_INDEX_INFO,
    P_EMPTY,
    P_BINARY,
    P_STRIPSPACE_MESSY,
    P_TRAILERS_NO_BLOCK,
    P_PATCH_CREATE,
    P_PATCH_CRLF,
    P_REF_UPDATES_NUL,
    P_REF_TRANSACTION,
    P_BATCH_COMMANDS,
    P_INDEX_INFO_DELETE,
    P_MKTAG,
    P_MKTAG_BAD_ORDER,
    P_FAST_IMPORT,
    P_FAST_IMPORT_BAD,
];

/// The payloads that are the *right shape* for a given subcommand.
///
/// Without this the pool is fifteen payloads wide and `mktree` would see a real
/// tree entry once in fifteen draws, so its parse path would be measured a
/// fifteenth as often as its reject path. The sampler prefers this list and
/// falls back to the whole pool on a minority of draws, so both are reached.
fn preferred_payloads(cmd: &str) -> &'static [&'static [u8]] {
    match cmd {
        "mktree" => &[P_TREE_ENTRY, P_TREE_ENTRY_BAD],
        // `mktag` used to fall through to the whole pool, which meant its header
        // parser and its object lookup were each reached about a fifteenth as
        // often as its rejection of arbitrary bytes. Two bodies now separate the
        // two failures: one that passes fsck and misses the object, one that
        // fails fsck before any lookup.
        "mktag" => &[P_MKTAG, P_MKTAG_BAD_ORDER, P_BINARY],
        "interpret-trailers" | "stripspace" | "mailinfo" | "mailsplit" | "fmt-merge-msg" => {
            &[P_TRAILERS, P_TRAILERS_NO_BLOCK, P_STRIPSPACE_MESSY, P_PATCH, P_EMPTY]
        }
        "patch-id" | "apply" | "am" => {
            &[P_PATCH, P_PATCH_TRUNCATED, P_PATCH_CREATE, P_PATCH_CRLF, P_EMPTY]
        }
        "update-ref" => {
            &[P_REF_UPDATES, P_REF_UPDATES_FAIL, P_REF_UPDATES_NUL, P_REF_TRANSACTION]
        }
        // `cat-file` is split out of the group below rather than added to it:
        // [`P_SYMLINK_SPECS`] is a request stream for one flag of one command,
        // and handing it to `rev-list --stdin` would be a `fatal: not a commit`
        // in a fifth of that command's draws. The four it shared are all still
        // here.
        "cat-file" => &[P_OIDS, P_BATCH_COMMANDS, P_PATHS, P_EMPTY, P_SYMLINK_SPECS],
        "rev-list" | "diff-tree" | "name-rev" | "pack-objects" | "for-each-ref" => {
            &[P_OIDS, P_BATCH_COMMANDS, P_PATHS, P_EMPTY]
        }
        "check-ignore" | "check-attr" | "check-mailmap" | "hash-object" | "ls-files" => {
            &[P_PATHS, P_PATHS_NUL, P_PATHS_CRLF, P_PATH_NO_EOL]
        }
        "update-index" => &[P_INDEX_INFO, P_INDEX_INFO_DELETE, P_PATHS, P_PATHS_NUL],
        // Two streams and an empty one. The empty stream is the *third* outcome
        // and not padding: verified against stock 2.55.0 in a fresh repository,
        // `fast-import --quiet < /dev/null` exits 0 and leaves `.git/objects`
        // with the same zero files it started with — no pack, no ref. That is
        // precisely what every `fast-import` case did before this command read a
        // payload at all, so keeping it in the pool keeps the old behaviour
        // reachable beside the two that are new.
        "fast-import" => &[P_FAST_IMPORT, P_FAST_IMPORT_BAD, P_EMPTY],
        "unpack-objects" | "index-pack" | "show-index" | "get-tar-commit-id" => {
            &[P_BINARY, P_EMPTY]
        }
        _ => STDIN_PAYLOADS,
    }
}

/// Subcommands whose *entire* input is stdin, with no flag to ask for it.
///
/// Enumerated because there is no way to derive it: `git mktree` reads stdin
/// unconditionally while `git hash-object` reads it only under `--stdin`, and
/// the difference is in each command's source, not in its argv.
const STDIN_ALWAYS: &[&str] = &[
    "mktree",
    "mktag",
    "stripspace",
    "patch-id",
    "interpret-trailers",
    "fmt-merge-msg",
    "mailinfo",
    "mailsplit",
    "unpack-objects",
    "get-tar-commit-id",
    "show-index",
    "column",
    // `apply` and `am` read stdin when they are given no file operand; when they
    // are given one, they ignore it. Both sides ignore it identically, so
    // supplying it unconditionally costs nothing and covers the no-operand form.
    "apply",
    "am",
    // `fast-import` takes no file operand at all — its whole input is the stream
    // on stdin (`builtin/fast-import.c` reads it with no flag to ask for it), so
    // it belongs here for the same reason `mktree` does. It was missing, and the
    // consequence was not a narrower id but a command that did nothing: with
    // stdin closed it read EOF, imported no commands, wrote nothing and exited
    // 0, so every case scored on the flag table alone. See [`P_FAST_IMPORT`].
    "fast-import",
];

/// Whether the sampled invocation actually asks for input.
///
/// Two rules, and no guessing beyond them:
///
///  * the subcommand is one of [`STDIN_ALWAYS`], which read stdin with no flag;
///  * **or** the sampled argv contains a token that means "read stdin" —
///    `--stdin` and its `--stdin-paths`/`--stdin-packs` relatives, the bare `-`
///    operand, `--annotate-stdin`, `--index-info`, the `--batch` family that
///    `cat-file` drives from a request stream, and the `…-from-file=-` forms
///    that name stdin as a file.
///
/// Anything else gets closed stdin, which is what every generated case had
/// before. Feeding a payload to a command that does not read one would not make
/// the case wrong — both sides ignore it — but it would put a payload hash in
/// the case id that means nothing, and an id that lies about what a case does is
/// worse than a narrower rule.
fn wants_stdin(cmd: &str, args: &[String]) -> bool {
    STDIN_ALWAYS.contains(&cmd)
        || args.iter().any(|a| {
            a == "-"
                || a == "--stdin"
                || a.starts_with("--stdin=")
                || a.starts_with("--stdin-")
                || a == "--annotate-stdin"
                || a == "--index-info"
                || a.starts_with("--batch")
                || a.ends_with("-from-file=-")
        })
}

/// The hand-written grammars: the commands whose flag sets are worth stating
/// deliberately rather than deriving from a manual page.
///
/// Two kinds, in that order. The first twelve are commands with **no** generated
/// grammar, described here because they have no other description. The last five
/// **shadow** a generated grammar for a verb whose mutating half the derivation
/// could not reach; the block comment above them says why, and what that costs.
///
/// These are **not** the whole fuzz corpus — [`all_grammars`] concatenates
/// [`crate::grammars_generated::generated`], which covers a hundred-odd more
/// commands including mutating ones (`init`, `cherry-pick`, `rebase`, `revert`,
/// `submodule`, `gc`, `repack`, …). Read-only was once the rule for both halves:
/// fuzzing a mutating command used to hang on an editor or a prompt, so the
/// corpus carried read-only grammars only. `env::harden` closed that by
/// neutralizing every interactive hook, and the generated grammars followed —
/// but only per *command*, and a subcommand-dispatching verb keeps the old shape
/// inside a grammar that no longer claims it. That is the gap the last five
/// entries close, and it is worth remembering the next time this file explains
/// what the harness does not do.
pub fn grammars() -> Vec<Grammar> {
    vec![
        // `rev-parse` is two commands wearing one name — a rev resolver and the
        // query surface every script uses to find out where it is — and the
        // second half was represented by three flags. The additions below are
        // the ones that change a *decision* rather than a string: the four
        // `--is-*` predicates (each a different question `setup.c` answers),
        // `--path-format=` (which re-renders every path-valued query and is a
        // mode, not an option), `--sq`/`--sq-quote` (a quoting engine of its
        // own), `--disambiguate=` (the short-oid walk the `abbrev-disambiguate`
        // config group configures and nothing invoked), the `--glob=`/
        // `--exclude=` ref filters, and the `--flags`/`--revs-only`/`--no-revs`
        // partition of an argument list into two kinds.
        //
        // `--parseopt` is deliberately absent: it reads a specification from
        // stdin, and [`wants_stdin`] does not know that, so a case drawing it
        // would carry an id that says nothing about the input the command
        // actually consumed.
        //
        // `--since=`/`--until=` are absent for a harder reason: `rev-parse`
        // *prints* what the date parsed to, and `approxidate` fills a date with
        // no time-of-day in from the **current clock**. Verified against stock
        // 2.55.0 — two runs of `rev-parse --since=2020-01-01 HEAD` two seconds
        // apart printed `--max-age=1577851712` and `--max-age=1577851713` — so
        // any case drawing one would diverge from itself, let alone across two
        // sides run one after the other. `log`/`rev-list` keep the same options
        // because there the parsed date only filters a walk that has nothing in
        // that window either way.
        Grammar {
            cmd: "rev-parse",
            flags: &[
                "--abbrev-ref", "--short", "--verify", "--quiet", "--git-dir",
                "--show-toplevel", "--is-inside-work-tree", "--is-bare-repository",
                "--symbolic", "--symbolic-full-name", "--all", "--branches", "--tags",
                "--abbrev-ref=strict", "--abbrev-ref=loose", "--short=4", "--short=40",
                "--is-inside-git-dir", "--is-shallow-repository", "--show-object-format",
                "--show-prefix", "--show-cdup", "--git-common-dir", "--shared-index-path",
                "--show-superproject-working-tree", "--resolve-git-dir=.git",
                "--path-format=relative", "--path-format=absolute",
                "--sq", "--sq-quote", "--end-of-options", "--local-env-vars",
                "--disambiguate=e69de29", "--disambiguate=dead", "--default=HEAD",
                "--glob=refs/heads/*", "--exclude=refs/tags/*", "--remotes",
                "--no-revs", "--revs-only", "--flags",
            ],
            positionals: REVS,
            shapes: STORE_SHAPES,
        },
        // `--porcelain=v2` is a different serializer from v1, not a variant of
        // it — it prints per-entry mode and oid triples nothing in v1 carries —
        // and it was absent while `runner::probe_state` used it, so the format
        // the harness trusts to describe a repository was never itself compared.
        // `-z` is the third serializer again (no quoting, NUL records).
        // `--ignored=` takes a mode where the bare `--ignored` takes none, and
        // `--ignore-submodules=` and `--no-ahead-behind` each remove a whole
        // walk from the answer. `status` also takes pathspecs, which it did not
        // draw at all: the porcelain that reads a pathspec is the one most
        // callers write.
        Grammar {
            cmd: "status",
            flags: &[
                "--porcelain", "--porcelain=v1", "--short", "--branch", "--long",
                "--untracked-files=all", "--untracked-files=no", "--untracked-files=normal",
                "--ignored", "--no-renames", "--find-renames",
                "--porcelain=v2", "--porcelain=bogus", "-z", "--null",
                "--ignored=matching", "--ignored=traditional", "--ignored=no",
                "--ignore-submodules=all", "--ignore-submodules=dirty",
                "--ignore-submodules=untracked", "--ignore-submodules=none",
                "--ahead-behind", "--no-ahead-behind", "--show-stash", "--no-column",
                "--column", "--renames", "--find-renames=50%", "-v", "-vv",
                "--untracked-files", "--no-untracked-files",
            ],
            positionals: PATHS,
            shapes: INDEX_SHAPES,
        },
        // The flag list here described what `log` *prints*. What it selects — a
        // different engine each time — was almost entirely absent: history
        // simplification (`--full-history`, `--simplify-merges`,
        // `--ancestry-path`, `--max-parents=`/`--min-parents=`), the grep engine
        // and its four mutually-qualifying modifiers, the reflog walk
        // (`-g`/`--walk-reflogs`/`--reflog`, which reads a store the commit walk
        // never touches), line-level history (`-L`, an engine of its own), and
        // `--diff-merges=`/`--remerge-diff`, which decide what a merge commit
        // even shows. Each of those changes which commits come out, not how they
        // are spelled, and a port can render every format correctly while
        // walking the wrong set.
        //
        // The positional list gains the rev pool for the same reason: `log`
        // resolving one of three names measured the walk and never the parser
        // that feeds it.
        Grammar {
            cmd: "log",
            flags: &[
                "--oneline", "-1", "-2", "--max-count=3", "--format=%H", "--format=%h %s",
                "--pretty=oneline", "--pretty=short", "--pretty=format:%an", "--name-only",
                "--name-status", "--stat", "--graph", "--all", "--reverse", "--no-merges",
                "--merges", "--date-order", "--topo-order",
                "-p", "--patch", "--no-patch", "--first-parent", "--full-history",
                "--simplify-merges", "--ancestry-path", "--boundary", "--left-right",
                "--cherry-mark", "--children", "--source", "--skip=1",
                "--max-parents=1", "--min-parents=2", "--follow",
                "-g", "--walk-reflogs", "--reflog",
                "--grep=fixture", "--grep=two", "--all-match", "--invert-grep",
                "--regexp-ignore-case", "--extended-regexp", "--fixed-strings",
                "--author=parity", "--committer=parity",
                "--since=2020-01-01", "--until=2030-01-01",
                "--diff-merges=first-parent", "--diff-merges=separate",
                "--diff-merges=bogus", "--remerge-diff",
                "-L1,1:src/lib.rs", "-L2,+1:README.md",
                "--decorate=full", "--decorate=short", "--no-decorate",
                "--abbrev-commit", "--no-abbrev-commit", "--date=raw", "--date=iso",
                "--date=format:%Y-%m-%d", "--date=bogus",
                "--pretty=fuller", "--pretty=raw", "--pretty=%(bogus)",
                "--notes", "--no-notes", "--full-diff", "-w", "-M", "-C",
                "--word-diff", "--stat=20",
            ],
            positionals: REVS,
            shapes: REV_SHAPES,
        },
        // `rev-list` is the walk `log` renders, so its flag list is where the
        // walk's *modes* belong: `--bisect`/`--bisect-vars`/`--bisect-all` are a
        // separate midpoint search (`git bisect` is built on them, and the
        // `gen/bisect` walks drive that machine from the porcelain side while
        // nothing drove it from here), `--filter=` is the partial-clone object
        // filter with its own spec grammar, `--no-walk`/`--do-walk` turn the walk
        // off and on, and `--disk-usage` reports a number derived from the object
        // store rather than from the walk at all. `--stdin` is here because it
        // makes [`wants_stdin`] fire and hands the same command its revisions
        // through a second parser.
        Grammar {
            cmd: "rev-list",
            flags: &[
                "--count", "--max-count=2", "--all", "--reverse", "--no-merges",
                "--merges", "--objects", "--parents", "--topo-order",
                "--first-parent", "--left-right", "--boundary", "--cherry-mark",
                "--cherry-pick", "--ancestry-path", "--children", "--header",
                "--timestamp", "--quiet", "--no-walk", "--do-walk", "--in-commit-order",
                "--bisect", "--bisect-vars", "--bisect-all",
                "--filter=blob:none", "--filter=tree:0", "--filter=blob:limit=1k",
                "--filter=bogus", "--filter-provided-objects", "--no-filter",
                "--missing=allow-any", "--missing=print", "--missing=bogus",
                "--exclude-promisor-objects", "--object-names", "--no-object-names",
                "--disk-usage", "--max-parents=1", "--min-parents=2",
                "--branches", "--tags", "--remotes", "--reflog", "--not",
                "--abbrev-commit", "--abbrev=7", "--pretty=oneline", "--stdin",
            ],
            positionals: REVS,
            shapes: REV_SHAPES,
        },
        // Four single-object queries described a command whose other half is a
        // *request stream*. The `--batch` family reads object names (or, under
        // `--batch-command`, commands) from stdin and answers each one, which is
        // a different loop, a different error policy and — with `--buffer` — a
        // different flush discipline; [`wants_stdin`] already routes a payload to
        // anything spelling `--batch`, so these flags are what makes that rule
        // fire for this command. `--textconv`/`--filters` run the content through
        // the attribute-driven conversion stack, `--allow-unknown-type` relaxes
        // the type check `-t` enforces, and `--batch-all-objects` walks the store
        // instead of taking an argument at all.
        Grammar {
            cmd: "cat-file",
            flags: &[
                "-t", "-s", "-p", "-e",
                "--batch", "--batch-check", "--batch-command", "--buffer",
                "--batch-check=%(objecttype) %(objectsize)", "--batch=%(objectname)",
                "--batch-all-objects", "--unordered",
                "--allow-unknown-type", "--textconv", "--filters", "--path=README.md",
                "--use-mailmap", "--follow-symlinks",
            ],
            positionals: REVS,
            shapes: STORE_SHAPES,
        },
        // `ls-tree` takes `<tree> [<path>…]`, and the path half was missing: the
        // pathspec restricts which entries are listed and is what turns the
        // command into a lookup rather than a dump. `--format=` is a whole
        // template language (`ref-filter`'s atoms) that the fixed layouts above
        // never reach, and `--long`/`--object-only` are two more layouts again.
        Grammar {
            cmd: "ls-tree",
            flags: &[
                "-r", "-t", "-d", "--name-only", "--name-status", "--full-tree", "--abbrev=7", "-z",
                "-l", "--long", "--object-only", "--full-name", "--abbrev",
                "--format=%(objectname) %(path)", "--format=%(objectmode) %(objecttype)",
                "--format=%(bogus)",
            ],
            positionals: &[
                "HEAD", "HEAD^{tree}", "main", "src", "src/", "README.md", "*.rs",
                ":(glob)**/*.rs", "no/such/path",
                // A ref that resolves and names no object, which only
                // [`Shape::Damaged`] has and which this command answers its own
                // way: verified against stock 2.55.0, `ls-tree
                // refs/heads/dangling` is `fatal: not a tree object` with rc 128
                // — not the `fatal: ambiguous argument` every other missing name
                // in this pool produces, because the name resolved and the
                // *lookup* is what failed.
                "refs/heads/dangling",
            ],
            shapes: STORE_SHAPES,
        },
        // The selectors were here; the things that change what a selection
        // *means* were not. `--error-unmatch` turns a silent empty answer into
        // rc 1 — the only flag in this command that moves the exit code, and the
        // one a script depends on. `--exclude-standard`/`--exclude=`/`-i` bring
        // the ignore machinery into a command that otherwise never consults it,
        // `--directory` collapses an untracked tree into one entry,
        // `--with-tree=` mixes a tree into the index listing, and `-t`/`-v` /
        // `--eol` are three more layouts — `-v` being the one that shows the
        // assume-unchanged and skip-worktree bits, which no other reader in this
        // harness prints.
        //
        // `--debug` is deliberately absent: it prints device, inode and
        // timestamps, which differ between two copies on the same machine and
        // would make every case that drew it fail for a reason that is not the
        // port.
        Grammar {
            cmd: "ls-files",
            flags: &[
                "--cached", "--stage", "--modified", "--deleted", "--others",
                "--unmerged", "--full-name", "-z", "--abbrev",
                "--error-unmatch", "--exclude-standard", "--ignored", "-i",
                "--exclude=*.md", "--exclude=src", "--directory", "--no-empty-directory",
                "--eol", "-t", "-v", "-f", "--deduplicate", "--sparse",
                "--resolve-undo", "--with-tree=HEAD", "--with-tree=main",
                "--recurse-submodules", "--abbrev=7",
                "--format=%(path) %(objectmode)", "--format=%(bogus)",
            ],
            positionals: PATHS,
            shapes: INDEX_SHAPES,
        },
        // The single most under-described grammar in this file, and the one whose
        // options are shared with `log`, `show`, `format-patch` and every other
        // verb that renders a diff — so a gap here was a gap in a dozen commands
        // at once.
        //
        // `--exit-code`/`--quiet` are the reason this list needed widening most:
        // they are the only diff options that change the **exit code** (verified
        // against stock 2.55.0: rc 1 where the same invocation without them is
        // rc 0), which makes them the one part of the diff surface a port cannot
        // get right by rendering correctly. After that come the engine choices —
        // `--diff-algorithm=`, `--anchored=`, `--indent-heuristic`, the four
        // whitespace-ignoring modes, `-B`/`-M<n>%`/`--find-copies-harder` — the
        // pickaxe (`-S`/`-G`/`--find-object=`, a content search, not a
        // renderer), the prefix controls the `diff-prefix` config group
        // configures from the other side, and `--no-index`, which compares two
        // paths with no repository involved at all.
        Grammar {
            cmd: "diff",
            flags: &[
                "--cached", "--staged", "--stat", "--shortstat", "--numstat",
                "--name-only", "--name-status", "--raw", "--no-color", "--unified=1",
                "--ignore-all-space", "--find-renames",
                "--exit-code", "--quiet",
                "-p", "--patch", "--no-patch", "-s", "--stat=20", "--compact-summary",
                "--summary", "--patch-with-stat", "--patch-with-raw",
                "--diff-filter=AMD", "--diff-filter=bogus",
                "-M50%", "-B", "-C", "--find-copies-harder", "--irreversible-delete",
                "--binary", "--full-index", "--abbrev=7", "-R",
                "--src-prefix=X/", "--dst-prefix=Y/", "--no-prefix", "--default-prefix",
                "--relative", "--relative=src", "--text", "-a",
                "-b", "-w", "--ignore-space-at-eol", "--ignore-blank-lines",
                "--ignore-cr-at-eol", "--ignore-matching-lines=fn",
                "--inter-hunk-context=2", "--function-context", "-U0",
                "--word-diff", "--word-diff=porcelain", "--word-diff-regex=.",
                "--color-words", "--color-moved", "--color-moved=zebra",
                "--color-moved-ws=allow-indentation-change", "--ws-error-highlight=all",
                "--check", "--anchored=fn", "--diff-algorithm=histogram",
                "--diff-algorithm=bogus", "--minimal", "--patience", "--histogram",
                "--indent-heuristic", "--no-indent-heuristic",
                "--textconv", "--no-textconv", "--ext-diff", "--no-ext-diff",
                "--output-indicator-new=>", "--line-prefix=| ",
                "-Sfn", "-Gfn", "--pickaxe-all", "--pickaxe-regex", "--find-object=HEAD",
                "--merge-base", "--cc", "--combined-all-paths", "--no-index",
            ],
            positionals: &["", "HEAD", "HEAD~1", "main", "main..HEAD", "README.md", "src"],
            shapes: INDEX_SHAPES,
        },
        // `show` dispatches on the *type* of what it is given — a commit, a tag,
        // a tree and a blob each take a different renderer — so the rev pool is
        // already doing most of the work here. What was missing is the choice of
        // renderer for the commit case: `--diff-merges=`/`--remerge-diff` decide
        // whether a merge shows anything at all, `--expand-tabs=` and
        // `--encoding=` re-render the message body, and `-s`/`--quiet` suppress
        // the diff the other flags configure.
        Grammar {
            cmd: "show",
            flags: &[
                "--oneline", "--no-patch", "--stat", "--name-only", "--format=%H", "--raw",
                "-s", "--quiet", "--patch", "--unified=0", "--numstat", "--shortstat",
                "--pretty=fuller", "--pretty=raw", "--format=%(bogus)",
                "--expand-tabs=4", "--no-expand-tabs", "--encoding=UTF-8",
                "--encoding=ISO-8859-1", "--first-parent",
                "--diff-merges=on", "--diff-merges=first-parent", "--diff-merges=bogus",
                "--remerge-diff", "--textconv", "--no-textconv",
                "--notes", "--no-notes", "--abbrev-commit", "--decorate", "--no-decorate",
                "--word-diff", "--color-moved", "--find-renames", "--name-status",
            ],
            positionals: REVS,
            shapes: REV_SHAPES,
        },
        // `branch --list` is a `ref-filter` front end, and none of the filter was
        // here: `--contains=`/`--no-contains=`/`--merged`/`--no-merged`/
        // `--points-at=` each run a reachability query to decide what to print,
        // and `--sort=` runs the same field parser `tag.sort`/`branch.sort` do
        // (verified: an unknown field is `fatal: unknown field name: bogus`,
        // rc 128, from the flag exactly as from the config key). The positional
        // was one empty string, so the pattern argument — the thing that decides
        // *which* branches a list names — was never given.
        Grammar {
            cmd: "branch",
            flags: &[
                "--list", "-a", "-r", "-v", "-vv", "--show-current", "--all", "--format=%(refname)",
                "--contains=HEAD", "--no-contains=HEAD", "--merged", "--no-merged",
                "--points-at=HEAD", "--points-at=does-not-exist",
                "--sort=refname", "--sort=-committerdate", "--sort=bogus",
                "--column", "--no-column", "--ignore-case", "-i", "--omit-empty",
                "--abbrev=7", "--no-abbrev", "-q",
                "--format=%(refname:short) %(upstream:track)", "--format=%(bogus)",
            ],
            positionals: &["", "main", "feature", "*e*", "feature*", "no-such-branch"],
            shapes: ALL_SHAPES,
        },
        // The same `ref-filter` surface again, over a different ref namespace and
        // with the peeling `branch` does not do — `%(objecttype)` on a tag is
        // `tag` for the annotated one and `commit` for the lightweight one, which
        // is a distinction only `Shape::Branched` carries and only a format
        // string shows. `-n<n>` prints that many lines of the annotation, which
        // is the one output here that reads the tag object's body.
        Grammar {
            cmd: "tag",
            flags: &[
                "--list", "-l", "-n", "--sort=refname", "--format=%(refname:short)",
                "-n1", "-n99", "--contains=HEAD", "--no-contains=HEAD",
                "--merged", "--no-merged", "--points-at=HEAD",
                "--sort=-refname", "--sort=version:refname", "--sort=taggerdate",
                "--sort=bogus", "--ignore-case", "-i", "--column", "--omit-empty",
                "--format=%(objecttype) %(refname)", "--format=%(bogus)",
            ],
            positionals: &[
                "", "v0.*", "V0.*", "v0.1.0", "*", "no-such-tag",
                // The six names [`Shape::TagChain`] adds. Four of them do not
                // peel to a commit in one step — `outer` and `outermost` point
                // at another tag object, `blobtag` at a blob and `treetag` at a
                // tree — which is a target this harness has never had.
                // `%(objecttype) %(*objecttype)` separates them: verified
                // against stock 2.55.0 on that shape,
                // `tag --list --format='%(refname:short) %(objecttype)
                // %(*objecttype)'` prints `blobtag tag blob`, `treetag tag
                // tree` and `commit` for the other four, so the deref atom is
                // answering about three different target types instead of
                // printing `commit` six times.
                "outermost", "outer", "inner", "light-to-tag", "blobtag", "treetag",
            ],
            shapes: &[Shape::Branched, Shape::Linear, Shape::TagChain],
        },
        // `describe` is a search with a budget, and the budget was not sampled.
        // `--candidates=` bounds how many tags it will consider — verified:
        // `--candidates=0` turns a successful describe into
        // `fatal: no tag exactly matches …` — `--match=`/`--exclude=` change the
        // candidate set itself, and `--exact-match`/`--contains` are two
        // different searches from the default one. `--abbrev=0` drops the
        // suffix entirely, which is the answer scripts parse.
        Grammar {
            cmd: "describe",
            flags: &[
                "--always", "--tags", "--all", "--long", "--abbrev=7", "--dirty",
                "--exact-match", "--contains", "--first-parent", "--broken",
                "--match=v0.*", "--match=no-such*", "--exclude=v0.2*",
                "--candidates=0", "--candidates=1", "--candidates=99",
                "--abbrev=0", "--abbrev=40", "--dirty=-X", "--broken=-B", "--debug",
            ],
            positionals: &[
                "", "HEAD", "main", "HEAD~1", "does-not-exist",
                // `describe` peels whatever it is given before it searches, and
                // on [`Shape::TagChain`] that peel is three deep and sometimes
                // does not end at a commit at all. Verified against stock
                // 2.55.0 on that shape: `describe` is `inner-2-g725c7d5` — the
                // distance is counted from a tag reached through two other tag
                // objects — `describe outermost` is `inner`, and
                // `describe blobtag` is `fatal: blobtag is neither a commit nor
                // blob` with rc 128, which is a refusal `describe`'s own
                // documentation does not obviously predict for a tag that peels
                // to a blob.
                "outermost", "light-to-tag", "blobtag", "treetag",
            ],
            shapes: &[Shape::Branched, Shape::Linear, Shape::Dirty, Shape::TagChain],
        },
        // `config` is the one command in this file that *reads the thing the
        // whole configuration dimension writes*, so it is where a scope or a
        // parse the rest of the harness delivered can be asked about directly.
        // `--show-origin`/`--show-scope` name where a value came from, which is
        // the exact fact [`ConfigScope`] exists to vary and which nothing could
        // previously print; `--type=` runs a named converter over the stored
        // string (verified: `--type=expiry-date core.bare` answers `0` rather
        // than refusing, which is not the obvious behaviour); `--default=`
        // supplies an answer for a key that is absent; and `--get-color`/
        // `--get-colorbool` are two more converters again, over the vocabulary
        // the `color-cascade` group writes.
        //
        // The shape stays `Linear`: `config` answers about a file, and the other
        // shapes differ in history rather than in configuration.
        Grammar {
            cmd: "config",
            flags: &[
                "--list", "--get", "--get-all", "--local", "--name-only",
                "--get-regexp", "--get-urlmatch", "--show-origin", "--show-scope",
                "--global", "--system", "--worktree", "--includes", "--no-includes",
                "--type=bool", "--type=int", "--type=path", "--type=expiry-date",
                "--type=color", "--type=bool-or-int", "--type=bogus",
                "--bool", "--int", "--path", "--null", "-z",
                "--default=X", "--get-color", "--get-colorbool", "--fixed-value",
                "--unset", "--unset-all", "--remove-section", "--rename-section",
            ],
            positionals: &[
                "core.bare", "user.name", "no.such.key", "core.abbrev",
                "diff.renames", "core.repositoryformatversion",
                "core\\..*", "no-section", "core.", ".key",
            ],
            shapes: &[Shape::Linear],
        },
        // Five flags described a command whose whole subject is *which commit a
        // line came from*. What decides that was absent: `-M`/`-C` follow a line
        // across a move within and between files, `-w` ignores whitespace when
        // deciding whether a line changed, `--ignore-rev`/`--ignore-revs-file`
        // skip a commit and reattribute its lines (and the second is the flag
        // spelling of `blame.ignoreRevsFile` in the `blame-render` group —
        // verified to die identically: `fatal: could not open object name list:
        // no-such`), and `-L` restricts the answer to a line range in three
        // different syntaxes, one of which is a regex.
        //
        // `--incremental` is a machine-readable protocol rather than a layout,
        // and `-t` prints the raw timestamp — both deterministic here because
        // `env::harden` pins the clock.
        Grammar {
            cmd: "blame",
            flags: &[
                "--porcelain", "--line-porcelain", "-s", "-l", "--show-name",
                "--incremental", "-c", "-t", "-f", "-n", "-e", "--root",
                "-w", "-M", "-C", "--minimal", "--first-parent", "--score-debug",
                "-L1,1", "-L2,+1", "-L/one/,+1", "-L99,100",
                "--abbrev=7", "--abbrev=40", "--date=iso", "--date=raw", "--date=bogus",
                "--ignore-rev=HEAD", "--ignore-rev=does-not-exist",
                "--ignore-revs-file=no-such", "--ignore-revs-file=.git/HEAD",
                "--color-lines", "--color-by-age",
            ],
            positionals: &["README.md", "src/lib.rs", "no/such/path", "src"],
            shapes: &[Shape::Linear, Shape::Branched],
        },
        // ------------------------------------------------------------------
        // The five verbs below are written by hand for a different reason from
        // the twelve above, and it is worth stating once rather than five times.
        //
        // Each of them already has a generated grammar, so each costs one extra
        // grammar's worth of cases rather than opening a command that had none.
        // What they buy is that the *mutating* half of the verb becomes
        // reachable at all. `grammars_generated.rs` is derived from git's
        // documentation and its header says what it is — "read-only ones only" —
        // which for a subcommand-dispatching verb means the pool of positionals
        // holds `list` and no `add`. All five are named in [`MUTATORS`], and
        // [`grammar_for`] answers with the first grammar carrying the name, so
        // a hand-written entry here is also what the `gen/observe/<verb>`
        // sequences draw from: before this, `gen/observe/worktree` could draw
        // `worktree list` and nothing else, and then ran observers over a
        // repository that nothing had written to.
        //
        // Two mechanical defects in the generated pools are worth recording so
        // that a future regeneration is measured against them rather than
        // trusted. A grammar's positional is one **argv token**
        // ([`sample_argv`] pushes it whole), so a pool entry spelling two words
        // is one operand containing a space. Verified against stock 2.55.0:
        //
        //   `git sparse-checkout 'set src'` -> `error: unknown subcommand: `set src'`, rc 129
        //   `git reflog 'show HEAD'`        -> `fatal: ambiguous argument 'show HEAD'`, rc 128
        //   `git symbolic-ref '-q --short'` -> `error: unknown switch ` '`, rc 129
        //
        // Every one of those is a usage error rather than the invocation it
        // reads as, and each of the three pools carries several of them:
        // `reflog`'s positionals spell its `show` and `exists` subcommands this
        // way and nothing else, `sparse-checkout`'s spell most of its
        // subcommands this way and nothing else, and `symbolic-ref`'s *flags*
        // are mostly multi-word combinations. The hand-written pools below spell
        // each token separately, which is also why they can be shorter than the
        // generated ones and still reach more: a two-word entry is one draw that
        // cannot dispatch.
        //
        // **None of these five verbs runs a program named by an operand**, and
        // that was checked per subcommand rather than assumed. It is the rule
        // [`BISECT_RUN_COMMANDS`] exists for: an operand drawn from a general
        // pool and then *executed* is how `bisect run HEAD` came to run
        // `/usr/bin/HEAD` and block until the case timeout. `notes` runs merge
        // strategies that are internal to `notes-merge.c`, `worktree` spawns
        // nothing, `reflog` and `symbolic-ref` take refs, and
        // `sparse-checkout`'s only file operand is `--rules-file=`, which is
        // read and not run. A verb that did execute an operand would need its
        // own pool here before it could be given a grammar at all.
        // ------------------------------------------------------------------

        // `notes` is a store with its own ref namespace, its own merge machine
        // and eleven subcommands, and the generated pool reaches three of them
        // (`list`, `show`, `get-ref`) — all readers. Nothing generated could add
        // a note, so `refs/notes/*` never moved and the flag half of the grammar
        // (`-m`, `-F`, `-C`, `--allow-empty`, `--separator=`, `--stripspace`)
        // was carried into invocations that reject flags before reading them.
        //
        // The value-taking short options are spelled **sticky** (`-mgen`,
        // `-CHEAD`) rather than bare, because a bare `-m` consumes whatever
        // positional the sampler drew next as its message and the case id then
        // reads as an operand that was never an operand. Verified against stock
        // 2.55.0: `notes add -mgen HEAD` writes the note (rc 0),
        // `notes append -mmore HEAD` appends a second paragraph, and
        // `notes add -CHEAD -f main` dies with `fatal: cannot read note data
        // from non-blob object 'HEAD'` — the reuse path reaching its type check.
        //
        // `--stdin` is here because it makes [`wants_stdin`] fire: `notes remove
        // --stdin` takes its object list from a payload, which is a second
        // parser for the same operand.
        Grammar {
            cmd: "notes",
            flags: &[
                "--ref=refs/notes/commits", "--ref=refs/notes/other", "--ref=other",
                "--ref=", "--no-ref", "-f", "--force", "--allow-empty",
                "--message=gen", "-mgen", "--file=README.md", "--file=does-not-exist",
                "--reuse-message=HEAD", "-CHEAD", "--reedit-message=HEAD",
                "--separator=;", "--separator", "--no-separator",
                "--stripspace", "--no-stripspace", "--ignore-missing", "--stdin",
                "-n", "--dry-run", "-q", "--quiet", "-v", "--verbose", "-e",
                "--strategy=ours", "--strategy=theirs", "--strategy=union",
                "--strategy=cat_sort_uniq", "--strategy=manual", "--strategy=bogus",
                "--commit", "--abort",
            ],
            positionals: &[
                "add", "append", "copy", "edit", "list", "merge", "prune", "remove",
                "show", "get-ref",
                "HEAD", "HEAD~1", "main", "feature", "other", "refs/notes/other",
                "does-not-exist", "",
            ],
            shapes: NOTES_SHAPES,
        },
        // `worktree` had exactly one reachable subcommand. The generated pool is
        // `list`, `wt`, `wt/README.md`, `linked`, `does-not-exist` and two
        // spellings of nothing, and its flags are `list`'s and `prune`'s — so
        // `add`, `remove`, `move`, `lock`, `unlock` and `repair` were reachable
        // only through the two curated round trips, and never with a drawn flag.
        //
        // Every flag below was read out of `git worktree <sub> -h` on stock
        // 2.55.0 rather than from the manual page, because the options are
        // per-subcommand and the top-level usage lists none of them:
        // `add` takes `-f/-b/-B/--orphan/-d/--checkout/--lock/--reason/-q/
        // --track/--guess-remote/--relative-paths`, `list` takes
        // `--porcelain/-v/--expire/-z`, `move` takes `-f/--relative-paths`,
        // `prune` takes `-n/-v/--expire`, `remove` takes `-f`, and `repair`
        // takes `--relative-paths`. Drawing them against the wrong subcommand is
        // deliberate — that is one of the two things a flat pool is good at —
        // but the set has to be the real one for the right pairings to happen at
        // all.
        //
        // The paths are all repository-relative and none of them climbs: a
        // `worktree add ..` would put an administrative file outside the
        // fixture root, which is the one place a case may not write.
        Grammar {
            cmd: "worktree",
            flags: &[
                "--porcelain", "-z", "-v", "--verbose", "-n", "--dry-run", "-q", "--quiet",
                "--expire=now", "--expire=never", "--expire=2020-01-01", "--expire=bogus",
                "-f", "--force", "-d", "--detach", "--checkout", "--no-checkout",
                "--lock", "--no-lock", "--reason=gen", "--orphan",
                "-bgen-wtb", "-Bgen-wtb", "--track", "--no-track",
                "--guess-remote", "--no-guess-remote",
                "--relative-paths", "--no-relative-paths",
            ],
            positionals: &[
                "add", "list", "lock", "move", "prune", "remove", "repair", "unlock",
                "wt", "wt-gen", "wt/README.md", "linked",
                // The two names [`Shape::WorktreeLocked`] adds beside its `wt`:
                // one registered and open, one registered with its directory
                // deleted. They are what makes `lock`, `unlock`, `remove` and
                // `prune` answer three different ways to the same argv instead
                // of one, and no other shape can spell either.
                "wt-open", "wt-gone",
                "HEAD", "main", "feature", "does-not-exist", "",
            ],
            // `WorktreeLocked` is the whole lock protocol, which
            // [`Shape::Worktree`] cannot pose: a case is one argv and cannot
            // lock a worktree before asking about one. Verified against stock
            // 2.55.0 on that shape: `worktree list --porcelain` prints
            // `locked held by the fixture` for `wt` and
            // `prunable gitdir file points to non-existent location` for
            // `wt-gone` — two lines this harness had never printed — `worktree
            // unlock wt-open` is `fatal: 'wt-open' is not locked` (rc 128),
            // `worktree unlock wt` is rc 0, and `worktree remove wt` refuses
            // with `fatal: cannot remove a locked working tree, lock reason:
            // held by the fixture` (rc 128).
            shapes: &[
                Shape::Linear,
                Shape::Branched,
                Shape::Detached,
                Shape::Dirty,
                Shape::Worktree,
                Shape::WorktreeLocked,
            ],
        },
        // `reflog` is two commands again: a `log` front end over `.git/logs`, and
        // the store's own `expire`/`delete`/`drop`/`write` writers. The generated
        // grammar describes the reader in a hundred flags and cannot reach a
        // single writer — `expire` and `delete` are not in its positionals, so
        // `--expire=`, `--expire-unreachable=`, `--rewrite`, `--updateref` and
        // `--stale-fix` were only ever parsed by `reflog show`, which rejects
        // them. `runner::probe_reflogs` reads every byte under `.git/logs`, so a
        // reflog write is fully observed the moment one can be spelled.
        //
        // The expiry values are chosen so that the wall clock cannot decide the
        // answer. `env::harden` pins `GIT_COMMITTER_DATE`, not the clock, so
        // every reflog entry in every fixture carries one fixed timestamp far in
        // the past: `now` and `all` expire all of them, `never` expires none,
        // and `2020-01-01` expires none — none of the four is near a boundary
        // where two runs seconds apart could disagree. A relative window like
        // `2.weeks.ago` would be just as safe today and is left out anyway,
        // because what makes it safe is a fixture date nobody promised to keep.
        //
        // `git reflog drop --dry-run` is deliberately reachable and is a
        // rejection rather than an operation: verified against stock 2.55.0,
        // `drop` does not take that option and answers ``error: unknown option
        // `dry-run'`` with rc 129, while `expire` and `delete` both accept it.
        // Three subcommands sharing a store and disagreeing about one option is
        // exactly the kind of table a port flattens.
        Grammar {
            cmd: "reflog",
            flags: &[
                "-n", "--dry-run", "--verbose", "--rewrite", "--updateref",
                "--all", "--single-worktree", "--stale-fix",
                "--expire=now", "--expire=never", "--expire=all",
                "--expire=2020-01-01", "--expire=bogus",
                "--expire-unreachable=now", "--expire-unreachable=never",
                "--expire-unreachable=bogus",
                "--oneline", "--no-abbrev", "--abbrev=7", "--date=iso", "--date=raw",
                "--format=%gd %gs", "--format=%(bogus)", "-n2", "--no-rewrite",
                "--no-updateref", "--no-all",
            ],
            positionals: &[
                "show", "list", "exists", "expire", "delete", "drop", "write",
                "HEAD", "main", "feature", "refs/heads/main", "refs/heads/feature",
                "HEAD@{0}", "HEAD@{1}", "main@{0}", "refs/heads/does-not-exist",
                "does-not-exist", "",
            ],
            shapes: ALL_SHAPES,
        },
        // `sparse-checkout` writes `.git/info/sparse-checkout` and the index's
        // skip-worktree bits, and it is the one piece of repository state
        // `runner::probe_state` does not read — `probe_op_state` names the files
        // it reads and that is not one of them, and `ls-files --stage` prints
        // the same three lines whether or not an entry is sparse. Which is why
        // an observer for it is added below beside this grammar: writing the
        // patterns and reading them back are useless apart.
        //
        // The generated pool spells six of its eight subcommands as two-word
        // positionals (`"set src"`, `"init --cone"`), which are single operands
        // and cannot dispatch — see the block comment above. Verified against
        // stock 2.55.0 for the shape of the reachable ones: `clean` refuses
        // without a mode (`fatal: for safety, refusing to clean without one of
        // --force or --dry-run`) and accepts either, `check-rules` reads its
        // paths from stdin unless `--rules-file` names one — which is why
        // `--stdin` and `--rules-file=` are both here, the first so
        // [`wants_stdin`] fires and the second so the file path is taken instead
        // — and `list`, `add`, `reapply` and `clean` all die with
        // `fatal: … not … sparse` on a repository that has no patterns, so the
        // `Sparse` shape is what makes most of this grammar do work.
        Grammar {
            cmd: "sparse-checkout",
            flags: &[
                "--cone", "--no-cone", "--sparse-index", "--no-sparse-index",
                "--skip-checks", "-z", "--stdin", "--force", "--dry-run", "-n",
                "--rules-file=.gitignore", "--rules-file=does-not-exist",
                "--no-rules-file",
            ],
            positionals: &[
                "init", "list", "set", "add", "reapply", "disable", "check-rules", "clean",
                "src", "inside", "inside/keep.txt", "outside", "README.md", "/src/",
                "does-not-exist", "",
            ],
            shapes: &[Shape::Sparse, Shape::Linear, Shape::Branched, Shape::Dirty],
        },
        // The smallest grammar here and the one whose generated version is most
        // clearly mechanical: its flag pool contains the empty string and six
        // multi-word entries (`"-q --short"`, `"--quiet --short --no-recurse"`)
        // that are single argv tokens, and `error: unknown switch ` '` is all
        // any of them can produce. The two flags that make this command a
        // *writer* — `-d`/`--delete`, and `-m` for the reflog reason — are
        // absent from it entirely, so the verb that names `HEAD` was represented
        // by its reader alone.
        //
        // `-m` is left bare on purpose, unlike `notes`' `-m` above: it takes the
        // next token as the reason, and here that consumes a *ref name* the
        // sampler drew, which shifts every following operand by one. That is a
        // real caller mistake and a real parse, and the id shows the whole argv
        // either way. Verified against stock 2.55.0:
        // `symbolic-ref -m gen refs/gen-sym refs/heads/main` writes the symref
        // (rc 0) and `symbolic-ref -d refs/gen-sym` removes it (rc 0), while
        // `symbolic-ref --delete -q refs/heads/main` answers
        // `fatal: Cannot delete refs/heads/main, not a symbolic ref` with rc 128.
        Grammar {
            cmd: "symbolic-ref",
            flags: &[
                "-q", "--quiet", "--no-quiet", "--short", "--no-short",
                "--recurse", "--no-recurse", "-d", "--delete", "-m", "--bogus-flag",
            ],
            positionals: &[
                "HEAD", "refs/gen-sym", "refs/heads/main", "refs/heads/feature",
                "refs/tags/v0.1.0", "refs/remotes/origin/HEAD",
                "MERGE_HEAD", "ORIG_HEAD", "gen",
                "refs/heads/does-not-exist", "does-not-exist", "bad..name", "",
            ],
            shapes: ALL_SHAPES,
        },
    ]
}

/// Every grammar the fuzzer draws from: the hand-written ones above, plus the
/// per-command grammars generated from git's own documentation.
fn all_grammars() -> Vec<Grammar> {
    let mut all = grammars();
    all.extend(crate::grammars_generated::generated());
    all
}

/// Generate `per_cmd` cases for each grammar from `seed`.
pub fn generate(seed: u64, per_cmd: usize) -> Vec<Case> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::new();
    for g in all_grammars() {
        for _ in 0..per_cmd {
            out.push(sample(&mut rng, &g));
        }
    }
    out
}

/// Replace the `=value` of a `--flag=value` token with an edge-case value, or
/// return the flag unchanged. Flags without `=` are left alone. This is how a
/// value parser meets empty / overflow / garbage inputs it never saw curated.
fn mutate_value(rng: &mut Rng, flag: &str) -> String {
    match flag.split_once('=') {
        Some((name, _)) if rng.chance(1, 3) => format!("{name}={}", rng.pick(VALUES)),
        _ => flag.to_string(),
    }
}

/// Draw the scope one entry is delivered from.
///
/// `primary` is the scope the draw as a whole leans toward, and the two thirds
/// bias toward it is the difference between measuring two things:
///
///  * **Same scope, twice.** Two entries for one key in one *file* is the
///    last-value-wins rule, which every scope implements separately — the file
///    parser by overwriting as it reads, `-c` by appending to a parameter list
///    that is walked in order. Without the bias, a two-entry draw would land in
///    one file only about a sixth of the time and the rule would be measured a
///    sixth as often as it is worth measuring.
///  * **Different scopes.** One key in two scopes is *precedence*, which is the
///    single thing a `-c`-only harness could never see at all. The remaining
///    third is what produces it.
///
/// Both are wanted, so neither is chosen: the bias buys the first without
/// closing off the second.
fn sample_scope(rng: &mut Rng, key: &str, primary: ConfigScope) -> ConfigScope {
    let allowed = scopes_for(key);
    // The `chance` roll is taken before the membership test, so the RNG consumes
    // the same amount whether or not `primary` is admissible for this key — a
    // draw whose stream position depended on the key would make an id
    // irreproducible from its seed.
    let take_primary = rng.chance(2, 3);
    if take_primary && allowed.contains(&primary) {
        primary
    } else {
        *rng.pick(allowed)
    }
}

/// Draw the configuration one case runs under: which keys, which values, and
/// **which scope each one is delivered from**.
///
/// Most cases get none: configuration is a second axis, and crossing it into
/// every case would leave the argv axis measured only under a perturbed git.
///
/// When it fires, two thirds of the draws take their keys from one
/// [`CONFIG_GROUPS`] entry rather than independently from [`CONFIG_KEYS`]. That
/// is the whole point of the group table: two keys drawn independently out of a
/// forty-key pool are almost never the two that interact, so a sampler that only
/// ever did that would spend its whole budget proving that unrelated settings do
/// not interfere. The remaining third keeps drawing independently, because the
/// flat pool is wider than the groups and a key that belongs to no group would
/// otherwise become unreachable.
///
/// One to three keys, as before, and **repetition is allowed**: the same key
/// drawn twice with two values is the last-wins premise, and it is only a
/// duplicate when the scope, the key *and* the value all repeat, which measures
/// nothing the first one did not.
///
/// A raw line is appended on a minority of draws, always into a file scope,
/// because a line is a thing only a file has — a third of them malformed (the
/// line-numbered refusal that `-c` cannot produce), a third legal-but-
/// unreachable (a continuation, a trailing comment, a folded spelling) and a
/// third valueless, which is the `NULL`-value branch of a real key and neither
/// of the other two.
///
/// It goes *after* the settings rather than being shuffled among them, and the
/// reason is the legal half rather than the malformed half. A legal file-only
/// line is a setting like any other, so putting it last is what makes it the one
/// that wins when it names a key an earlier stanza also named — the same
/// last-wins question, asked of the form only a file can express. The malformed
/// half aborts the whole read wherever it sits, so its position is not a choice
/// anybody has.
fn sample_config(rng: &mut Rng) -> Vec<ConfigEntry> {
    if !rng.chance(1, 3) {
        return Vec::new();
    }
    let primary = *rng.pick(ConfigScope::ALL);
    let group = if rng.chance(2, 3) { Some(rng.pick(CONFIG_GROUPS)) } else { None };

    let mut out: Vec<ConfigEntry> = Vec::new();
    for _ in 0..=rng.below(3) {
        let (key, own) = match group {
            Some(g) => *rng.pick(g.keys),
            None => *rng.pick(CONFIG_KEYS),
        };
        // Two thirds from the key's own values, one third from the generic edge
        // pool: the first measures what the setting *does*, the second measures
        // what happens when it cannot be parsed.
        let value = if rng.chance(2, 3) { *rng.pick(own) } else { *rng.pick(CONFIG_EDGE_VALUES) };
        let scope = sample_scope(rng, key, primary);
        let entry = ConfigEntry::set(scope, key, value);
        if !out.contains(&entry) {
            out.push(entry);
        }
    }

    if rng.chance(1, 5) {
        let scope = *rng.pick(ConfigScope::FILES);
        // Three pools, one draw. The valueless pool is a third of the raw lines
        // rather than a handful of entries appended to the legal one because its
        // members are not legal-or-malformed at all — a line with no value parses
        // fine and is then accepted or refused by the *key's* callback, which is a
        // third outcome and the only one that separates `NULL` from the empty
        // string. See [`CONFIG_VALUELESS_LINES`].
        let pool = match rng.below(3) {
            0 => CONFIG_BAD_LINES,
            1 => CONFIG_ODD_LINES,
            _ => CONFIG_VALUELESS_LINES,
        };
        out.push(ConfigEntry::raw(scope, *rng.pick(pool)));
    }
    out
}

/// Draw extra environment variables.
///
/// Rarer than configuration because each one redirects discovery or storage
/// wholesale — `GIT_DIR` decides which repository the command is even talking
/// about — so a high rate would drown the other dimensions in cases that all
/// fail for the same reason. At most two, and never the same variable twice:
/// a second draw of one key would silently win over the first and the case id
/// would name a setting that never applied.
fn sample_env(rng: &mut Rng) -> Vec<(String, String)> {
    if !rng.chance(1, 5) {
        return Vec::new();
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for _ in 0..=rng.below(2) {
        let (key, values) = *rng.pick(ENV_VARS);
        if out.iter().any(|(k, _)| k == key) {
            continue;
        }
        out.push((key.to_string(), rng.pick(values).to_string()));
    }
    out
}

/// Draw global options to place before the subcommand.
fn sample_globals(rng: &mut Rng) -> Vec<Vec<String>> {
    if !rng.chance(1, 3) {
        return Vec::new();
    }
    let mut out: Vec<Vec<String>> = Vec::new();
    for _ in 0..=rng.below(2) {
        let opt: Vec<String> = rng.pick(GLOBAL_OPTIONS).iter().map(|t| t.to_string()).collect();
        if !out.contains(&opt) {
            out.push(opt);
        }
    }
    out
}

/// Draw a working directory the sampled shape actually contains, or the fixture
/// root.
///
/// Drawn from [`COMMON_DIRS`] plus [`shape_dirs`] as one flat range so a shape
/// that adds layouts of its own — a linked worktree, a submodule, a bare
/// repository — reaches them proportionally rather than needing a second roll.
fn sample_cwd(rng: &mut Rng, shape: Shape) -> Option<&'static str> {
    // Most cases stay at the root: the argv dimensions are what the grammars
    // describe, and running them all from inside `.git` would measure discovery
    // over and over instead of the commands.
    if !rng.chance(1, 6) {
        return None;
    }
    let extra = shape_dirs(shape);
    let i = rng.below(COMMON_DIRS.len() + extra.len());
    Some(if i < COMMON_DIRS.len() { COMMON_DIRS[i] } else { extra[i - COMMON_DIRS.len()] })
}

/// Build one invocation. Far more aggressive than a flag or two: it stacks
/// **repeated** flags (deep enough to trip re-parse and last-wins bugs), mutates
/// flag values, supplies **multiple** positionals, interleaves flags and
/// positionals in argument order, and injects a `--` separator — every degree
/// of freedom a real caller eventually exercises and none of which the corpus
/// covers. Still a pure function of the RNG, so any failure replays from its
/// seed.
///
/// argv is only one of the dimensions a git invocation has, and for a long time
/// it was the only one sampled: configuration, environment, global options,
/// working directory and stdin were fixed at "none" for every generated case, so
/// the whole of `handle_options`, of `git_config_get_*`, and of discovery was
/// reachable from curated cases alone. Each of those is now drawn here, on its
/// own probability, so a generated case is a point in the whole space rather
/// than in one axis of it.
fn sample(rng: &mut Rng, g: &Grammar) -> Case {
    // Drawn first because the working directory depends on it: only the
    // directories a shape actually contains are candidates.
    let shape = *rng.pick(g.shapes);

    let args = sample_argv(rng, g, 6, 3);

    // stdin is fed only where the sampled argv (or the subcommand itself) asks
    // for it — see `wants_stdin` for the two rules that decide.
    let stdin = sample_stdin(rng, g.cmd, &args);

    Case {
        cmd: g.cmd,
        args,
        config: sample_config(rng),
        globals: sample_globals(rng),
        shape,
        stdin,
        // stderr stays uncompared for generated cases. Opting in is a statement
        // that a particular message *is* the behaviour, which is a curated
        // judgement; asserting it across sampled argv would compare prose the
        // port is specified not to reproduce.
        compare_stderr: false,
        cwd: sample_cwd(rng, shape),
        env: sample_env(rng),
    }
}

/// Draw the argv of one invocation: the subcommand, its flags, its positionals,
/// and the order they are written in.
///
/// Split out of [`sample`] because a *step* of a generated sequence needs
/// exactly this and nothing else — a [`crate::runner::Step`] carries argv and
/// stdin, and shape, config, globals, cwd and environment live on the sequence's
/// envelope. Writing a second argv sampler for sequences would mean a second
/// place where the flag-repetition, value-mutation, interleaving and `--`
/// rules live, and the two would drift; the whole reason the sequence generator
/// draws from [`Grammar`] at all is to avoid a second flag table, and a second
/// *sampler* is the same mistake one level up.
///
/// `max_flags`/`max_pos` are the only difference between the two callers.
/// [`sample`] passes the historical `6`/`3`; the sequence generator passes small
/// numbers, because a mutation buried under six stacked flags usually dies at
/// parse time, and a step that died at parse time leaves the steps after it
/// observing a repository nothing wrote to. The draw *order* is unchanged either
/// way, and [`Rng::count_upto`] consumes the same three values whatever its
/// bound, so an existing seed still produces the case ids it produced before
/// this split.
/// The byte a generated grammar uses to hold several argv tokens in one entry.
///
/// A grammar entry is one `&'static str` and reaches the child as one argument:
/// `Case::argv` extends from `args` without splitting, and the samplers below
/// push each drawn entry with `Vec<String>::push`. That was fine until the
/// entries were read off git's own documentation, where an example is written
/// the way a caller types it — `HEAD -- README.md`, `set src`, `-m 1`. Each of
/// those arrived as a single argument, and stock git answered
///
///     fatal: ambiguous argument 'HEAD -- README.md'
///     error: unknown subcommand: `set src'
///
/// on both sides. The case matched, and measured nothing about the port. The id
/// hid it too: `Case::id_tail` joins argv with a space, so one token containing
/// spaces renders exactly like several tokens.
///
/// `scripts/gen_grammars.pl` now distinguishes the two — a JSON string is one
/// token however many spaces it holds, a JSON array is several — and encodes an
/// array with this separator, because the element type is pinned to
/// `&'static str` by `mutate_value`, by the `RESUME_EXTRA` membership tests and
/// by both `rng.pick(...).to_string()` call sites. Splitting on it here is what
/// turns the encoding back into argv, and it leaves every legitimate
/// space-bearing entry — `--since=1 year ago`, `path with spaces` — untouched,
/// since none of them contains a unit separator.
const TOKEN_SEP: char = '\u{1f}';

/// Expand any multi-token entries in `args` into the argv they encode.
fn split_tokens(args: Vec<String>) -> Vec<String> {
    if !args.iter().any(|a| a.contains(TOKEN_SEP)) {
        return args;
    }
    args.iter().flat_map(|a| a.split(TOKEN_SEP).map(str::to_string)).collect()
}

fn sample_argv(rng: &mut Rng, g: &Grammar, max_flags: usize, max_pos: usize) -> Vec<String> {
    // Up to `max_flags` flags, WITH repetition allowed. Repeats are not
    // dilution: a re-declared flag is exactly what surfaces last-wins and
    // re-parse bugs.
    let mut flag_tokens: Vec<String> = Vec::new();
    if !g.flags.is_empty() {
        for _ in 0..rng.count_upto(max_flags) {
            let flag = *rng.pick(g.flags);
            flag_tokens.push(mutate_value(rng, flag));
        }
    }

    // Up to `max_pos` positionals, repetition allowed (`git log HEAD HEAD` is
    // valid and has its own behavior). Empty positionals are dropped, not
    // emitted.
    let mut pos_tokens: Vec<String> = Vec::new();
    if !g.positionals.is_empty() {
        for _ in 0..rng.count_upto(max_pos) {
            let p = *rng.pick(g.positionals);
            if !p.is_empty() {
                pos_tokens.push(p.to_string());
            }
        }
    }

    let mut args = vec![g.cmd.to_string()];

    // Ordering: usually flags-then-positionals as a caller writes it, but a
    // fraction of the time interleave them, which tests that option parsing does
    // not depend on flags preceding operands (git's does not; a buggy port's
    // might). A `--` separator is injected before the positionals sometimes,
    // both with and without interleaving.
    let sep = !pos_tokens.is_empty() && rng.chance(1, 4);
    if rng.chance(1, 3) && !flag_tokens.is_empty() && !pos_tokens.is_empty() {
        // Interleave by draining the two lists in a random order.
        let mut fi = flag_tokens.into_iter().peekable();
        let mut pi = pos_tokens.into_iter().peekable();
        let mut sep_done = !sep;
        while fi.peek().is_some() || pi.peek().is_some() {
            let take_flag = match (fi.peek().is_some(), pi.peek().is_some()) {
                (true, false) => true,
                (false, true) => false,
                _ => rng.chance(1, 2),
            };
            if take_flag {
                args.push(fi.next().unwrap());
            } else {
                if !sep_done {
                    args.push("--".to_string());
                    sep_done = true;
                }
                args.push(pi.next().unwrap());
            }
        }
        if !sep_done {
            // No positional was emitted after all; nothing to separate.
        }
    } else {
        args.extend(flag_tokens);
        if sep {
            args.push("--".to_string());
        }
        args.extend(pos_tokens);
    }
    split_tokens(args)
}

/// The payload for an invocation that asks for one, and `None` for one that does
/// not. Split out of [`sample`] for the same reason [`sample_argv`] is: a
/// sequence step needs the identical rule, and a second copy of it would let a
/// step be handed input the command never reads — which is exactly what
/// [`wants_stdin`] documents as worse than a narrower rule.
fn sample_stdin(rng: &mut Rng, cmd: &str, args: &[String]) -> Option<&'static [u8]> {
    wants_stdin(cmd, args).then(|| {
        // Two thirds from the payloads shaped for this command, one third from
        // the whole pool, so the parse path and the reject path are both reached.
        let pool = if rng.chance(2, 3) { preferred_payloads(cmd) } else { STDIN_PAYLOADS };
        *rng.pick(pool)
    })
}

/// Greedily drop one element at a time from a vector-valued dimension of a case,
/// keeping every drop that still fails.
///
/// `field` picks the vector out of a case; it is a plain function pointer rather
/// than a closure so the same walk serves `args`, `config`, `globals` and `env`
/// without four copies of the index bookkeeping. `from` is the first droppable
/// index — 1 for `args`, whose element 0 is the subcommand and is never dropped.
fn drop_each<T: Clone>(
    best: &mut Case,
    from: usize,
    field: fn(&mut Case) -> &mut Vec<T>,
    still_fails: &mut dyn FnMut(&Case) -> bool,
) {
    let mut i = from;
    while i < field(best).len() {
        let mut candidate = best.clone();
        field(&mut candidate).remove(i);
        if still_fails(&candidate) {
            *best = candidate; // keep index: the list shifted left under us
        } else {
            i += 1;
        }
    }
}

/// Shrink a failing case to a minimal still-failing one by greedily dropping
/// one sampled fact at a time. `still_fails` re-runs the candidate; the
/// subcommand at `args[0]` is never dropped.
///
/// Reported failures are worth far more minimized: a three-flag failure usually
/// reduces to one flag, which names the actual defect. That argument applies to
/// every dimension the fuzzer samples, not only to argv — a failure reported
/// with five config keys, two environment variables, a working directory and a
/// stdin payload attached is worth much less than the same failure reported with
/// the one of them that is responsible, and while argv was the only dimension
/// sampled it was also the only one that needed peeling.
///
/// Order is coarsest first. The whole-fact dimensions (stdin, cwd, shape) are
/// single substitutions that often remove the failure's entire premise, and the
/// environment redirects discovery wholesale, so trying them before the
/// token-by-token walk through argv means the expensive walk usually runs on an
/// already-smaller case.
///
/// # Dropping is not the only reduction
///
/// Four of the dimensions a case carries cannot be *dropped* at all — every case
/// has a shape, every config entry has a scope, a payload is either present or
/// absent, and a working directory is a path rather than a list. Peeling was the
/// whole vocabulary while argv was the whole case, and it left those four
/// unreduced while the corpus started drawing all of them: a failure reported on
/// `Shape::Promisor` from `.git/worktrees/wt` with a key delivered through
/// `.git/config.worktree` says nothing about whether any of that mattered.
///
/// So each of them is *simplified* rather than dropped, and in every one the
/// substitute is a value the harness already treats as the plain case:
///
///  * **Shape** walks [`SIMPLER_SHAPES`], stopping at the case's own shape. The
///    fixture is built by the runner from the shape alone, so a candidate on a
///    plainer one is an ordinary case and not a special form.
///  * **Config scope** moves an entry to [`ConfigScope::CommandLine`], which is
///    the scope every entry had before scopes were sampled and the one a reader
///    can retype as a `-c`. A keyed entry only; a raw line is meaningless
///    anywhere but a file and is left where it is.
///  * **Stdin** shortens the payload to a line-boundary prefix, shortest first,
///    which is a strict reduction of a `&'static [u8]` literal and stays
///    `'static` because a subslice of a `'static` slice is one.
///  * **Cwd** walks the parents of the sampled directory, shortest first. The
///    runner creates a missing directory on both sides, so a parent is always a
///    runnable case.
///
/// Every one of those is a fixed, ordered walk with no randomness in it: the same
/// failing case and the same predicate produce the same minimal case on every
/// run, which is the property that makes a printed `→` line something a reader
/// can act on.
///
/// [`crate::runner::Case::size`] does not count any of the four, so a run that
/// only simplified them leaves the size unchanged and `main` prints nothing —
/// the reduction still happens, and it shows up in the id whenever anything else
/// was dropped alongside it.
pub fn shrink(case: &Case, still_fails: &mut dyn FnMut(&Case) -> bool) -> Case {
    let mut best = case.clone();

    // Closed stdin and the fixture root are the *defaults* every case had before
    // these dimensions existed, so falling back to them is a real minimization
    // and not a different case.
    if best.stdin.is_some() {
        let candidate = Case { stdin: None, ..best.clone() };
        if still_fails(&candidate) {
            best = candidate;
        }
    }
    if best.cwd.is_some() {
        let candidate = Case { cwd: None, ..best.clone() };
        if still_fails(&candidate) {
            best = candidate;
        }
    }
    simplify_shape(&mut best, still_fails);

    drop_each(&mut best, 0, |c| &mut c.env, still_fails);
    drop_each(&mut best, 0, |c| &mut c.globals, still_fails);
    drop_each(&mut best, 0, |c| &mut c.config, still_fails);
    drop_each(&mut best, 1, |c| &mut c.args, still_fails);

    // The four non-droppable dimensions, on whatever survived the peeling. After
    // it rather than before: a scope walk over five config entries costs five
    // re-runs, and four of them are usually about to be dropped.
    simplify_scopes(&mut best, still_fails);
    shorten_stdin(&mut best, still_fails);
    shorten_cwd(&mut best, still_fails);
    best
}

/// The shapes a failing case is re-tried on, plainest first.
///
/// Three rather than the whole [`ALL_SHAPES`] list, and these three because they
/// are the ones every other shape is a decoration of: [`Shape::Linear`] is
/// documented as the floor case, `Branched` adds refs to it and `Dirty` adds
/// worktree state. Trying forty shapes would cost forty re-runs to report a fact
/// — "it also fails on `Octopus`" — that is not simpler than the one the case
/// already carries.
///
/// A shape is only tried if it comes *before* the case's own in this list, so a
/// case that already runs on `Linear` is left alone and one on `Dirty` is only
/// re-tried on `Linear` and `Branched`. A shape outside the list is treated as
/// after all three, which is what makes the exotic shapes — the ones a reader
/// most wants removed from a report — reducible.
const SIMPLER_SHAPES: &[Shape] = &[Shape::Linear, Shape::Branched, Shape::Dirty];

/// Re-run the case on a plainer fixture, keeping the first that still fails.
fn simplify_shape(best: &mut Case, still_fails: &mut dyn FnMut(&Case) -> bool) {
    let own = SIMPLER_SHAPES.iter().position(|s| *s == best.shape).unwrap_or(SIMPLER_SHAPES.len());
    for shape in &SIMPLER_SHAPES[..own] {
        let candidate = Case { shape: *shape, ..best.clone() };
        if still_fails(&candidate) {
            *best = candidate;
            return;
        }
    }
}

/// Move each keyed config entry to the command line, keeping every move that
/// still fails.
///
/// One entry at a time and left alone when the move stops the failure, because
/// the scope *is* the finding for a whole class of case: `core.worktree` is inert
/// from `-c` and fatal from `.git/config`, a raw line only exists in a file, and
/// `extensions.worktreeConfig` gates whether `.git/config.worktree` is read at
/// all. A shrinker that rewrote the scope unconditionally would report those as
/// command-line cases that do not reproduce.
fn simplify_scopes(best: &mut Case, still_fails: &mut dyn FnMut(&Case) -> bool) {
    for i in 0..best.config.len() {
        if best.config[i].is_raw() || best.config[i].scope == ConfigScope::CommandLine {
            continue;
        }
        let mut candidate = best.clone();
        candidate.config[i].scope = ConfigScope::CommandLine;
        if still_fails(&candidate) {
            *best = candidate;
        }
    }
}

/// Cut the stdin payload down to its shortest line-boundary prefix that still
/// fails.
///
/// Prefixes rather than arbitrary subsets: a payload is a *language* — a patch, a
/// mailbox, a fast-import stream — and dropping a line from the middle produces
/// input whose refusal is about the hole rather than about the port. A prefix is
/// the truncation git itself has to survive, and `P_PATCH_TRUNCATED` is already
/// in the pool because that path is worth measuring.
///
/// Shortest first, so the answer is the least input that reproduces, and capped
/// at [`STDIN_PREFIX_TRIES`] candidates so a long payload cannot turn one
/// reported failure into fifty re-runs.
fn shorten_stdin(best: &mut Case, still_fails: &mut dyn FnMut(&Case) -> bool) {
    let Some(payload) = best.stdin else { return };
    let mut ends: Vec<usize> = payload
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .map(|(i, _)| i + 1)
        .filter(|end| *end < payload.len())
        .collect();
    ends.truncate(STDIN_PREFIX_TRIES);
    for end in ends {
        let candidate = Case { stdin: Some(&payload[..end]), ..best.clone() };
        if still_fails(&candidate) {
            *best = candidate;
            return;
        }
    }
}

/// How many stdin prefixes one shrink may try. Every payload in [`STDIN_PAYLOADS`]
/// is under a dozen lines, so this bounds the cost without reaching a payload's
/// real end.
const STDIN_PREFIX_TRIES: usize = 12;

/// Move the working directory up toward the fixture root, keeping the shallowest
/// parent that still fails.
///
/// `shrink` already tried the root itself and failed, so the failure needs *a*
/// directory; this asks how much of the path it needs. `.git/objects/pack`
/// reducing to `.git` is the difference between "inside the pack directory" and
/// "inside the git directory", and the second is the sentence a discovery bug is
/// written in.
fn shorten_cwd(best: &mut Case, still_fails: &mut dyn FnMut(&Case) -> bool) {
    let Some(dir) = best.cwd else { return };
    // Ascending prefixes, shortest first: `a`, `a/b`, … and never the whole
    // string, which is what the case already has.
    let parents: Vec<&'static str> = dir
        .char_indices()
        .filter(|(_, c)| *c == '/')
        .map(|(i, _)| &dir[..i])
        .collect();
    for parent in parents {
        let candidate = Case { cwd: Some(parent), ..best.clone() };
        if still_fails(&candidate) {
            *best = candidate;
            return;
        }
    }
}

// ===========================================================================
// Generated sequences
// ===========================================================================
//
// Everything above generates **one** invocation against a pristine fixture.
// Everything git gets wrong twice is a *second* invocation reading what a first
// one wrote. `corpus/sequences.rs` closes that with twenty-odd hand-written
// workflows, and it closed it well — the first run of the curated sequence
// corpus found eight defects, five of them reflog messages no single case could
// see. But that corpus is exactly the artifact the single-case fuzzer exists to
// supplement: it covers what a human thought to check. This section covers what
// nobody thought to check, in the dimension where this port has historically
// broken.
//
// # Why a random chain of commands is not a sequence
//
// The naive generator draws N invocations and calls them a workflow. `git tag`
// followed by `git ls-files` is two independent cases wearing a sequence's
// clothes: it costs two invocations and two state probes per side and measures
// nothing the two cases would not have measured apart, because neither step
// reads anything the other wrote. Worse, it *looks* like coverage — the run
// prints more sequences, the invocation count goes up, and the number nobody may
// tune upward has been tuned upward with noise.
//
// So the dependency between step N and step N+1 is not left to chance; it is the
// thing being generated. Three families, and each one names the dependency it
// encodes:
//
//  * [`STOPPERS`] — **state-machine walks.** Park the repository in an
//    interrupted operation, then walk the resumption verbs. Every verb after the
//    first reads `.git/sequencer/`, `.git/rebase-merge/`, `.git/rebase-apply/`,
//    `CHERRY_PICK_HEAD`, `REVERT_HEAD` or `MERGE_HEAD` — state the entry step
//    wrote and nothing else could have. **Illegal transitions are drawn
//    deliberately**, not tolerated: `rebase --continue` with a cherry-pick in
//    progress, `--skip` after `--abort`, `--quit` twice. A port implements the
//    legal transitions because the documentation lists them; the illegal ones it
//    invents, and an invented refusal is indistinguishable from a correct one
//    until something compares it.
//  * [`MUTATORS`] — **mutate then observe.** One sampled mutating invocation,
//    then the readers whose answer it should have changed. `for-each-ref` after
//    a ref write, `stash list` after a stash push, `reflog` after anything that
//    moves `HEAD`, `ls-files --stage` after an index write. A wrong write is
//    caught by a right read, at the step that read it. This family also covers
//    the case a curated corpus never writes down: when the sampled mutation
//    *fails*, the observers assert that a failed mutation changed **nothing** —
//    which is where half-applied writes live and which no single case can see,
//    because a single case only ever looks at the repository once.
//  * [`ROUND_TRIPS`] — **an operation and its inverse.** `stash push`/`pop`,
//    `worktree add`/`remove`, `branch -m` and back, `sparse-checkout
//    set`/`disable`. The inverse operates on state the forward step created, and
//    the end state must equal the start state on both sides — so a `remove` that
//    half-cleans, or an `unset` that leaves a stanza behind, is a state
//    difference at the step that failed to clean rather than a mystery later.
//    This is also the one family that reads the forward half's artifact **by
//    name**: [`RoundTrip::reads`] asks one question about the thing that was just
//    written, once while it exists and once after the inverse, so the answer is a
//    value the earlier step produced rather than a generic listing that happens
//    to contain it.
//
// # What is drawn from the grammars and what cannot be
//
// Flags, positionals and shapes come from [`all_grammars`] via [`sample_argv`] —
// there is no second flag table here, and a grammar widened tomorrow widens
// these sequences the same day.
//
// Three things are stated here because no grammar encodes them, and each is a
// property of git rather than of a command line:
//
//  * **Which invocation stops.** A grammar says `cherry-pick` takes a rev; it
//    does not say that `cherry-pick theirs` on [`Shape::Conflicted`] conflicts
//    while `cherry-pick HEAD` does not. Drawing the entry step uniformly from the
//    grammar would leave nearly every walk resuming an operation that was never
//    started, which is one already-covered refusal repeated a thousand times.
//    [`STOPPERS`] therefore names premises the curated corpus has already proven
//    stop — and then decorates them with grammar-drawn flags, so the entry
//    invocation is not fixed and the walks that *do* fail to stop are reached
//    anyway.
//  * **The resumption alphabet.** `--continue`/`--skip`/`--abort`/`--quit` are
//    the transitions of `sequencer.c`'s state machine. The union of that
//    alphabet with each command's own grammar is taken rather than the
//    intersection, in both directions: the generated grammars carry `--abort`,
//    `--quit` and `--skip` for the sequencer commands but not `--continue`, so an
//    intersection would drop the single most important verb, while a fixed list
//    alone would never reach `am --retry`, which only `am`'s grammar knows about.
//  * **Which verbs mutate.** Nothing in a grammar says whether a command writes.
//    [`MUTATORS`] is a classification, and its criterion is stated there.
//
// # Cost
//
// One generated sequence costs its own step count in invocations and state
// probes **per side**, exactly as a curated one does — see
// [`crate::runner::Sequence`] for why that is the cheap shape and why the first
// divergence ends the run.
//
// The count is `--fuzz-sequences` per *entry point*, mirroring the single-case
// rule of `--fuzz` per grammar: every stopper, every mutator and every
// round-trip pair is drawn at least once at 1, so raising the knob deepens
// coverage uniformly instead of deepening whatever the RNG happened to favour.
// Entry points number [`STOPPERS`] + 1 (bisect) + [`MUTATORS`] + [`ROUND_TRIPS`],
// and steps average five to seven, so the family costs roughly five to six
// invocations per side per entry point per unit of the knob. `main` prints the
// exact sequence and invocation counts for the run it is about to do, which is
// the number to trust: a figure written here would be stale the first time an
// entry point is added.
//
// `--fuzz-sequences 0` turns the family off for a cheap argv-only sweep, and the
// knob defaults to `--fuzz` so a caller who does not care has one knob. It has
// to be a separate knob at all because the two corpora have very different unit
// prices — one invocation against one — and a reader who wants a deep argv sweep
// should not be made to buy a six-fold sequence bill to get it.
//
// # Determinism
//
// The sequence stream is seeded from `seed` mixed with [`SEQUENCE_STREAM`], so it
// is independent of the single-case stream: a sequence failure replays from its
// seed at any `--fuzz`, which it would not if both families drew from one RNG and
// `--fuzz` decided how far along it the sequences started.

/// Mixed into the seed so generated sequences draw from a stream the single-case
/// generator cannot shift. Arbitrary odd 64-bit constant; only its fixedness
/// matters.
const SEQUENCE_STREAM: u64 = 0xD1B5_4A32_D192_ED03;

/// A premise that parks a repository in an interrupted operation.
///
/// `setup` is run first and is *itself compared*, so by the time `entry` runs the
/// premise has been proven identical on both sides rather than assumed — the same
/// argument `corpus/sequences.rs` makes for doing setup in steps rather than in a
/// shape.
struct Stopper {
    /// Headline verb the whole walk is scored under, and what `--only` filters
    /// on. The entry command, not the resumption command: the finding is about
    /// the operation that stopped.
    cmd: &'static str,
    /// Slug rendered into every step id, after the family name.
    name: &'static str,
    shape: Shape,
    /// Steps that put the fixture into the state `entry` needs.
    setup: &'static [&'static [&'static str]],
    /// The invocation(s) that stop. More than one where stopping needs a
    /// predecessor to have succeeded — `am` stops on a mailbox it has already
    /// applied. Grammar-drawn flags are attached to the last of them.
    entry: &'static [&'static [&'static str]],
    /// Payload for every `entry` step. `am` reads its mailbox here; the
    /// resumption verbs that follow are fed nothing, which is the whole reason
    /// [`crate::runner::Step`] carries stdin per step.
    entry_stdin: Option<&'static [u8]>,
}

/// Premises the curated corpus has already proven stop, reused here as the
/// starting points of walks it does not take.
///
/// Deliberately the same premises rather than new ones: a premise that does not
/// actually stop turns its whole walk into resumption verbs against a clean
/// repository, which is a refusal the corpus already covers. Reusing proven ones
/// spends the budget on the transitions instead — which is what is unmeasured.
///
/// The four premises the six new fixture shapes made expressible are held to the
/// same rule: each was run against stock 2.55.0 first, and each entry's comment
/// records the exit codes and the parked files that were observed rather than
/// the ones the documentation promises. Each is one entry point — one sequence
/// per unit of `--fuzz-sequences`, three to eight steps long depending on how
/// many transitions and observers the draw puts after the entry, run on both
/// sides — and each reaches a state that has no other route into this file:
///
///   `merge-criss-cross`       the virtual-merge-base path, stage 1 holding a
///                             blob no commit has
///   `merge-unrelated-forced`  a refusal, then the same merge forced, over a
///                             conflict with no base at all
///   `pick-already-applied`    an operation in progress over a *clean* index
///   `gc-damaged-store`        a maintenance verb told to rewrite a store it
///                             cannot read
///
/// Four more were added with the nine shapes after those, held to the same rule
/// — each run against stock 2.55.0 first, each entry recording the exit codes
/// and parked files that were observed — and each reaching a premise that has no
/// other route into this file:
///
///   `merge-rerere-replay`     a conflict git has seen before, replayed out of
///                             `.git/rr-cache` inside the case
///   `commit-hook-refused`     a hook that refuses, and the flag that skips it
///   `push-hook-refused`       the same over a transport, then the refusal that
///                             happens in the *receiving* repository
///   `shallow-deepen`          a truncated history extended, then read
const STOPPERS: &[Stopper] = &[
    Stopper {
        cmd: "cherry-pick",
        name: "pick-conflict",
        shape: Shape::Conflicted,
        setup: &[&["merge", "--abort"]],
        entry: &[&["cherry-pick", "theirs"]],
        entry_stdin: None,
    },
    // Three picks conflicting on the first, so `.git/sequencer/todo` still holds
    // two while the walk runs — the part of the sequencer a port is most likely
    // to forget entirely, and empty in every two-commit history.
    Stopper {
        cmd: "cherry-pick",
        name: "pick-todo",
        shape: Shape::Whitespace,
        setup: &[&["restore", "."], &["checkout", "-b", "side", "main~4"]],
        entry: &[&["cherry-pick", "main~2", "main~1", "main"]],
        entry_stdin: None,
    },
    // A pick whose patch the branch **already has**. Every stopper above parks a
    // broken index; this one parks a clean one, which is the distinction a port
    // is most likely to collapse. Verified against stock 2.55.0 on
    // [`Shape::Cherry`], whose `topic` already carries `main~1`'s patch:
    //
    //   cherry-pick main~1   rc 1, `The previous cherry-pick is now empty, …`
    //   status --porcelain   **empty** — nothing staged, nothing modified
    //   CHERRY_PICK_HEAD     written      .git/sequencer   absent
    //   cherry-pick --continue  rc 1, the identical refusal again
    //   cherry-pick --skip      rc 0, and `topic` is back at `cherry: topic only`
    //
    // So this is an operation that is genuinely in progress over a repository
    // that looks pristine to every reader that does not open `CHERRY_PICK_HEAD`,
    // and one whose legal continuation is `--skip` rather than `--continue` —
    // the reverse of every other walk in this list. A port that decides "a pick
    // is running" from unmerged index entries reports nothing to resume here,
    // and one that treats `--continue` as always-forward commits an empty commit
    // git refuses to make. The shape is the only one carrying a duplicated patch
    // id, so nothing else can express the premise.
    Stopper {
        cmd: "cherry-pick",
        name: "pick-already-applied",
        shape: Shape::Cherry,
        setup: &[],
        entry: &[&["cherry-pick", "main~1"]],
        entry_stdin: None,
    },
    // `revert` shares `sequencer.c` and writes `REVERT_HEAD` instead of
    // `CHERRY_PICK_HEAD`; a port that wires the shared engine to one filename
    // walks every cherry-pick stopper above and falls over here.
    Stopper {
        cmd: "revert",
        name: "revert-conflict",
        shape: Shape::Whitespace,
        setup: &[&["restore", "."]],
        entry: &[&["revert", "--no-edit", "main~2"]],
        entry_stdin: None,
    },
    Stopper {
        cmd: "rebase",
        name: "rebase-conflict",
        shape: Shape::Conflicted,
        setup: &[&["merge", "--abort"]],
        entry: &[&["rebase", "theirs"]],
        entry_stdin: None,
    },
    // `-i` is reachable because `env::harden` pins `GIT_SEQUENCE_EDITOR=true`,
    // which accepts the generated todo unedited: the todo is written, read back
    // and executed with nothing waiting on a human.
    Stopper {
        cmd: "rebase",
        name: "rebase-i-conflict",
        shape: Shape::Conflicted,
        setup: &[&["merge", "--abort"]],
        entry: &[&["rebase", "-i", "theirs"]],
        entry_stdin: None,
    },
    // A stop that is not a conflict: a failing `exec` parks the rebase with a
    // *clean* worktree and a half-consumed todo, which is the `done`/
    // `git-rebase-todo` split a conflict stop never shows.
    Stopper {
        cmd: "rebase",
        name: "rebase-exec-stop",
        shape: Shape::Renamed,
        setup: &[],
        entry: &[&["rebase", "-i", "--exec", "false", "HEAD~2"]],
        entry_stdin: None,
    },
    Stopper {
        cmd: "merge",
        name: "merge-conflict",
        shape: Shape::Conflicted,
        setup: &[&["merge", "--abort"]],
        entry: &[&["merge", "theirs"]],
        entry_stdin: None,
    },
    // A conflict that leaves **no `MERGE_HEAD`**, which every other stopper here
    // writes one of. `--squash` is documented as not updating `HEAD`, and what
    // that means for the parked state is not obvious until it is measured.
    // Verified against stock 2.55.0 on this shape after `merge --abort`:
    //
    //   merge --squash theirs   rc 1, `CONFLICT (add/add)`, index `AA conflict.txt`
    //   MERGE_HEAD              absent          MERGE_MODE   absent
    //   MERGE_MSG               written         SQUASH_MSG   written
    //   AUTO_MERGE              written
    //   merge --abort           `fatal: There is no merge to abort (MERGE_HEAD missing).`, rc 128
    //   merge --continue        `fatal: There is no merge in progress (MERGE_HEAD missing).`, rc 128
    //   cherry-pick --continue  `error: no cherry-pick or revert in progress`, rc 128
    //
    // So this is a repository with a conflicted index and nothing to resume, and
    // every resumption verb the walk draws must refuse. A port that decides
    // "a merge is in progress" from the index — an unmerged entry exists —
    // rather than from `MERGE_HEAD` passes `merge-conflict` above and offers to
    // continue or abort an operation stock says is not running. That is a whole
    // class of wrong answer that no existing premise can produce, and it costs
    // one entry point.
    Stopper {
        cmd: "merge",
        name: "merge-squash-conflict",
        shape: Shape::Conflicted,
        setup: &[&["merge", "--abort"]],
        entry: &[&["merge", "--squash", "theirs"]],
        entry_stdin: None,
    },
    // A merge that stopped **without** conflicting: `--no-commit` parks
    // `MERGE_HEAD` and `MERGE_MSG` over a *clean, fully staged* index. Every
    // other stopper in this list parks a broken tree, so "is an operation in
    // progress" and "does the index have conflicts" were the same question in
    // every walk, and a port that answers the first with the second was right
    // every time. Verified against stock 2.55.0 on this shape:
    // `merge --no-commit div-other` exits **0** with `Automatic merge went well;
    // stopped before committing as requested`, writes `MERGE_HEAD` and an empty
    // `MERGE_MODE`, and leaves `status --porcelain` reporting `A  other.txt`
    // staged beside the shape's own `M hot.txt`/`M keep.txt`/`?? squat.txt`.
    //
    // `div-other` is the branch chosen from `fixture::mergeable_history` on
    // purpose: it adds `other.txt` and touches none of the three paths this
    // shape leaves dirty, so the merge is not refused per path before it can
    // stop. The shape's dirt is still there while the walk runs, which is what
    // makes `merge --abort` here a different unwind from the one after a
    // conflict — it has to restore a worktree that was already modified.
    Stopper {
        cmd: "merge",
        name: "merge-no-commit-stop",
        shape: Shape::MergeableDirty,
        setup: &[],
        entry: &[&["merge", "--no-commit", "div-other"]],
        entry_stdin: None,
    },
    // A merge with **two merge bases**, parked. The recursive strategy merges the
    // bases with each other into a virtual base and merges against that, and
    // until [`Shape::CrissCross`] existed no premise in this harness entered
    // that path at all — so a port that picks one of the two bases and proceeds
    // was scored identical to one that builds the virtual base. Verified against
    // stock 2.55.0 on the shape, whose `HEAD` is `cc-left`:
    //
    //   merge-base --all cc-left cc-right   two ids, `0a24ba32…` and `27e7a991…`
    //   merge cc-right                      rc 1, `Auto-merging cc.txt`,
    //                                       `CONFLICT (content): … clash.txt`
    //   status --porcelain                  `M  cc.txt` staged, `UU clash.txt`
    //   MERGE_HEAD / MERGE_MODE / AUTO_MERGE   all written
    //   cat-file -p :1:clash.txt            `<<<<<<<<< Temporary merge branch 1`
    //   merge --abort                       rc 0
    //
    // Stage 1 holding a blob that exists in **no commit** is the whole finding,
    // and it is a state difference rather than a stdout one: both sides can
    // print the same conflict summary while their indexes disagree about what
    // the base was. The staged `M cc.txt` beside the conflict is the other half
    // — one path merged cleanly *through* the virtual base — so a walk that
    // aborts here has to unwind both.
    Stopper {
        cmd: "merge",
        name: "merge-criss-cross",
        shape: Shape::CrissCross,
        setup: &[],
        entry: &[&["merge", "cc-right"]],
        entry_stdin: None,
    },
    // A merge refused for a reason that is not a conflict, and then forced. No
    // other premise here has a *first* entry step that fails on purpose: the
    // refusal is half of what is being measured, because a port that never
    // implemented the check reaches the same parked state one step earlier and
    // agrees with stock from there on. Verified against stock 2.55.0 on
    // [`Shape::Unrelated`], from `main`:
    //
    //   merge alien-clash                              rc 128,
    //       `fatal: refusing to merge unrelated histories`, nothing written
    //   merge --allow-unrelated-histories alien-clash  rc 1,
    //       `CONFLICT (add/add): Merge conflict in README.md`
    //   ls-files -u        stages **2 and 3 only** — no stage 1 exists
    //   MERGE_HEAD / MERGE_MODE / AUTO_MERGE   all written
    //
    // An add/add between two roots is the only conflict this harness can produce
    // with no common ancestor to diff against, so every resumption verb the walk
    // then draws is addressed to a merge whose base is absent rather than empty.
    Stopper {
        cmd: "merge",
        name: "merge-unrelated-forced",
        shape: Shape::Unrelated,
        setup: &[],
        entry: &[
            &["merge", "alien-clash"],
            &["merge", "--allow-unrelated-histories", "alien-clash"],
        ],
        entry_stdin: None,
    },
    // `.git/rebase-apply/`, which nothing else parks in. The mailbox applies
    // once and then fails against the tree it just created, so the stop needs no
    // corrupt input to manufacture.
    Stopper {
        cmd: "am",
        name: "am-mailbox-stop",
        shape: Shape::Patches,
        setup: &[],
        entry: &[&["am", "mail/one.eml"], &["am", "mail/one.eml"]],
        entry_stdin: None,
    },
    Stopper {
        cmd: "am",
        name: "am-stdin-stop",
        shape: Shape::Linear,
        setup: &[],
        entry: &[&["am"], &["am"]],
        entry_stdin: Some(crate::corpus::MBOX),
    },
    // A conflicting `stash pop`: unmerged index, `AUTO_MERGE` written, and the
    // entry *kept*. `stash` has no resumption verbs of its own, which is the
    // point — every verb the walk draws here is an illegal transition against a
    // state that is genuinely stuck, and that is the corner where a port stops
    // having documentation to copy.
    Stopper {
        cmd: "stash",
        name: "stash-pop-conflict",
        shape: Shape::Stashed,
        setup: &[
            &["stash", "push", "-m", "gen"],
            &["stash", "pop", "stash@{3}"],
            &["commit", "-am", "gen-base"],
        ],
        entry: &[&["stash", "pop"]],
        entry_stdin: None,
    },
    // The one premise here that is not an interrupted *operation*: a repository
    // that cannot be read, and a maintenance verb told to rewrite it. It is a
    // stopper because the machinery fits and nothing else in this file reaches
    // it — `gc`'s grammar is generated and its shape list, which this file does
    // not own, has no `Damaged`, so `gen/observe/gc` can never draw one.
    //
    // What it measures is a refusal that has to leave **nothing behind**.
    // Verified against stock 2.55.0 on [`Shape::Damaged`]:
    //
    //   gc --prune=now   rc 128
    //       `error: refs/heads/dangling does not point to a valid object!`
    //       `fatal: bad object refs/heads/dangling`
    //       `fatal: failed to run repack`
    //   .git/objects     byte for byte what it was — the corrupt loose object
    //                    `ab12345…` still there, no `pack/`, no `gc.log`
    //   gc               the same three lines and the same rc, run twice
    //   prune            rc 128, `fatal: unable to parse object: refs/heads/dangling`
    //
    // `runner::probe_storage` walks `.git/objects`, so a port that deletes the
    // corrupt object, or that leaves the half-written pack of a repack it could
    // not finish, is a state difference at this step rather than a mystery in a
    // later one. That is the specific defect the curated corpus found from the
    // other direction, and a generated walk is how a *drawn* `gc` argument
    // reaches it: a third of walks decorate the entry from `gc`'s own flag pool
    // (`--aggressive`, `--cruft`, `--keep-largest-pack`, `--no-prune`), which is
    // eleven more ways to ask the same question.
    //
    // The resumption verbs the walk then draws are all cross-machine, exactly as
    // for `stash-pop-conflict` above, and on this shape they are the ordinary
    // refusals — verified: `cherry-pick --abort` is `error: no cherry-pick or
    // revert in progress`, `merge --abort` is `fatal: There is no merge to abort
    // (MERGE_HEAD missing).`, `rebase --continue` is `fatal: no rebase in
    // progress`. The damage does not change them, so what those steps are worth
    // is the state comparison after each one, not their stdout.
    Stopper {
        cmd: "gc",
        name: "gc-damaged-store",
        shape: Shape::Damaged,
        setup: &[],
        entry: &[&["gc", "--prune=now"]],
        entry_stdin: None,
    },
    // A conflict git has **seen before**, replayed out of `.git/rr-cache`.
    //
    // Every other premise in this list produces its parked state from the
    // objects alone. This one produces it from a side store the merge machinery
    // writes and reads and nothing else in the repository refers to, and until
    // [`Shape::Rerere`] existed no fixture had one: a case is one argv, so it
    // cannot conflict, resolve, and then ask about the resolution. The setup
    // step unwinds the merge the shape is parked in and the entry re-runs it, so
    // the replay happens **inside the case** rather than at build time — which
    // is the only way stdout can show it.
    //
    // Verified against stock 2.55.0 on the shape:
    //
    //   merge --abort        rc 0, `status --porcelain` empty
    //   merge rr-side        rc 1, and on stdout:
    //       `Recorded preimage for 'fresh.txt'`
    //       `Resolved 'other.txt' using previous resolution.`
    //       `Resolved 'rr.txt' using previous resolution.`
    //   status --short       `AA fresh.txt`, `UU other.txt`, `UU rr.txt`
    //   cat rr.txt           `resolved one` / `base two` / `resolved three`
    //                        — the recorded text, with no conflict markers
    //   cat fresh.txt        `<<<<<<< HEAD` … the markers, still there
    //   rerere status        `fresh.txt`      rerere remaining   `fresh.txt`
    //
    // So one entry step reaches all three outcomes at once: a resolution
    // replayed, a preimage recorded, and an index whose stages disagree with a
    // worktree that is not conflicted. `runner`'s op-state probe reads
    // `MERGE_RR` by name, so a port that resolves from the cache without
    // updating it — or that updates it without resolving — is a state difference
    // at this step. `rerere.enabled` is in the shape's repository config, so the
    // replay does not depend on a drawn configuration key.
    Stopper {
        cmd: "merge",
        name: "merge-rerere-replay",
        shape: Shape::Rerere,
        setup: &[&["merge", "--abort"]],
        entry: &[&["merge", "rr-side"]],
        entry_stdin: None,
    },
    // A hook that **refuses**, mid-workflow.
    //
    // [`Shape::Hooked`] installs hooks that all exit 0, deliberately, so until
    // [`Shape::HooksFail`] existed `--no-verify` could not be measured at all:
    // with no hook that refuses, skipping the hooks and running them are the
    // same outcome. This is the premise that separates them, and the separation
    // is drawn rather than fixed: a third of these walks decorate the entry from
    // `commit`'s own flag pool, which carries `--no-verify`, `-n`, `--dry-run`
    // and `--amend` — so a share of them turn the refusal into something else on
    // purpose, which is the same gate every other stopper's entry is under.
    //
    // Verified against stock 2.55.0 on the shape, whose worktree is left dirty
    // so `commit -a` has something to be refused over:
    //
    //   commit -a -m gen              rc 1, `pre-commit refuses` on stderr
    //   status --short                ` M side-base.txt`, `?? hook-pre-commit.txt`
    //                                 — the hook ran and wrote, and nothing
    //                                 committed
    //   log --oneline -1              unchanged
    //   commit -a -m gen --no-verify  rc 0, `[main a53d299] gen`
    //   log -1 --format=%B            `gen` **plus** `prepared-by-hook`
    //   ls hook-*.txt                 pre-commit, prepare-commit-msg, post-commit
    //
    // The last two lines are the finding this premise exists for and neither is
    // obvious: `--no-verify` does **not** skip `prepare-commit-msg`, so a commit
    // that bypassed the gate still went through the message rewrite, and
    // `post-commit` exits 1 while the commit stands, because git ignores that
    // hook's status. A port that treats `--no-verify` as "run no hooks", or that
    // propagates a `post-commit` failure, disagrees on exactly one of those and
    // on nothing else.
    Stopper {
        cmd: "commit",
        name: "commit-hook-refused",
        shape: Shape::HooksFail,
        setup: &[],
        entry: &[&["commit", "-a", "-m", "gen"]],
        entry_stdin: None,
    },
    // The same refusal over a transport, and then the one `--no-verify` cannot
    // bypass.
    //
    // Two entry steps because the pair is the measurement. The first is refused
    // on **this** side by `pre-push`; the second skips the local hooks and is
    // refused on the **other** side by the peer's `update` hook, which no flag
    // on this side can reach. Verified against stock 2.55.0 on the shape, whose
    // peer is the bare `./.remote.git` inside the fixture:
    //
    //   push origin main                 rc 1, `pre-push refuses`,
    //       `error: failed to push some refs to './.remote.git'`;
    //       `hook-pre-push.txt` written, remote refs unmoved
    //   push --dry-run origin main       rc 1 — the **same** refusal: a dry run
    //       still runs the hook
    //   push --no-verify origin main     rc 0, `91ddf90..f32913c  main -> main`
    //   push --no-verify origin veto     rc 1,
    //       `remote: error: hook declined to update refs/heads/veto`,
    //       `! [remote rejected] veto -> veto (hook declined)`
    //
    // `--dry-run` and `--no-verify` are two flags a port is likely to wire to
    // one "skip the side effects" branch, and stock says they do opposite
    // things: one runs the hook and writes nothing, the other writes and runs no
    // hook. Both are in `push`'s flag pool, so the decoration draw reaches that
    // pairing on its own. `runner::probe_peer` takes the peer's ref list and
    // object census after every step, so which of the two refusals moved the
    // remote is answered rather than inferred.
    //
    // Nothing here can reach the network: the peer is a relative path inside the
    // fixture and every copy resolves to its own.
    Stopper {
        cmd: "push",
        name: "push-hook-refused",
        shape: Shape::HooksFail,
        setup: &[],
        entry: &[&["push", "origin", "main"], &["push", "--no-verify", "origin", "veto"]],
        entry_stdin: None,
    },
    // A shallow repository deepened, and then read.
    //
    // Not an interrupted operation — the same exception [`gc-damaged-store`]
    // above is, and for the same reason: the machinery fits, and nothing else in
    // this file can reach the premise. `fetch`'s generated grammar carries
    // `--deepen=1`, `--depth=`, `--unshallow`, `--shallow-since=` and
    // `--update-shallow`, and its shape list — which this file does not own —
    // has no `Shallow`, so every one of those flags has only ever been drawn
    // against a repository that had nothing to deepen.
    //
    // Verified against stock 2.55.0 on the shape:
    //
    //   log --oneline                 two commits, stopping at the graft
    //   .git/shallow                  `bd1c76c6…`, `fc222945…`
    //   fetch --deepen=1              rc 0, 0.12s wall
    //   .git/shallow                  rewritten: `db3a7471…`, `edfab1b7…`
    //   log --oneline                 three commits
    //   rev-parse --is-shallow-repository   `true`
    //   fsck                          rc 0, silent
    //   fetch --unshallow             rc 0, 0.10s; `.git/shallow` **removed**,
    //                                 `log --oneline` six commits
    //
    // `.git/shallow` is in `runner`'s op-state file list, so the graft boundary
    // is compared after every step of the walk whatever the step was — which is
    // what makes the observers worth their invocation here even though none of
    // them prints the file. The transport is the fixture's own `./.remote.git`;
    // no draw can block on a fetch that cannot complete, because the one it can
    // make is a local directory read.
    Stopper {
        cmd: "fetch",
        name: "shallow-deepen",
        shape: Shape::Shallow,
        setup: &[],
        entry: &[&["fetch", "--deepen=1"]],
        entry_stdin: None,
    },
];

/// The commands with a `--continue`/`--skip`/`--abort`/`--quit` state machine.
///
/// A fact about `sequencer.c` and `builtin/am.c`, not about any flag table: it is
/// the set a resumption verb may be *addressed to*, and drawing the verb's
/// command from here rather than from the stopper is what produces the illegal
/// cross-machine transitions this family exists for.
const STATEFUL: &[&str] = &["cherry-pick", "revert", "rebase", "merge", "am"];

/// The state machine's alphabet.
const RESUME_TOKENS: &[&str] = &["--continue", "--skip", "--abort", "--quit"];

/// Transitions and state queries only some machines have, taken from the command's
/// own grammar so a command that does not have one never sees it.
///
/// This is the half of the alphabet that *is* derivable: `--retry` belongs to
/// `am` alone and `--edit-todo` to `rebase` alone, and the grammar already knows
/// which. `--show-current-patch` is a query rather than a transition and is here
/// because reading the parked state is the cheapest way to catch a machine that
/// stopped in the wrong place.
const RESUME_EXTRA: &[&str] = &["--retry", "--edit-todo", "--show-current-patch"];

/// `git bisect`'s alphabet.
///
/// Stated rather than filtered out of the bisect grammar's positionals because
/// that list mixes verbs and revs in one flat pool with no marking of which is
/// which — a walk drawn from it uniformly spends most of its steps on
/// `bisect v0.1.0`, which is a usage error rather than a transition. The grammar
/// still supplies this family's flags, and [`REVS`] supplies the operands.
/// `run` and `replay` are the two verbs that drive the machine from outside it:
/// `run` answers every step from a command's exit status instead of from a
/// caller, and `replay` re-feeds a log. Both are reachable only here — the
/// grammar's positionals do not distinguish a verb from a rev. `visualize`/
/// `view` are deliberately absent: they launch a viewer.
///
/// `run`'s operand is **not** drawn from [`REVS`], and that is a correctness
/// requirement rather than a preference: `bisect run <word>` *executes* the
/// word. `HEAD` is the obvious rev to draw and is also `/usr/bin/HEAD`, the
/// libwww-perl request tool, which with no URL reads stdin — verified against
/// stock 2.55.0, `git bisect run HEAD` inside a started bisect printed
/// `running 'HEAD'` and then blocked until a 20-second timeout killed it. A
/// generator that hands this verb a rev pool is a generator that hangs on a
/// machine where some rev happens to name a program. [`BISECT_RUN_COMMANDS`]
/// supplies the operand instead.
const BISECT_VERBS: &[&str] = &[
    "start", "good", "bad", "skip", "next", "log", "terms", "reset", "old", "new", "help",
    "run", "replay",
];

/// The only operands `bisect run` is ever given.
///
/// Two programs that exist on every POSIX machine, take no input and terminate
/// at once, which between them reach both of `run`'s outcomes on the fixtures'
/// short histories. Verified against stock 2.55.0 on `Branched` with `bad`/
/// `good` already answered: `bisect run false` converges and prints
/// `<oid> is the first 'bad' commit` with rc 0, and `bisect run true` marks the
/// same commit both ways and answers
/// `error: bisect run failed: 'git bisect good' exited with error code -1` with
/// rc 1. The empty entry is `bisect run` with no command at all, which is
/// `error: 'git bisect run' failed: no command provided.`
const BISECT_RUN_COMMANDS: &[&str] = &["false", "true", ""];

/// Read-only invocations whose answer a mutation is supposed to change.
///
/// The selection criterion is that one: every entry reports a *fact about the
/// repository* rather than about its own arguments, so putting one after a
/// mutation asks whether the mutation landed. A reader whose output is a
/// function of its argv alone would pass identically before and after any write
/// and would only cost an invocation.
///
/// All of them are read-only. `status` refreshes the index as a side effect, but
/// it does so on both sides and the curated corpus already interleaves it inside
/// stateful workflows, so it is proven not to be a difference by itself.
///
/// The observer is drawn uniformly and is therefore often *not* the reader that
/// would show a given mutation — `show-ref` says nothing about a moved file.
/// That is deliberate and costs nothing, because the observer is not the oracle:
/// [`crate::runner::run_sequence`] takes the full state comparison after every
/// step regardless of what the step was, so a wrong write is caught whether or
/// not the reader beside it would have printed it. What the observer adds is a
/// *readable* surface for the difference and one more invocation of a reader
/// against a repository state no fixture describes — which is why matching
/// observers to mutators by hand would buy nothing and cost a table.
const OBSERVERS: &[&[&str]] = &[
    &["status", "--porcelain"],
    &["status", "--porcelain=v2", "--branch"],
    &["rev-parse", "HEAD"],
    &["rev-parse", "--abbrev-ref", "HEAD"],
    &["log", "--oneline", "--all"],
    &["reflog"],
    &["for-each-ref", "--format=%(refname) %(objectname) %(upstream)"],
    &["ls-files", "--stage"],
    &["diff", "--name-status"],
    &["diff", "--cached", "--name-status"],
    &["stash", "list"],
    &["branch", "--list", "-a"],
    &["tag", "--list"],
    &["worktree", "list"],
    &["show-ref", "--head"],
    // The index *flags*, which no other reader here prints. `probe_state` uses
    // `ls-files --stage`, and that layout shows mode, oid and stage while saying
    // nothing about assume-unchanged, skip-worktree or fsmonitor-valid — so a
    // step that set one of those bits and a step that did not produced the same
    // observation. `-v` is the layout that distinguishes them (`H` against `h`,
    // verified against stock 2.55.0 either side of
    // `update-index --assume-unchanged`), which is what makes the
    // `update-index-assume-unchanged` round trip below observable at the step
    // that failed rather than not at all.
    &["ls-files", "-v"],
    // What the working tree and the index think the line endings are. The
    // `crlf` config group changes exactly this, and nothing read it back:
    // `status` reports a path as modified or not, which conflates a conversion
    // that happened with one that was not needed.
    &["ls-files", "--eol"],
    // Where each setting came from after the step ran. `probe_state` compares
    // `config --list --local`, which is the file; this is the *resolved* view
    // with its origin, so a step that wrote the right value into the wrong scope
    // is named by the observer rather than inferred from a later difference.
    &["config", "--list", "--show-scope"],
    // The sparsity patterns. This is the only observer that reads a file the
    // state probe does not: `probe_op_state` names the files it reads and
    // `.git/info/sparse-checkout` is not among them, `probe_storage` looks only
    // at `.git/objects`, and `ls-files --stage` — what `probe_state` compares —
    // prints an identical line for an entry whether or not it is sparse. So a
    // step that wrote the right skip-worktree bits from the wrong patterns, or
    // the patterns without the bits, was invisible to every reader here.
    // Verified against stock 2.55.0 on a repository with no patterns:
    // `fatal: this worktree is not sparse`, rc 128 — a deterministic answer on
    // the shapes where it says nothing, which is what makes it safe to draw
    // uniformly beside the readers that always answer.
    &["sparse-checkout", "list"],
];

/// Verbs whose purpose is to write repository state.
///
/// No grammar carries this: a grammar describes a command line, and nothing in a
/// command line says whether running it changes anything. The criterion for
/// membership is narrower than "mutates" — it is **mutates something an
/// [`OBSERVERS`] entry or the state probe reports**. A write no reader can see
/// makes the steps after it noise, which is the failure mode this whole section
/// is built to avoid, so `var` and `check-ignore` are absent while `gc` and
/// `repack` are present (`runner::probe_storage` walks the object layout they
/// rewrite).
///
/// Every name here must have a grammar; [`every_generator_verb_has_a_grammar`]
/// asserts it, so a rename in the generated grammars fails `cargo test` instead
/// of silently dropping a family member from every future run.
const MUTATORS: &[&str] = &[
    "add", "am", "apply", "checkout", "checkout-index", "cherry-pick", "clean", "commit",
    "commit-graph", "commit-tree", "fast-import", "fetch", "filter-branch", "gc", "merge",
    "mktag", "multi-pack-index", "mv", "notes", "pack-refs", "prune", "prune-packed", "pull",
    "push", "read-tree", "rebase", "reflog", "refs", "remote", "repack", "replace", "replay",
    "rerere", "reset", "restore", "revert", "rm", "sparse-checkout", "stage", "stash",
    "submodule", "switch", "symbolic-ref", "update-index", "update-ref", "worktree",
    "write-tree",
];

/// An operation and its inverse, with the shape the pair is meaningful on.
struct RoundTrip {
    cmd: &'static str,
    name: &'static str,
    shape: Shape,
    forward: &'static [&'static [&'static str]],
    inverse: &'static [&'static [&'static str]],
    /// Reads that **name the thing the forward half created**, run once between
    /// the halves and again after the inverse.
    ///
    /// The one dependency a generated step could not express until now. Every
    /// other step in this file is either a fixed script or an argv drawn from a
    /// pool, and [`OBSERVERS`] is drawn uniformly and deliberately names nothing —
    /// which is right for a family whose oracle is the state probe, and is why a
    /// wrong write shows up there as "some step changed the repository" rather
    /// than as an answer to a question about the thing that was written.
    ///
    /// A read here is the answer to that question. `stash push -m gen` is
    /// followed by `log -g --format=%gd %gs refs/stash`, which prints
    /// `stash@{0} On main: gen` — a *value the earlier step produced*, read back
    /// under the name that step chose. Nothing about that breaks the rule that a
    /// step's argv is a compile-time literal, because the name is the literal:
    /// the pair already had to name `gen` to create it, and the read names the
    /// same string. What the read cannot do is carry an id forward — no step can
    /// substitute an oid a previous step printed — so every entry below is
    /// phrased as a question whose *answer* is the produced value rather than as
    /// an argument containing it.
    ///
    /// Run **twice** on purpose, and the second run is the half that pays: after
    /// the inverse the same question must have the other answer, and the two
    /// answers are usually a value and a refusal — `refs/heads/gen-ref` resolving
    /// to an oid, then `fatal: Needed a single revision` at rc 128. A port whose
    /// inverse half-cleans answers the first reading correctly and the second one
    /// with the value that should be gone, at the step that asked rather than
    /// three steps later.
    ///
    /// Empty for the five pairs whose artifact is already named by something
    /// else, since a second reader of the same fact costs an invocation per side
    /// and adds nothing: `sparse-init-disable` (the `sparse-checkout list`
    /// observer), `bisect-log-replay` (`bisect log`, in its own halves),
    /// `bundle-create-unbundle` (`bundle verify` and `list-heads`, in its forward
    /// half), `read-tree-back` and `apply-symlink-patch`, whose artifact is the
    /// whole index or the whole worktree and is what `probe_state` compares
    /// first.
    reads: &'static [&'static [&'static str]],
    /// Payload for the **first** forward step, and only for it.
    ///
    /// Narrow on purpose. Two of the pairs below are round trips through a
    /// *stream* rather than through a repository object — `fast-import` and
    /// `update-ref --stdin` both take their whole instruction list on stdin —
    /// and there is no other way to spell them: a step's argv cannot redirect a
    /// file, and the payload has to be a `&'static [u8]` literal for the case to
    /// replay byte for byte. Every other pair leaves this `None`, and no inverse
    /// step ever takes one, because the half that *reads back* what the forward
    /// half wrote reads it from the repository — which is the property this
    /// family exists to measure.
    ///
    /// [`generated_steps_only_get_stdin_where_it_is_read`] holds this to the same
    /// rule as every other payload in the file: the step it lands on must be one
    /// [`wants_stdin`] says reads input.
    forward_stdin: Option<&'static [u8]>,
}

/// The inverse pairs.
///
/// Written out rather than drawn from the grammars, and the reason is the one
/// thing this family measures: *inverseness*. A grammar-drawn flag on the
/// forward step silently destroys it — `stash push --keep-index` changes what
/// `pop` restores, `worktree add --detach` changes what `remove` has to clean —
/// and a round-trip whose inverse no longer inverts is a sequence whose premise
/// its own first step destroyed. That is the nonsense case this file must not
/// generate, so the pairs are exact and the drawn part is which pair runs, which
/// of the pair's [`RoundTrip::reads`] is asked, which observers sit between the
/// halves, and the envelope.
///
/// Both halves are still compared step by step like everything else, so the
/// finding is never "these differ after four commands": a `disable` that leaves
/// `.git/info/sparse-checkout` behind is a state difference at the `disable`.
const ROUND_TRIPS: &[RoundTrip] = &[
    RoundTrip {
        cmd: "stash",
        name: "stash-push-pop",
        shape: Shape::Dirty,
        forward: &[&["stash", "push", "-m", "gen"]],
        inverse: &[&["stash", "pop"]],
        reads: &[
            &["stash", "show", "--stat", "stash@{0}"],
            &["log", "-g", "--format=%gd %gs", "refs/stash"],
        ],
        forward_stdin: None,
    },
    // The untracked half: `-u` stashes a file that was never in the index, and
    // popping it has to put it back *untracked*, which is a different code path
    // from restoring a tracked modification.
    RoundTrip {
        cmd: "stash",
        name: "stash-push-untracked-pop",
        shape: Shape::Dirty,
        forward: &[&["stash", "push", "-u", "-m", "gen"]],
        inverse: &[&["stash", "pop"]],
        reads: &[
            &["stash", "show", "--include-untracked", "--stat", "stash@{0}"],
            &["log", "-g", "--format=%gd %gs", "refs/stash"],
        ],
        forward_stdin: None,
    },
    RoundTrip {
        cmd: "branch",
        name: "branch-rename-back",
        shape: Shape::Linear,
        forward: &[&["branch", "-m", "main", "gen-renamed"]],
        inverse: &[&["branch", "-m", "gen-renamed", "main"]],
        reads: &[
            &["log", "-g", "--format=%gd %gs", "refs/heads/gen-renamed"],
            &["rev-parse", "--verify", "refs/heads/gen-renamed"],
        ],
        forward_stdin: None,
    },
    // `add` writes `.git/worktrees/<n>/{gitdir,HEAD,commondir}` and a `.git`
    // file in the new tree; `remove` has to delete both ends of that pair.
    RoundTrip {
        cmd: "worktree",
        name: "worktree-add-remove",
        shape: Shape::Branched,
        forward: &[&["worktree", "add", "-b", "gen-wtb", "wt-gen"]],
        inverse: &[&["worktree", "remove", "wt-gen"]],
        reads: &[
            &["worktree", "list", "--porcelain"],
            &["rev-parse", "--verify", "refs/heads/gen-wtb"],
        ],
        forward_stdin: None,
    },
    RoundTrip {
        cmd: "worktree",
        name: "worktree-lock-unlock",
        shape: Shape::Worktree,
        forward: &[&["worktree", "lock", "wt"]],
        inverse: &[&["worktree", "unlock", "wt"]],
        reads: &[
            &["worktree", "list", "--porcelain"],
        ],
        forward_stdin: None,
    },
    RoundTrip {
        cmd: "sparse-checkout",
        name: "sparse-set-disable",
        shape: Shape::Sparse,
        forward: &[&["sparse-checkout", "set", "inside"]],
        inverse: &[&["sparse-checkout", "disable"]],
        reads: &[
            &["ls-files", "-t", "outside"],
        ],
        forward_stdin: None,
    },
    RoundTrip {
        cmd: "sparse-checkout",
        name: "sparse-init-disable",
        shape: Shape::Linear,
        forward: &[&["sparse-checkout", "init", "--cone"]],
        inverse: &[&["sparse-checkout", "disable"]],
        reads: &[],
        forward_stdin: None,
    },
    RoundTrip {
        cmd: "tag",
        name: "tag-add-delete",
        shape: Shape::Branched,
        forward: &[&["tag", "gen-tag", "HEAD"]],
        inverse: &[&["tag", "-d", "gen-tag"]],
        reads: &[
            &["rev-parse", "--verify", "refs/tags/gen-tag"],
            &["cat-file", "-t", "gen-tag"],
        ],
        forward_stdin: None,
    },
    // `switch -` resolves `@{-1}` out of the reflog, so the inverse half reads
    // state the forward half wrote into a place neither command names.
    RoundTrip {
        cmd: "switch",
        name: "switch-create-back",
        shape: Shape::Branched,
        forward: &[&["switch", "-c", "gen-branch"]],
        inverse: &[&["switch", "-"], &["branch", "-D", "gen-branch"]],
        reads: &[
            &["log", "-g", "--format=%gd %gs", "refs/heads/gen-branch"],
        ],
        forward_stdin: None,
    },
    RoundTrip {
        cmd: "checkout",
        name: "checkout-create-back",
        shape: Shape::Branched,
        forward: &[&["checkout", "-b", "gen-co"]],
        inverse: &[&["checkout", "main"], &["branch", "-D", "gen-co"]],
        reads: &[
            &["log", "-g", "--format=%gd %gs", "refs/heads/gen-co"],
        ],
        forward_stdin: None,
    },
    RoundTrip {
        cmd: "update-ref",
        name: "update-ref-create-delete",
        shape: Shape::Linear,
        forward: &[&["update-ref", "refs/heads/gen-ref", "HEAD"]],
        inverse: &[&["update-ref", "-d", "refs/heads/gen-ref"]],
        reads: &[
            &["log", "-g", "--format=%gd %gs", "refs/heads/gen-ref"],
            &["rev-parse", "--verify", "refs/heads/gen-ref"],
        ],
        forward_stdin: None,
    },
    RoundTrip {
        cmd: "remote",
        name: "remote-add-remove",
        shape: Shape::BehindRemote,
        forward: &[&["remote", "add", "gen", "./.remote.git"]],
        inverse: &[&["remote", "remove", "gen"]],
        reads: &[
            &["remote", "get-url", "gen"],
            &["config", "--get", "remote.gen.url"],
        ],
        forward_stdin: None,
    },
    RoundTrip {
        cmd: "notes",
        name: "notes-add-remove",
        shape: Shape::Linear,
        forward: &[&["notes", "add", "-m", "gen note", "HEAD"]],
        inverse: &[&["notes", "remove", "HEAD"]],
        reads: &[
            &["notes", "show", "HEAD"],
            &["rev-parse", "--verify", "refs/notes/commits"],
        ],
        forward_stdin: None,
    },
    RoundTrip {
        cmd: "commit",
        name: "commit-then-reset",
        shape: Shape::Linear,
        forward: &[&["commit", "--allow-empty", "-m", "gen"]],
        inverse: &[&["reset", "--hard", "HEAD~1"]],
        reads: &[
            &["log", "-g", "--format=%gd %gs", "HEAD"],
            &["log", "-1", "--format=%s %P"],
        ],
        forward_stdin: None,
    },
    RoundTrip {
        cmd: "add",
        name: "add-then-restore-staged",
        shape: Shape::Dirty,
        forward: &[&["add", "untracked.txt"]],
        inverse: &[&["restore", "--staged", "untracked.txt"]],
        reads: &[
            &["ls-files", "--stage", "untracked.txt"],
        ],
        forward_stdin: None,
    },
    RoundTrip {
        cmd: "rm",
        name: "rm-cached-then-add",
        shape: Shape::Linear,
        forward: &[&["rm", "--cached", "README.md"]],
        inverse: &[&["add", "README.md"]],
        reads: &[
            &["ls-files", "--stage", "README.md"],
        ],
        forward_stdin: None,
    },
    RoundTrip {
        cmd: "config",
        name: "config-set-unset",
        shape: Shape::Linear,
        forward: &[&["config", "gen.key", "value"]],
        inverse: &[&["config", "--unset", "gen.key"]],
        reads: &[
            &["config", "--get", "gen.key"],
        ],
        forward_stdin: None,
    },
    RoundTrip {
        cmd: "mv",
        name: "mv-there-and-back",
        shape: Shape::Linear,
        forward: &[&["mv", "README.md", "gen-moved.md"]],
        inverse: &[&["mv", "gen-moved.md", "README.md"]],
        reads: &[
            &["ls-files", "--stage", "gen-moved.md"],
            &["diff", "--cached", "-M", "--name-status"],
        ],
        forward_stdin: None,
    },
    RoundTrip {
        cmd: "symbolic-ref",
        name: "symref-set-delete",
        shape: Shape::Linear,
        forward: &[&["symbolic-ref", "refs/gen-sym", "refs/heads/main"]],
        inverse: &[&["symbolic-ref", "-d", "refs/gen-sym"]],
        reads: &[
            &["symbolic-ref", "refs/gen-sym"],
        ],
        forward_stdin: None,
    },
    // `--soft` moves only `HEAD` and records `ORIG_HEAD`; the inverse reads that
    // record back, so a port that moves the branch without writing `ORIG_HEAD`
    // fails at the inverse rather than at the step that skipped the write.
    RoundTrip {
        cmd: "reset",
        name: "reset-soft-orig-head",
        shape: Shape::Branched,
        forward: &[&["reset", "--soft", "HEAD~1"]],
        inverse: &[&["reset", "--soft", "ORIG_HEAD"]],
        reads: &[
            &["rev-parse", "--verify", "ORIG_HEAD"],
            &["log", "-g", "--format=%gd %gs", "HEAD"],
        ],
        forward_stdin: None,
    },
    RoundTrip {
        cmd: "read-tree",
        name: "read-tree-back",
        shape: Shape::Branched,
        forward: &[&["read-tree", "HEAD~1"]],
        inverse: &[&["read-tree", "HEAD"]],
        reads: &[],
        forward_stdin: None,
    },
    // The only way this harness can produce an **ambiguous refname**. `Branched`
    // has a branch called `feature`; tagging `feature` makes one name resolve
    // through two rules at once, and the fixtures cannot be edited from here to
    // arrange that statically. Verified against stock 2.55.0: with both refs
    // present, `rev-parse feature` prints
    // `warning: refname 'feature' is ambiguous.` and answers with the **tag**
    // (`refs/tags/` precedes `refs/heads/` in `refs.c:ref_rev_parse_rules`, and
    // the tag was cut one commit behind the branch, so the two answers are
    // different oids rather than the same one),
    // `show-ref feature` lists `refs/heads/feature` and `refs/tags/feature`, and
    // `tag -d feature` prints `Deleted tag 'feature' (was 5915d79)` and leaves
    // the branch behind. Every observer between the
    // halves — and `feature` in [`REVS`], drawn by any step in the window — is
    // then asking a question with two right-looking answers, which is where a
    // port that walks `ref_rev_parse_rules` in the wrong order stops agreeing.
    RoundTrip {
        cmd: "tag",
        name: "tag-shadows-branch",
        shape: Shape::Branched,
        forward: &[&["tag", "feature", "HEAD"]],
        inverse: &[&["tag", "-d", "feature"]],
        reads: &[
            &["show-ref", "feature"],
            &["rev-parse", "feature"],
        ],
        forward_stdin: None,
    },
    // An index *flag* round trip. Every other pair here moves a ref, a file or a
    // stanza; this one changes a bit inside `.git/index` and nothing else — no
    // ref moves, no content changes, and `ls-files --stage` (which is what
    // `probe_state` compares) prints the same three lines either way. The
    // difference is only visible to the `ls-files -v` observer added above, and
    // that is the point: a port whose `--no-assume-unchanged` clears the wrong
    // bit, or clears it for every entry, ends the round trip in a state that
    // looks identical to every reader that does not ask.
    RoundTrip {
        cmd: "update-index",
        name: "update-index-assume-unchanged",
        shape: Shape::Linear,
        forward: &[&["update-index", "--assume-unchanged", "README.md"]],
        inverse: &[&["update-index", "--no-assume-unchanged", "README.md"]],
        reads: &[
            &["ls-files", "-v", "README.md"],
        ],
        forward_stdin: None,
    },
    // The only pair whose two halves are the *same command* with `-R` between
    // them, and the only one that moves a file mode rather than a ref, an entry
    // or a stanza. `patches/symlink.patch` is `diff main sym-pending`, written
    // into the fixture at build time, and it does three things a patch in this
    // harness had never done: create a symlink, create a zero-byte file, and
    // replace a regular file with a symlink — the `T` of `--raw`. Verified
    // against stock 2.55.0 on [`Shape::Symlinks`]:
    //
    //   status --porcelain   ` M link-wt`, `?? stray-empty.txt`, `?? stray-link`
    //   apply patches/symlink.patch    rc 0
    //   status --porcelain   the three above **plus** ` T dir/target.txt`,
    //                        `?? later-empty.txt`, `?? later-link`
    //   apply -R patches/symlink.patch rc 0
    //   status --porcelain   the original three, exactly
    //
    // The equality of the first and last of those is the assertion, and it is
    // what `runner::probe_state` compares first. A port whose `apply` writes the
    // symlink as a regular file containing its target ends the round trip with a
    // `100644` where a `120000` belongs and the reverse half then fails to
    // recognise its own preimage; one that handles creation and not type change
    // ends it with `dir/target.txt` still a symlink. `apply`'s generated grammar
    // has no `Symlinks` in its shape list, so nothing else in this file can hand
    // that command a patch with a mode change in it. One entry point: one
    // sequence per unit of `--fuzz-sequences`, four to eight steps depending on
    // how many observers the draw puts around the two halves.
    RoundTrip {
        cmd: "apply",
        name: "apply-symlink-patch",
        shape: Shape::Symlinks,
        forward: &[&["apply", "patches/symlink.patch"]],
        inverse: &[&["apply", "-R", "patches/symlink.patch"]],
        reads: &[],
        forward_stdin: None,
    },
    // ----------------------------------------------------------------------
    // Pairs where one verb writes a *file* and another reads it back.
    //
    // The five below close a gap `corpus/transport_local.rs` states in its own
    // header as unclosable — "a round trip through a file the case itself wrote
    // … is two invocations; a case is one" — and it is unclosable for a case and
    // not for a sequence. Until now the read halves were only ever measured on
    // their error paths: `bundle unbundle` against a file that is not a bundle,
    // `am` against a mailbox nobody produced. Each pair costs one entry point,
    // which is `--fuzz-sequences` sequences of five to eight steps per side.
    // ----------------------------------------------------------------------

    // `bundle create` writes a packfile with a ref list on the front and three
    // readers parse it back. Verified against stock 2.55.0 on a repository with
    // two branches and two tags: `create gen.bundle --all` exits 0, `verify`
    // prints the four refs and `The bundle records a complete history.`,
    // `list-heads` prints the same four lines without the prose, and `unbundle`
    // prints them again while unpacking the pack into `.git/objects/pack` —
    // creating **no refs**, which is why this pair returns the repository to a
    // state equal to the one it started from with only `gen.bundle` untracked
    // beside it. The file is written into the worktree rather than under `.git`
    // deliberately: `status --untracked-files=all` is the first thing
    // `probe_state` runs, so a port that writes the bundle somewhere else is a
    // difference at the step that wrote it rather than at the step that failed
    // to read it.
    RoundTrip {
        cmd: "bundle",
        name: "bundle-create-unbundle",
        shape: Shape::Branched,
        forward: &[
            &["bundle", "create", "gen.bundle", "--all"],
            &["bundle", "verify", "gen.bundle"],
            &["bundle", "list-heads", "gen.bundle"],
        ],
        inverse: &[&["bundle", "unbundle", "gen.bundle"]],
        reads: &[],
        forward_stdin: None,
    },
    // The mail round trip, and the one whose end state is *byte-identical* to
    // its start state rather than merely equivalent. `format-patch` serializes a
    // commit into a mail, `reset --hard` throws the commit away, and `am`
    // reconstructs it from the mail — and because `env::harden` pins both
    // identities and `GIT_COMMITTER_DATE`, and `format-patch` carries the author
    // date in the mail header, the reconstruction has the same object id.
    // Verified against stock 2.55.0 on a two-commit history: `rev-parse HEAD`
    // read `d775b986a6d95734898cf9348813782f191d40d0` before the round trip and
    // `d775b986a6d95734898cf9348813782f191d40d0` after it, with
    // `Applying: add two` and rc 0 from `am`.
    //
    // That equality is the assertion. A port whose `format-patch` drops a
    // header, or whose `am` fills a missing one from the clock, ends with a
    // different commit id and the step comparison names which of the two did it.
    // `--numbered-files` is what makes the pair expressible at all: without it
    // the output is `0001-<subject>.patch`, whose name is a function of the
    // fixture's commit message, and a step's argv is a literal.
    RoundTrip {
        cmd: "format-patch",
        name: "format-patch-am",
        shape: Shape::Branched,
        forward: &[
            &["format-patch", "--numbered-files", "-o", "gen-patches", "HEAD~1..HEAD"],
            &["reset", "--hard", "HEAD~1"],
        ],
        inverse: &[&["am", "gen-patches/1"]],
        reads: &[
            &["apply", "--stat", "gen-patches/1"],
        ],
        forward_stdin: None,
    },
    // The stream round trip. `fast-import` is the largest input language in git
    // and, until [`P_FAST_IMPORT`] existed, this harness never handed it one —
    // see [`STDIN_ALWAYS`] for what that cost. Here the stream creates a root
    // commit on `refs/heads/gen-fi`, `fast-export --all` serializes the whole
    // repository back out through the reader half of the same format, and the
    // inverse removes the ref the stream created.
    //
    // `Linear` is the shape because `fast-export --all` prints every object in
    // the repository and the point is the round trip, not the volume. The
    // `--quiet` on the import is not cosmetic: the statistics block it
    // suppresses is written to stderr, which generated steps do not compare, but
    // it also contains `Memory total` and an allocator count, and leaving a
    // machine-dependent number out of a stream nobody reads is cheaper than
    // explaining every time why it is allowed to differ.
    RoundTrip {
        cmd: "fast-import",
        name: "fast-import-export",
        shape: Shape::Linear,
        forward: &[&["fast-import", "--quiet", "--done"], &["fast-export", "--all"]],
        inverse: &[&["update-ref", "-d", "refs/heads/gen-fi"]],
        reads: &[
            &["rev-parse", "--verify", "refs/heads/gen-fi"],
            &["cat-file", "-p", "refs/heads/gen-fi:gen.txt"],
        ],
        forward_stdin: Some(P_FAST_IMPORT),
    },
    // `update-ref --stdin` is a *transaction*: a command list read from a
    // payload, applied all-or-nothing. `update-ref-create-delete` above covers
    // the one-ref argv form, which shares an entry point's worth of steps with
    // this one and none of its code — the batch parser, the transaction and the
    // rollback are only reachable through stdin. Verified against stock 2.55.0:
    // `create refs/heads/parity-fuzz HEAD` on stdin exits 0 and
    // `for-each-ref refs/heads/` then lists `refs/heads/parity-fuzz` beside the
    // fixture's own branches; `update-ref -d` on it exits 0 and removes it.
    // [`P_REF_UPDATES`] is that payload, already in the pool and already the
    // preferred one for this command.
    RoundTrip {
        cmd: "update-ref",
        name: "update-ref-stdin-transaction",
        shape: Shape::Linear,
        forward: &[&["update-ref", "--stdin"]],
        inverse: &[&["update-ref", "-d", "refs/heads/parity-fuzz"]],
        reads: &[
            &["rev-parse", "--verify", "refs/heads/parity-fuzz"],
            &["log", "-g", "--format=%gd %gs", "refs/heads/parity-fuzz"],
        ],
        forward_stdin: Some(P_REF_UPDATES),
    },
    // `bisect` writes `.git/BISECT_LOG` as it goes and `bisect replay` reads a
    // log back — and the two do not compose, which is the finding this pair
    // pins. Verified against stock 2.55.0 on a two-commit history:
    //
    //   bisect start                 rc 0, `status: waiting for both …`
    //   bisect bad HEAD              rc 0, `status: waiting for 'good' commit(s) …`
    //   bisect good HEAD~1           rc 0, `<oid> is the first 'bad' commit`
    //   .git/BISECT_LOG              present, five lines
    //   bisect replay .git/BISECT_LOG  rc **1**
    //   .git/BISECT_LOG              gone
    //   bisect reset                 rc 0
    //
    // `replay` resets the bisection before it reads its input, and the reset
    // unlinks the very file it was told to replay — so naming git's own log
    // fails, on stock, every time. A port that opens the file before resetting
    // succeeds where stock fails, and the difference is an exit code rather than
    // prose. Nothing else in this harness can produce it: the log's path is the
    // only file a step can name without capturing output, and a case is one
    // invocation.
    //
    // `bisect reset` closes the pair whether or not `replay` already reset:
    // verified rc 0 both while bisecting and with no bisection in progress, so
    // the sequence ends on the same state it started from either way.
    // ----------------------------------------------------------------------
    // Five pairs on the shapes added for the states they move: an index bit, a
    // staged rename, a worktree lock, a tag object in the middle of a chain, and
    // an object that is not in the repository yet. Each is expressible on
    // exactly one shape and on no other, which is why none of them could be
    // written before those shapes existed — and each costs one entry point,
    // which is `--fuzz-sequences` sequences of four to eight steps per side.
    // ----------------------------------------------------------------------

    // The intent-to-add bit, on and off. The bit is not in any listing: verified
    // against stock 2.55.0, `ls-files --stage -v ita-new.txt` reads
    // `H 100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 0` at the start of this
    // pair and the identical line at the end of it, which is also what an
    // ordinary staged add of an empty file would print. So an inverse that
    // restores the entry and forgets the intent leaves every listing right and
    // one rendering wrong — `status` says `AM ita-new.txt` where ` A
    // ita-new.txt` belongs, because the index would hold real content the
    // worktree disagrees with rather than a placeholder it is allowed to.
    //
    // The inverse is two steps because git has no verb that clears the bit:
    // `rm --cached` drops the entry and `add -N` re-creates it as an intent.
    // Verified against stock 2.55.0 on [`Shape::IntentToAdd`], comparing
    // `status --short` and `ls-files --stage` before and after the whole pair:
    //
    //   before                 ` A ita-new.txt`
    //   add ita-new.txt        `A  ita-new.txt`   (rc 0)
    //   rm --cached -f …       `rm 'ita-new.txt'` (rc 0)
    //   add -N ita-new.txt     rc 0
    //   after                  byte-identical to before, both probes
    //
    // A port whose `add -N` writes a real entry ends the pair with `A ` where
    // ` A` belongs and every other reader agrees with stock; one whose plain
    // `add` preserves the bit never leaves the start state at all, and the
    // observer between the halves is what tells those two apart.
    RoundTrip {
        cmd: "add",
        name: "intent-to-add-materialize",
        shape: Shape::IntentToAdd,
        forward: &[&["add", "ita-new.txt"]],
        inverse: &[&["rm", "--cached", "-f", "ita-new.txt"], &["add", "-N", "ita-new.txt"]],
        reads: &[
            &["ls-files", "--stage", "-v", "ita-new.txt"],
        ],
        forward_stdin: None,
    },
    // A staged rename **cancelled**, and staged again. `mv-there-and-back` above
    // renames a path that history knows; this one renames the destination half
    // of a rename that is already sitting in the index, so the forward step has
    // to collapse an existing pair rather than create one — and the record it
    // removes is the one this corpus has never produced.
    //
    // Verified against stock 2.55.0 on [`Shape::PendingRename`], comparing
    // `status --porcelain=v2` and `ls-files --stage` across the pair:
    //
    //   before   `2 R. N… R100 pure-renamed.txt<TAB>pure.txt` — the rename
    //            record, beside `2 RM … R100 near-renamed.txt` (renamed in the
    //            index and modified in the worktree at once), `2 R. … R60
    //            far-renamed.txt` and `2 .R … R100 wt-renamed.txt` (a rename
    //            that is not staged at all)
    //   mv pure-renamed.txt pure.txt   rc 0; the `pure` record is **gone** from
    //            `status --short`, which now lists six paths where it listed
    //            seven
    //   mv pure.txt pure-renamed.txt   rc 0
    //   after    byte-identical to before, both probes
    //
    // The port cannot emit a `2` record at all, so the forward half is where it
    // stops agreeing and the inverse is what proves the index came back rather
    // than merely looking settled.
    RoundTrip {
        cmd: "mv",
        name: "pending-rename-cancel",
        shape: Shape::PendingRename,
        forward: &[&["mv", "pure-renamed.txt", "pure.txt"]],
        inverse: &[&["mv", "pure.txt", "pure-renamed.txt"]],
        reads: &[
            &["ls-files", "--stage", "pure.txt"],
            &["diff", "--cached", "-M", "--name-status"],
        ],
        forward_stdin: None,
    },
    // A worktree lock released and re-taken **with its reason**.
    // `worktree-lock-unlock` above starts from an unlocked worktree and locks it
    // with no reason, which is the empty-file half of the protocol;
    // [`Shape::WorktreeLocked`] starts from a locked one, so this pair runs the
    // other way round and carries the string. The two are different files on
    // disk and different answers from `worktree list --porcelain`.
    //
    // Verified against stock 2.55.0 on the shape:
    //
    //   .git/worktrees/wt/locked   `held by the fixture\n` (20 bytes, od -c)
    //   worktree list --porcelain  `locked held by the fixture` for `wt`,
    //                              `prunable gitdir file points to
    //                              non-existent location` for `wt-gone`
    //   worktree unlock wt         rc 0; `locked` gone from the admin directory
    //   worktree lock --reason …   rc 0; `locked` back, byte for byte
    //
    // `worktree list` is already an observer, so the middle of the pair prints
    // the state the forward half changed. A port that writes an empty `locked`
    // for a `--reason` lock, or that reports `locked` without the reason, ends
    // the round trip looking correct to a presence check and different to this
    // one.
    RoundTrip {
        cmd: "worktree",
        name: "worktree-unlock-relock",
        shape: Shape::WorktreeLocked,
        forward: &[&["worktree", "unlock", "wt"]],
        inverse: &[&["worktree", "lock", "--reason", "held by the fixture", "wt"]],
        reads: &[
            &["worktree", "list", "--porcelain"],
        ],
        forward_stdin: None,
    },
    // A tag object in the middle of a chain, deleted and rebuilt **to the same
    // id**. `tag-add-delete` above creates a lightweight ref and removes it;
    // this one destroys an object other refs point through and reconstructs it,
    // which is only expressible on [`Shape::TagChain`] — no other shape has a
    // tag whose target is a tag.
    //
    // The equality of the two ids is the assertion, and it holds because
    // `env::harden` pins the tagger identity and `GIT_COMMITTER_DATE`, so a tag
    // object is a function of its target, its name and its message alone.
    // Verified against stock 2.55.0 on the shape:
    //
    //   rev-parse outermost   `5d511ffb740e87283f0693f2d6a2edffd050258e`
    //   tag -d outermost      rc 0, `Deleted tag 'outermost' (was 5d511ff)`
    //   tag -a outermost -m … outer   rc 0 (a nested-tag hint on stderr, which
    //                         no case compares)
    //   rev-parse outermost   `5d511ffb740e87283f0693f2d6a2edffd050258e` again
    //   cat-file -p outermost `object 24b224a9…` / `type tag` / `tag outermost`
    //   describe --tags outermost   `inner` — the peel still reaches the commit
    //
    // A port whose `tag -a` writes the header fields in a different order, or
    // that resolves `outer` to the commit it eventually peels to rather than to
    // the tag object, ends with a different id at the same refname and every
    // observer that prints a name still agrees.
    RoundTrip {
        cmd: "tag",
        name: "tag-chain-delete-recreate",
        shape: Shape::TagChain,
        forward: &[&["tag", "-d", "outermost"]],
        inverse: &[&["tag", "-a", "outermost", "-m", "outermost tag, points at outer", "outer"]],
        reads: &[
            &["rev-parse", "outermost"],
            &["cat-file", "-p", "outermost"],
        ],
        forward_stdin: None,
    },
    // A blob that is **not in the repository**, demanded by a step and supplied
    // by the promisor remote.
    //
    // The one pair here whose halves are not object-store-neutral, and that is
    // the point rather than a defect in it: the forward half checks out a branch
    // whose content was filtered away at clone time, so git has to notice the
    // absence, ask the promisor for it, and write what comes back. Every other
    // shape in this harness has every object it references, which left
    // `--filter=`, `rev-list --missing=`, `--exclude-promisor-objects` and the
    // whole lazy-fetch path with no repository to be true of.
    //
    // Verified against stock 2.55.0 on [`Shape::Promisor`]:
    //
    //   rev-list --missing=print --objects --all | grep -c '^?'   `3`
    //   .git/objects/pack       two promisor packs
    //   checkout pc-side        rc 0, `Switched to a new branch 'pc-side'`,
    //                           0.12s wall
    //   cat hist.txt            `hist v1` — the filtered blob, now present
    //   … | grep -c '^?'        `2`
    //   .git/objects/pack       a **third** promisor pack
    //   status --porcelain      empty, before and after
    //   checkout main           rc 0, 0.02s
    //
    // `probe_storage` elides the checksum inside a pack filename and keeps
    // duplicates, so the fetched pack shows up as one more `pack/pack-<hash>`
    // line on both sides rather than as a name two implementations could not be
    // expected to agree on. A port that treats a promisor absence as damage
    // fails at the checkout; one that fetches more than it was asked for is a
    // storage line the other side does not have.
    //
    // The remote is the fixture's own `./.remote.git` — verified,
    // `config remote.origin.url` reads exactly that — so the fetch is a local
    // directory read and cannot block or leave the machine.
    RoundTrip {
        cmd: "checkout",
        name: "promisor-blob-on-demand",
        shape: Shape::Promisor,
        forward: &[&["checkout", "pc-side"]],
        inverse: &[&["checkout", "main"]],
        reads: &[
            &["cat-file", "-p", "pc-side:hist.txt"],
            &["rev-list", "--missing=print", "--objects", "--all"],
        ],
        forward_stdin: None,
    },
    RoundTrip {
        cmd: "bisect",
        name: "bisect-log-replay",
        shape: Shape::Branched,
        forward: &[
            &["bisect", "start"],
            &["bisect", "bad", "HEAD"],
            &["bisect", "good", "HEAD~1"],
        ],
        inverse: &[&["bisect", "replay", ".git/BISECT_LOG"], &["bisect", "reset"]],
        reads: &[],
        forward_stdin: None,
    },
];

/// Generate `per_entry` sequences for every entry point, from `seed`.
///
/// The three families are emitted in a fixed order and each entry point is drawn
/// `per_entry` times, so the corpus this returns — the sequences, their steps and
/// their ids — is a pure function of `(seed, per_entry)` and a reported step
/// replays exactly. That is the property this function owns. It is not the same
/// claim as "the report is byte-identical": a handful of cases carry values
/// *stock* re-rolls every run (`filter-branch`'s elapsed-seconds progress line,
/// `blame`'s wall clock on uncommitted lines, `unpack-file`'s random temp name,
/// `quiltimport`'s commit ids), and those move the report's bytes no matter what
/// any generator does. [`crate::runner::Verdict::Nondeterministic`] is where they
/// are accounted for.
pub fn generate_sequences(seed: u64, per_entry: usize) -> Vec<Sequence> {
    let mut rng = Rng::new(seed ^ SEQUENCE_STREAM);
    let grammars = all_grammars();
    let mut out = Vec::new();

    for stopper in STOPPERS {
        for n in 0..per_entry {
            out.push(walk(&mut rng, stopper, &grammars, n));
        }
    }
    for n in 0..per_entry {
        out.push(bisect_walk(&mut rng, &grammars, n));
    }
    for cmd in MUTATORS {
        // A name with no grammar is a bug in `MUTATORS`, caught at `cargo test`
        // by `every_generator_verb_has_a_grammar`. Skipped rather than panicked
        // at run time so one stale name cannot take the whole sweep down.
        let Some(g) = grammar_for(&grammars, cmd) else { continue };
        for n in 0..per_entry {
            out.push(mutate_then_observe(&mut rng, g, n));
        }
    }
    for rt in ROUND_TRIPS {
        for n in 0..per_entry {
            out.push(round_trip(&mut rng, rt, n));
        }
    }
    out
}

/// The grammar for `cmd`, if the fuzzer has one.
fn grammar_for<'a>(grammars: &'a [Grammar], cmd: &str) -> Option<&'a Grammar> {
    grammars.iter().find(|g| g.cmd == cmd)
}

/// Apply the envelope dimensions a generated sequence draws.
///
/// Only two, and both are drawn by the samplers the single-case generator
/// already uses. The working directory is here because git re-resolves which
/// repository it is in on *every* invocation, so a stateful operation resumed
/// from a subdirectory asks whether step 4 finds the repository step 3 wrote to
/// — a break the curated corpus has one case for and no more. Configuration is
/// here because settings like `merge.conflictStyle` and `rerere.enabled` change
/// what a whole workflow does rather than what one invocation prints.
///
/// Environment and global options are deliberately not drawn: `GIT_DIR`
/// redirection across steps is already curated, `-C <dir>` duplicates the
/// working directory, and every extra dimension lands in a step id that already
/// carries the whole script.
fn envelope_dims(rng: &mut Rng, seq: Sequence, shape: Shape) -> Sequence {
    let config = sample_config(rng);
    let cwd = sample_cwd(rng, shape);
    let seq = if config.is_empty() { seq } else { seq.with_scoped_config(config) };
    match cwd {
        Some(dir) => seq.in_dir(dir),
        None => seq,
    }
}

/// Append 1..=`max` observers.
fn observe(rng: &mut Rng, mut seq: Sequence, max: usize) -> Sequence {
    for _ in 0..=rng.below(max) {
        seq = seq.step(rng.pick(OBSERVERS));
    }
    seq
}

/// One resumption invocation: a command from [`STATEFUL`] and a verb from its
/// alphabet.
///
/// `own` biases the draw toward the machine that is actually running — two
/// thirds — so the legal walk is reached often while the cross-machine
/// transitions that a port has no documentation for still come up on a third of
/// the draws.
fn resume_step(rng: &mut Rng, own: &str, grammars: &[Grammar]) -> Vec<String> {
    let cmd = if rng.chance(2, 3) && STATEFUL.contains(&own) {
        own
    } else {
        *rng.pick(STATEFUL)
    };
    let mut verbs: Vec<&str> = RESUME_TOKENS.to_vec();
    if let Some(g) = grammar_for(grammars, cmd) {
        verbs.extend(g.flags.iter().copied().filter(|f| RESUME_EXTRA.contains(f)));
    }
    vec![cmd.to_string(), rng.pick(&verbs).to_string()]
}

/// The record an interrupted operation parks, read back **by name**.
///
/// The same dependency [`RoundTrip::reads`] adds to the round trips, for the
/// family where it is cheapest to state: a stopper's entry step writes a
/// pseudo-ref (or, for `am`, a mail) whose name is fixed by git and known at
/// compile time, and every one of the resumption verbs is defined by what it does
/// to that record. `--abort` clears it, `--quit` leaves the work and clears it,
/// `--continue` consumes it — so a walk that never asks about it measures the
/// transitions through the state probe alone and cannot say *which* record a port
/// forgot to clean.
///
/// Every row was run against stock 2.55.0 on a premise from this table, before
/// and after the operation was aborted — `cherry-pick theirs` and `merge theirs`
/// on [`Shape::Conflicted`], `revert --no-edit main~2` on `Whitespace`,
/// `rebase theirs` on `Conflicted`, `am mail/one.eml` twice on `Patches`,
/// `fetch --deepen=1` on `Shallow`, and `merge --no-commit div-other` on
/// `MergeableDirty`, which is the row the obvious reading gets wrong: a merge
/// that *succeeded* and stopped before committing parks `MERGE_HEAD` exactly like
/// one that conflicted (verified, `e62c76f…` then the refusal after `--abort`):
///
/// ```text
/// cherry-pick  CHERRY_PICK_HEAD  d3928f9… → fatal: Needed a single revision (128)
/// revert       REVERT_HEAD       35a528b… → the same refusal
/// rebase       REBASE_HEAD       38ab0cb… → the same refusal
/// merge        MERGE_HEAD        d3928f9… → the same refusal
/// am           the parked mail   `am --show-current-patch=raw` prints the whole
///                                mail `.git/rebase-apply` holds; with nothing in
///                                progress it is `fatal: Resolve operation not in
///                                progress, we are not resuming.` (128)
/// fetch        FETCH_HEAD        b56bdca… after `fetch --deepen=1`, and the same
///                                id afterwards — the one row whose answer is not
///                                supposed to change
/// ```
///
/// `None` for the four stoppers with no such record: `stash`'s entry leaves the
/// entry itself, which the `stash list` observer already prints; `gc`, `commit`
/// and `push` park nothing a plumbing command can name — a refused commit leaves
/// only `COMMIT_EDITMSG`, and no verb reads a file by path.
fn parked_read(cmd: &str) -> Option<&'static [&'static str]> {
    Some(match cmd {
        "cherry-pick" => &["rev-parse", "--verify", "CHERRY_PICK_HEAD"],
        "revert" => &["rev-parse", "--verify", "REVERT_HEAD"],
        "rebase" => &["rev-parse", "--verify", "REBASE_HEAD"],
        "merge" => &["rev-parse", "--verify", "MERGE_HEAD"],
        "am" => &["am", "--show-current-patch=raw"],
        "fetch" => &["rev-parse", "--verify", "FETCH_HEAD"],
        _ => return None,
    })
}

/// A state-machine walk: setup, the invocation that stops, then resumption verbs
/// with observers between them.
///
/// The parked record is read twice — once as soon as the entry has stopped and
/// once after the transitions have run — for the reason [`round_trip`] gives for
/// bracketing the inverse: the first answer measures the write and the second
/// measures the clean-up, and only the pair distinguishes "never parked" from
/// "parked and cleared". Two steps per walk, on the sixteen stoppers that have a
/// record.
fn walk(rng: &mut Rng, s: &Stopper, grammars: &[Grammar], n: usize) -> Sequence {
    let mut seq =
        Sequence::new(s.cmd, format!("gen/walk/{}#{n}", s.name), s.shape);
    for step in s.setup {
        seq = seq.step(step);
    }

    // The entry, decorated on a minority of draws with 1..=2 grammar flags on
    // its last invocation.
    //
    // Two filters and a gate. Resumption tokens are excluded because decorating
    // `cherry-pick theirs` with `--abort` defeats the premise before the walk
    // starts, and the walk draws that verb on its own terms anyway. The gate is
    // there because *any* flag can break the premise — `--strategy=bogus` dies
    // at parse time, `-n` stops the operation from starting — and a walk whose
    // entry never stopped spends every one of its steps on a refusal the corpus
    // already covers. A minority is the right rate rather than none: the entry
    // invocation should not be one of eleven fixed command lines, and a
    // decoration that turns a stop into a non-stop is itself a transition worth
    // comparing. Most walks keep the proven premise; some do not, on purpose.
    let decorations: Vec<String> = match grammar_for(grammars, s.cmd) {
        Some(g) if rng.chance(1, 3) => {
            let usable: Vec<&str> = g
                .flags
                .iter()
                .copied()
                .filter(|f| !RESUME_TOKENS.contains(f) && !RESUME_EXTRA.contains(f))
                .collect();
            let count = if usable.is_empty() { 0 } else { rng.count_upto(2) };
            // Through `split_tokens` for the same reason `sample_argv` is: this
            // pushes a grammar flag into a step without passing through the
            // sampler, so a multi-token flag would arrive here still encoded.
            split_tokens((0..count).map(|_| rng.pick(&usable).to_string()).collect())
        }
        _ => Vec::new(),
    };
    for (i, step) in s.entry.iter().enumerate() {
        let mut args: Vec<String> = step.iter().map(|t| t.to_string()).collect();
        if i + 1 == s.entry.len() {
            // After the subcommand, before its operands: a flag written after a
            // rev is still parsed by git, but the id reads as an invocation
            // somebody would write.
            let tail = args.split_off(1);
            args.extend(decorations.iter().cloned());
            args.extend(tail);
        }
        seq = seq.step_argv(args, s.entry_stdin);
    }

    // What the entry parked, named. See [`parked_read`].
    let parked = parked_read(s.cmd);
    if let Some(step) = parked {
        seq = seq.step(step);
    }

    // The walk. Each verb is followed by an observer half the time — often
    // enough that a wrong write is attributed to the verb that made it, rarely
    // enough that the walk is mostly transitions rather than mostly reads.
    for _ in 0..=rng.below(3) {
        seq = seq.step_argv(resume_step(rng, s.cmd, grammars), None);
        if rng.chance(1, 2) {
            seq = seq.step(rng.pick(OBSERVERS));
        }
    }
    if let Some(step) = parked {
        seq = seq.step(step);
    }
    seq = observe(rng, seq, 2);
    envelope_dims(rng, seq, s.shape)
}

/// `git bisect`'s own state machine: a start, then answers, then whatever the
/// walk draws — including answering a bisect that was never started and resetting
/// one twice.
fn bisect_walk(rng: &mut Rng, grammars: &[Grammar], n: usize) -> Sequence {
    let g = grammar_for(grammars, "bisect");
    let shape = match g {
        Some(g) => *rng.pick(g.shapes),
        None => Shape::Branched,
    };
    let mut seq = Sequence::new("bisect", format!("gen/bisect#{n}"), shape);

    // `start` first, decorated from the grammar — `--term-new=`/`--term-old=`
    // rename the verbs the rest of the walk uses, which is a rename a port can
    // implement for `terms` and forget for the answers.
    let mut start = vec!["bisect".to_string(), "start".to_string()];
    if let Some(g) = g {
        for _ in 0..rng.count_upto(2) {
            start.push(rng.pick(g.flags).to_string());
        }
    }
    // Latent today — `bisect`'s grammar has no multi-token flag — but the same
    // shape as the decoration path above, and a grammar edit should not have to
    // remember this call site.
    seq = seq.step_argv(split_tokens(start), None);

    for _ in 0..=rng.below(5) {
        let verb = *rng.pick(BISECT_VERBS);
        let mut args = vec!["bisect".to_string(), verb.to_string()];
        // An operand a third of the time. `bisect bad HEAD~2` and `bisect bad`
        // are different transitions — one names a commit, the other means "the
        // one you just checked out" — and only a walk can reach the second.
        //
        // `run` takes its operand from [`BISECT_RUN_COMMANDS`] instead, because
        // its operand is a program it executes rather than a rev it resolves;
        // see that constant for the hang a rev pool produces here. The roll and
        // the pick are taken either way, so the RNG stream — and therefore every
        // case id downstream of it — does not depend on which verb came up.
        if rng.chance(1, 3) {
            let pool = if verb == "run" { BISECT_RUN_COMMANDS } else { REVS };
            let operand = *rng.pick(pool);
            if !operand.is_empty() {
                args.push(operand.to_string());
            }
        }
        seq = seq.step_argv(args, None);
        if rng.chance(1, 3) {
            seq = seq.step(rng.pick(OBSERVERS));
        }
    }
    seq = seq.step(&["bisect", "log"]).step(&["bisect", "reset"]);
    seq = observe(rng, seq, 2);
    envelope_dims(rng, seq, shape)
}

/// A sampled mutation followed by the readers whose answer it should have
/// changed, and sometimes a second mutation and a second round of readers.
///
/// The second mutation is what makes this more than a case with readers attached:
/// it runs against whatever the first one left, which for a sampled argv is a
/// state no fixture describes.
fn mutate_then_observe(rng: &mut Rng, g: &Grammar, n: usize) -> Sequence {
    let shape = *rng.pick(g.shapes);
    let mut seq = Sequence::new(g.cmd, format!("gen/observe/{}#{n}", g.cmd), shape);

    // A baseline read a third of the time. It is drawn rather than forced
    // because the two fixtures are copies of one template and are equal by
    // construction before anything runs, so its only value is proving the
    // observer itself agrees — which the single-case corpus already covers on a
    // pristine fixture. Worth an invocation sometimes, not always.
    if rng.chance(1, 3) {
        seq = seq.step(rng.pick(OBSERVERS));
    }

    for round in 0..if rng.chance(1, 3) { 2 } else { 1 } {
        let args = sample_argv(rng, g, 2, 2);
        let stdin = sample_stdin(rng, g.cmd, &args);
        seq = seq.step_argv(args, stdin);
        seq = observe(rng, seq, if round == 0 { 2 } else { 1 });
    }
    envelope_dims(rng, seq, shape)
}

/// An operation, then its inverse, with reads between the halves.
///
/// One [`RoundTrip::reads`] entry is drawn per sequence and asked **twice** —
/// once while the forward half's artifact exists and once after the inverse has
/// removed it. Drawn rather than run in full because the pairs carry one to three
/// of them and a sequence that ran every read twice would spend more steps on
/// reading than on the round trip; asked twice rather than once because the
/// second answer is the one that catches an inverse that half-cleans, and a
/// question asked only after the inverse cannot tell "never written" from
/// "written and removed".
fn round_trip(rng: &mut Rng, rt: &RoundTrip, n: usize) -> Sequence {
    let mut seq =
        Sequence::new(rt.cmd, format!("gen/roundtrip/{}#{n}", rt.name), rt.shape);
    if rng.chance(1, 3) {
        seq = seq.step(rng.pick(OBSERVERS));
    }
    for (i, step) in rt.forward.iter().enumerate() {
        // The payload rides on the first forward step and nowhere else; see
        // [`RoundTrip::forward_stdin`] for why that is the whole contract.
        let stdin = if i == 0 { rt.forward_stdin } else { None };
        seq = seq.step_argv(step.iter().map(|t| t.to_string()).collect(), stdin);
    }
    // The draw is taken whether or not the pair has reads, so the RNG stream —
    // and every sequence id downstream of it — does not depend on which pair
    // came up.
    let read = rng.below(rt.reads.len().max(1));
    let named: Option<&'static [&'static str]> = rt.reads.get(read).copied();
    if let Some(step) = named {
        seq = seq.step(step);
    }
    seq = observe(rng, seq, 2);
    for step in rt.inverse {
        seq = seq.step(step);
    }
    if let Some(step) = named {
        seq = seq.step(step);
    }
    seq = observe(rng, seq, 2);
    envelope_dims(rng, seq, rt.shape)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::split_config_key;

    /// The sampled environment may only *add* variables, never re-point one of
    /// `env::harden`'s pins. The runner asserts this per case, which catches it
    /// at run time; this catches it at `cargo test` time, before a pool entry
    /// that would abort every case gets committed.
    #[test]
    fn sampled_env_vars_are_never_pinned() {
        for (key, values) in ENV_VARS {
            assert!(!crate::env::is_pinned(key), "{key} is pinned by env::harden");
            assert!(!values.is_empty(), "{key} has no values to draw from");
            // `ConfigScope::Env` writes `GIT_CONFIG_COUNT`/`KEY_<n>`/`VALUE_<n>`
            // onto the same child, so a variable in that family drawn here would
            // be silently overwritten — and `run_side` asserts on the collision,
            // which would abort a whole sweep on whichever case drew both.
            assert!(
                !key.starts_with("GIT_CONFIG_"),
                "{key} collides with the config env scope; deliver it as a ConfigEntry instead"
            );
            for value in *values {
                // The two sides run against copies at different roots, so a
                // literal absolute path would name one side's repository to both.
                assert!(
                    !value.starts_with('/'),
                    "{key}={value} must use the repo placeholder, not an absolute path"
                );
            }
        }
    }

    /// [`STORE_SHAPES`] is [`REV_SHAPES`] plus the two store shapes, and the
    /// only reason it is written out rather than derived is that `&[Shape]`
    /// cannot be concatenated in a const. So the containment is asserted here
    /// instead: a shape added to `REV_SHAPES` tomorrow and forgotten here would
    /// leave `rev-parse`, `cat-file` and `ls-tree` quietly narrower than `log`
    /// and `rev-list`, which is a hole nothing in a report would show.
    #[test]
    fn store_shapes_extends_rev_shapes() {
        for shape in REV_SHAPES {
            assert!(
                STORE_SHAPES.contains(shape),
                "{} is in REV_SHAPES and not in STORE_SHAPES",
                shape.name()
            );
        }
        for shape in STORE_ONLY_SHAPES {
            assert!(STORE_SHAPES.contains(shape), "{} is what STORE_SHAPES is for", shape.name());
            assert!(!REV_SHAPES.contains(shape), "{} is not a history", shape.name());
        }
        // The pool that decides what a *walk* reads must not silently become the
        // pool that decides what a store reader reads. Counted against
        // [`STORE_ONLY_SHAPES`] rather than against a literal, so the assertion
        // keeps its meaning when either pool grows: a shape added to
        // `STORE_SHAPES` alone fails here unless it is also declared to be one
        // of the store-only ones.
        assert_eq!(STORE_SHAPES.len(), REV_SHAPES.len() + STORE_ONLY_SHAPES.len());
    }

    /// Every config key has values, and no value is an absolute path — config
    /// goes into argv unsubstituted, so a literal root would name one side's
    /// copy to both.
    ///
    /// The grouped pool is held to the same rules as the flat one, and to one
    /// more: a group with a single key is not a group. The whole reason
    /// [`CONFIG_GROUPS`] exists is that two keys can be drawn together, and a
    /// one-key entry would spend a two-thirds share of the budget doing what the
    /// flat pool already does.
    #[test]
    fn config_pool_is_well_formed() {
        let well_formed = |key: &str, values: &[&str], whose: &str| {
            assert!(key.contains('.'), "{whose}: {key} is not a section.key");
            assert!(!values.is_empty(), "{whose}: {key} has no values to draw from");
            assert!(
                values.iter().all(|v| !v.starts_with('/')),
                "{whose}: {key} names an absolute path"
            );
            assert!(
                split_config_key(key).is_some(),
                "{whose}: {key} cannot be written as a config file stanza"
            );
        };
        for (key, values) in CONFIG_KEYS {
            well_formed(key, values, "CONFIG_KEYS");
        }
        for group in CONFIG_GROUPS {
            assert!(group.keys.len() > 1, "group {} has nothing to interact with", group.name);
            for (key, values) in group.keys {
                well_formed(key, values, group.name);
            }
        }
    }

    /// `.gitmodules` is only read for `submodule.*`, so a key that is not one
    /// must never be routed there: git would parse the file and discard the
    /// stanza, and the case would look like it set a key while setting nothing.
    /// Every other key must reach every other scope, or the scope dimension
    /// silently collapses back to the `-c` it replaced.
    #[test]
    fn only_submodule_keys_reach_the_gitmodules_scope() {
        let every_key = CONFIG_KEYS
            .iter()
            .chain(CONFIG_GROUPS.iter().flat_map(|g| g.keys.iter()))
            .map(|(k, _)| *k);
        let (mut submodule_keys, mut other_keys) = (0, 0);
        for key in every_key {
            let scopes = scopes_for(key);
            if key.starts_with("submodule.") {
                submodule_keys += 1;
                assert!(scopes.contains(&ConfigScope::Modules), "{key} cannot reach .gitmodules");
            } else {
                other_keys += 1;
                assert!(!scopes.contains(&ConfigScope::Modules), "{key} would be discarded there");
            }
            for scope in ConfigScope::ALL {
                if *scope != ConfigScope::Modules {
                    assert!(scopes.contains(scope), "{key} cannot reach {}", scope.name());
                }
            }
        }
        assert!(submodule_keys > 0, "no key can reach the .gitmodules scope at all");
        assert!(other_keys > 0, "every key is a submodule key");
    }

    /// Every scope is actually drawn. A scope that silently rounded to zero —
    /// because a probability was wrong, or because `scopes_for` excluded it —
    /// would leave the whole point of the widening unmeasured while every number
    /// in the report stayed exactly as plausible as before.
    #[test]
    fn every_config_scope_is_drawn() {
        let cases = generate(20250821, 6);
        for scope in ConfigScope::ALL {
            assert!(
                cases.iter().any(|c| c.config.iter().any(|e| e.scope == *scope)),
                "no case delivered a setting from the {} scope",
                scope.name()
            );
        }
    }

    /// The two shapes a scope dimension exists to reach, neither of which a
    /// one-key `-c` sampler can produce:
    ///
    ///  * one key set **twice in one file**, which is last-value-wins;
    ///  * one key set **in two scopes**, which is precedence.
    ///
    /// Asserted together because the bias in [`sample_scope`] trades them off
    /// against each other: a bias of 1 would give only the first, a bias of 0
    /// only rarely the first, and either mistake leaves half of what the scope
    /// dimension is for unmeasured while the case count stays the same.
    #[test]
    fn config_draws_reach_last_wins_and_precedence() {
        let cases = generate(0xC0FFEE, 6);
        let mut repeated_in_one_file = 0;
        let mut same_key_two_scopes = 0;
        for case in &cases {
            for (i, a) in case.config.iter().enumerate() {
                for b in case.config.iter().skip(i + 1) {
                    if a.key.is_none() || a.key != b.key {
                        continue;
                    }
                    if a.scope == b.scope && a.scope.is_file() {
                        repeated_in_one_file += 1;
                    } else if a.scope != b.scope {
                        same_key_two_scopes += 1;
                    }
                }
            }
        }
        assert!(repeated_in_one_file > 0, "no draw ever set one key twice in one file");
        assert!(same_key_two_scopes > 0, "no draw ever set one key in two scopes");
    }

    /// A raw line is a thing only a file has, so it may only be drawn for a file
    /// scope — a raw entry on `-c` or in `GIT_CONFIG_KEY_<n>` would be silently
    /// dropped by [`crate::runner::install_config`], and a case id would then
    /// name a premise that never reached either side.
    ///
    /// Both pools have to be reached as well: the malformed one measures the
    /// line-numbered refusal, the legal one measures the parser on inputs `-c`
    /// cannot express, and they are drawn on one coin flip that a wrong constant
    /// could send entirely one way.
    #[test]
    fn raw_config_lines_only_land_in_file_scopes() {
        let cases = generate(31337, 8);
        let raws: Vec<&ConfigEntry> =
            cases.iter().flat_map(|c| c.config.iter()).filter(|e| e.is_raw()).collect();
        assert!(!raws.is_empty(), "no case ever drew a raw config line");
        for entry in &raws {
            assert!(
                entry.scope.is_file(),
                "raw line delivered to the non-file scope {}",
                entry.scope.name()
            );
        }
        assert!(
            raws.iter().any(|e| CONFIG_BAD_LINES.contains(&e.value.as_str())),
            "no case ever drew a malformed config line"
        );
        assert!(
            raws.iter().any(|e| CONFIG_ODD_LINES.contains(&e.value.as_str())),
            "no case ever drew a legal file-only config line"
        );
        // The third pool. Its members are the only way this file can deliver a
        // `NULL` value rather than an empty string — `ConfigEntry::set` always
        // writes `key=value` — so a draw that never reached it would leave every
        // callback's valueless branch unmeasured.
        assert!(
            raws.iter().any(|e| CONFIG_VALUELESS_LINES.contains(&e.value.as_str())),
            "no case ever drew a valueless config line"
        );
    }

    /// The `::config[…]` segment names the scope of every entry it carries, and
    /// carries every non-command-line entry. This is the property that makes a
    /// scoped failure reproducible by hand: without the scope the reader knows a
    /// key and a value and not which of six places to put them, and the case
    /// they reconstruct is a different case.
    #[test]
    fn case_ids_name_the_scope_of_every_scoped_entry() {
        let mut checked = 0;
        for case in generate(777, 6) {
            let scoped: Vec<&ConfigEntry> =
                case.config.iter().filter(|e| e.scope != ConfigScope::CommandLine).collect();
            if scoped.is_empty() {
                assert!(!case.id().contains("::config["), "empty config segment in {}", case.id());
                continue;
            }
            let id = case.id();
            let segment = id
                .split("::config[")
                .nth(1)
                .and_then(|s| s.split(']').next())
                .expect("a config segment");
            for entry in scoped {
                assert!(
                    segment.contains(&format!("{}:", entry.scope.name())),
                    "{} does not name the {} scope",
                    id,
                    entry.scope.name()
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "no generated case carried a scoped config entry");
    }

    /// The group table is what makes an *interaction* reachable, so it has to
    /// actually fire: a group draw must put two keys of one group into one case
    /// often enough to be worth the two-thirds share it costs.
    #[test]
    fn grouped_draws_put_interacting_keys_in_one_case() {
        let cases = generate(2468, 6);
        let mut hits = 0;
        for case in &cases {
            let keys: Vec<&str> =
                case.config.iter().filter_map(|e| e.key.as_deref()).collect();
            if keys.len() < 2 {
                continue;
            }
            if CONFIG_GROUPS.iter().any(|g| {
                keys.iter().filter(|k| g.keys.iter().any(|(gk, _)| gk == *k)).count() >= 2
            }) {
                hits += 1;
            }
        }
        assert!(hits > 0, "no case ever drew two keys from one interacting group");
    }

    /// A payload is only attached where the invocation asks for it, by one of
    /// the two rules `wants_stdin` documents — and never anywhere else, because
    /// a stdin hash in a case id that the command never reads is a lie about
    /// what the case does.
    #[test]
    fn stdin_is_attached_only_where_it_is_read() {
        let argv = |ts: &[&str]| ts.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        assert!(wants_stdin("mktree", &argv(&["mktree"])));
        assert!(wants_stdin("update-ref", &argv(&["update-ref", "--stdin"])));
        assert!(wants_stdin("cat-file", &argv(&["cat-file", "--batch-check"])));
        assert!(wants_stdin("hash-object", &argv(&["hash-object", "--stdin"])));
        assert!(wants_stdin("name-rev", &argv(&["name-rev", "--annotate-stdin"])));
        assert!(wants_stdin("update-index", &argv(&["update-index", "--index-info"])));
        assert!(wants_stdin("commit", &argv(&["commit", "--pathspec-from-file=-"])));
        assert!(wants_stdin("stripspace", &argv(&["stripspace"])));

        assert!(!wants_stdin("status", &argv(&["status", "--porcelain"])));
        assert!(!wants_stdin("hash-object", &argv(&["hash-object", "README.md"])));
        assert!(!wants_stdin("log", &argv(&["log", "--oneline"])));
    }

    /// Generation is a pure function of `(seed, per_cmd)`: the same seed must
    /// produce the same case ids, argument for argument, or a reported failure
    /// cannot be replayed. This is the property every new sampled dimension is
    /// most likely to break, since each one draws from the same stream.
    #[test]
    fn generation_is_reproducible_from_its_seed() {
        let ids = |seed: u64| -> Vec<String> {
            generate(seed, 3).iter().map(|c| c.id()).collect()
        };
        assert_eq!(ids(0x5A5A_C0DE), ids(0x5A5A_C0DE));
        assert_ne!(ids(1), ids(2), "different seeds must explore different points");
    }

    /// The new dimensions actually fire, and land in the case id where the
    /// report and `scripts/split_failures.pl` can see them. A probability that
    /// silently rounded to zero would leave the whole widening unmeasured while
    /// every number in the report stayed plausible.
    #[test]
    fn every_sampled_dimension_is_reachable() {
        let cases = generate(4242, 6);
        assert!(cases.iter().any(|c| !c.config.is_empty()), "no case sampled a config key");
        assert!(cases.iter().any(|c| !c.globals.is_empty()), "no case sampled a global option");
        assert!(cases.iter().any(|c| !c.env.is_empty()), "no case sampled an environment variable");
        assert!(cases.iter().any(|c| c.cwd.is_some()), "no case sampled a working directory");
        assert!(cases.iter().any(|c| c.stdin.is_some()), "no case sampled a stdin payload");

        // Rendered, not merely stored — and rendered in the right place for the
        // scope: a command-line entry belongs in the argv segment, everything
        // else in the `::config[…]` segment, because a reader reproducing the
        // case types the first and writes the second into a file.
        let with_cmdline = cases
            .iter()
            .find(|c| c.config.iter().any(|e| e.scope == ConfigScope::CommandLine))
            .expect("no case sampled a command-line config key");
        assert!(with_cmdline.id().contains("-c "), "config missing from {}", with_cmdline.id());
        let with_scoped = cases
            .iter()
            .find(|c| c.config.iter().any(|e| e.scope != ConfigScope::CommandLine))
            .expect("no case sampled a scoped config key");
        assert!(
            with_scoped.id().contains("::config["),
            "scoped config missing from {}",
            with_scoped.id()
        );
        let with_cwd = cases.iter().find(|c| c.cwd.is_some()).unwrap();
        assert!(with_cwd.id().contains("::cwd["), "cwd missing from {}", with_cwd.id());
        let with_env = cases.iter().find(|c| !c.env.is_empty()).unwrap();
        assert!(with_env.id().contains("::env["), "env missing from {}", with_env.id());
    }

    /// The id grammar `scripts/split_failures.pl` parses — `<shape>::<cmd>::` off
    /// the front, both segments free of whitespace — survives every dimension,
    /// so a widened fuzzer does not silently stop being triageable.
    #[test]
    fn case_ids_keep_the_shape_and_command_segments_first() {
        for case in generate(4242, 4) {
            let id = case.id();
            let (shape, rest) = id.trim_start_matches('!').split_once("::").expect("shape segment");
            let (cmd, _) = rest.split_once("::").expect("command segment");
            assert!(!shape.contains(char::is_whitespace), "shape segment has a space: {id}");
            assert!(!cmd.contains(char::is_whitespace), "command segment has a space: {id}");
            assert_eq!(cmd, case.cmd);
        }
    }

    /// Shrinking minimizes every dimension, not only argv. The oracle here calls
    /// a case failing whenever it still carries the one config key that matters,
    /// so a shrinker that only walked `args` would report all five facts.
    #[test]
    fn shrink_minimizes_the_sampled_dimensions() {
        let case = Case::new("status", &["status", "--short", "--branch"], Shape::Linear)
            .with_config(&[("core.abbrev", "4"), ("status.short", "true")])
            .with_globals(&[&["--no-pager"], &["-C", "src"]])
            .with_env(&[("GIT_NAMESPACE", "ns")])
            .in_dir(".git");
        let case = Case { stdin: Some(P_PATHS), ..case };

        let minimal = shrink(&case, &mut |c| {
            c.config.iter().any(|e| e.key.as_deref() == Some("status.short"))
        });

        assert_eq!(
            minimal.config,
            vec![ConfigEntry::set(ConfigScope::CommandLine, "status.short", "true")]
        );
        assert!(minimal.globals.is_empty());
        assert!(minimal.env.is_empty());
        assert_eq!(minimal.cwd, None);
        assert_eq!(minimal.stdin, None);
        assert_eq!(minimal.args, vec!["status".to_string()]);
        assert_eq!(minimal.size(), 1);
    }

    /// The four dimensions that cannot be dropped are *simplified* instead.
    ///
    /// A case has exactly one shape, one working directory and one payload, and
    /// every config entry has exactly one scope, so `drop_each` has nothing to say
    /// about any of them — and all four are now drawn by the corpus. The predicate
    /// here accepts anything that still sets *something*, which is the case where
    /// the minimal answer is the plainest one the harness can express.
    #[test]
    fn shrink_simplifies_what_it_cannot_drop() {
        let case = Case::new("status", &["status"], Shape::Promisor)
            .with_scoped_config(vec![ConfigEntry::set(
                ConfigScope::Worktree,
                "status.short",
                "true",
            )])
            .in_dir(".git/objects/pack");
        let case = Case { stdin: Some(P_PATHS), ..case };

        // Everything reduces except the config entry itself, so the walk reaches
        // the scope rather than emptying the list before it gets there.
        let minimal = shrink(&case, &mut |c| !c.config.is_empty());

        assert_eq!(minimal.shape, Shape::Linear, "the shape was never simplified");
        assert_eq!(
            minimal.config[0].scope,
            ConfigScope::CommandLine,
            "the config scope was never simplified"
        );
        // stdin and cwd are dropped outright when the predicate accepts it, which
        // is a better answer than a shorter one.
        assert_eq!(minimal.stdin, None);
        assert_eq!(minimal.cwd, None);
    }

    /// A dimension whose *value* is the finding is left alone.
    ///
    /// The half of the previous test that matters more: a shrinker that rewrote
    /// the scope, the shape or the payload unconditionally would report a case
    /// that does not reproduce. Each predicate below pins one dimension, and the
    /// shrunk case has to come back carrying it — while the parts nothing pins
    /// still reduce.
    #[test]
    fn shrink_keeps_the_dimension_that_is_the_finding() {
        let case = Case::new("status", &["status", "--short"], Shape::Sparse)
            .with_scoped_config(vec![ConfigEntry::set(ConfigScope::Repo, "core.worktree", "..")])
            .in_dir("src/nested");
        let case = Case { stdin: Some(P_PATHS), ..case };

        // Only a file-scoped entry reproduces — the `core.worktree` shape, where
        // `-c` is inert and the file is fatal.
        let minimal = shrink(&case, &mut |c| {
            c.config.iter().any(|e| e.scope == ConfigScope::Repo)
        });
        assert_eq!(minimal.config[0].scope, ConfigScope::Repo);
        assert_eq!(minimal.config[0].key.as_deref(), Some("core.worktree"));

        // Only the sparse fixture reproduces.
        let minimal = shrink(&case, &mut |c| c.shape == Shape::Sparse);
        assert_eq!(minimal.shape, Shape::Sparse);

        // Only a payload with every line in it reproduces.
        let minimal = shrink(&case, &mut |c| c.stdin == Some(P_PATHS));
        assert_eq!(minimal.stdin, Some(P_PATHS));

        // Only the deepest directory reproduces.
        let minimal = shrink(&case, &mut |c| c.cwd == Some("src/nested"));
        assert_eq!(minimal.cwd, Some("src/nested"));
    }

    /// Shrinking is a pure function of the case and the predicate.
    ///
    /// The new simplifications are ordered walks with no RNG in them, and they
    /// have to stay that way: a shrink that depended on which candidate happened
    /// to be tried first would print a different `→` line for the same failure on
    /// the next run, which is worse than not shrinking at all. Predicate is a
    /// pure function of the case, so two runs may only differ if `shrink` itself
    /// carries state.
    #[test]
    fn shrinking_is_deterministic() {
        let case = Case::new("log", &["log", "--oneline", "-1", "--graph"], Shape::Octopus)
            .with_config(&[("core.abbrev", "4"), ("log.decorate", "full")])
            .with_globals(&[&["--no-pager"]])
            .with_env(&[("GIT_NAMESPACE", "ns")])
            .in_dir(".git/refs/heads");
        let case = Case { stdin: Some(P_PATHS), ..case };
        let predicate = |c: &Case| c.args.len() > 2 && c.shape != Shape::Linear;

        let first = shrink(&case, &mut |c| predicate(c));
        let second = shrink(&case, &mut |c| predicate(c));
        assert_eq!(first.id(), second.id());
        assert_eq!(first.shape, second.shape);
    }

    /// The stdin walk cuts at line boundaries and only ever shortens.
    ///
    /// A payload is a language — a patch, a mailbox, a fast-import stream — and a
    /// cut inside a line produces input whose refusal is about the cut. The
    /// predicate keeps any payload holding the first line, so the answer must be
    /// the first line and nothing after it.
    #[test]
    fn shrink_cuts_stdin_at_a_line_boundary() {
        let case = Case::new("hash-object", &["hash-object", "--stdin"], Shape::Linear);
        let case = Case { stdin: Some(P_PATHS), ..case };
        let minimal = shrink(&case, &mut |c| {
            c.stdin.is_some_and(|s| s.starts_with(b"README.md\n"))
        });
        assert_eq!(minimal.stdin, Some(&b"README.md\n"[..]));
    }

    // -----------------------------------------------------------------------
    // Generated sequences
    // -----------------------------------------------------------------------

    /// Every verb the sequence generator names must have a grammar, or the entry
    /// point silently vanishes from every future run while the sequence count
    /// stays plausible. Asserted against [`all_grammars`] rather than against a
    /// second list, so a rename in the generated grammars fails here instead of
    /// quietly shrinking the corpus.
    #[test]
    fn every_generator_verb_has_a_grammar() {
        let grammars = all_grammars();
        for cmd in MUTATORS {
            assert!(grammar_for(&grammars, cmd).is_some(), "MUTATORS names {cmd}, which has no grammar");
        }
        for cmd in STATEFUL {
            assert!(grammar_for(&grammars, cmd).is_some(), "STATEFUL names {cmd}, which has no grammar");
        }
        for s in STOPPERS {
            assert!(grammar_for(&grammars, s.cmd).is_some(), "stopper {} has no grammar", s.name);
        }
        assert!(grammar_for(&grammars, "bisect").is_some(), "the bisect walk has no grammar");
    }

    /// The tables are well formed in the ways a malformed entry would not be
    /// caught by anything else: an empty argv aborts a whole sequence in
    /// `Sequence::step_case`, a `git`-prefixed token would run `git git status`,
    /// and a round trip with no inverse is not a round trip.
    #[test]
    fn generator_tables_are_well_formed() {
        let check = |argv: &[&str], what: &str| {
            assert!(!argv.is_empty(), "{what} has an empty step");
            assert_ne!(argv[0], "git", "{what} repeats the binary name");
            assert!(!argv[0].starts_with('-'), "{what} starts with a flag, not a subcommand");
        };
        for s in STOPPERS {
            assert!(!s.entry.is_empty(), "stopper {} never starts anything", s.name);
            for step in s.setup.iter().chain(s.entry) {
                check(step, s.name);
            }
        }
        for rt in ROUND_TRIPS {
            assert!(!rt.forward.is_empty(), "round trip {} has no forward half", rt.name);
            assert!(!rt.inverse.is_empty(), "round trip {} has no inverse half", rt.name);
            for step in rt.forward.iter().chain(rt.inverse) {
                check(step, rt.name);
            }
        }
        for rt in ROUND_TRIPS {
            for step in rt.reads {
                check(step, rt.name);
            }
        }
        for o in OBSERVERS {
            check(o, "observer");
        }
    }

    /// A walk asks about the parked record on both sides of its transitions.
    ///
    /// Same invariant as the round trips', in the family where the record is a
    /// pseudo-ref rather than an artifact: written by the entry, read once while
    /// the operation is in progress, and read again after the walk has run
    /// whatever transitions it drew. A generator that asked only before the
    /// transitions would measure that the entry parked something and never that a
    /// transition cleared it.
    #[test]
    fn walks_read_the_parked_record_on_both_sides() {
        let seqs = generate_sequences(555, 1);
        let mut reached = 0;
        for s in STOPPERS {
            let Some(read) = parked_read(s.cmd) else { continue };
            reached += 1;
            let name = format!("gen/walk/{}#0", s.name);
            let seq = seqs.iter().find(|q| q.name == name).expect("walk generated");
            let want: Vec<String> = read.iter().map(|t| t.to_string()).collect();
            let asked: Vec<usize> = (0..seq.len())
                .filter(|i| seq.step_case(*i).args == want)
                .collect();
            assert_eq!(asked.len(), 2, "{name} asks {want:?} {} times, not twice", asked.len());
            let entry_end = s.setup.len() + s.entry.len();
            assert_eq!(asked[0], entry_end, "{name} does not read the record right after the entry");
            assert!(asked[1] > asked[0], "{name} reads the record twice in the same place");
        }
        assert_eq!(reached, 16, "the set of stoppers with a parked record changed");
    }

    /// A pair's named read is asked on **both** sides of the inverse.
    ///
    /// The two askings are the whole family: one answer while the forward half's
    /// artifact exists and one after it should be gone. A generator that emitted
    /// only the first would measure the write and never the clean-up, which is
    /// the half the round trips exist for — so this pins the bracketing rather
    /// than the presence.
    #[test]
    fn round_trip_reads_bracket_the_inverse() {
        let seqs = generate_sequences(555, 1);
        for rt in ROUND_TRIPS {
            if rt.reads.is_empty() {
                continue;
            }
            let name = format!("gen/roundtrip/{}#0", rt.name);
            let s = seqs.iter().find(|q| q.name == name).expect("round trip generated");
            let argvs: Vec<Vec<String>> = (0..s.len()).map(|i| s.step_case(i).args).collect();
            let drawn: Vec<&&[&str]> = rt
                .reads
                .iter()
                .filter(|r| {
                    let want: Vec<String> = r.iter().map(|t| t.to_string()).collect();
                    argvs.contains(&want)
                })
                .collect();
            assert_eq!(
                drawn.len(),
                1,
                "{name} should ask exactly one of its reads, asked {}",
                drawn.len()
            );
            let want: Vec<String> = drawn[0].iter().map(|t| t.to_string()).collect();
            let asked: Vec<usize> =
                argvs.iter().enumerate().filter(|(_, a)| **a == want).map(|(i, _)| i).collect();
            assert_eq!(asked.len(), 2, "{name} asks {want:?} {} times, not twice", asked.len());
            let first_inverse: Vec<String> =
                rt.inverse[0].iter().map(|t| t.to_string()).collect();
            let inverse_at = argvs
                .iter()
                .position(|a| *a == first_inverse)
                .expect("the inverse half runs");
            assert!(
                asked[0] < inverse_at && asked[1] > inverse_at,
                "{name} asks {want:?} at {asked:?}, which does not bracket the inverse at \
                 {inverse_at}"
            );
        }
    }

    /// Generation is a pure function of `(seed, per_entry)` — the property a
    /// reported sequence failure is reproduced by, and the one every new draw is
    /// most likely to break since they all share one stream.
    ///
    /// The second half is the reason the sequence stream is seeded apart from the
    /// case stream: a sequence must replay from its seed whatever `--fuzz` was,
    /// which would not hold if `fuzz::generate` had consumed the same RNG first.
    #[test]
    fn sequence_generation_is_reproducible_from_its_seed() {
        let ids = |seed: u64, per: usize| -> Vec<String> {
            generate_sequences(seed, per)
                .iter()
                .flat_map(|s| (0..s.len()).map(|i| s.step_id(i)).collect::<Vec<_>>())
                .collect()
        };
        assert_eq!(ids(0x5A5A_C0DE, 2), ids(0x5A5A_C0DE, 2));
        assert_ne!(ids(1, 2), ids(2, 2), "different seeds must explore different workflows");

        // Independent of the case stream: draining `generate` first must not move
        // the sequences it did not produce.
        let before = ids(0x5A5A_C0DE, 1);
        let _ = generate(0x5A5A_C0DE, 3);
        assert_eq!(before, ids(0x5A5A_C0DE, 1));
    }

    /// Every entry point is drawn `per_entry` times, so raising the knob deepens
    /// coverage uniformly instead of deepening whatever the RNG favoured. A
    /// generator that silently dropped a family would still print a plausible
    /// sequence count, which is the class of lie this crate must not tell.
    #[test]
    fn every_entry_point_is_drawn() {
        let entry_points = STOPPERS.len() + 1 + MUTATORS.len() + ROUND_TRIPS.len();
        for per in [1usize, 3] {
            let seqs = generate_sequences(99, per);
            assert_eq!(seqs.len(), entry_points * per);
        }
        let seqs = generate_sequences(99, 1);
        for s in STOPPERS {
            assert!(
                seqs.iter().any(|q| q.name == format!("gen/walk/{}#0", s.name)),
                "no walk generated for stopper {}",
                s.name
            );
        }
        for cmd in MUTATORS {
            assert!(
                seqs.iter().any(|q| q.name == format!("gen/observe/{cmd}#0")),
                "no observe sequence generated for {cmd}"
            );
        }
        for rt in ROUND_TRIPS {
            assert!(
                seqs.iter().any(|q| q.name == format!("gen/roundtrip/{}#0", rt.name)),
                "no round trip generated for {}",
                rt.name
            );
        }
        assert!(seqs.iter().any(|q| q.name == "gen/bisect#0"));
        assert!(generate_sequences(99, 0).is_empty(), "zero must generate nothing");
    }

    /// The structural property that separates a workflow from a bag of unrelated
    /// invocations: **every** generated sequence has at least two steps, and
    /// every one of them ends with a step that reads state an earlier step could
    /// have written.
    ///
    /// A generator that emitted a single-step "sequence" would be paying the
    /// sequence machinery's price for a case, and one that ended on its mutation
    /// would never look at what the mutation did — which is the entire premise of
    /// the mutate-then-observe family.
    #[test]
    fn generated_sequences_end_on_a_read() {
        let readers: Vec<&str> = OBSERVERS.iter().map(|o| o[0]).collect();
        for s in generate_sequences(7, 2) {
            assert!(s.len() >= 2, "{} is a single invocation, not a workflow", s.name);
            let last = s.step_case(s.len() - 1);
            assert!(
                readers.contains(&last.args[0].as_str()),
                "{} ends on {:?}, which reads nothing back",
                s.name,
                last.args
            );
        }
    }

    /// A walk's steps after the entry are addressed to a state machine, and the
    /// cross-machine ones — the illegal transitions a port has no documentation
    /// for — must actually be reached rather than rounded away by the 2/3 bias.
    /// A probability that silently became zero would leave the most valuable half
    /// of this family unmeasured while the sequence count stayed the same.
    #[test]
    fn walks_reach_illegal_cross_machine_transitions() {
        let seqs = generate_sequences(1234, 4);
        let (mut cross, mut own) = (0, 0);
        // Only walks whose stopper *has* a machine of its own count: the
        // stash-pop stopper is not in `STATEFUL`, so every verb it draws is
        // trivially cross and would let a broken bias pass this test.
        for s in seqs
            .iter()
            .filter(|s| s.name.starts_with("gen/walk/") && STATEFUL.contains(&s.cmd()))
        {
            for i in 0..s.len() {
                let args = s.step_case(i).args;
                let is_resume = args.len() == 2
                    && STATEFUL.contains(&args[0].as_str())
                    && RESUME_TOKENS.contains(&args[1].as_str());
                if !is_resume {
                    continue;
                }
                if args[0] == s.cmd() {
                    own += 1;
                } else {
                    cross += 1;
                }
            }
        }
        assert!(cross > 0, "no walk ever addressed a verb to another machine");
        assert!(own > 0, "no walk ever took its own machine's legal transition");
    }

    /// A round trip runs its forward half before its inverse, in order, with the
    /// reads between them. Order is the whole content of the family: reversed, it
    /// would be two independent invocations that happen to share a repository.
    #[test]
    fn round_trips_run_forward_before_inverse() {
        let seqs = generate_sequences(555, 1);
        for rt in ROUND_TRIPS {
            let name = format!("gen/roundtrip/{}#0", rt.name);
            let s = seqs.iter().find(|q| q.name == name).expect("round trip generated");
            let argvs: Vec<Vec<String>> = (0..s.len()).map(|i| s.step_case(i).args).collect();
            let position = |want: &[&str]| -> usize {
                let want: Vec<String> = want.iter().map(|t| t.to_string()).collect();
                argvs.iter().position(|a| *a == want).unwrap_or_else(|| {
                    panic!("{name} never runs {want:?}; steps were {argvs:?}")
                })
            };
            let last_forward = rt.forward.iter().map(|s| position(s)).max().unwrap();
            let first_inverse = rt.inverse.iter().map(|s| position(s)).min().unwrap();
            assert!(
                last_forward < first_inverse,
                "{name} runs its inverse before its forward half"
            );
        }
    }

    /// A step is only handed a payload where the invocation asks for one, exactly
    /// as a single case is — the sequence generator reuses [`sample_stdin`]
    /// rather than deciding for itself, and this pins that it did not grow a
    /// second rule. A payload delivered to a step that does not read it makes the
    /// step id lie about what the step does.
    #[test]
    fn generated_steps_only_get_stdin_where_it_is_read() {
        for s in generate_sequences(88, 2) {
            for i in 0..s.len() {
                let case = s.step_case(i);
                if case.stdin.is_some() {
                    assert!(
                        wants_stdin(&case.args[0], &case.args),
                        "{} step {} was fed a payload it never reads: {:?}",
                        s.name,
                        i + 1,
                        case.args
                    );
                }
            }
        }
    }

    /// The id grammar `scripts/split_failures.pl` parses survives a generated
    /// sequence: `<shape>::<cmd>::` off the front, both segments free of
    /// whitespace, and the command segment the verb the sequence is scored under.
    /// A generated failure that files under no subcommand disappears from the
    /// per-command briefs, which is worse than one that shouts.
    #[test]
    fn generated_sequence_ids_keep_the_shape_and_command_segments_first() {
        for s in generate_sequences(31337, 2) {
            for i in 0..s.len() {
                let id = s.step_id(i);
                let (shape, rest) =
                    id.trim_start_matches('!').split_once("::").expect("shape segment");
                let (cmd, rest) = rest.split_once("::").expect("command segment");
                assert!(!shape.contains(char::is_whitespace), "shape segment has a space: {id}");
                assert!(!cmd.contains(char::is_whitespace), "command segment has a space: {id}");
                assert_eq!(cmd, s.cmd());
                assert!(rest.starts_with("seq["), "sequence segment missing from {id}");
            }
        }
    }
}
