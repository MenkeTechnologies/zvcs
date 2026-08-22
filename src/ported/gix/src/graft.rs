//! The repository's commit **graft table** — git's `prepare_commit_graft()`
//! (`commit.c:316-330`).
//!
//! ```c
//! void prepare_commit_graft(struct repository *r)
//! {
//!         const char *graft_file;
//!
//!         if (r->parsed_objects->commit_graft_prepared)
//!                 return;
//!         if (!startup_info->have_repository)
//!                 return;
//!
//!         graft_file = repo_get_graft_file(r);
//!         read_graft_file(r, graft_file);
//!         /* make sure shallows are read */
//!         is_repository_shallow(r);
//!         r->parsed_objects->commit_graft_prepared = 1;
//! }
//! ```
//!
//! Two files, one table: `info/grafts` first (with `ignore_dups = 1`, so the
//! first line naming a commit wins), then `<GIT_DIR>/shallow` through
//! `register_shallow()` with `ignore_dups = 0` (shallow.c:32-45) — which is why a
//! commit named by both ends up **shallow**, not grafted.
//!
//! The read happens once and is never re-checked: `commit_graft_prepared` latches,
//! so a graft file that appears mid-command has no effect. [`Repository`]'s cache
//! is that latch, per repository instance.
//!
//! The [table itself](gix_revwalk::graft::Table) documents where the substitution
//! is applied and why grafts are ungated by the replace-object switches.

use std::path::PathBuf;

use crate::Repository;

/// The lazily read graft table, latched for the lifetime of a [`Repository`]
/// exactly as `parsed_objects->commit_graft_prepared` latches in git.
pub(crate) type TableStorage = std::cell::OnceCell<std::sync::Arc<gix_revwalk::graft::Table>>;

/// The table of grafts in effect, an alias for convenience.
pub type Table = gix_revwalk::graft::Table;

impl Repository {
    /// `repo_get_graft_file()` (repository.c:139-144): `$GIT_GRAFT_FILE` if set,
    /// otherwise `info/grafts` under the **common** directory.
    ///
    /// The common directory is what `expand_base_dir(&repo->graft_file,
    /// o->graft_file, repo->commondir, "info/grafts")` (repository.c:186) uses, so
    /// every linked worktree of a repository shares one graft file.
    ///
    /// Note that the path is returned whether or not the file exists.
    pub fn graft_file(&self) -> PathBuf {
        match std::env::var_os("GIT_GRAFT_FILE") {
            Some(path) => PathBuf::from(path),
            None => self.common_dir().join("info").join("grafts"),
        }
    }

    /// The graft table in effect for this repository, read on first use and cached
    /// afterwards — git's [`prepare_commit_graft()`](self).
    ///
    /// The table is empty (and shared, so cheap to clone) when the repository has
    /// neither an `info/grafts` file nor a `shallow` one, which is the common case
    /// and the one every caller should stay fast for.
    ///
    /// Reading `info/grafts` is also what prints git's deprecation advice and its
    /// `bad graft data` / `duplicate graft data` errors; see
    /// [`report_graft_file_read`].
    pub fn commit_grafts(&self) -> &std::sync::Arc<Table> {
        self.grafts.get_or_init(|| std::sync::Arc::new(self.read_commit_grafts()))
    }

    /// `true` when no graft is in effect, i.e. git's `grafts_nr == 0` **and** the
    /// repository is not shallow — the condition `commit_graph_compatible()`
    /// (commit-graph.c:234-239) tests before it will open a commit-graph.
    pub(crate) fn commit_grafts_are_empty(&self) -> bool {
        self.commit_grafts().is_empty()
    }

    /// The body of [`Repository::commit_grafts`]: `read_graft_file()` followed by
    /// `is_repository_shallow()`, in that order, because the shallow entries must
    /// be able to overwrite a graft-file line for the same commit.
    fn read_commit_grafts(&self) -> Table {
        let mut table = Table::default();
        let graft_file = self.graft_file();
        match gix_revwalk::graft::read(&graft_file, self.object_hash(), &mut table) {
            // `fopen_or_warn()` is silent for a missing file, and
            // `prepare_commit_graft()` carries on with what it has.
            Ok(None) | Err(_) => {}
            Ok(Some(complaints)) => report_graft_file_read(self, &graft_file, &complaints),
        }
        // `register_shallow()` (shallow.c:32-45) enters every listed commit with
        // `nr_parent = -1` and `ignore_dups = 0`, so it replaces a graft-file entry
        // for the same commit rather than being dropped as a duplicate.
        if let Ok(Some(commits)) = self.shallow_commits() {
            for id in commits.iter() {
                table.register(*id, gix_revwalk::graft::Graft::Shallow, false);
            }
        }
        table
    }
}

