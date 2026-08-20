//! Port of `repo-settings.c`'s `prepare_repo_settings()` — the per-repository
//! settings block git resolves once, before any command touches the object
//! database or the index.
//!
//! git builds `struct repo_settings` lazily but exactly once per process
//! (`repo-settings.c:30-45`, guarded by `r->settings.initialized`). Two kinds of
//! key live in it, and both matter here:
//!
//! * **Cascading `feature.*` macros.** `feature.manyFiles` and
//!   `feature.experimental` are not settings in their own right; they are
//!   *defaults for other settings*, applied first so a later explicit key still
//!   wins (`repo-settings.c:47-63`). `feature.manyFiles` sets `index_version = 4`,
//!   `index_skip_hash = 1`, `core_untracked_cache = UNTRACKED_CACHE_WRITE` and
//!   `pack_use_path_walk = 1`; `feature.experimental` sets the fetch negotiation
//!   algorithm to `skipping` plus three pack-objects knobs.
//! * **Values every repository read depends on.** `core.packedGitWindowSize` and
//!   `core.packedGitLimit` size the mmap windows `packfile.c` opens over a pack
//!   (`repo-settings.c:141-155`).
//!
//! # Why this is a gate, not just a reader
//!
//! git reads all of the above through `repo_config_get_bool` /
//! `repo_config_get_ulong`, which die on a value they cannot parse. Because
//! `prepare_repo_settings()` runs before the command does its work, a bad value
//! in any of these keys is fatal for essentially every repository command, with
//! nothing else printed first. Verified against git 2.55.0:
//!
//! ```text
//! $ git -c core.packedGitLimit=bogus status --porcelain
//! fatal: bad numeric config value 'bogus' for 'core.packedgitlimit' in file .git/config: invalid unit
//! $ git -c feature.manyFiles=bogus status --porcelain
//! fatal: bad boolean config value 'bogus' for 'feature.manyfiles'
//! ```
//!
//! Note the two different diagnostics, and note that the key is spelled
//! *lowercase* in both: `die_bad_number()` and `git_config_bool()` print the name
//! their caller handed them, and `repo-settings.c` writes every key as a
//! lowercase literal. (A caller that spells the key in camelCase gets camelCase
//! back — `checkout.thresholdForParallelism` is read that way in
//! `parallel-checkout.c:65` and reports that way.) The numeric diagnostic carries
//! the ` in file <path>` origin clause, the boolean one does not.
//!
//! # What this port honors, and what it only validates
//!
//! Honored, with an observable effect on the bytes zvcs writes:
//!
//! * `feature.manyFiles` → the default for `index.skipHash`, which
//!   [`crate::config::index_write_options`] turns into a zeroed index trailer.
//! * `pack.usePathWalk` → the default for `pack-objects --path-walk`, which
//!   decides whether the two `warning: cannot use <option> with --path-walk`
//!   diagnostics fire; see [`RepoSettings::pack_use_path_walk`].
//!
//! Read, validated, and diagnosed exactly as git does, but with no further
//! effect because the machinery they tune does not exist in this port:
//!
//! * `core.packedGitWindowSize` / `core.packedGitLimit` size `packfile.c`'s
//!   sliding mmap windows. gitoxide maps a pack in one piece
//!   (`gix-pack`), so there is no window to size and no mapped-bytes budget to
//!   cap. The value is still parsed, and the window size still rounded the way
//!   `repo-settings.c:147-152` rounds it, so a bad value fails identically and
//!   [`RepoSettings::packed_git_window_size`] reports what git would have used.
//! * `feature.experimental` → the default fetch negotiation algorithm
//!   ([`RepoSettings::negotiation_algorithm`]) and
//!   [`RepoSettings::pack_use_multi_pack_reuse`].
//! * `pack.useSparse`, `pack.readReverseIndex` and
//!   `pack.useBitmapBoundaryTraversal` — see their fields below for what each
//!   would have steered.
//! * `feature.manyFiles`' other two effects. `index.version = 4` is not written
//!   because `gix-index`'s writer emits v2/v3 only (its `detect_required_version`
//!   picks between the two and the entry writer has no prefix-compressed path
//!   form), and `core.untrackedCache = write` is not written because that index
//!   writer never emits the `UNTR` extension.

