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
//!   `--index-version 2|4` on `Linear`/`Branched`, `--split-index`,
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
//! 1. **`GIT_INDEX_VERSION`.** Zero cases in the whole corpus set this variable,
//!    and it is a *different source* for the same lever from `index.version` and
//!    `--index-version`: a port could implement either of those and still ignore
//!    it. It also has a rule of its own — it is consulted only where the writer
//!    has no version to preserve — and an invalid value produces a warning on
//!    stderr that no other spelling produces.
//! 2. **`feature.manyFiles`.** Zero cases. It is the aggregate profile
//!    (`index.version=4` + `index.skipHash=true` + `core.untrackedCache=true`),
//!    and a port that reads the three keys individually but not the profile that
//!    sets them behaves differently from git on the setting Microsoft's
//!    large-repository documentation tells people to turn on first.
//! 3. **`index.recordEndOfIndexEntries` and `index.recordOffsetTable`.** Zero
//!    cases for either. These two decide whether `EOIE` and `IEOT` are written
//!    at all, they can be set against `index.threads` rather than with it, and
//!    the port turns out to honour them everywhere except on an index with no
//!    entries — which is the case below that fails.
//! 4. **`--split-index` beside another index-writing option**, on `Shape::Linear`
//!    rather than on the split-index fixture. Stock drops the split — writing no
//!    `link` extension and no shared index file — when the same argv also
//!    carries `--refresh` or an `--index-version` naming a version the index
//!    does not already have. `fixture_gaps3.rs` cannot ask this question,
//!    because its shape is split before the case starts.
//! 5. **`--index-version 4` where the entries are not plain stage-0 entries** —
//!    three merge stages of one path (`Conflicted`) and entries carrying
//!    extended flags (`Sparse`). Those are two different v4 encodings from the
//!    `Linear` one `worktree_index.rs` already runs, and they are the two that
//!    break next if v4 writing is ever implemented.
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
//!   system <uname>`. Read out of an untracked cache stock 2.55.0 wrote in this
//!   crate's own scratch directory: `55 4e 54 52 00 00 01 29 80 27 'Location
//!   /private/tmp/…/mf_a, system Darwin'`. Two fixture copies live at two
//!   different paths, so the extension's *length* differs between them by
//!   construction — before any implementation difference — and two builds of the
//!   same shape at paths of different lengths gave `UNTR:303` and `UNTR:304`.
//!   That is why every `core.untrackedCache` case below pins the key **off**:
//!   `feature.manyFiles` turns it on, and a case that let it stay on would be
//!   scored `Nondeterministic` and measure nothing at all. `index_plumbing.rs`
//!   reached the same conclusion from the other end; this is the byte-level
//!   reason for it.
//! * **`core.fsmonitor=true` is not measurable either, for two independent
//!   reasons.** It spawns a *detached background daemon* that outlives the case
//!   — measured: after one `update-index --refresh` under it,
//!   `git fsmonitor--daemon run --detach --ipc-threads=8` was still running and
//!   holding the fixture. And the `FSMN` extension it writes carries a v2 token
//!   that is `builtin:0.<pid>.<UTC wall clock to microseconds>Z:0` — the bytes
//!   read back were `builtin:0.29292.20260830T051106.481907Z:0`. A pid and a
//!   clock cannot be compared between two runs of anything. No case here
//!   configures it; `index_plumbing.rs` and `worktree_index.rs` already pin the
//!   *unconfigured* default, which is the part that can be pinned.
//! * **`update-index --refresh` / `--really-refresh` cannot carry a format
//!   case.** There is a real divergence there — under `index.threads` or
//!   `index.recordEndOfIndexEntries` stock rewrites the index and gains the
//!   extension where the port does not rewrite it at all — but whether stock
//!   writes depends on whether the refresh found a stat to update, and repeated
//!   runs against a freshly built repository disagreed with each other (`386`
//!   bytes on one run, `446` with `IEOT`+`EOIE` on the next, for the same
//!   command). Stock is not reproducible there, so the finding is recorded and
//!   not shipped as a case.
//! * **A split index the *port* creates has a nondeterministic file name** —
//!   `sharedindex.2016f59a…` and `sharedindex.936afcff…` from two runs of one
//!   command — because the shared half carries stat data. `probe_index_meta`
//!   elides checksum-bearing names and reports only the parsed meta
//!   (`v2 entries=2 ext=[]`, identical across those two runs), so the one case
//!   below that provokes it is still comparable. Nothing here asks the *shared*
//!   index to be rewritten, which is where `fixture_gaps3.rs` lives.
//!
//! Every behaviour asserted in this file was measured against stock 2.55.0
//! (`/opt/homebrew/bin/git`) twice in identical scratch repositories before the
//! case was written; the citations are to `read-cache.c`
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
    many_files_profile(out);
    offset_table_and_eoie(out);
    version_against_split_index(out);
    version_of_awkward_entries(out);
}

