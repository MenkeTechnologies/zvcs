//! Differential corpus cases for the **on-disk index file's format**: which
//! version of it a command writes, and which extensions get chained onto the
//! end.
//!
//! Not what the index *holds* — that is `ls-files --stage`, and half the corpus
//! already measures it. This module is about the bytes underneath: `DIRC`, the
//! big-endian version word, and the `(signature, length, body)` chain after the
//! entries. Stock git has to be able to read what the port writes and the port
//! has to be able to read what stock writes, and the levers that decide which
//! bytes get written are `index.version`, `GIT_INDEX_VERSION`,
//! `feature.manyFiles`, `index.skipHash`, `index.recordEndOfIndexEntries`,
//! `index.recordOffsetTable`, `index.threads`, `--index-version=` and
//! `--split-index`.
//!
//! # How the territory is divided
//!
//! Five modules already stand on parts of this ground, and none of what they own
//! is repeated here:
//!
//! * **`corpus/index_plumbing.rs`** — the nearest neighbour. Owns `ls-files`'s
//!   selection algebra and output shapes, `update-index --index-info`/`--stdin`
//!   as *grammars*, `read-tree`'s merge modes and refusals, `checkout-index`'s
//!   create/overwrite rules, and `status --porcelain=v2`. Its
//!   `update_index_format` group owns the `index.version=2|3|4` config on
//!   `--cacheinfo`, `--index-version 3|9`, `--show-index-version` on
//!   `Conflicted`, `index.skipHash`, `--split-index` under `core.splitIndex`,
//!   `--test-untracked-cache`, `--fsmonitor-valid`, the `core.checkStat` /
//!   `core.trustctime` / `core.fileMode` refresh trio, and the one
//!   `core.preloadIndex`+`index.threads=1` `ls-files` case. It also owns
//!   `read-tree --empty` under `index.version=4`.
//! * **`corpus/worktree_index.rs`** — owns `update-index`'s bare flag surface:
//!   `--refresh`/`--really-refresh`, `--add`/`--remove`/`--force-remove`,
//!   `--info-only`, `--again`, `--assume-unchanged`, `--skip-worktree`,
//!   `--chmod=`, `--cacheinfo` in both spellings, `--show-index-version`,
//!   `--index-version 4` on `Linear` and `--index-version 2` on `Branched`,
//!   `--split-index`,
//!   `--untracked-cache`, `--fsmonitor`, and `checkout-index`'s own flags.
//! * **`corpus/fixture_gaps3.rs`** — owns `Shape::SplitIndex` and everything run
//!   against it, including `splitIndex.maxPercentChange`.
//! * **`corpus/sparse_family.rs`** — owns the sparse index, the `sdir`
//!   extension, `core.sparseCheckout*` and `checkout-index --stage=all` on
//!   `Sparse`.
//! * **`corpus/object_pack.rs`** / **`corpus/plumbing_objects.rs`** — own the
//!   pack and loose-object formats and `read-tree`'s one/two/three-tree forms.
//! * **`corpus/sequences.rs`** — owns the multi-step split/unsplit sequences.
//!
//! What is added here is the part none of them reaches:
//!
//! 1. **`GIT_INDEX_VERSION`.** No curated case sets this variable — `fuzz.rs`
//!    samples it (`fuzz::ENV_VARS`), so it is reachable under `--fuzz`, but a
//!    default run never touches it — and it is a *different source* for the same
//!    lever from `index.version` and `--index-version`: a port could implement
//!    either of those and still ignore it. It also has two rules of its own: it
//!    is consulted only where the writer has no version to preserve, it **beats
//!    the configuration key in both directions**, and an invalid value produces a
//!    warning on stderr that no other spelling produces.
//! 2. **`feature.manyFiles`.** No curated case; `fuzz.rs` samples the key. It is
//!    the aggregate profile (`index.version=4` + `index.skipHash=true` +
//!    `core.untrackedCache=true`), and a port that reads the three keys
//!    individually but not the profile that sets them behaves differently from
//!    git on the setting Microsoft's large-repository documentation tells people
//!    to turn on first.
//! 3. **`index.recordEndOfIndexEntries` and `index.recordOffsetTable`.** No
//!    curated case for either; `fuzz.rs` samples both. They decide whether `EOIE`
//!    and `IEOT` are written at all, they can be set against `index.threads`
//!    rather than with it, and the port turns out to honour them everywhere
//!    except on an index with no entries — which is the case below that fails.
//! 4. **`--split-index` beside another index-writing option**, on `Shape::Linear`
//!    rather than on the split-index fixture. Stock drops the split — writing no
//!    `link` extension and no shared index file — when the same argv also
//!    carries `--refresh` or an `--index-version` naming a version the index
//!    does not already have. `fixture_gaps3.rs` cannot ask this question,
//!    because its shape is split before the case starts.
//! 5. **`--index-version 4` where the entries are not plain stage-0 entries** —
//!    three merge stages of one path (`Conflicted`), entries carrying extended
//!    flags (`Sparse`), and names that stress the prefix compression itself
//!    (`AwkwardPaths`: a name with an embedded `"`, and a multi-byte UTF-8 name
//!    last in sort order). Those are three different v4 encodings from the
//!    `Linear` one `worktree_index.rs` already runs, and they are the ones that
//!    break next if v4 writing is ever implemented.
//!
//! # The two questions this module was written to answer
//!
//! **Is `index.version=4`'s prefix compression implemented, or silently
//! ignored?** *Writing it is not implemented and the request is silently
//! ignored; reading it is implemented and correct.* Both halves measured by hand
//! against stock 2.55.0 and `target/debug/git`:
//!
//! ```text
//! $ git update-index --index-version 4          # Shape::Linear replica
//! stock: 240 bytes, v4;  entry 1 encodes strip=9 suffix="src/lib.rs"
//! port : 245 bytes, v2;  entry 1 encodes the whole name, NUL-padded
//! ```
//!
//! The port exits 0 and says nothing, so the only signal is the version word.
//! The same silent ignore happens through `-c index.version=4`,
//! `GIT_INDEX_VERSION=4` and `feature.manyFiles=true`. Reading, by contrast, is
//! right: handed a v4 index stock wrote over `Shape::Sparse` (760 bytes, three
//! skip-worktree entries), the port's `ls-files -v --stage`, `status --porcelain`
//! and `write-tree` were byte-identical to stock's, prefix-decompressed names and
//! extended flags included.
//!
//! **Does the port write an index stock reads differently?** *No index it was
//! made to write here was misread by stock.* Every shape of index this module
//! provokes was read back with stock's `ls-files --stage` (and, where the
//! objects exist, `write-tree` and `fsck`) after the port had written it —
//! including the split index the port creates where stock creates none, whose
//! `link:68` two-entry form stock resolved to the same two paths and the same
//! `write-tree` as the port's own — and stock agreed with the port about the
//! contents every time. Where stock did complain it complained about the port's
//! file and its own file identically (`fsck` says `missing tree 4b825dc6…` over
//! both `read-tree --empty` results; `write-tree` refuses over both
//! `--cacheinfo`-built indexes, because the empty blob those name is not in the
//! fixture's store). What the port gets wrong is the *format selection* — which
//! version, which extensions — never the encoding of what it did select. Two
//! consequences worth stating because they are the shape of the risk rather than
//! a defect found:
//!
//! * The port **downgrades a v4 index it read**. `update-index --add --cacheinfo`
//!   against the v4 `Sparse` index above left stock's copy at v4 (812 bytes) and
//!   the port's at v3 (839 bytes), same eight entries in the same order with the
//!   same skip-worktree bits, and stock's `ls-files -v --stage` over the two
//!   files was byte-identical. That is a version regression rather than
//!   corruption, and it is
//!   **not reachable from a single invocation** — no `Shape` ships a v4 index and
//!   this module may not add one — so it is recorded here and not shipped as a
//!   case. `sequences.rs` is where it could live.
//! * `index.skipHash` *is* implemented: under it both sides write twenty zero
//!   bytes where the trailing checksum goes, verified directly.
//!
//! # What is measurable here, and what is not
//!
//! `runner::probe_index_meta` reads `.git/index` (and any `sharedindex.*` beside
//! it) and reports `v<version> entries=<n> ext=[SIG:len(detail),…]`. That is the
//! whole instrument, and it decides the shape of this module:
//!
//! * **The version word and the extension chain are comparable**, so every case
//!   here is scored on the state digest rather than on stdout — most of them
//!   print nothing at all.
//! * **A v4 index reads `ext=<unparsed>`**, symmetrically on both sides, because
//!   v4 entry names are prefix-compressed and the chain cannot be reached
//!   without decompressing every one. The *version* is still compared, which is
//!   all the v4 cases below need.
//! * **`UNTR` is not measurable and never will be, on this harness.** Not for a
//!   cautious reason: the extension's body opens with git's environment ident
//!   string, and that string is `Location <absolute path of the worktree>,
//!   system <uname>`. Read back out of an untracked cache stock 2.55.0 wrote in
//!   this crate's own scratch directory, the extension was `UNTR:451` and its
//!   body began `Location /private/tmp/claude-501/…/untr_a/r, system Darwin`.
//!   Two fixture copies live at two paths of different lengths, so the
//!   extension's *length* differs between them by construction, before any
//!   implementation difference. That is why every `core.untrackedCache` case
//!   below pins the key **off**: `feature.manyFiles` turns it on, and a case that
//!   let it stay on would be comparing two path lengths. `index_plumbing.rs`
//!   reached the same conclusion from the other end; this is the byte-level
//!   reason for it.
//! * **`core.fsmonitor=true` is not measurable either, for two independent
//!   reasons.** It spawns a *detached background daemon* that outlives the case
//!   — measured: after one `update-index --refresh` under it,
//!   `git fsmonitor--daemon run --detach --ipc-threads=8` was still running and
//!   holding the fixture, and had to be killed by hand. And the `FSMN` extension
//!   it writes carries a v2 token that is
//!   `builtin:0.<pid>.<UTC wall clock to microseconds>Z:0` — the bytes read back
//!   were `builtin:0.64078.20260830T111910.560348Z:0`, in a 70-byte extension. A
//!   pid and a clock cannot be compared between two runs of anything. No case
//!   here configures it; `index_plumbing.rs` and `worktree_index.rs` already pin
//!   the *unconfigured* default, which is the part that can be pinned.
//! * **`update-index --refresh` / `--really-refresh` cannot carry a format
//!   case.** There is a real divergence there — under `index.threads` or
//!   `index.recordEndOfIndexEntries` stock rewrites the index and gains the
//!   extension where the port does not rewrite it at all — but whether stock
//!   writes depends on whether the refresh found a stat to update, and four runs
//!   of `-c index.threads=2 update-index --refresh` against four freshly built
//!   `Shape::Linear` replicas split two-and-two: `245 bytes ext=[TREE]` on the
//!   first two, `305 bytes ext=[IEOT,TREE,EOIE]` on the second two. Stock is not
//!   reproducible there, so the finding is recorded and not shipped as a case.
//! * **A split index the *port* creates has a nondeterministic file name** —
//!   `sharedindex.e92163c0…` and `sharedindex.0fe9e0cf…` from two runs of one
//!   command against two identically built repositories — because the shared
//!   half carries stat data. `probe_index_meta` elides checksum-bearing names and
//!   reports only the parsed meta (`v2 entries=2 ext=[]`, identical across those
//!   two runs), so the cases below that provoke it are still comparable. Nothing
//!   here asks the *shared* index to be rewritten, which is where
//!   `fixture_gaps3.rs` lives.
//! * **How stock divides entries between the split half and the shared half is
//!   stat-sensitive, and this is the one soft spot in the module.** `update-index
//!   --split-index` writes *some* entries into `.git/index` and the rest only
//!   into `sharedindex.<hash>`, and which is which depends on what the refresh
//!   inside it found. Measured on `Shape::Linear`: three copies of one template,
//!   run three seconds after the copy, all gave `297 bytes entries=2`; three
//!   repositories built and split within the same second gave `297 bytes
//!   entries=2`, `161 bytes entries=0` and `297 bytes entries=2`. The port
//!   always writes `297 bytes entries=2`. The harness copies a prebuilt template
//!   and so lands on the reproducible side — its own stock-vs-stock repeat check
//!   is what enforces that, and it passes — but the copy-then-run discipline is
//!   load-bearing for the `--index-version 2 --split-index` control below, and a
//!   future reader who sees that row flake should look here first. It is also why
//!   no *new* `--split-index` case is added: `--split-index` on `Conflicted`
//!   diverges (stock `142 bytes entries=0 ext=[link:60,TREE]` against the port's
//!   `406 bytes entries=4 ext=[link:68,TREE]`, stable over five runs, and stock
//!   reads the port's file back identically), but it is the same stat-sensitive
//!   machinery and a case resting on it would be a coin the harness has to keep
//!   re-flipping.
//!
//! Every behaviour asserted in this file was measured against stock 2.55.0
//! (`/opt/homebrew/bin/git`) and `target/debug/git` in hand-built replicas of
//! the shapes named, twice each, with `.git/index` parsed byte by byte rather
//! than inferred from `ls-files`; the citations are to `read-cache.c`
//! (`get_index_format_default`, `do_write_index`), `builtin/update-index.c` and
//! `builtin/read-tree.c`.