use crate::config::config_ulong;

/// `DEFAULT_PACKED_GIT_WINDOW_SIZE` (`git-compat-util.h:573-578`) for a 64-bit
/// build: 1 GiB. (A 32-bit build uses 32 MiB; zvcs targets 64-bit hosts.)
pub const DEFAULT_PACKED_GIT_WINDOW_SIZE: u64 = 1024 * 1024 * 1024;

/// `DEFAULT_PACKED_GIT_LIMIT` (`git-compat-util.h:587-588`) for a 64-bit build:
/// `(1024 * 1024) * (32 * 1024 * 1024)` — 32 TiB, i.e. deliberately out of reach
/// so that a 64-bit git never evicts a pack window for lack of address space.
pub const DEFAULT_PACKED_GIT_LIMIT: u64 = (1024 * 1024) * (32 * 1024 * 1024);

/// `enum fetch_negotiation_setting` (`repo-settings.h`), as far as this port
/// needs it: the value `feature.experimental` defaults and
/// `fetch.negotiationAlgorithm` overrides.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum NegotiationAlgorithm {
    /// `FETCH_NEGOTIATION_CONSECUTIVE` — git's default.
    Consecutive,
    /// `FETCH_NEGOTIATION_SKIPPING` — what `feature.experimental` selects.
    Skipping,
}

/// The subset of `struct repo_settings` this port resolves.
#[derive(Copy, Clone, Debug)]
pub struct RepoSettings {
    /// `feature.manyFiles`, as read (before it cascades).
    pub many_files: bool,
    /// `feature.experimental`, as read (before it cascades).
    pub experimental: bool,
    /// `r->settings.index_skip_hash` after the `feature.manyFiles` cascade but
    /// *before* an explicit `index.skipHash`, which `repo-settings.c:79` layers
    /// on top with this as its default.
    pub index_skip_hash: bool,
    /// `r->settings.fetch_negotiation_algorithm` after the
    /// `feature.experimental` cascade and before `fetch.negotiationAlgorithm`.
    pub negotiation_algorithm: NegotiationAlgorithm,
    /// `core.packedGitWindowSize`, rounded down to a multiple of twice the page
    /// size and floored at one such multiple, exactly as `repo-settings.c:147-152`
    /// rounds it. [`DEFAULT_PACKED_GIT_WINDOW_SIZE`] when the key is unset — git
    /// leaves the default *unrounded*, and so does this.
    pub packed_git_window_size: u64,
    /// `core.packedGitLimit`, or [`DEFAULT_PACKED_GIT_LIMIT`] when unset.
    pub packed_git_limit: u64,
    /// `r->settings.pack_use_sparse` (`repo-settings.c:77`), the default for
    /// `pack-objects --sparse` (`builtin/pack-objects.c:5162-5166`). On in git,
    /// and *not* reset by the `feature.*` macros — `repo_cfg_bool` is called with
    /// a literal default of 1.
    ///
    /// It selects the sparse reachability algorithm for
    /// `mark_edges_uninteresting()`, which is a traversal shortcut: it can leave
    /// extra objects in the pack but never removes a needed one. This port walks
    /// the full closure either way, so nothing downstream reads this.
    pub pack_use_sparse: bool,
    /// `r->settings.pack_use_path_walk` (`repo-settings.c:78`), the default for
    /// `pack-objects --path-walk`. Off in git.
    ///
    /// Note the default argument at `repo-settings.c:78` is the *literal* `0`,
    /// not `r->settings.pack_use_path_walk`, so the `feature.experimental` /
    /// `feature.manyFiles` cascade that set it at lines 57 and 63 is overwritten
    /// again whenever `pack.usePathWalk` is unset — i.e. in git 2.55.0 neither
    /// macro can actually turn the path walk on. Confirmed against git 2.55.0:
    /// `-c feature.experimental=true pack-objects --revs --stdout
    /// --delta-islands` is silent, while `-c pack.usePathWalk=true` on the same
    /// line warns. The two lines below reproduce that, deliberately.
    pub pack_use_path_walk: bool,
    /// `r->settings.pack_read_reverse_index` (`repo-settings.c:82`), on in git.
    ///
    /// It decides whether `packfile.c` loads an on-disk `.rev` or rebuilds the
    /// reverse index in memory — the same mapping either way, so it changes no
    /// output. gitoxide's pack lookup builds its own, so there is nothing to
    /// switch off.
    pub pack_read_reverse_index: bool,
    /// `r->settings.pack_use_bitmap_boundary_traversal` (`repo-settings.c:83-85`),
    /// defaulting to whatever `feature.experimental` left rather than to a
    /// literal — so this one *does* keep the macro's value when unset.
    ///
    /// It selects the boundary-based traversal in `pack-bitmap.c`, an
    /// acceleration that needs a `.bitmap` to read; there is no bitmap reader in
    /// the vendored `gix-pack`, so no traversal to redirect.
    pub pack_use_bitmap_boundary_traversal: bool,
    /// `r->settings.pack_use_multi_pack_reuse` (`repo-settings.c:56`). It has no
    /// config key of its own — only `feature.experimental` sets it — and it
    /// promotes `pack-objects`' pack-reuse mode to `MULTI_PACK_REUSE`
    /// (`builtin/pack-objects.c:5167-5168`), which `pack.allowPackReuse` then
    /// overrides. Verbatim pack reuse is not implemented here; see
    /// [`crate::porcelain::pack_objects`]'s `PackReuse`.
    pub pack_use_multi_pack_reuse: bool,
}

