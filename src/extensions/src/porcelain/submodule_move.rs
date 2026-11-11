//! `submodule_move_head()` (submodule.c:2126-2240) and the places `unpack_trees()`
//! and `entry.c` reach it from when `--recurse-submodules` (or `submodule.recurse`)
//! is in effect: `check_submodule_move_head()` (unpack-trees.c:315-333), the
//! gitlink arms of `checkout_entry_ca()` / `write_entry()` (entry.c:397-404,
//! :517-535) and `unlink_entry()` (entry.c:594-607).
//!
//! The submodule is moved the way stock moves it, by running plumbing inside it:
//! `diff-index --quiet --cached HEAD` for the dirty-index check,
//! `read-tree --recurse-submodules --super-prefix=<p>/ (-n|-u) (--reset|-m) ...`
//! for the worktree and index, and `update-ref HEAD --no-deref <new>` for `HEAD`.
//! The `read-tree` child recurses into nested submodules through this same module.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Result;
use gix::bstr::{BStr, BString, ByteSlice};

/// `SUBMODULE_MOVE_HEAD_DRY_RUN` (submodule.h).
pub(crate) const MOVE_HEAD_DRY_RUN: u32 = 1 << 0;
/// `SUBMODULE_MOVE_HEAD_FORCE` (submodule.h).
pub(crate) const MOVE_HEAD_FORCE: u32 = 1 << 1;

/// `should_update_submodules()` (submodule.c:745-748): the command-line
/// `--[no-]recurse-submodules` when given, `submodule.recurse` otherwise.
pub(crate) fn should_update_submodules(repo: &gix::Repository, option: Option<bool>) -> bool {
    option.unwrap_or_else(|| repo.config_snapshot().boolean("submodule.recurse") == Some(true))
}

/// One `.gitmodules` entry as `submodule_from_path(the_repository, null_oid(), path)`
/// sees it, with `is_submodule_active()` (submodule.c:270-293) already answered.
#[derive(Clone)]
struct Sub {
    name: BString,
    active: bool,
}

/// The superproject side of a recursing unpack: the `.gitmodules` the command has
/// loaded, the `--super-prefix` it runs under, and where its work tree is.
///
/// git loads `.gitmodules` once and reloads it only in `check_updates()`, after the
/// new `.gitmodules` was checked out (`load_gitmodules_file()`, unpack-trees.c:344-359),
/// so this holds a snapshot that [`Recursion::reload`] refreshes at that point. A
/// snapshot also keeps the removal of a submodule — which takes `.gitmodules` with
/// it — answering from the file the removal started from.
pub(crate) struct Recursion {
    git_dir: PathBuf,
    workdir: PathBuf,
    /// `empty_tree_oid_hex(the_repository->hash_algo)`.
    empty_tree: String,
    super_prefix: Option<String>,
    subs: HashMap<BString, Sub>,
}

/// A gitlink `check_updates()` is to check out: its path, the commit it records and
/// what stood at the path before the unpack touched the work tree.
pub(crate) struct Gitlink {
    pub(crate) path: BString,
    pub(crate) id: gix::ObjectId,
    pub(crate) before: PreState,
}

/// Whether a submodule's directory held a populated submodule before the unpack
/// wrote anything — what `checkout_entry_ca()`'s `check_path()` and
/// `is_submodule_populated_gently()` see when they reach the gitlink.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PreState {
    /// `check_path()` fails: `write_entry()` makes the directory.
    Missing,
    /// Something is there but `<path>/.git` does not resolve to a repository.
    Unpopulated { is_dir: bool },
    /// `is_submodule_populated_gently()` says yes.
    Populated,
}

impl Recursion {
    /// `None` unless `should_update_submodules()`, which is exactly when
    /// `submodule_from_ce()` (submodule.c:750-758) can answer anything at all.
    pub(crate) fn new(repo: &gix::Repository, enabled: bool, super_prefix: Option<&str>) -> Result<Option<Self>> {
        if !enabled {
            return Ok(None);
        }
        let Some(workdir) = repo.workdir() else {
            return Ok(None);
        };
        let mut this = Recursion {
            git_dir: repo.git_dir().to_owned(),
            workdir: workdir.to_owned(),
            empty_tree: repo.object_hash().empty_tree().to_string(),
            super_prefix: super_prefix.map(str::to_owned),
            subs: HashMap::new(),
        };
        this.load(repo, false)?;
        Ok(Some(this))
    }