use crate::fixture::Shape;
use crate::runner::Case;

/// The empty blob — a constant of the hash function rather than a fact about
/// any fixture, so `--cacheinfo` can name it from a literal argv.
const EMPTY_BLOB: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

/// `feature.manyFiles` turns the untracked cache on, and the untracked cache is
/// the one index extension this harness cannot compare (see the module header:
/// its body embeds the worktree's absolute path). Every case that draws the
/// profile pins the key off, so what is measured is the profile's *other* two
/// implications — `index.version=4` and `index.skipHash=true` — rather than a
/// path length.
const MANY_FILES: &[(&str, &str)] = &[
    ("feature.manyFiles", "true"),
    ("core.untrackedCache", "false"),
];

/// Append this subsystem's cases to the corpus.
pub fn cases(out: &mut Vec<Case>) {
    env_index_version(out);
    env_against_config(out);
    many_files_profile(out);
    offset_table_and_eoie(out);
    version_against_split_index(out);
    version_of_awkward_entries(out);
}

/// `GIT_INDEX_VERSION`: the format lever no curated case had ever set.
///
/// # Why it is not a third spelling of `index.version`
///
/// `read-cache.c:get_index_format_default` reads the environment variable
/// first and the `index.version` key second, and `do_write_index` consults
/// neither unless the index being written has no version of its own — which is
/// to say, unless the writer built the index from scratch rather than reading
/// one off disk. Measured on stock 2.55.0 in hand-built replicas of these
/// shapes, and the split is sharp enough to be worth stating as the contract
/// these cases pin. The result column is the index the command left behind; the
/// port's column is what it left behind for the same command:
///
/// | invocation | shape | stock, `GIT_INDEX_VERSION=4` | port |
/// |---|---|---|---|
/// | `read-tree HEAD` | `Linear` | **v4**, 240 B | v2, 245 B |
/// | `read-tree --empty` | `Linear` | **v4**, 65 B | v2, 65 B |
/// | `read-tree HEAD` | `Sparse` | **v4**, 754 B | v2, 778 B |
/// | `read-tree --reset HEAD` | `Conflicted` | v2, 325 B | v2, 325 B |
/// | `read-tree --prefix=graft/ HEAD` | `Linear` | v2, 444 B | v2, 444 B |
/// | `update-index --add untracked.txt` | `Dirty` | v2, 386 B | v2, 386 B |
///
/// So the variable is not "write version N"; it is "when you have to *choose* a
/// version, choose N", and three of those six rows are the negative half of the
/// contract. A port that applied the variable to every write would pass a case
/// built only from the first three rows and fail the last three, which is why
/// they are here.
///
/// An invalid value is the third thing the variable does that no other spelling
/// does: `warning: GIT_INDEX_VERSION set, but the value is invalid.` followed by
/// `Using version 3`, on stderr, at exit 0 — and then version 3 is demoted to 2
/// anyway because no entry needs the extended flags. **The port prints nothing
/// and writes the same index**, so those cases are `Case::strict`: the warning is
/// the entire observable difference. Reproduced by hand for `1`, `7` and `abc`,
/// stock warning on all three and the port silent on all three.
///
/// `GIT_INDEX_VERSION=2` on a write that would have chosen 2 anyway had a case
/// here and no longer does: the index it produces is byte-for-byte the index the
/// variable-free `read-tree HEAD` produces, so no implementation could pass the
/// unset spelling and fail that one. What replaced it is [`env_against_config`],
/// where asking for 2 *does* decide something.
fn env_index_version(out: &mut Vec<Case>) {
    // The variable applied. `read-tree` builds its result index from the tree
    // rather than editing the one on disk, so this is the path where git has a
    // version to choose. Stock writes v4 and its prefix-compressed names; the
    // port writes v2.
    for args in [
        &["read-tree", "HEAD"][..],
        &["read-tree", "--empty"][..],
    ] {
        out.push(
            Case::new("read-tree", args, Shape::Linear)
                .with_env(&[("GIT_INDEX_VERSION", "4")]),
        );
    }
    // The same, where the index git replaces holds skip-worktree bits. It comes
    // out v2 without the variable — `read-tree` drops those bits — so this pins
    // the version choice and not the flag width.
    out.push(
        Case::new("read-tree", &["read-tree", "HEAD"], Shape::Sparse)
            .with_env(&[("GIT_INDEX_VERSION", "4")]),
    );

    // The variable *not* applied, three ways. Each of these is an agreement
    // case and each is load-bearing: they are what a port that treated
    // `GIT_INDEX_VERSION` as an unconditional "write version N" would break.
    out.push(
        Case::new("read-tree", &["read-tree", "--reset", "HEAD"], Shape::Conflicted)
            .with_env(&[("GIT_INDEX_VERSION", "4")]),
    );
    out.push(
        Case::new("read-tree", &["read-tree", "--prefix=graft/", "HEAD"], Shape::Linear)
            .with_env(&[("GIT_INDEX_VERSION", "4")]),
    );
    out.push(
        Case::new("update-index", &["update-index", "--add", "untracked.txt"], Shape::Dirty)
            .with_env(&[("GIT_INDEX_VERSION", "4")]),
    );
    // The explicit option decides, and an index that already carries a version
    // keeps it: v2 out, not v4, with the variable asking for 4.
    out.push(
        Case::new("update-index", &["update-index", "--index-version", "2"], Shape::Linear)
            .with_env(&[("GIT_INDEX_VERSION", "4")]),
    );

    // Version 3 asked for and version 2 written, because `do_write_index`
    // demotes v3 to v2 when no entry needs the extended flags. The value is
    // valid, so there is no warning — the difference between this and the
    // invalid cases below is entirely on stderr.
    out.push(
        Case::new("read-tree", &["read-tree", "HEAD"], Shape::Linear)
            .with_env(&[("GIT_INDEX_VERSION", "3")]),
    );

    // Three invalid values across the two ways of being invalid: below the
    // supported range, above it, and not a number at all. All three produce the
    // identical two-line warning and an index no different from the one written
    // without the variable, so the whole measurement is `compare_stderr`.
    for (value, args) in [
        ("1", &["read-tree", "HEAD"][..]),
        ("7", &["read-tree", "--empty"][..]),
        ("abc", &["read-tree", "--empty"][..]),
    ] {
        out.push(
            Case::strict("read-tree", args, Shape::Linear)
                .with_env(&[("GIT_INDEX_VERSION", value)]),
        );
    }
}