impl RepoSettings {
    /// `prepare_repo_settings(r)` for the keys above.
    ///
    /// `Err` carries the exact line git's `die()` prints, minus the `fatal: `
    /// prefix the caller adds — see the module docs for the two shapes.
    ///
    /// The read order is git's: the two `feature.*` macros first, then the
    /// settings they default, then the non-boolean block. That order is what
    /// makes an explicit `index.skipHash=false` beat `feature.manyFiles=true`
    /// instead of the other way round.
    pub fn load(repo: &gix::Repository) -> Result<Self, String> {
        // repo-settings.c:47-48 — read before anything they cascade into.
        let many_files = config_bool_strict(repo, "feature.manyfiles")?.unwrap_or(false);
        let experimental = config_bool_strict(repo, "feature.experimental")?.unwrap_or(false);

        // repo-settings.c:50-63: the cascade. `feature.experimental` sets the
        // negotiation algorithm plus three pack knobs; `feature.manyFiles` sets
        // the index knobs plus the path walk.
        let negotiation_algorithm = if experimental {
            NegotiationAlgorithm::Skipping
        } else {
            NegotiationAlgorithm::Consecutive
        };
        let mut index_skip_hash = many_files;
        let mut pack_use_bitmap_boundary_traversal = experimental;
        let pack_use_multi_pack_reuse = experimental;

        // repo-settings.c:77-78. Both defaults here are literals, which is why
        // `pack.usePathWalk` unset lands on 0 even under `feature.experimental`.
        let pack_use_sparse = config_bool_strict(repo, "pack.usesparse")?.unwrap_or(true);
        let pack_use_path_walk = config_bool_strict(repo, "pack.usepathwalk")?.unwrap_or(false);

        // repo-settings.c:79 — `repo_cfg_bool(r, "index.skiphash", …, r->settings.index_skip_hash)`,
        // i.e. the cascaded value is this key's *default*, not its competitor.
        if let Some(v) = config_bool_strict(repo, "index.skiphash")? {
            index_skip_hash = v;
        }

        // repo-settings.c:82-85. `pack.readReverseIndex` takes a literal default
        // of 1; `pack.useBitmapBoundaryTraversal` takes the cascaded value, so
        // `feature.experimental` survives here where it did not at line 78.
        let pack_read_reverse_index =
            config_bool_strict(repo, "pack.readreverseindex")?.unwrap_or(true);
        if let Some(v) = config_bool_strict(repo, "pack.usebitmapboundarytraversal")? {
            pack_use_bitmap_boundary_traversal = v;
        }

        // repo-settings.c:143-152. The rounding is git's, comment included: the
        // window must be a multiple of `pagesize * 2`, and a value smaller than
        // one such multiple is raised to it rather than rejected.
        let packed_git_window_size = match config_ulong(repo, "core.packedgitwindowsize")? {
            Some(v) => {
                let pgsz_x2 = page_size_x2();
                let units = (v / pgsz_x2).max(1);
                units.saturating_mul(pgsz_x2)
            }
            None => DEFAULT_PACKED_GIT_WINDOW_SIZE,
        };

        // repo-settings.c:154-155.
        let packed_git_limit =
            config_ulong(repo, "core.packedgitlimit")?.unwrap_or(DEFAULT_PACKED_GIT_LIMIT);

        Ok(RepoSettings {
            many_files,
            experimental,
            index_skip_hash,
            negotiation_algorithm,
            packed_git_window_size,
            packed_git_limit,
            pack_use_sparse,
            pack_use_path_walk,
            pack_read_reverse_index,
            pack_use_bitmap_boundary_traversal,
            pack_use_multi_pack_reuse,
        })
    }
}