    /// `after_checkout` is the reload `check_updates()` makes: the index on disk is
    /// still the one the unpack started from, so whether it holds a gitlink says
    /// nothing about the `.gitmodules` just written, which is read regardless.
    fn load(&mut self, repo: &gix::Repository, after_checkout: bool) -> Result<()> {
        let index = repo.index_or_empty()?;
        let subs: Vec<gix::Submodule<'_>> = match after_checkout {
            false => super::submodule::submodules(repo)?,
            true => {
                if let Some(msg) = crate::config::gitmodules_bad_line(&self.workdir) {
                    return Err(crate::fatal::die(msg));
                }
                repo.submodules()?.map(Iterator::collect).unwrap_or_default()
            }
        };
        self.subs = subs
            .iter()
            .filter_map(|sm| {
                let path = sm.path().ok()?;
                let active = super::submodule::is_submodule_active(repo, &index, sm, &path).unwrap_or(false);
                Some((path, Sub { name: sm.name().to_owned(), active }))
            })
            .collect();
        Ok(())
    }

    /// `repo_read_gitmodules()` again, once `check_updates()` has written the new
    /// `.gitmodules` (unpack-trees.c:473-474, :350-356).
    pub(crate) fn reload(&mut self) -> Result<()> {
        let repo = gix::open(&self.git_dir)?;
        self.load(&repo, true)
    }

    /// `submodule_from_ce()`: a gitlink that `.gitmodules` names. The caller has
    /// already established that the entry is a gitlink.
    pub(crate) fn is_submodule(&self, path: &BStr) -> bool {
        self.subs.contains_key(path)
    }

    fn full(&self, path: &BStr) -> PathBuf {
        self.workdir.join(gix::path::from_bstr(path).as_ref())
    }

    /// `path` in the superproject's work tree.
    pub(crate) fn workdir_path(&self, path: &BStr) -> Option<PathBuf> {
        Some(self.full(path))
    }

    /// What `checkout_entry_ca()` will find at `path`, recorded before the unpack
    /// writes or removes anything.
    pub(crate) fn pre_state(&self, path: &BStr) -> PreState {
        let full = self.full(path);
        match std::fs::symlink_metadata(&full) {
            Err(_) => PreState::Missing,
            Ok(meta) => match populated_gently(&full) {
                Ok(true) => PreState::Populated,
                _ => PreState::Unpopulated { is_dir: meta.is_dir() },
            },
        }
    }

    /// `file_exists(ce->name)` (wrapper.c), the gate `merged_entry()` puts before its
    /// `check_submodule_move_head()` (unpack-trees.c:2592, :2621).
    pub(crate) fn exists(&self, path: &BStr) -> bool {
        std::fs::metadata(self.full(path)).is_ok()
    }

    /// `check_submodule_move_head()` (unpack-trees.c:315-333): a dry run of the
    /// move, forced under `o->reset`. `true` means the caller must reject `path` as
    /// `ERROR_WOULD_LOSE_SUBMODULE`.
    pub(crate) fn check_move_head(
        &self,
        path: &BStr,
        old: Option<&str>,
        new: Option<&str>,
        reset: bool,
    ) -> Result<bool> {
        if !self.is_submodule(path) {
            return Ok(false);
        }
        let mut flags = MOVE_HEAD_DRY_RUN;
        if reset {
            flags |= MOVE_HEAD_FORCE;
        }
        Ok(self.move_head(path, old, new, flags)? != 0)
    }

    /// The gitlink arms of `checkout_entry_ca()` (entry.c:517-535) and of
    /// `write_entry()` (entry.c:397-404), for an entry `check_updates()` writes with
    /// `state.force = 1` (unpack-trees.c:435). `before` is what was at the path when
    /// the unpack began. Returns `checkout_entry()`'s error status.
    pub(crate) fn checkout_entry(&self, path: &BStr, new: &str, before: PreState) -> Result<i32> {
        if !self.is_submodule(path) {
            return Ok(0);
        }
        let full = self.full(path);
        match before {
            PreState::Populated => self.move_head(path, Some("HEAD"), Some(new), MOVE_HEAD_FORCE),
            PreState::Unpopulated { is_dir } => {
                // `if (!(st.st_mode & S_IFDIR)) unlink_or_warn(ce->name);`
                if !is_dir {
                    if let Err(err) = std::fs::remove_file(&full) {
                        if err.kind() != std::io::ErrorKind::NotFound {
                            eprintln!("warning: unable to unlink '{path}': {}", crate::external::strerror(&err));
                        }
                    }
                }
                self.move_head(path, None, Some(new), 0)
            }
            PreState::Missing => {
                // `write_entry()`'s `mkdir(path, 0777)`; the worktree writer may
                // already have made it for the gitlink.
                if !full.is_dir() && std::fs::create_dir(&full).is_err() {
                    eprintln!("error: cannot create submodule directory {path}");
                    return Ok(-1);
                }
                self.move_head(path, None, Some(new), MOVE_HEAD_FORCE)
            }
        }
    }

    /// `unlink_entry()`'s submodule half (entry.c:597-601):
    /// `submodule_move_head(ce->name, super_prefix, "HEAD", NULL, SUBMODULE_MOVE_HEAD_FORCE)`.
    pub(crate) fn unlink_entry(&self, path: &BStr) -> Result<()> {
        if self.is_submodule(path) {
            self.move_head(path, Some("HEAD"), None, MOVE_HEAD_FORCE)?;
        }
        Ok(())
    }

    /// The gitlinks among `paths` of `index` that `check_updates()` will hand to
    /// `checkout_entry()`, each with what is at its path now — read before anything
    /// is removed or written.
    pub(crate) fn gitlinks<'p>(
        &self,
        index: &gix::index::State,
        paths: impl IntoIterator<Item = &'p BStr>,
    ) -> Vec<Gitlink> {
        paths
            .into_iter()
            .filter_map(|path| {
                let e = index.entry_by_path(path)?;
                (e.mode == gix::index::entry::Mode::COMMIT).then(|| Gitlink {
                    path: path.to_owned(),
                    id: e.id,
                    before: self.pre_state(path),
                })
            })
            .collect()
    }

    /// `check_updates()`'s gitlinks, after the files are out: `.gitmodules` is read
    /// again when the unpack wrote it (unpack-trees.c:473-474), then each gitlink is
    /// checked out — a submodule moved, any other gitlink a directory made
    /// (`write_entry()`, entry.c:397-404). Returns whether any failed.
    pub(crate) fn checkout_gitlinks(&mut self, gitlinks: &[Gitlink], wrote_gitmodules: bool) -> Result<bool> {
        if gitlinks.is_empty() {
            return Ok(false);
        }
        if wrote_gitmodules {
            self.reload()?;
        }
        let mut errs = false;
        for g in gitlinks {
            let path = g.path.as_bstr();
            if self.is_submodule(path) {
                errs |= self.checkout_entry(path, &g.id.to_string(), g.before)? != 0;
            } else if g.before == PreState::Missing && std::fs::create_dir(self.full(path)).is_err() {
                eprintln!("error: cannot create submodule directory {path}");
                errs = true;
            }
        }
        Ok(errs)
    }

    /// `merged_entry()`'s submodule check for a one-way reset — `reset --hard`,
    /// `checkout -f`, `switch --discard-changes` — where `o->reset` makes it forced
    /// (unpack-trees.c:327-328) and `verify_uptodate()` passes every entry it does
    /// not have to look at (:2237-2238). Under `UNPACK_RESET_PROTECT_UNTRACKED`
    /// (`protect_untracked`) a gitlink the index lacks first goes through
    /// `verify_absent()`'s submodule arm (:2510-2531). The first path refused, in
    /// index order, as `add_rejected_path()` without `show_all_errors` stops there.
    pub(crate) fn oneway_reset_refusal(
        &self,
        old: &gix::index::State,
        new: &gix::index::State,
        protect_untracked: bool,
    ) -> Result<Option<BString>> {
        let backing = new.path_backing();
        for e in new.entries().iter().filter(|e| e.mode == gix::index::entry::Mode::COMMIT) {
            let path = e.path_in(backing);
            if !self.is_submodule(path) {
                continue;
            }
            let was = old
                .entry_by_path(path)
                .filter(|o| o.stage_raw() == 0)
                .map(|o| (o.id, o.mode));
            if was == Some((e.id, e.mode)) {
                continue;
            }
            let new_hex = e.id.to_string();
            if was.is_none()
                && protect_untracked
                && std::fs::symlink_metadata(self.full(path)).is_ok()
                && self.check_move_head(path, Some(&new_hex), None, true)?
            {
                return Ok(Some(path.to_owned()));
            }
            let old_hex = was.map(|(id, _)| id.to_string());
            if self.exists(path) && self.check_move_head(path, old_hex.as_deref(), Some(&new_hex), true)? {
                return Ok(Some(path.to_owned()));
            }
        }
        Ok(None)
    }

    /// `submodule_move_head()` (submodule.c:2126-2240). Returns 0 or -1, having
    /// reported the failure.
    pub(crate) fn move_head(&self, path: &BStr, old_head: Option<&str>, new_head: Option<&str>, flags: u32) -> Result<i32> {
        // `if (!is_submodule_active(the_repository, path)) return 0;`
        let Some(sub) = self.subs.get(path).filter(|s| s.active) else {
            return Ok(0);
        };
        let full = self.full(path);
        // `if (old_head && !is_submodule_populated_gently(path, error_code_ptr)) return 0;`
        // The forced move passes an error-code pointer so that a `.git` leading nowhere
        // is "not populated" rather than fatal.
        if old_head.is_some() {
            match populated_gently(&full) {
                Ok(true) => {}
                Ok(false) => return Ok(0),
                Err(err) if flags & MOVE_HEAD_FORCE != 0 => {
                    let _ = err;
                    return Ok(0);
                }
                Err(err) => {
                    let shown = format!("{path}/.git");
                    crate::git_fatal!(
                        "{}",
                        crate::porcelain::rev_parse::gitfile_error_message(Path::new(&shown), err)
                    );
                }
            }
        }

        if old_head.is_some() && flags & MOVE_HEAD_FORCE == 0 && self.has_dirty_index(path)? {
            eprintln!("error: submodule '{path}' has dirty index");
            return Ok(-1);
        }

        if flags & MOVE_HEAD_DRY_RUN == 0 {
            let repo = gix::open(&self.git_dir)?;
            if old_head.is_some() {
                if !super::rm::submodule_uses_gitfile(&full, &path.to_string())? {
                    super::submodule::absorb_one(&repo, &path.to_owned(), self.super_prefix.as_deref())?;
                } else {
                    let dotgit = full.join(".git");
                    if let Ok(Some(git_dir)) = crate::porcelain::rev_parse::read_gitfile_gently(&dotgit) {
                        let git_dir = std::fs::canonicalize(&git_dir).unwrap_or(git_dir);
                        if validate_submodule_git_dir(&git_dir, sub.name.as_ref()) < 0 {
                            crate::git_fatal!(
                                "refusing to create/use '{}' in another submodule's git dir. \
                                 Enabling extensions.submodulePathConfig should fix this.",
                                git_dir.display()
                            );
                        }
                    }
                }
            } else {
                let gitdir = super::submodule::submodule_name_to_gitdir(&repo, sub.name.as_ref())?;
                connect_work_tree_and_git_dir(&full, &gitdir, false)?;
                // "make sure the index is clean as well"
                self.reset_index(path)?;
            }
            if old_head.is_some() && flags & MOVE_HEAD_FORCE != 0 {
                let gitdir = super::submodule::submodule_name_to_gitdir(&repo, sub.name.as_ref())?;
                connect_work_tree_and_git_dir(&full, &gitdir, true)?;
            }
        }

        let empty = &self.empty_tree;
        let mut cp = self.child(path)?;
        cp.args(["read-tree", "--recurse-submodules"]);
        cp.arg(format!("--super-prefix={}{path}/", self.super_prefix.as_deref().unwrap_or("")));
        cp.arg(if flags & MOVE_HEAD_DRY_RUN != 0 { "-n" } else { "-u" });
        if flags & MOVE_HEAD_FORCE != 0 {
            cp.arg("--reset");
        } else {
            cp.arg("-m");
            cp.arg(old_head.unwrap_or(empty));
        }
        cp.arg(new_head.unwrap_or(empty));
        if !run(cp)? {
            eprintln!("error: Submodule '{path}' could not be updated.");
            return Ok(-1);
        }

        if flags & MOVE_HEAD_DRY_RUN == 0 {
            match new_head {
                Some(new) => {
                    // "also set the HEAD accordingly"
                    let mut cp = self.child(path)?;
                    cp.args(["update-ref", "HEAD", "--no-deref", new]);
                    if !run(cp)? {
                        return Ok(-1);
                    }
                }
                None => {
                    let dotgit = full.join(".git");
                    if let Err(err) = std::fs::remove_file(&dotgit) {
                        if err.kind() != std::io::ErrorKind::NotFound {
                            eprintln!(
                                "warning: unable to unlink '{}': {}",
                                Path::new(&path.to_string()).join(".git").display(),
                                crate::external::strerror(&err)
                            );
                        }
                    }
                    // `if (is_empty_dir(path)) rmdir_or_warn(path);`
                    if std::fs::read_dir(&full).is_ok_and(|mut d| d.next().is_none()) {
                        if let Err(err) = std::fs::remove_dir(&full) {
                            eprintln!("warning: unable to rmdir '{path}': {}", crate::external::strerror(&err));
                        }
                    }
                    let repo = gix::open(&self.git_dir)?;
                    super::submodule::unset_core_worktree(&repo, sub.name.as_ref(), &path.to_string());
                }
            }
        }
        Ok(0)
    }

    /// `submodule_has_dirty_index()` (submodule.c:2076-2093).
    fn has_dirty_index(&self, path: &BStr) -> Result<bool> {
        let mut cp = self.child(path)?;
        cp.args(["diff-index", "--quiet", "--cached", "HEAD"]).stdout(Stdio::null());
        crate::cstdio::before_spawn();
        match cp.status() {
            Ok(status) => Ok(!status.success()),
            Err(_) => crate::git_fatal!("could not recurse into submodule '{path}'"),
        }
    }

    /// `submodule_reset_index()` (submodule.c:2095-2117).
    fn reset_index(&self, path: &BStr) -> Result<()> {
        let mut cp = self.child(path)?;
        cp.args(["read-tree", "-u", "--reset"]);
        cp.arg(format!("--super-prefix={}{path}/", self.super_prefix.as_deref().unwrap_or("")));
        cp.arg(&self.empty_tree);
        if !run(cp)? {
            crate::git_fatal!("could not reset submodule index");
        }
        Ok(())
    }

    /// A `cp.git_cmd = 1; cp.no_stdin = 1; cp.dir = path;` child under
    /// `prepare_submodule_repo_env()` (submodule.c:495-498): every `local_repo_env`
    /// variable but the two carrying `-c` configuration is cleared, and
    /// `GIT_DIR=.git` set (run-command.c `prepare_other_repo_env()`).
    fn child(&self, path: &BStr) -> Result<Command> {
        let mut cp = Command::new(crate::hosted::git_exe()?);
        cp.current_dir(self.full(path)).stdin(Stdio::null());
        for name in crate::alternate_refs::LOCAL_REPO_ENV {
            if *name != "GIT_CONFIG_PARAMETERS" && *name != "GIT_CONFIG_COUNT" {
                cp.env_remove(name);
            }
        }
        cp.env("GIT_DIR", ".git");
        Ok(cp)
    }
}