/// The environment variable **against** the configuration key, both directions —
/// a precedence rule nothing in the corpus reached, because nothing set both
/// sources at once.
///
/// `get_index_format_default` returns on the environment variable if it parses,
/// and only falls through to `index.version` if it does not. So the variable
/// wins whichever way the two disagree, and the two directions fail differently:
///
/// | configuration | environment | stock | port |
/// |---|---|---|---|
/// | `index.version=2` | `GIT_INDEX_VERSION=4` | **v4**, 65 B | v2, 65 B |
/// | `index.version=4` | `GIT_INDEX_VERSION=2` | v2, 65 B | v2, 65 B |
/// | `feature.manyFiles=true` | `GIT_INDEX_VERSION=2` | v2, 65 B | v2, 65 B |
///
/// All on `read-tree --empty` over `Shape::Linear`, each run twice against stock
/// and identical both times.
///
/// The first row is a divergence the port fails today, for the same reason it
/// fails every other v4 row: it never writes v4. The second and third are
/// agreements, and they are here for what they will catch rather than for what
/// they catch now. A port that implements v4 writing by reading `index.version`
/// and forgetting the variable passes row 1 and *keeps* passing rows 2 and 3 —
/// but one that implements it by reading the variable and letting the key
/// override it fails row 2, and one that lets the `feature.manyFiles` profile
/// override it fails row 3. Neither of those two mistakes is visible from any
/// case that sets one source at a time, which is every other case in this
/// module. That is the whole reason the rows exist, and it is stated plainly
/// because as of this writing they compare two v2 indexes and prove nothing on
/// their own.
fn env_against_config(out: &mut Vec<Case>) {
    out.push(
        Case::new("read-tree", &["read-tree", "--empty"], Shape::Linear)
            .with_config(&[("index.version", "2")])
            .with_env(&[("GIT_INDEX_VERSION", "4")]),
    );
    out.push(
        Case::new("read-tree", &["read-tree", "--empty"], Shape::Linear)
            .with_config(&[("index.version", "4")])
            .with_env(&[("GIT_INDEX_VERSION", "2")]),
    );
    out.push(
        Case::new("read-tree", &["read-tree", "--empty"], Shape::Linear)
            .with_config(MANY_FILES)
            .with_env(&[("GIT_INDEX_VERSION", "2")]),
    );
    // The variable's version choice and the `EOIE` decision in one command, on
    // the index where the port gets both wrong at once: stock writes
    // `v4, 97 B, ext=[TREE,EOIE]` and the port writes `v2, 65 B, ext=[TREE]`.
    // Separately each half is pinned above and in `offset_table_and_eoie`;
    // together they are the case that says the two are independent rather than
    // one bug reported twice.
    out.push(
        Case::new("read-tree", &["read-tree", "--empty"], Shape::Linear)
            .with_config(&[("index.recordEndOfIndexEntries", "true")])
            .with_env(&[("GIT_INDEX_VERSION", "4")]),
    );
}