/// `getpagesize() * 2`, the unit `core.packedGitWindowSize` is rounded to.
/// `sysconf(_SC_PAGESIZE)` is what `getpagesize()` reports on both platforms this
/// runs on; a sysconf that fails or answers nonsense falls back to 4 KiB so the
/// rounding still has a sane unit rather than dividing by zero.
fn page_size_x2() -> u64 {
    // SAFETY: `sysconf` takes an int and returns a long; no pointers involved.
    let raw = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page = u64::try_from(raw).unwrap_or(4096).max(1);
    page * 2
}

/// git's `git_config_bool()` (config.c:1292-1298) for a dotted `key`: the value
/// as [`crate::optint::maybe_bool`] reads it, or the `die()` line git prints for
/// one it cannot — `bad boolean config value '<raw>' for '<key>'`.
///
/// Unlike the numeric diagnostic there is **no** ` in file <path>` origin clause;
/// `git_config_bool` has no `kvi` to build one from. Verified against git 2.55.0
/// with the key set in `.git/config` as well as via `-c`.
///
/// A key written with no value at all (`[feature]\n\tmanyFiles\n`) is **true**:
/// `git_parse_maybe_bool_text` answers `1` for a `NULL` value before it looks at
/// any text (`parse.c:168-169`), which is a different answer from the empty
/// string's `false`. That is why the value comes from
/// [`crate::config::last_value_implicit`] rather than the flattening reader — the
/// plain accessor renders both spellings as `""`.
///
/// `key` doubles as the name in the message, so callers pass it exactly as the C
/// caller spells it — lowercase for everything `repo-settings.c` reads.
pub fn config_bool_strict(repo: &gix::Repository, key: &str) -> Result<Option<bool>, String> {
    match crate::config::last_value_implicit(repo, key) {
        None => Ok(None),
        Some(None) => Ok(Some(true)),
        Some(Some(raw)) => match crate::optint::maybe_bool(&raw) {
            Some(v) => Ok(Some(v)),
            None => Err(format!("bad boolean config value '{raw}' for '{key}'")),
        },
    }
}