/// `run_command()`: whether the child ran and exited 0.
fn run(mut cp: Command) -> Result<bool> {
    // `start_command()`'s `fflush(NULL)` (run-command.c:743).
    crate::cstdio::before_spawn();
    Ok(cp.status().is_ok_and(|s| s.success()))
}

/// `is_submodule_populated_gently()` (submodule.c:295-305) over
/// `resolve_gitdir_gently()`: a git directory at `<path>/.git`, or a gitfile that
/// leads to one. `Err` carries the `read_gitfile_gently()` failure a caller without
/// an error-code pointer dies on.
fn populated_gently(full: &Path) -> std::result::Result<bool, crate::porcelain::rev_parse::GitfileError> {
    let dotgit = full.join(".git");
    if crate::porcelain::rev_parse::is_git_directory(&dotgit) {
        return Ok(true);
    }
    Ok(crate::porcelain::rev_parse::read_gitfile_gently(&dotgit)?.is_some())
}

/// `validate_submodule_legacy_git_dir()` (submodule.c:2402-2445): no leading part of
/// the submodule's git dir below `modules/` may itself be a git directory, which is
/// what keeps a submodule named `hippo/hooks` out of `hippo`'s.
fn validate_submodule_git_dir(git_dir: &Path, name: &BStr) -> i32 {
    let dir = git_dir.to_string_lossy().into_owned();
    let name = name.to_str_lossy();
    let Some(start) = dir.strip_suffix(name.as_ref()).filter(|s| s.ends_with('/')).map(str::len) else {
        eprintln!("BUG: submodule.c:2414: submodule name '{name}' not a suffix of git dir '{dir}'");
        std::process::abort();
    };
    for (i, c) in dir.char_indices().skip_while(|(i, _)| *i < start) {
        if c == '/' && crate::porcelain::rev_parse::is_git_directory(Path::new(&dir[..i])) {
            eprintln!("error: submodule git dir '{dir}' is inside git dir '{}'", &dir[..i]);
            return -1;
        }
    }
    0
}