/// git suppresses the deprecation advice for the one command whose whole job is
/// to read the graft file — `convert_graft_file()` sets
/// `no_graft_file_deprecated_advice = 1` (builtin/replace.c:522) before its own
/// read, and `read_graft_file()` checks that flag first (commit.c:293).
static NO_GRAFT_FILE_DEPRECATED_ADVICE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set git's `no_graft_file_deprecated_advice`, silencing the deprecation advice
/// for the rest of the process. `git replace --convert-graft-file` is the only
/// caller in git, and must call this *before* it reads the file.
pub fn suppress_deprecation_advice() {
    NO_GRAFT_FILE_DEPRECATED_ADVICE.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The graft files already reported on, so a repository opened twice in one
/// process does not report twice.
///
/// git's guard is `parsed_objects->commit_graft_prepared`, one per `struct
/// repository`; keying on the resolved graft-file path is the same granularity for
/// a process that opens the same repository more than once, and still reports
/// separately for a submodule with a graft file of its own.
fn already_reported(graft_file: &std::path::Path) -> bool {
    use std::sync::Mutex;
    static SEEN: Mutex<Option<std::collections::HashSet<PathBuf>>> = Mutex::new(None);
    let mut seen = SEEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    !seen.get_or_insert_with(Default::default).insert(graft_file.to_owned())
}

/// Everything `read_graft_file()` (commit.c:287-314) prints once it has opened the
/// graft file: the deprecation advice first, then one `error()` per rejected or
/// duplicated line, in file order.
///
/// ```c
/// if (!no_graft_file_deprecated_advice &&
///     advice_enabled(ADVICE_GRAFT_FILE_DEPRECATED))
///         advise(_("Support for <GIT_DIR>/info/grafts is deprecated\n" ...));
/// while (!strbuf_getwholeline(&buf, fp, '\n')) {
///         struct commit_graft *graft = read_graft_line(&buf);
///         if (!graft)
///                 continue;
///         if (register_commit_graft(r, graft, 1))
///                 error("duplicate graft data: %s", buf.buf);
/// }
/// ```
///
/// None of it changes the exit code: `read_graft_file()` returns 0 and the grafts
/// it did parse stay in effect.
fn report_graft_file_read(repo: &Repository, graft_file: &std::path::Path, complaints: &[gix_revwalk::graft::Complaint]) {
    if already_reported(graft_file) {
        return;
    }
    if !NO_GRAFT_FILE_DEPRECATED_ADVICE.load(std::sync::atomic::Ordering::Relaxed)
        && graft_file_deprecated_advice_enabled(repo)
    {
        advise(
            "Support for <GIT_DIR>/info/grafts is deprecated\n\
             and will be removed in a future Git version.\n\
             \n\
             Please use \"git replace --convert-graft-file\"\n\
             to convert the grafts into replace refs.\n\
             \n\
             Turn this message off by running\n\
             \"git config set advice.graftFileDeprecated false\"",
        );
    }
    for complaint in complaints {
        match complaint {
            gix_revwalk::graft::Complaint::BadData(line) => eprintln!("error: bad graft data: {line}"),
            gix_revwalk::graft::Complaint::Duplicate(line) => eprintln!("error: duplicate graft data: {line}"),
        }
    }
}

/// `advice_enabled(ADVICE_GRAFT_FILE_DEPRECATED)` for the `graftFileDeprecated`
/// slot (advice.c:60): the hint shows unless `GIT_ADVICE` is false or
/// `advice.graftFileDeprecated` is set to false.
fn graft_file_deprecated_advice_enabled(repo: &Repository) -> bool {
    if let Some(value) = std::env::var_os("GIT_ADVICE") {
        if matches!(value.to_str(), Some("0" | "false" | "no" | "off" | "")) {
            return false;
        }
    }
    repo.config_snapshot().boolean("advice.graftFileDeprecated") != Some(false)
}

/// `vadvise()`'s line framing: every line of `body` on stderr behind `hint: `,
/// and a bare `hint:` — no trailing space — for an empty line.
///
/// ### Deviation
///
/// git additionally paints each whole line with `color.advice.hint` (default
/// `GIT_COLOR_YELLOW`) when `color.advice` allows it. That slot's renderer lives
/// in the porcelain layer, above this crate, so the hint is printed uncolored
/// here. `color.advice` has no `color.ui` fallback and defaults to `auto`, so on
/// a piped stderr — every test, every parity run, every `2>file` — git prints it
/// uncolored too and the bytes match; the difference is visible only on an
/// interactive terminal.
fn advise(body: &str) {
    for line in body.split('\n') {
        if line.is_empty() {
            eprintln!("hint:");
        } else {
            eprintln!("hint: {line}");
        }
    }
}