/// `feature.manyFiles`: the aggregate profile, and the three keys it stands for.
///
/// `feature.manyFiles=true` is not a setting of its own — it is a *default* for
/// `index.version=4`, `index.skipHash=true` and `core.untrackedCache=true`, each
/// of which an explicit setting still overrides. That structure is what these
/// cases are for: reading the three keys and not the profile, or reading the
/// profile and letting it beat an explicit key, are two different bugs and
/// neither is visible from a case that sets the keys directly.
///
/// Measured on stock 2.55.0 and `target/debug/git`, all with
/// `core.untrackedCache=false` pinned for the reason in the module header:
///
/// * `feature.manyFiles=true read-tree --empty` → stock **v4** 65 B, port v2
///   65 B. `read-tree HEAD` the same way: stock v4 240 B, port v2 245 B.
/// * The profile also turns `index.skipHash` on, and it reaches both sides: the
///   trailer of every index above is twenty zero bytes on stock *and* on the
///   port. Cancelling it (`index.skipHash=false`) restores a real checksum on
///   both and leaves the version divergence alone, which is what separates the
///   two implications.
/// * `feature.manyFiles=true index.version=2 read-tree --empty` → v2 on both
///   sides: the explicit key beats the profile.
/// * `feature.manyFiles=true update-index --add --cacheinfo …` → v2 298 B on
///   both sides: the profile supplies a *default* version, and an index read off
///   disk already has one, so there is nothing to default.
/// * `feature.manyFiles=false read-tree --empty` → v2 65 B on both sides.
fn many_files_profile(out: &mut Vec<Case>) {
    // The profile's `index.version=4` implication, on the two `read-tree` forms
    // that have a version to choose.
    out.push(
        Case::new("read-tree", &["read-tree", "--empty"], Shape::Linear)
            .with_config(MANY_FILES),
    );
    out.push(
        Case::new("read-tree", &["read-tree", "HEAD"], Shape::Linear)
            .with_config(MANY_FILES),
    );
    // With the profile's *other* implication cancelled. `index.skipHash` decides
    // whether the trailing checksum is written or left null, and the port
    // implements it — verified directly: under `index.skipHash=true` both sides
    // wrote twenty zero bytes where the hash goes. Cancelling it here isolates
    // the version implication from it, so a port that mixed the two up has one
    // case that separates them.
    out.push(
        Case::new("read-tree", &["read-tree", "--empty"], Shape::Linear)
            .with_config(&[
                ("feature.manyFiles", "true"),
                ("core.untrackedCache", "false"),
                ("index.skipHash", "false"),
            ]),
    );
    // Precedence: the explicit key beats the profile that would have defaulted
    // it. An implementation that applies the profile after the keys writes v4
    // here and stock writes v2.
    out.push(
        Case::new("read-tree", &["read-tree", "--empty"], Shape::Linear)
            .with_config(&[
                ("feature.manyFiles", "true"),
                ("core.untrackedCache", "false"),
                ("index.version", "2"),
            ]),
    );
    // The profile against an index that already has a version: nothing to
    // default, so nothing changes. The agreement half of the same rule
    // `env_index_version` pins for the environment variable.
    let add = format!("100644,{EMPTY_BLOB},many.txt");
    out.push(
        Case::new("update-index", &["update-index", "--add", "--cacheinfo", &add], Shape::Linear)
            .with_config(MANY_FILES),
    );
    // The explicit-off spelling of the default, which is not the same code path
    // as the key being absent.
    out.push(
        Case::new("read-tree", &["read-tree", "--empty"], Shape::Dirty)
            .with_config(&[("feature.manyFiles", "false")]),
    );
}