/// `connect_work_tree_and_git_dir()` (dir.c:4118-4156): `<work_tree>/.git` names
/// `git_dir` and `git_dir`'s `core.worktree` names the work tree, each relative to
/// the other; `recurse_into_nested` repeats it for every active submodule the
/// submodule's own index and `.gitmodules` know (`connect_wt_gitdir_in_nested()`,
/// dir.c:4068-4116).
pub(crate) fn connect_work_tree_and_git_dir(work_tree: &Path, git_dir: &Path, recurse_into_nested: bool) -> Result<()> {
    // `safe_create_leading_directories_const()` for both files.
    if std::fs::create_dir_all(work_tree).is_err() {
        crate::git_fatal!("could not create directories for {}", work_tree.join(".git").display());
    }
    if std::fs::create_dir_all(git_dir).is_err() {
        crate::git_fatal!("could not create directories for {}", git_dir.join("config").display());
    }
    let git_dir = std::fs::canonicalize(git_dir)?;
    let work_tree = std::fs::canonicalize(work_tree)?;
    // `write_file()` terminates the line.
    std::fs::write(
        work_tree.join(".git"),
        format!("gitdir: {}\n", super::worktree::relative_path(&git_dir, &work_tree).display()),
    )?;
    let value = super::worktree::relative_path(&work_tree, &git_dir);
    if crate::config_store::set_multivar_in_file(
        &git_dir.join("config"),
        "core.worktree",
        "core.worktree",
        "core".len(),
        Some(value.to_string_lossy().as_bytes()),
        crate::config_store::ValuePattern::Any,
        None,
        false,
    )
    .is_err()
    {
        crate::git_fatal!("could not set 'core.worktree' to '{}'", value.display());
    }
    if recurse_into_nested {
        connect_wt_gitdir_in_nested(&work_tree, &git_dir)?;
    }
    Ok(())
}