/// `GIT_INDEX_VERSION`: the format lever no case in this crate had ever set.
///
/// # Why it is not a third spelling of `index.version`
///
/// `read-cache.c:get_index_format_default` reads the environment variable
/// first and the `index.version` key second, and `do_write_index` consults
/// neither unless the index being written has no version of its own — which is
/// to say, unless the writer built the index from scratch rather than reading
/// one off disk. Measured on stock 2.55.0, and the split is sharp enough to be
/// worth stating as the contract these cases pin:
///
/// | invocation | index before | `GIT_INDEX_VERSION=4` result |
/// |---|---|---|
/// | `read-tree HEAD` | v2, 245 B | **v4**, 240 B |
/// | `read-tree --empty` | v2, 245 B | **v4**, 65 B |
/// | `read-tree --reset HEAD` | v2, 386 B | v2, 325 B |
/// | `read-tree --prefix=graft/ HEAD` | v2, 245 B | v2, 444 B |
/// | `update-index --add untracked.txt` | v2, 306 B | v2, 386 B |
///
/// So the variable is not "write version N"; it is "when you have to *choose* a
/// version, choose N", and three of those five rows are the negative half of the
/// contract. A port that applied the variable to every write would pass a case
/// built only from the first two rows and fail the last three, which is why they
/// are here.
///
/// An invalid value is the third thing the variable does that no other spelling
/// does: `warning: GIT_INDEX_VERSION set, but the value is invalid.` followed by
/// `Using version 3`, on stderr, at exit 0 — and then version 3 is demoted to 2
/// anyway because no entry needs the extended flags. Those cases are
/// `Case::strict` because the warning is the entire observable difference: the
/// index that comes out is byte-identical either way.
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
    out.push(
        Case::new("read-tree", &["read-tree", "HEAD"], Shape::Linear)
            .with_env(&[("GIT_INDEX_VERSION", "2")]),
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

/// `feature.manyFiles`: the aggregate profile, and the three keys it stands for.
///
/// `feature.manyFiles=true` is not a setting of its own — it is a *default* for
/// `index.version=4`, `index.skipHash=true` and `core.untrackedCache=true`, each
/// of which an explicit setting still overrides. That structure is what these
/// cases are for: reading the three keys and not the profile, or reading the
/// profile and letting it beat an explicit key, are two different bugs and
/// neither is visible from a case that sets the keys directly.
///
/// Measured on stock 2.55.0, all with `core.untrackedCache=false` pinned for the
/// reason in the module header:
///
/// * `feature.manyFiles=true read-tree --empty` → **v4**, where the port writes
///   v2. Same for `read-tree HEAD`.
/// * `feature.manyFiles=true index.version=2 read-tree --empty` → v2 on both
///   sides: the explicit key beats the profile.
/// * `feature.manyFiles=true update-index --add --cacheinfo …` → v2 on both
///   sides: the profile supplies a *default* version, and an index read off disk
///   already has one, so there is nothing to default.
/// * `feature.manyFiles=false read-tree --empty` → v2 on both sides.
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
/// | configuration | chain written |
/// |---|---|
/// | `recordEndOfIndexEntries=true` | `TREE`, `EOIE` (330 B) |
/// | `threads=2` | `IEOT`, `TREE`, `EOIE` (366 B) |
/// | `threads=2 recordEndOfIndexEntries=false` | `IEOT`, `TREE` |
/// | `threads=2 recordOffsetTable=false` | `TREE`, `EOIE` |
///
/// The one place it comes apart is an index with **no entries**: stock writes
/// `EOIE` onto an empty index (`97` bytes against the port's `65`) and the port
/// writes none, under either spelling of the request. That is the last two cases
/// here, and it is a defect in the port's writer rather than in its reader —
/// stock reads the port's short index without complaint.
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
/// The port splits anyway. Measured on `Shape::Linear`, four runs of stock each:
///
/// | argv | stock | port |
/// |---|---|---|
/// | `--split-index` | 297 B, 1 shared | 297 B, 1 shared |
/// | `--index-version 2 --split-index` | 297 B, 1 shared | 297 B, 1 shared |
/// | `--index-version 3 --split-index` | **245 B, 0 shared** | 297 B, 1 shared |
/// | `--index-version 4 --split-index` | **240 B, v4, 0 shared** | 297 B, 1 shared |
/// | `--split-index --refresh` | **245 B, 0 shared** | 297 B, 1 shared |
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
/// # Determinism
///
/// This is the one group in the module that provokes the port into writing a
/// checksum-named file, and the shared index carries stat data — two runs of one
/// command produced `sharedindex.2016f59a…` and `sharedindex.936afcff…`.
/// `probe_index_meta` elides checksum-bearing names and reports the parsed meta,
/// which was `v2 entries=2 ext=[]` on both of those runs, so the digest is
/// stable. Stock's side writes no such file at all.
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
/// `worktree_index.rs` runs `--index-version 2|4` on `Linear` and `Branched`,
/// whose indexes are two and three plain entries. Those are the easy encodings.
/// The three below are the ones with a rule attached, and each has a distinct
/// failure available to it:
///
/// * **`Conflicted` at version 4.** Three entries for two paths, two of them
///   differing only in their stage number. Version 4 prefix-compresses each name
///   against the previous one, so `conflict.txt` at stage 2 and `conflict.txt`
///   at stage 3 encode as a full name and then a *zero-length suffix* — the
///   degenerate case of the compression, and the one an encoder written from the
///   documentation is most likely to get wrong. Stock writes v4 at 361 bytes;
///   the port writes v2 at 386.
/// * **`Sparse` at version 4.** Seven entries, three of them carrying the
///   skip-worktree bit, so the index is v3 before the command runs. Going to v4
///   keeps the extended flags and changes the name encoding underneath them.
///   Stock: v4, 760 bytes. Port: v3, 786.
/// * **`Sparse` and `IntentToAdd` at version 2.** The demotion that must *not*
///   happen: an entry with an extended flag cannot be expressed in version 2, so
///   git writes version 3 whatever was asked for. Both sides agree, and this is
///   the case a port that treats `--index-version` as an assignment rather than
///   as a floor would break — silently, by dropping every skip-worktree and
///   intent-to-add bit in the index.
fn version_of_awkward_entries(out: &mut Vec<Case>) {
    out.push(Case::new("update-index", &["update-index", "--index-version", "4"], Shape::Conflicted));
    out.push(Case::new("update-index", &["update-index", "--index-version", "4"], Shape::Sparse));
    out.push(Case::new("update-index", &["update-index", "--index-version", "2"], Shape::Sparse));
    out.push(Case::new("update-index", &["update-index", "--index-version", "2"], Shape::IntentToAdd));
    // Asked for exactly what the entries need. Nothing moves, which is the point:
    // an implementation that rewrites the entry table whenever the version option
    // is present has a different set of bytes here from one that notices the
    // version is already right.
    out.push(Case::new("update-index", &["update-index", "--index-version", "3"], Shape::IntentToAdd));
}