/// `EOIE` and `IEOT`: the two extensions that exist so a reader can split the
/// entry table across threads, and the three keys that decide whether they are
/// written.
///
/// `index.recordEndOfIndexEntries` and `index.recordOffsetTable` had no case in
/// the corpus at all, and `index.threads` had one (`index_plumbing.rs`, on
/// `ls-files`, where nothing is written). They are not one knob: `index.threads`
/// greater than one turns both extensions on, and either key can then be set
/// against it — `recordEndOfIndexEntries=false` with threads leaves `IEOT` and
/// removes `EOIE`, `recordOffsetTable=false` with threads leaves `EOIE` and
/// removes `IEOT`.
///
/// The port implements all of this, and these are pinning cases rather than
/// failing ones. Measured against stock 2.55.0 on `Shape::Linear`, both sides
/// byte-for-byte in length and chain:
///
/// | configuration | chain written | bytes |
/// |---|---|---|
/// | `recordEndOfIndexEntries=true` | `TREE`, `EOIE` | 330 |
/// | `threads=2` | `IEOT`, `TREE`, `EOIE` | 358 |
/// | `threads=2 recordEndOfIndexEntries=false` | `IEOT`, `TREE` | 326 |
/// | `threads=2 recordOffsetTable=false` | `TREE`, `EOIE` | 330 |
///
/// The one place it comes apart is an index with **no entries**: stock writes
/// `EOIE` onto an empty index (`97` bytes, `ext=[TREE:25,EOIE:24]`, against the
/// port's `65` bytes and `ext=[TREE:25]`) and the port writes none, under either
/// spelling of the request. That is the last two cases here, and it is a defect
/// in the port's writer rather than in its reader: stock reads the port's short
/// index back as an empty index (`ls-files --stage` prints nothing, exit 0), and
/// the port reads stock's 97-byte one back as an empty index too (`ls-files
/// --stage` nothing, `status --porcelain` the four expected deletion and
/// untracked rows). `fsck` complains `missing tree 4b825dc6…` at exit 2 over
/// *both* files identically — that is the `TREE` extension caching an empty tree
/// the fixture's object store never had, not anything to do with `EOIE`.
fn offset_table_and_eoie(out: &mut Vec<Case>) {
    let add = format!("100644,{EMPTY_BLOB},eoie.txt");

    // The four combinations, on the write path that always writes.
    for config in [
        &[("index.recordEndOfIndexEntries", "true")][..],
        &[("index.recordEndOfIndexEntries", "false"), ("index.threads", "2")][..],
        &[("index.recordOffsetTable", "false"), ("index.threads", "2")][..],
        &[("index.recordEndOfIndexEntries", "true"), ("index.skipHash", "true")][..],
    ] {
        out.push(
            Case::new("update-index", &["update-index", "--add", "--cacheinfo", &add], Shape::Linear)
                .with_config(config),
        );
    }

    // The same request against the three index shapes whose entry table is not a
    // flat stage-0 list, because `IEOT` partitions *that* table: entries
    // carrying extended flags (`Sparse` is v3 by construction), three merge
    // stages of one path (`Conflicted`), and intent-to-add entries
    // (`IntentToAdd`, also v3). A partitioner that counts entries the way
    // `ls-files` counts paths gets a different block table on each of these.
    for shape in [Shape::Sparse, Shape::Conflicted, Shape::IntentToAdd] {
        out.push(
            Case::new("update-index", &["update-index", "--add", "--cacheinfo", &add], shape)
                .with_config(&[("index.threads", "2")]),
        );
    }

    // `read-tree` rather than `update-index`, so the extensions are written by
    // the tree-unpacking writer instead of the entry-editing one. Both are the
    // same function in git and there is no reason for them to be the same
    // function in a port.
    out.push(
        Case::new("read-tree", &["read-tree", "HEAD"], Shape::Linear)
            .with_config(&[("index.threads", "2")]),
    );
    out.push(
        Case::new("read-tree", &["read-tree", "HEAD"], Shape::Sparse)
            .with_config(&[("index.threads", "2")]),
    );

    // The empty index. Stock writes `EOIE` over zero entries; the port writes
    // the chain without it. Both spellings of the request, because they reach
    // the decision by different routes — one sets the key, the other sets
    // `index.threads` and lets git default the key from it.
    out.push(
        Case::new("read-tree", &["read-tree", "--empty"], Shape::Linear)
            .with_config(&[("index.recordEndOfIndexEntries", "true")]),
    );
    out.push(
        Case::new("read-tree", &["read-tree", "--empty"], Shape::Linear)
            .with_config(&[("index.threads", "2")]),
    );
}