/// `connect_wt_gitdir_in_nested()` (dir.c:4068-4116).
fn connect_wt_gitdir_in_nested(sub_worktree: &Path, sub_gitdir: &Path) -> Result<()> {
    // "If the submodule has no working tree, we can ignore it."
    // The `.git` file written just before names `sub_gitdir`.
    let Ok(subrepo) = gix::open(sub_worktree) else {
        return Ok(());
    };
    let Ok(index) = subrepo.index_or_empty() else {
        crate::git_fatal!("index file corrupt in repo {}", sub_gitdir.display());
    };
    let mut seen: Vec<BString> = Vec::new();
    {
        let backing = index.path_backing();
        for e in index.entries().iter().filter(|e| e.mode.is_submodule()) {
            let path = e.path_in(backing).to_owned();
            // "Skip entries with the same name in different stages"
            if seen.last() != Some(&path) {
                seen.push(path);
            }
        }
    }
    if seen.is_empty() {
        return Ok(());
    }
    let Ok(subs) = super::submodule::submodules(&subrepo) else {
        return Ok(());
    };
    for path in seen {
        let Some(sm) = subs.iter().find(|s| s.path().is_ok_and(|p| *p == path)) else {
            continue;
        };
        if !super::submodule::is_submodule_active(&subrepo, &index, sm, &path).unwrap_or(false) {
            continue;
        }
        let sub_wt = sub_worktree.join(gix::path::from_bstr(path.as_bstr()).as_ref());
        let sub_gd = super::submodule::submodule_name_to_gitdir(&subrepo, sm.name())?;
        connect_work_tree_and_git_dir(&sub_wt, &sub_gd, true)?;
    }
    Ok(())
}