/// `--split-index` beside another index-writing option, on a shape that is not
/// already split.
///
/// # The rule, measured rather than assumed
///
/// `update-index --split-index` on `Shape::Linear` splits: `.git/index` comes
/// out 297 bytes carrying `link:68`, and a `sharedindex.<hash>` of 184 bytes
/// appears beside it. `worktree_index.rs` owns that case and both binaries pass
/// it.
///
/// Put a second index-writing option in the same argv and stock 2.55.0 stops
/// splitting — silently, at exit 0, leaving the index exactly as it found it.
/// The port splits anyway. Measured on `Shape::Linear`:
///
/// | argv | stock | port |
/// |---|---|---|
/// | `--split-index` | 297 B, 1 shared | 297 B, 1 shared |
/// | `--index-version 2 --split-index` | 297 B, 1 shared | 297 B, 1 shared |
/// | `--index-version 3 --split-index` | **245 B, 0 shared** | 297 B, 1 shared |
/// | `--index-version 4 --split-index` | **240 B, v4, 0 shared** | 297 B, 1 shared |
/// | `--split-index --index-version 4` | **240 B, v4, 0 shared** | 297 B, 1 shared |
/// | `--split-index --refresh` | **245 B, 0 shared** | 297 B, 1 shared |
/// | `--refresh --split-index` | **245 B, 0 shared** | 297 B, 1 shared |
///
/// Two things follow, and the case list is built to separate them. It is **not**
/// specific to version 4 — version 3 does it too, and version 3 on this index
/// is demoted straight back to 2, so the resulting file is byte-identical to the
/// one that was there. And it is **not** specific to `--index-version` at all —
/// `--refresh` does it with no version option in the argv. What the two have in
/// common is that they are the other options that make `update-index` write, and
/// `--index-version 2` — which asks for the version already in place, so it asks
/// for no write — leaves the split alone. That last row is the control case
/// below: without it, "the port splits when stock does not" would be
/// indistinguishable from "the port splits whenever `--split-index` is given",
/// which is the correct behaviour.
///
/// The consequence is a file: the port leaves a `sharedindex.<hash>` in `.git`
/// that stock never created, and an index that points at it. That is why these
/// cases live here rather than in `fixture_gaps3.rs`, whose `Shape::SplitIndex`
/// cases all start from a repository that is *already* split and so cannot ask
/// whether the split should have happened.
///
/// # Determinism, and the one row that has to be watched
///
/// This is the one group in the module that provokes the port into writing a
/// checksum-named file, and the shared index carries stat data — two runs of one
/// command against two identically built repositories produced
/// `sharedindex.e92163c0…` and `sharedindex.0fe9e0cf…`. `probe_index_meta`
/// elides checksum-bearing names and reports the parsed meta, which was
/// `v2 entries=2 ext=[]` on both of those runs, so the digest is stable. Stock's
/// side writes no such file at all on the five diverging rows.
///
/// The **control row** — `--index-version 2 --split-index`, the one where stock
/// does split — is the exception and the module header says why: how many
/// entries stock leaves in the split half depends on what the refresh inside it
/// found. Three copies of one template, split three seconds after the copy, all
/// gave `297 B entries=2`; three repositories built and split within the same
/// second gave `297 B entries=2`, `161 B entries=0`, `297 B entries=2`. The
/// harness runs from copies of a prebuilt template, which is the reproducible
/// side, and its own stock-vs-stock repeat check is what keeps that honest. The
/// port writes `297 B entries=2` unconditionally. If this row ever reports
/// `zvcs-flaky`, the answer is that stock moved, not that the port did.
/// Stock reads the port's split index back correctly either way: `ls-files
/// --stage`, `write-tree` and `fsck` over the port's `link:68`, 2-entry split
/// index all matched the port's own answers.
fn version_against_split_index(out: &mut Vec<Case>) {
    // A version the index does not already have, both option orders, at the two
    // versions that are not the current one.
    for args in [
        &["update-index", "--index-version", "4", "--split-index"][..],
        &["update-index", "--split-index", "--index-version", "4"][..],
        &["update-index", "--index-version", "3", "--split-index"][..],
    ] {
        out.push(Case::new("update-index", args, Shape::Linear));
    }
    // No version option anywhere: `--refresh` alone is enough to make stock drop
    // the split. Both orders, because a resolution implemented as "whichever
    // option came last" would agree on one of them and not the other.
    out.push(Case::new("update-index", &["update-index", "--split-index", "--refresh"], Shape::Linear));
    out.push(Case::new("update-index", &["update-index", "--refresh", "--split-index"], Shape::Linear));
    // The control. `--index-version 2` names the version the index already has,
    // so it asks for nothing, and the split goes ahead on both sides. Without
    // this case the group would not distinguish "the port splits where stock
    // does not" from "the port splits whenever asked", which is correct.
    out.push(Case::new("update-index", &["update-index", "--index-version", "2", "--split-index"], Shape::Linear));

    // The configuration route rather than the option. `core.splitIndex=true`
    // does not force a split onto a command that is changing the version:
    // stock writes a plain v4 index and no shared file, the port writes a plain
    // v2 index and no shared file — so this pair measures the version choice
    // with the split question answered the same way on both sides, and its
    // `--index-version 3` twin is a full agreement.
    out.push(
        Case::new("update-index", &["update-index", "--index-version", "4"], Shape::Linear)
            .with_config(&[("core.splitIndex", "true")]),
    );
    out.push(
        Case::new("update-index", &["update-index", "--index-version", "3"], Shape::Linear)
            .with_config(&[("core.splitIndex", "true")]),
    );
}

/// `--index-version` where the entries are not a flat list of stage-0 paths.
///
/// `worktree_index.rs` runs `--index-version 4` on `Linear` and
/// `--index-version 2` on `Branched`, whose indexes are two and three plain
/// stage-0 entries. Those are the easy encodings.
/// The ones below are the ones with a rule attached, and each has a distinct
/// failure available to it. Every row was read back out of `.git/index` entry by
/// entry rather than through `ls-files`, because the encoding is the point:
///
/// * **`Conflicted` at version 4.** Four entries for three paths, two of them
///   differing only in their stage number. Version 4 prefix-compresses each name
///   against the previous one, so `conflict.txt` at stage 2 and `conflict.txt`
///   at stage 3 encode as a full name and then `strip=0` with a *zero-length
///   suffix* — the degenerate case of the compression, and the one an encoder
///   written from the documentation is most likely to get wrong. Stock writes v4
///   at 361 bytes; the port writes v2 at 386.
/// * **`Sparse` at version 4.** Seven entries, three of them carrying the
///   skip-worktree bit, so the index is v3 before the command runs. Going to v4
///   keeps the extended flags and changes the name encoding underneath them.
///   Stock: v4, 760 bytes, `outside/nested/deep.txt` encoding as
///   `strip=8 suffix="nested/deep.txt"` against the previous entry's flags.
///   Port: v3, 786.
/// * **`AwkwardPaths` at version 4.** Six entries whose names are what the
///   compression has to survive rather than what it likes: `quote"name.txt`
///   carries an embedded double quote, `üñïçødé.txt` is multi-byte UTF-8 and
///   sorts *last*, so it is the entry encoded against the longest strip, and
///   `nested/deep/path.txt` supplies a two-level prefix for the entry after it.
///   Stock: v4, 621 bytes, with the final entry at `strip=14`. Port: v2, 633.
///   The same shape under `GIT_INDEX_VERSION=4 read-tree HEAD` is the second
///   case, because `read-tree` reaches the writer through the tree-unpacking
///   path rather than the entry-editing one.
/// * **`Sparse` and `IntentToAdd` at version 2.** The demotion that must *not*
///   happen: an entry with an extended flag cannot be expressed in version 2, so
///   git writes version 3 whatever was asked for — `Sparse` 786 B v3,
///   `IntentToAdd` 706 B v3. Both sides agree, and this is the case a port that
///   treats `--index-version` as an assignment rather than as a floor would
///   break — silently, by dropping every skip-worktree and intent-to-add bit in
///   the index.
fn version_of_awkward_entries(out: &mut Vec<Case>) {
    out.push(Case::new("update-index", &["update-index", "--index-version", "4"], Shape::Conflicted));
    out.push(Case::new("update-index", &["update-index", "--index-version", "4"], Shape::Sparse));
    out.push(Case::new(
        "update-index",
        &["update-index", "--index-version", "4"],
        Shape::AwkwardPaths,
    ));
    out.push(
        Case::new("read-tree", &["read-tree", "HEAD"], Shape::AwkwardPaths)
            .with_env(&[("GIT_INDEX_VERSION", "4")]),
    );
    out.push(Case::new("update-index", &["update-index", "--index-version", "2"], Shape::Sparse));
    out.push(Case::new("update-index", &["update-index", "--index-version", "2"], Shape::IntentToAdd));
    // Asked for exactly what the entries need. Nothing moves, which is the point:
    // an implementation that rewrites the entry table whenever the version option
    // is present has a different set of bytes here from one that notices the
    // version is already right.
    out.push(Case::new("update-index", &["update-index", "--index-version", "3"], Shape::IntentToAdd));
}


