//! The questions commands ask `setup.c` after setup has run.
//!
//! Most of `setup.c` executes before a builtin is entered and has no caller here.
//! What is left are the few predicates a command consults about the repository it
//! was handed — whether the current directory sits in the work tree, whether it
//! sits in the git directory — and the argument checks built on them. They belong
//! in one place because they are one rule: a command that re-derives "is there a
//! work tree here" from `workdir()` alone gets a different answer than git does
//! whenever the cwd is somewhere else, and the answers have to agree across verbs.

use std::path::{Path, PathBuf};

/// `strbuf_realpath()`: `path` symlink-resolved and absolute, or unchanged when it
/// cannot be resolved.
pub fn realpath(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

/// The work tree, symlink-resolved, or `None` when the repository has none.
fn work_tree(repo: &gix::Repository) -> Option<PathBuf> {
    std::fs::canonicalize(repo.workdir()?).ok()
}

/// `is_inside_dir()`: whether the current directory is `dir` or below it.
fn is_inside_dir(dir: &Path) -> bool {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| std::fs::canonicalize(cwd).ok())
        .is_some_and(|cwd| cwd.starts_with(dir))
}

/// git's `is_inside_git_dir()` (setup.c:472-478): whether the cwd is the git
/// directory or below it.
/// `git_path()`'s rendering of a path inside the git directory.
///
/// `setup.c` has already moved to the top of the work tree by the time a message
/// is printed, so an ordinary repository shows its git directory as `.git`
/// however deep the command was run. A separate git dir (`--git-dir`, a linked
/// worktree, a submodule) has no such shorthand and is printed in full.
///
/// Shared rather than per-verb: any command that quotes a path it is about to
/// write inside `$GIT_DIR` has to render it the same way, or two verbs report
/// different names for the same file.
pub fn git_path_display(repo: &gix::Repository, path: &Path) -> String {
    let git_dir = repo.git_dir();
    let shown = match repo.workdir() {
        Some(top) if git_dir == top.join(".git") => Path::new(".git"),
        _ => git_dir,
    };
    match path.strip_prefix(git_dir) {
        Ok(rest) => shown.join(rest).display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

pub fn is_inside_git_dir(repo: &gix::Repository) -> bool {
    is_inside_dir(&realpath(repo.git_dir()))
}

/// git's `is_inside_work_tree()` (setup.c:480-494): whether the cwd sits in the
/// work tree, which says nothing about the git directory. Both this and
/// [`is_inside_git_dir`] are true at once for `GIT_DIR=.` inside a `.git`
/// directory, because that setup makes the git directory its own work tree.
pub fn is_inside_work_tree(repo: &gix::Repository) -> bool {
    work_tree(repo).is_some_and(|top| is_inside_dir(&top))
}

/// git's `verify_non_filename()` (setup.c:299-310): an argument already parsed as
/// a revision must not also name an existing file, or the command line is
/// ambiguous and git refuses to guess.
///
/// The first line of that function is the part that is easy to lose:
///
/// ```c
/// if (!is_inside_work_tree(repo) || is_inside_git_dir(repo))
///         return;
/// ```
///
/// Standing outside a work tree — in a bare repository, or in a `.git` directory —
/// there are no worktree paths for a revision to collide with, so the check does
/// not run at all. Without that guard `git grep <pattern> HEAD` in a bare
/// repository calls `HEAD` ambiguous, because a file by that name is sitting right
/// there in the cwd.
///
/// Returns the message git would `die()` with, or `None` when the argument is
/// unambiguous.
pub fn verify_non_filename(repo: &gix::Repository, arg: &str) -> Option<String> {
    if !is_inside_work_tree(repo) || is_inside_git_dir(repo) {
        return None;
    }
    if arg.starts_with('-') {
        return None; // flag
    }
    if !check_filename(arg) {
        return None;
    }
    Some(format!(
        "ambiguous argument '{arg}': both revision and filename\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'"
    ))
}

/// git's `looks_like_pathspec()` (setup.c:232-260): an argument that can only have
/// been meant as a pathspec, whether or not anything by that name exists.
///
/// ```c
/// for (p = arg; *p; p++) {
///         if (escaped) {
///                 escaped = 0;
///         } else if (is_glob_special(*p)) {
///                 if (*p == '\\')
///                         escaped = 1;
///                 else
///                         return 1;
///         }
/// }
///
/// /* long-form pathspec magic */
/// if (starts_with(arg, ":("))
///         return 1;
///
/// return 0;
/// ```
///
/// `is_glob_special()` is the `GIT_GLOB_SPECIAL` class of `sane_ctype`, which
/// ctype.c:12 spells out as `*, ?, [, \\`. An unescaped `*`, `?` or `[` says the
/// caller means to *match* paths, so the argument need not exist on disk.
/// Backslash is in the class too, but the `if` above turns it into the escape flag
/// instead of an accept — on its own it only escapes the next character rather
/// than widening the match, so `a\*b` is a name, not a pattern.
///
/// The two conditions git checks are exactly these, and nothing else:
///
/// * A bare leading `:` is deliberately *not* enough. Short-form magic such as
///   `:/`, `:!` and `:^` is handled by [`check_filename`] (setup.c:178-186), which
///   strips it and stats what is left, so `:/nope` is a missing path rather than an
///   accepted pathspec.
/// * `:(` needs no closing `)` here. An unterminated `:(icase` is accepted by this
///   function and rejected later, by the pathspec parser, with its own
///   `Missing ')' at the end of pathspec magic` message.
pub fn looks_like_pathspec(arg: &str) -> bool {
    let mut escaped = false;
    for b in arg.bytes() {
        if escaped {
            escaped = false;
        } else if b == b'\\' {
            escaped = true;
        } else if matches!(b, b'*' | b'?' | b'[') {
            return true;
        }
    }
    arg.starts_with(":(")
}

/// git's `verify_filename()` (setup.c): a token sitting where a path is expected
/// has to be able to be one. Returns the message git would `die()` with, or
/// `None` when the token passes.
///
/// `diagnose_misspelt_rev` is git's flag for the *first* such token — the one the
/// caller had just tried and failed to read as a revision, so its failure is
/// ambiguous between a misspelt revision and a missing path. Every token after it
/// is already known to be in path position, so its failure can only be a missing
/// path, and it gets the shorter message.
pub fn verify_filename(arg: &str, diagnose_misspelt_rev: bool) -> Option<String> {
    if arg.starts_with('-') {
        return Some(format!("option '{arg}' must come before non-option arguments"));
    }
    if looks_like_pathspec(arg) || check_filename(arg) {
        return None;
    }
    if !diagnose_misspelt_rev {
        return Some(format!(
            "{arg}: no such path in the working tree.\n\
             Use 'git <command> -- <path>...' to specify paths that do not exist locally."
        ));
    }
    Some(format!(
        "ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'"
    ))
}

/// git's `check_filename()` (setup.c:173-200): whether `arg` names something that
/// exists in the worktree. It strips the leading pathspec magic that still leaves
/// a path behind and stats what remains. Magic with nothing after it counts as
/// existing without a stat — `:/` names the root, and excluding everything with a
/// bare `:!`/`:^` is pointless but legal. A bare empty argument gets no such
/// exemption: it reaches the stat and fails it, which is why
/// `git grep --no-index <pattern> ""` is a fatal rather than a match-nothing.
///
/// Paths are resolved against the current directory, which is where git resolves
/// them from too for every form but `:/<path>`: that one is root-relative, and git
/// can say so because it has already changed directory to the root by this point.
/// The two agree whenever the command is run from the root.
pub fn check_filename(arg: &str) -> bool {
    let path = match [":/", ":!", ":^"]
        .into_iter()
        .find_map(|magic| arg.strip_prefix(magic))
    {
        Some("") => return true,
        Some(rest) => rest,
        None => arg,
    };
    std::fs::symlink_metadata(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::{check_filename, looks_like_pathspec};

    /// The whole of git's rule, one case per branch of setup.c:232-260. Every verb
    /// that splits revisions from paths reads it from here, so a change that suits
    /// one caller has to be defended against all of them.
    #[test]
    fn matches_the_rule_in_setup_c() {
        // An unescaped glob special is a pattern, existing or not.
        assert!(looks_like_pathspec("no*pe"));
        assert!(looks_like_pathspec("a?b"));
        assert!(looks_like_pathspec("a[bc]"));
        // Backslash is `GIT_GLOB_SPECIAL` too, but it escapes rather than accepts.
        assert!(!looks_like_pathspec(r"a\*b"));
        assert!(!looks_like_pathspec(r"a\"));
        assert!(looks_like_pathspec(r"a\\*b"), "the escape is consumed by the second backslash");
        // Long-form magic, with or without its closing paren: git checks only the
        // two-byte prefix and leaves `Missing ')'` to the pathspec parser.
        assert!(looks_like_pathspec(":(top)nope"));
        assert!(looks_like_pathspec(":(icase"));
        assert!(looks_like_pathspec(":("));
        // Short-form magic is *not* long-form magic; `check_filename` stats it.
        assert!(!looks_like_pathspec(":/nope"));
        assert!(!looks_like_pathspec(":!nope"));
        assert!(!looks_like_pathspec(":^nope"));
        assert!(!looks_like_pathspec(":nope"));
        assert!(!looks_like_pathspec(":"));
        // Plain names are plain names.
        assert!(!looks_like_pathspec("README.md"));
        assert!(!looks_like_pathspec(""));
    }

    /// The other half of the pair: what [`looks_like_pathspec`] declines,
    /// `check_filename()` decides by stripping short magic and stating the rest. A
    /// bare `:/` (the repository root) and a bare `:!`/`:^` (excluding everything)
    /// are present without a lookup, which is why `git backfill README.md :/` exits
    /// 0 in a repository that has no file named `:/`.
    #[test]
    fn check_filename_strips_short_magic_before_stating() {
        assert!(check_filename(":/"));
        assert!(check_filename(":!"));
        assert!(check_filename(":^"));
        assert!(!check_filename(":/definitely-not-present"));
        assert!(!check_filename(":!definitely-not-present"));
        assert!(!check_filename(":^definitely-not-present"));
        // No magic to strip, so the whole argument is stated as-is.
        assert!(!check_filename(":definitely-not-present"));
        assert!(!check_filename("definitely-not-present"));
        // An empty argument reaches the stat and fails it — `:/` gets an exemption,
        // `""` does not, which is why `git grep --no-index <pat> ""` is fatal.
        assert!(!check_filename(""));
    }
}

// ---------------------------------------------------------------------------
// The gates repository setup runs before a command is entered
// ---------------------------------------------------------------------------
//
// Everything above answers a question a command asks *after* setup. What follows
// is setup itself — the four refusals `setup_git_directory_gently()` can raise
// before `run_builtin()` ever calls the verb, plus the one diagnostic the object
// database emits on the way up. They live together because their *order* is
// observable and is the part that is easy to get wrong: git decides in one pass
// down `setup_git_directory_gently_1()`, so which message a caller sees when two
// things are wrong at once is fixed, and a port that checks them in a different
// order reports the wrong one.
//
// Measured against git 2.55.0, in the order they fire:
//
// 1. [`object_directory_gate`] — `is_git_directory()` (setup.c:433-436) refuses to
//    recognise a candidate directory at all when `$GIT_OBJECT_DIRECTORY` names
//    something it cannot reach, so discovery walks past the repository and ends at
//    "not a git repository". No configuration has been read yet, which is why this
//    one beats even a malformed `GIT_CONFIG_COUNT`.
// 2. [`command_line_config_gate`] — `git_config_from_parameters()` (config.c:731-780).
//    The first read of configuration is the one `get_allowed_bare_repo()` and
//    `ensure_valid_ownership()` make, so a bad command-line override is reported
//    before either policy refusal.
// 3. `disallowed_bare_repository` (`lib.rs`) — `safe.bareRepository` (setup.c:1676-1678),
//    checked one line *before* ownership for a bare repository.
// 4. [`dubious_ownership`] — `ensure_valid_ownership()` (setup.c:1405-1435).
// 5. [`report_missing_alternates`] — not a refusal; `odb_is_source_usable()`
//    (odb.c:59-73) names each alternate object directory that has gone missing and
//    the command carries on.

use std::process::ExitCode;

/// `is_path_owned_by_current_uid()` (git-compat-util.h:313-332).
///
/// ```c
/// static inline int is_path_owned_by_current_uid(const char *path,
///                                                struct strbuf *report UNUSED)
/// {
///         struct stat st;
///         uid_t euid;
///
///         if (lstat(path, &st))
///                 return 0;
///
///         euid = geteuid();
///         if (euid == ROOT_UID)
///         {
///                 if (st.st_uid == ROOT_UID)
///                         return 1;
///                 else
///                         extract_id_from_env("SUDO_UID", &euid);
///         }
///
///         return st.st_uid == euid;
/// }
/// ```
///
/// `lstat`, not `stat`: a symlink is judged by who owns the link. The `root`
/// branch is the `sudo` case — a repository being operated on as root is only
/// "ours" when root owns it outright, otherwise the *invoking* user's id is read
/// back out of `$SUDO_UID` and that is who the path has to belong to. A path that
/// cannot be stat'd is not owned by anyone as far as this is concerned.
pub fn is_path_owned_by_current_uid(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(st) = std::fs::symlink_metadata(path) else {
        return false;
    };
    const ROOT_UID: u32 = 0;
    let mut euid = unsafe { libc::geteuid() };
    if euid == ROOT_UID {
        if st.uid() == ROOT_UID {
            return true;
        }
        // `extract_id_from_env("SUDO_UID", &euid)`: a value that is not a whole
        // decimal number leaves the id alone rather than replacing it.
        if let Ok(raw) = std::env::var("SUDO_UID") {
            if !raw.is_empty() {
                if let Ok(id) = raw.parse::<u32>() {
                    euid = id;
                }
            }
        }
    }
    st.uid() == euid
}

/// `strbuf_realpath(&buf, path, 0)` — the non-dying form, which resolves as much
/// of `path` as exists and keeps the rest verbatim.
///
/// The difference from [`realpath`] matters for `safe.directory` alone: git
/// normalizes `/some/where/*` before comparing it, and `/some/where` exists while
/// `/some/where/*` does not. `std::fs::canonicalize` refuses the whole path in
/// that case, so the existing prefix is resolved and the unresolvable tail
/// re-joined onto it.
fn realpath_lenient(path: &Path) -> Option<PathBuf> {
    if let Ok(resolved) = std::fs::canonicalize(path) {
        return Some(resolved);
    }
    let parent = path.parent()?;
    let name = path.file_name()?;
    if parent.as_os_str().is_empty() {
        return None;
    }
    Some(realpath_lenient(parent)?.join(name))
}

/// `interpolate_path(value, 0)` as `git_config_pathname()` calls it, for the two
/// forms a `safe.directory` entry can use: a leading `~`/`~user` and the
/// `%(prefix)/` installation-relative form. Anything else is returned unchanged.
///
/// `None` is git's `NULL`, which `git_config_pathname()` turns into a failure —
/// and [`safe_directory_allows`] then skips the entry, because a `~nosuchuser`
/// exemption cannot match a real repository anyway.
fn interpolate_path(value: &str) -> Option<PathBuf> {
    if let Some(rest) = value.strip_prefix("%(prefix)/") {
        // git resolves this against its own install prefix. gitoxide's
        // `gix_path::env` knows the same location, and a `safe.directory` written
        // this way is aimed at repositories git itself installed.
        let prefix = gix::path::env::exe_invocation()
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)?;
        return Some(prefix.join(rest));
    }
    let Some(after_tilde) = value.strip_prefix('~') else {
        return Some(PathBuf::from(value));
    };
    let (user, rest) = match after_tilde.find('/') {
        Some(cut) => (&after_tilde[..cut], &after_tilde[cut + 1..]),
        None => (after_tilde, ""),
    };
    let home = if user.is_empty() {
        PathBuf::from(std::env::var_os("HOME")?)
    } else {
        home_of(user)?
    };
    Some(if rest.is_empty() { home } else { home.join(rest) })
}

/// `getpw_str()`: `getpwnam(3)`'s home directory for a user, or `None` when the
/// system does not know the name.
fn home_of(user: &str) -> Option<PathBuf> {
    use std::ffi::{CStr, CString};
    let name = CString::new(user).ok()?;
    // Safety: `getpwnam` returns a pointer into a static buffer, read before any
    // other call can overwrite it.
    let pw = unsafe { libc::getpwnam(name.as_ptr()) };
    if pw.is_null() {
        return None;
    }
    let dir = unsafe { (*pw).pw_dir };
    if dir.is_null() {
        return None;
    }
    let dir = unsafe { CStr::from_ptr(dir) };
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(dir.to_bytes().to_vec())))
}

/// `safe_directory_cb()` (setup.c:1337-1395): whether the accumulated
/// `safe.directory` entries exempt `path`, which is the work tree of a
/// non-bare repository and the git directory of a bare one, already normalized.
///
/// ```c
/// if (!value || !*value) {
///         data->is_safe = 0;
/// } else if (!strcmp(value, "*")) {
///         data->is_safe = 1;
/// } else {
///         …
///         if (!is_absolute_path(allowed) && strcmp(allowed, ".")) {
///                 warning(_("safe.directory '%s' not absolute"), allowed);
///                 goto next;
///         }
///         normalized = real_pathdup(allowed, 0);
///         if (!normalized)
///                 goto next;
///         if (ends_with(normalized, "/*")) {
///                 size_t len = strlen(normalized);
///                 if (!fspathncmp(normalized, data->path, len - 1))
///                         data->is_safe = 1;
///         } else if (!fspathcmp(data->path, normalized)) {
///                 data->is_safe = 1;
///         }
/// ```
///
/// Four things in there are observable and each is easy to lose:
///
/// * **The scan does not stop at a match.** `is_safe` is a variable the callback
///   keeps rewriting, so a later entry can take an exemption *away* — and an empty
///   value is exactly that: `safe.directory = ` resets whatever came before it, so
///   the last word wins. `-c safe.directory=<path> -c safe.directory=` refuses,
///   the reverse order accepts.
/// * **`*` is not a glob.** It is a literal value meaning "everything"; `/*` at the
///   end of a real path *is* a prefix match, and it matches only what is *below*
///   the directory. `fspathncmp` compares `len - 1` bytes, so the trailing `*` is
///   dropped and the `/` before it is not — `/a/b/*` matches `/a/b/c` and does not
///   match `/a/b` itself.
/// * **A relative entry is refused with a warning**, because it could not be
///   relative to anything meaningful: the exemption lives in a configuration file
///   that may be shared across machines. `.` is the one exception — it means "the
///   repository I am standing at the top of" and normalizes to the current
///   directory.
/// * **A path that does not exist is silently skipped**, not warned about, for the
///   same shared-`~/.gitconfig` reason: an entry naming a repository on another
///   machine is not an error here.
///
/// The comparison is `fspathcmp`, which is `strcmp` unless `core.ignoreCase` is
/// set — and configuration has not been read at this point in setup, so it is
/// always `strcmp`. Compared as bytes here for the same reason.
fn safe_directory_allows(path: &Path) -> bool {
    let config = crate::config::global_config();
    let mut is_safe = false;
    for value in config.strings("safe.directory").unwrap_or_default() {
        let Ok(value) = std::str::from_utf8(value.as_ref()) else {
            continue;
        };
        if value.is_empty() {
            is_safe = false;
            continue;
        }
        if value == "*" {
            is_safe = true;
            continue;
        }
        let Some(allowed) = interpolate_path(value) else {
            continue;
        };
        if !allowed.is_absolute() && allowed != Path::new(".") {
            eprintln!("warning: safe.directory '{}' not absolute", allowed.display());
            continue;
        }
        let Some(normalized) = realpath_lenient(&allowed) else {
            continue;
        };
        let normalized = normalized.as_os_str().as_encoded_bytes();
        let candidate = path.as_os_str().as_encoded_bytes();
        match normalized.strip_suffix(b"/*") {
            // `fspathncmp(normalized, data->path, len - 1)`: the `*` is dropped,
            // the `/` is not, so the entry covers what is under the directory
            // rather than the directory itself.
            Some(prefix) => {
                if candidate.starts_with(prefix) && candidate.len() > prefix.len() {
                    is_safe = true;
                }
            }
            None => {
                if candidate == normalized {
                    is_safe = true;
                }
            }
        }
    }
    is_safe
}

/// `ensure_valid_ownership()` (setup.c:1405-1435).
///
/// ```c
/// if (!git_env_bool("GIT_TEST_ASSUME_DIFFERENT_OWNER", 0) &&
///     (!gitfile || is_path_owned_by_current_user(gitfile, report)) &&
///     (!worktree || is_path_owned_by_current_user(worktree, report)) &&
///     (!gitdir || is_path_owned_by_current_user(gitdir, report)))
///         return 1;
///
/// data.path = real_pathdup(worktree ? worktree : gitdir, 0);
/// if (!data.path)
///         return 0;
/// git_protected_config(safe_directory_cb, &data);
/// ```
///
/// All three of the paths a repository is reached through have to be ours: the
/// `.git` *file* when there is one (a submodule or a linked work tree), the work
/// tree, and the git directory. Any one of them owned by someone else is enough to
/// stop, because any one of them is enough to make git run their code — a
/// `core.fsmonitor` or a hook in a git directory you do not own is arbitrary
/// execution under your account, which is what this exists to prevent.
///
/// The path an exemption is written against is only ever *one* of them —
/// the work tree when there is one, the git directory otherwise — whichever of the
/// three actually failed. That is why the message names the same path it tells you
/// to add.
///
/// `GIT_TEST_ASSUME_DIFFERENT_OWNER` is git's own hook for exercising this without
/// a second user account: it skips the ownership test and goes straight to
/// `safe.directory`, so the exemption logic is still fully live under it. It is
/// read with `git_env_bool`, so it is a boolean and not merely a presence check.
fn ensure_valid_ownership(gitfile: Option<&Path>, worktree: Option<&Path>, gitdir: &Path) -> bool {
    let assume_different = env_bool("GIT_TEST_ASSUME_DIFFERENT_OWNER", false);
    if !assume_different
        && gitfile.is_none_or(is_path_owned_by_current_uid)
        && worktree.is_none_or(is_path_owned_by_current_uid)
        && is_path_owned_by_current_uid(gitdir)
    {
        return true;
    }
    let Some(path) = realpath_lenient(worktree.unwrap_or(gitdir)) else {
        return false;
    };
    safe_directory_allows(&path)
}

/// `git_env_bool()` (parse.c:193-208), the one reader every environment
/// *boolean* in git goes through:
///
/// ```c
/// int git_env_bool(const char *k, int def)
/// {
///         const char *v = getenv(k);
///         int val;
///         if (!v)
///                 return def;
///         val = git_parse_maybe_bool(v);
///         if (val < 0)
///                 die(_("bad boolean environment value '%s' for '%s'"), v, k);
///         return val;
/// }
/// ```
///
/// Two properties are the whole point and are easy to lose separately. An unset
/// variable takes the default — it is never "false" — and a value that is not a
/// boolean at all is `die()`, not a fall back to the default. The grammar is
/// `git_parse_maybe_bool()`'s, ported in [`crate::optint::maybe_bool`]: the
/// words `true`/`yes`/`on`/`false`/`no`/`off` case-insensitively, the empty
/// string for false, and any integer the base-0 grammar reads as its
/// truthiness — so `0x10` and `1k` are true and a value past `int` range is not
/// a boolean.
///
/// **This is not a licence to validate every `GIT_*` variable.** git validates
/// exactly the variables it reads *through this function*, at the moment that
/// read happens, and coerces every other one silently. `GIT_NO_REPLACE_OBJECTS`
/// and `GIT_SKIP_HASH` are plain `getenv()` presence tests and accept
/// `bogus` with no complaint; `GIT_DIR`, `GIT_ALLOW_PROTOCOL` and friends are
/// not booleans at all. Adding a caller here that git does not have turns a
/// value stock git accepts into a refusal, which is a worse divergence than the
/// one it fixes. Every call site below cites the C line it stands in for.
pub(crate) fn git_env_bool(key: &str, default: bool) -> bool {
    let Ok(raw) = std::env::var(key) else {
        return default;
    };
    git_env_bool_value(key, &raw)
}

/// [`git_env_bool`]'s second half, for the callers that already hold the value —
/// [`crate::config`] reads the environment through an injectable closure so a
/// test can supply one, and would otherwise have to consult the real environment
/// a second time to reach the same verdict.
pub(crate) fn git_env_bool_value(key: &str, raw: &str) -> bool {
    match crate::optint::maybe_bool(raw) {
        Some(v) => v,
        None => {
            eprintln!("fatal: bad boolean environment value '{raw}' for '{key}'");
            crate::hosted::exit(crate::fatal::EXIT_FATAL as i32);
        }
    }
}

/// The spelling the ownership checks in this module already used.
fn env_bool(key: &str, default: bool) -> bool {
    git_env_bool(key, default)
}

/// `sq_quote_buf_pretty()` (quote.c:50-70): shell quoting applied only when the
/// text needs it, so the copy-and-paste line in the ownership message reads as a
/// plain path in the common case and as a quoted one when the path has a space.
///
/// ```c
/// static const char ok_punct[] = "+,-./:=@_^";
/// if (!*src) { strbuf_addstr(dst, "''"); return; }
/// for (p = src; *p; p++)
///         if (!isalnum(*p) && !strchr(ok_punct, *p)) { sq_quote_buf(dst, src); return; }
/// strbuf_addstr(dst, src);
/// ```
fn sq_quote_pretty(text: &str) -> String {
    const OK_PUNCT: &[u8] = b"+,-./:=@_^";
    if text.is_empty() {
        return "''".to_owned();
    }
    if text
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || OK_PUNCT.contains(&b))
    {
        return text.to_owned();
    }
    let mut out = Vec::new();
    crate::porcelain::rev_parse::sq_quote_buf(&mut out, text.as_bytes());
    String::from_utf8_lossy(&out).into_owned()
}

/// The `GIT_DIR_INVALID_OWNERSHIP` refusal (setup.c:1651-1656, 1979-1993).
///
/// ```c
/// die(_("detected dubious ownership in repository at '%s'\n"
///       "%s"
///       "To add an exception for this directory, call:\n"
///       "\n"
///       "\tgit config --global --add safe.directory %s"),
///     dir.buf, report.buf, quoted.buf);
/// ```
///
/// The `%s` in the middle is `report`, which only the Windows build fills in (with
/// the account that does own the path); on every other platform
/// `is_path_owned_by_current_uid()` takes its `report` argument `UNUSED` and the
/// message has nothing between the two halves. `strbuf_complete(&report, '\n')`
/// adds no newline to an empty buffer, so there is no blank line either.
///
/// Three parts of *where* this fires are as load-bearing as the message:
///
/// * **`$GIT_DIR` skips it entirely.** `setup_git_directory_gently_1()` returns
///   `GIT_DIR_EXPLICIT` at setup.c:1560-1564, before the discovery loop that holds
///   the check. Naming a repository outright is taken as consent.
/// * **The path named is the repository, not the current directory.** git reports
///   `dir.buf`, which the walk has already trimmed back to the top of the work
///   tree, so running from a subdirectory names the same path as running from the
///   top — and that is the path the exemption has to be written against.
/// * **Only the commands that need a repository die.** The ones git runs with
///   `RUN_SETUP_GENTLY` get `*nongit_ok = 1` and carry on, which is
///   [`crate::NO_SETUP_VERBS`]. Verified against git 2.55.0: `status`, `log`,
///   `branch`, `add`, `commit`, `fetch`, `push`, `gc`, `fsck`, `ls-files`,
///   `describe`, `blame`, `worktree`, `stash`, `remote`, `tag`, `reflog`,
///   `rev-parse HEAD` and `cat-file -p HEAD` all refuse; `version`, `help`,
///   `config`, `init`, `hash-object`, `shortlog`, `patch-id`, `stripspace`,
///   `interpret-trailers`, `bugreport`, `column`, `diff` and `ls-remote` do not.
///
/// The verbs that never call `setup_git_directory_gently_1()`, and so never read
/// `GIT_DISCOVERY_ACROSS_FILESYSTEM` at all.
///
/// Not the same list as [`crate::NO_SETUP_VERBS`], which is about what happens
/// when discovery comes up *empty*: `config`, `var`, `grep` and `hash-object`
/// are all on that list and all still run the discovery walk, so all four die on
/// a malformed `GIT_DISCOVERY_ACROSS_FILESYSTEM`. What is here is the narrower
/// set that has no walk to run — `version` and `help` answer without a
/// repository, `init`/`init-db`/`clone` create one rather than find one, and the
/// rest either take the repository as an argument or read no configuration.
///
/// Measured against git 2.55.0 with `GIT_DISCOVERY_ACROSS_FILESYSTEM=bogus`:
/// `git --version` and `git init <dir>` exit 0, while `git config --list`,
/// `git var GIT_EDITOR`, `git hash-object --stdin`, `git status` and
/// `git rev-parse --git-dir` all exit 128 with
/// `fatal: bad boolean environment value 'bogus' for
/// 'GIT_DISCOVERY_ACROSS_FILESYSTEM'` — inside a repository and outside one
/// alike, since the variable is read before the walk that would find one.
const NO_DISCOVERY_VERBS: &[&str] = &[
    "check-ref-format",
    "clone",
    "credential-cache",
    "credential-cache--daemon",
    "credential-store",
    "get-tar-commit-id",
    "help",
    "init",
    "init-db",
    "mailsplit",
    "remote-ext",
    "remote-fd",
    "stripspace",
    "upload-archive",
    "upload-archive--writer",
    "url-parse",
    "verify-pack",
    "version",
];

/// `git_env_bool("GIT_DISCOVERY_ACROSS_FILESYSTEM", 0)` (setup.c:1597), the first
/// line of `setup_git_directory_gently_1()`'s discovery loop:
///
/// ```c
/// one_filesystem = !git_env_bool("GIT_DISCOVERY_ACROSS_FILESYSTEM", 0);
/// if (one_filesystem)
///         current_device = get_device_or_die(dir->buf, NULL, 0);
/// ```
///
/// It runs *before* the walk, so it precedes every other setup refusal —
/// `$GIT_OBJECT_DIRECTORY`, `safe.bareRepository`, `safe.directory` — and does
/// not need a repository to exist. Only the value is honoured here; the
/// one-filesystem walk itself is `gix`'s, which does not cross mount points
/// either.
pub fn discovery_environment_gate(sub: &str) {
    if NO_DISCOVERY_VERBS.contains(&sub) {
        return;
    }
    let _ = git_env_bool("GIT_DISCOVERY_ACROSS_FILESYSTEM", false);
}

/// `git_env_bool(NO_LAZY_FETCH_ENVIRONMENT, 0)` (setup.c:1066), the last line of
/// `setup_git_env_internal()`:
///
/// ```c
/// if (git_env_bool(NO_LAZY_FETCH_ENVIRONMENT, 0))
///         fetch_if_missing = 0;
/// ```
///
/// `setup_git_env_internal()` runs when a git directory has been *established* —
/// found by the walk, named by `$GIT_DIR`, or freshly created by `git init` — so
/// unlike [`discovery_environment_gate`] this one is silent when there is no
/// repository. Measured against git 2.55.0 with `GIT_NO_LAZY_FETCH=bogus`:
/// outside a repository `git config --list`, `git var GIT_EDITOR` and
/// `git hash-object --stdin` all exit 0, while `git init <dir>` exits 128; inside
/// a repository every verb that reaches setup exits 128 with
/// `fatal: bad boolean environment value 'bogus' for 'GIT_NO_LAZY_FETCH'`.
///
/// Placed after the ownership gates because that is the order git reaches them:
/// the walk applies `safe.bareRepository` and `safe.directory` at setup.c:1651-1678
/// and only the caller that accepts the result goes on to set the environment up.
pub fn no_lazy_fetch_environment_gate(sub: &str) {
    let creates_repository = matches!(sub, "init" | "init-db");
    if !creates_repository && gix::discover(".").is_err() {
        return;
    }
    let _ = git_env_bool("GIT_NO_LAZY_FETCH", false);
}

/// Returns the exit code to leave with, or `None` to continue.
pub fn dubious_ownership(sub: &str) -> Option<ExitCode> {
    if crate::NO_SETUP_VERBS.contains(&sub) {
        return None;
    }
    if std::env::var_os("GIT_DIR").is_some() {
        return None;
    }
    let repo = gix::discover(".").ok()?;
    let git_dir = realpath(repo.git_dir());
    let work_tree = discovered_directory(&git_dir);
    // `gitfile` is the `.git` *file* the work tree was reached through, which
    // exists only when the git directory is somewhere else. git passes the path it
    // read, so the file's own ownership is checked alongside what it points at.
    let gitfile = work_tree.as_ref().map(|top| top.join(".git")).filter(|p| p.is_file());
    if ensure_valid_ownership(gitfile.as_deref(), work_tree.as_deref(), &git_dir) {
        return None;
    }
    let path = work_tree.unwrap_or(git_dir);
    let shown = path.display().to_string();
    eprintln!(
        "fatal: detected dubious ownership in repository at '{shown}'\n\
         To add an exception for this directory, call:\n\
         \n\
         \tgit config --global --add safe.directory {}",
        sq_quote_pretty(&shown)
    );
    Some(ExitCode::from(crate::fatal::EXIT_FATAL))
}

/// The directory `setup_git_directory_gently_1()` was standing in when it found
/// the repository — its `dir->buf`, which is what the ownership check is handed
/// as the work tree and what the refusal names.
///
/// This is deliberately *not* `Repository::workdir()`. The check runs inside the
/// discovery loop (setup.c:1651), long before `setup_discovered_git_dir()` applies
/// `$GIT_WORK_TREE` (setup.c:1217-1228) or `core.worktree`, so a redirected work
/// tree is invisible to it. Reading `workdir()` instead made
/// `GIT_WORK_TREE=<missing dir> git ls-files -o` in a bare repository fail the
/// ownership test — `lstat` cannot stat a directory that is not there, so nothing
/// owns it — and report dubious ownership where git reports nothing at all.
///
/// `None` is git's `GIT_DIR_BARE` arm (setup.c:1674-1682), which passes `NULL` for
/// both the gitfile and the work tree and checks only the git directory.
fn discovered_directory(git_dir: &Path) -> Option<PathBuf> {
    let cwd = realpath(&std::env::current_dir().ok()?);
    for dir in cwd.ancestors() {
        // `read_gitfile_gently("<dir>/.git")` — a directory or a gitfile, either
        // way the walk stops here and `dir` is the work tree.
        if std::fs::symlink_metadata(dir.join(".git")).is_ok() {
            return Some(dir.to_owned());
        }
        // `if (is_git_directory(dir->buf))` — the directory *is* the repository, so
        // there is no work tree to check.
        if dir == git_dir {
            return None;
        }
    }
    None
}

/// `is_git_directory()`'s object-database probe (setup.c:433-442).
///
/// ```c
/// if (getenv(DB_ENVIRONMENT)) {
///         if (access(getenv(DB_ENVIRONMENT), X_OK))
///                 goto done;
/// }
/// else {
///         strbuf_setlen(&path, len);
///         strbuf_addstr(&path, "/objects");
///         if (access(path.buf, X_OK))
///                 goto done;
/// }
/// ```
///
/// `$GIT_OBJECT_DIRECTORY` *replaces* the `objects` directory in the test that
/// decides whether a candidate directory is a repository at all. So pointing it at
/// something that is not there does not produce a complaint about the object
/// database — it un-recognises every repository on the way up, and discovery ends
/// at the ceiling with git's ordinary "not a git repository". That is the whole
/// diagnostic; there is no second message naming the variable.
///
/// It is `access(X_OK)`, not "is a directory": a regular file fails it (the
/// execute bit is not set on `a.txt`), an empty string fails it, and a directory
/// that exists passes even when it holds no objects — in which case setup succeeds
/// and the *command* fails later, on the object it cannot find.
///
/// Nothing has read configuration by this point, which is why this fires ahead of
/// [`command_line_config_gate`]: `GIT_CONFIG_COUNT=bogus GIT_OBJECT_DIRECTORY=<missing>`
/// reports the missing repository, not the bad override.
///
/// Returns the exit code to leave with, or `None` to continue.
pub fn object_directory_gate(sub: &str) -> Option<ExitCode> {
    if crate::NO_SETUP_VERBS.contains(&sub) {
        return None;
    }
    let objdir = std::env::var_os("GIT_OBJECT_DIRECTORY")?;
    if access_x_ok(Path::new(&objdir)) {
        return None;
    }
    // The variable is unusable, so no candidate directory can be a repository.
    // Which message that produces depends on how the repository would have been
    // named: `$GIT_DIR` is the explicit form and reports the directory it was
    // handed (setup.c:1127-1133), everything else walked up and hit the ceiling
    // (setup.c:1966-1969).
    let msg = match std::env::var_os("GIT_DIR") {
        Some(git_dir) => {
            format!("not a git repository: '{}'", Path::new(&git_dir).display())
        }
        None => crate::fatal::no_repository_walked(),
    };
    eprintln!("fatal: {msg}");
    Some(ExitCode::from(crate::fatal::EXIT_FATAL))
}

/// `access(path, X_OK)`: the caller may search the directory.
fn access_x_ok(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // Safety: `c` is a NUL-terminated path that outlives the call.
    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
}

/// `git_config_from_parameters()` (config.c:731-780) and the `die()` its caller
/// wraps it in (config.c:1601-1602).
///
/// ```c
/// count = strtoul(env, &endp, 10);
/// if (*endp) {
///         ret = error(_("bogus count in %s"), CONFIG_COUNT_ENVIRONMENT);
///         goto out;
/// }
/// if (count > INT_MAX) {
///         ret = error(_("too many entries in %s"), CONFIG_COUNT_ENVIRONMENT);
///         goto out;
/// }
/// …
///         if (!key) { ret = error(_("missing config key %s"), envvar.buf); goto out; }
/// …
///         if (!value) { ret = error(_("missing config value %s"), envvar.buf); goto out; }
/// ```
/// ```c
/// if (!opts->ignore_cmdline && git_config_from_parameters(fn, data) < 0)
///         die(_("unable to parse command-line config"));
/// ```
///
/// Two lines, not one: `error()` says what was wrong with the environment and
/// `die()` says the command line could not be parsed, and the exit code is 128.
/// This port previously reported gitoxide's own wording through the generic
/// `zvcs: <verb>: …` channel at exit 1, which is the wrong prefix *and* the wrong
/// code — a caller testing for 128 saw success.
///
/// The grammar is `strtoul(env, &endp, 10)`, which is worth spelling out because
/// three of its edge cases are reachable:
///
/// * An **empty** value parses as `0` with `endp` at the terminator, so
///   `GIT_CONFIG_COUNT=` is not an error — it is zero overrides.
/// * `strtoul` **skips leading whitespace**, so `" 1"` is one override and the
///   failure that follows is `missing config key GIT_CONFIG_KEY_0`, not a bogus
///   count.
/// * `strtoul` **wraps a negative**, so `-1` becomes `ULONG_MAX` and trips the
///   `> INT_MAX` arm: `too many entries`, not `bogus count`.
///
/// Only commands that never read configuration escape it. Verified against git
/// 2.55.0: `version`, a bare `help`, and `stripspace` exit 0 under a bogus count;
/// everything else measured — including `help -a`, `check-ref-format`, `var`,
/// `config --list`, `init`, `diff --no-index`, `ls-remote`, `merge-file -h`, and
/// an unknown verb on its way to `help_unknown_cmd` — reports it and exits 128.
///
/// Returns the exit code to leave with, or `None` to continue.
pub fn command_line_config_gate(sub: &str, args: &[String]) -> Option<ExitCode> {
    // Verified against git 2.55.0: `version`, a bare `help` and `stripspace` exit 0
    // under a bogus count; everything else measured reports it and exits 128.
    let reads_no_config = sub == "version" || sub == "stripspace" || (sub == "help" && args.is_empty());
    match command_line_config_count() {
        Err(reason) => {
            if reads_no_config {
                return None;
            }
            eprintln!("error: {reason}");
            eprintln!("fatal: unable to parse command-line config");
            Some(ExitCode::from(crate::fatal::EXIT_FATAL))
        }
        // `strtoul("")` is `0` with `endp` at the terminator, so an empty
        // `GIT_CONFIG_COUNT` is zero overrides and not an error. gitoxide's parser
        // is stricter and rejects the empty string outright, so the value is
        // rewritten to the `0` it means before anything reads configuration.
        Ok(0) => {
            if std::env::var_os("GIT_CONFIG_COUNT").is_some_and(|v| v != "0") {
                // Safety: this runs on the main thread, before the verb is
                // dispatched — the same point at which `push_config_override()`
                // publishes `-c` overrides through these variables.
                std::env::set_var("GIT_CONFIG_COUNT", "0");
            }
            None
        }
        Ok(_) => None,
    }
}

/// The number of `-c`-equivalent overrides the environment declares, or the
/// `error()` line [`command_line_config_gate`] reports.
fn command_line_config_count() -> Result<u64, String> {
    let Ok(raw) = std::env::var("GIT_CONFIG_COUNT") else {
        return Ok(0);
    };
    let Some(count) = strtoul_10(&raw) else {
        return Err("bogus count in GIT_CONFIG_COUNT".to_owned());
    };
    if count > i32::MAX as u64 {
        return Err("too many entries in GIT_CONFIG_COUNT".to_owned());
    }
    for i in 0..count {
        let key_var = format!("GIT_CONFIG_KEY_{i}");
        let Ok(key) = std::env::var(&key_var) else {
            return Err(format!("missing config key {key_var}"));
        };
        let value_var = format!("GIT_CONFIG_VALUE_{i}");
        if std::env::var_os(&value_var).is_none() {
            return Err(format!("missing config value {value_var}"));
        }
        // `config_parse_pair()`'s own refusal, reported through the same pair of
        // lines: a key with no section cannot name anything.
        if !key.contains('.') {
            return Err(format!("key does not contain a section: {key}"));
        }
    }
    Ok(count)
}

/// `strtoul(value, &endp, 10)` followed by git's `if (*endp)` test. `None` is
/// "there was trailing junk", which is the `bogus count` arm.
///
/// Three details of C's conversion are reachable through this variable and all
/// three were measured against git 2.55.0:
///
/// * **Leading whitespace and a sign are consumed**, so `" 1"`, `"+1"` and `"007"`
///   are all one override and the failure that follows is the missing key.
/// * **A negative wraps** into `ULONG_MAX`, which the caller's `> INT_MAX` test
///   then reports as `too many entries` rather than a bogus count.
/// * **When no digits are converted, `endp` is left at the *start* of the string**
///   (C's "if no conversion is performed, the value of nptr is stored in
///   `*endptr`"), not after the whitespace it skipped. So `""` is zero and every
///   other digit-free value — `" "`, `"+"`, `"-"`, `"bogus"` — is a bogus count.
///   Trimming first and then testing the remainder would call `" "` zero.
fn strtoul_10(value: &str) -> Option<u64> {
    let rest = value.trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r']);
    let (negate, rest) = match rest.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, rest.strip_prefix('+').unwrap_or(rest)),
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        // No conversion, so `endp == nptr`: the whole original value is "left
        // over", and only an empty one ends there.
        return value.is_empty().then_some(0);
    }
    if digits.len() != rest.len() {
        return None;
    }
    // `ERANGE` saturates at `ULONG_MAX`, which is past `INT_MAX` either way.
    let magnitude = digits.parse::<u64>().unwrap_or(u64::MAX);
    Some(if negate { magnitude.wrapping_neg() } else { magnitude })
}

/// `odb_is_source_usable()`'s missing-alternate diagnostic (odb.c:59-73), over the
/// entries `parse_alternates()` (odb.c:102-167) reads out of
/// `$GIT_ALTERNATE_OBJECT_DIRECTORIES`.
///
/// ```c
/// /* Detect cases where alternate disappeared */
/// if (!is_directory(path)) {
///         error(_("object directory %s does not exist; "
///                 "check .git/objects/info/alternates"),
///               path);
///         goto out;
/// }
/// ```
///
/// It is an `error()`, not a `die()`: the entry is dropped and the command carries
/// on with whatever object databases are left, which is why
/// `GIT_ALTERNATE_OBJECT_DIRECTORIES=/nonexistent git log` still prints the log.
/// The message names `.git/objects/info/alternates` whether the entry came from
/// that file or from the environment, because `odb_is_source_usable()` cannot tell
/// them apart by the time it runs.
///
/// The parsing that feeds it is `parse_alternates(odb->alternate_db, PATH_SEP, NULL, …)`:
/// entries are `:`-separated, an entry beginning with `#` is a comment, one
/// beginning with `"` is C-quoted, an empty entry is skipped, and every survivor is
/// run through `strbuf_realpath()` — which is what turns a relative entry into a
/// path under the current directory and why the message shows an absolute one.
/// `is_directory()` is a `stat`, so a regular file is reported as not existing.
///
/// **Where this differs from git, deliberately.** git prepares alternates lazily,
/// the first time an object is looked up, and re-prepares them when a lookup
/// misses — so the same line appears once for `log`, `status` or `cat-file`, seven
/// times for `gc`, and not at all for `rev-parse HEAD`, `ls-files` or `branch`,
/// which never reach the object database. Reproducing that count would mean
/// hooking every object read. This reports each missing entry exactly once, before
/// the command runs, for any command that opens a repository — so the diagnostic
/// and its text are git's, and the repetition is not.
pub fn report_missing_alternates(sub: &str) {
    if crate::NO_SETUP_VERBS.contains(&sub) {
        return;
    }
    let Some(raw) = std::env::var_os("GIT_ALTERNATE_OBJECT_DIRECTORIES") else {
        return;
    };
    for entry in parse_alternates(&raw.to_string_lossy()) {
        if !entry.is_dir() {
            eprintln!(
                "error: object directory {} does not exist; check .git/objects/info/alternates",
                entry.display()
            );
        }
    }
}

/// `parse_alternates()` (odb.c:102-167) over a `PATH_SEP`-separated list.
///
/// ```c
/// while (*string) {
///         const char *end;
///         if (*string == '#') {
///                 /* comment; consume up to next separator */
///                 end = strchrnul(string, sep);
///         } else if (*string == '"' && !unquote_c_style(&buf, string, &end)) {
///                 /* quoted path; unquote_c_style has copied the data … */
///         } else {
///                 /* normal, unquoted path */
///                 end = strchrnul(string, sep);
///                 strbuf_add(&buf, string, end - string);
///         }
///         if (*end) end++;
///         string = end;
///         if (!buf.len) continue;
///         …
/// ```
///
/// The three arms are not interchangeable: only the unquoted one stops at the
/// separator, so a *quoted* entry ends at its closing quote and may contain a `:`
/// of its own. A `"` that does not open valid quoting falls through to the
/// unquoted arm rather than failing, which is what the `&&` in the condition
/// buys.
fn parse_alternates(list: &str) -> Vec<PathBuf> {
    let bytes = list.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let (entry, next): (Vec<u8>, usize) = if bytes[at] == b'#' {
            // A comment contributes nothing and runs to the next separator.
            (Vec::new(), memchr_sep(bytes, at))
        } else if bytes[at] == b'"' {
            match unquote_c_style_step(bytes, at) {
                Some((text, end)) => (text, end),
                None => {
                    let end = memchr_sep(bytes, at);
                    (bytes[at..end].to_vec(), end)
                }
            }
        } else {
            let end = memchr_sep(bytes, at);
            (bytes[at..end].to_vec(), end)
        };
        at = if next < bytes.len() { next + 1 } else { next };
        // `if (!buf.len) continue;` — an empty entry contributes nothing, which is
        // why `GIT_ALTERNATE_OBJECT_DIRECTORIES=` is silent.
        if entry.is_empty() {
            continue;
        }
        let path = PathBuf::from(<std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(entry));
        // `strbuf_realpath(&buf, pathbuf.buf, 0)`, which resolves a relative entry
        // against the current directory, then the trailing-slash trim.
        let Some(resolved) = realpath_lenient(&path)
            .or_else(|| std::env::current_dir().ok().map(|cwd| cwd.join(&path)))
        else {
            continue;
        };
        out.push(resolved);
    }
    out
}

/// `strchrnul(string, sep)` for git's `PATH_SEP`, which is `:` everywhere this
/// port runs: the offset of the next separator, or the end of the string.
fn memchr_sep(bytes: &[u8], from: usize) -> usize {
    bytes[from..]
        .iter()
        .position(|&b| b == b':')
        .map_or(bytes.len(), |off| from + off)
}

/// `unquote_c_style()` (quote.c) in its `endp`-reporting form: decode the quoted
/// run that starts at `at` and report where it ended. `None` is git's failure
/// return, which leaves the caller to treat the text as unquoted.
fn unquote_c_style_step(bytes: &[u8], at: usize) -> Option<(Vec<u8>, usize)> {
    let mut out = Vec::new();
    let mut i = at + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some((out, i + 1)),
            b'\\' => {
                i += 1;
                let &c = bytes.get(i)?;
                out.push(match c {
                    b'a' => 0x07,
                    b'b' => 0x08,
                    b'f' => 0x0c,
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    b'v' => 0x0b,
                    b'"' | b'\\' => c,
                    b'0'..=b'7' => {
                        // Up to three octal digits, as `git_parse_c_escape` reads
                        // them; the value wraps into a byte.
                        let mut value = u32::from(c - b'0');
                        for _ in 0..2 {
                            match bytes.get(i + 1) {
                                Some(&d @ b'0'..=b'7') => {
                                    value = value * 8 + u32::from(d - b'0');
                                    i += 1;
                                }
                                _ => break,
                            }
                        }
                        u8::try_from(value & 0xff).ok()?
                    }
                    _ => return None,
                });
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    // Ran off the end without a closing quote.
    None
}

// ---------------------------------------------------------------------------
// Which transports a URL is allowed to reach
// ---------------------------------------------------------------------------

/// `transport_check_allowed()` (transport.c:1156-1160): the refusal a connection
/// attempt raises when policy forbids its transport.
///
/// ```c
/// void transport_check_allowed(const char *type)
/// {
///         if (!is_transport_allowed(type, -1))
///                 die(_("transport '%s' not allowed"), type);
/// }
/// ```
///
/// git calls it from `git_connect()` for `git`, `ssh` and `file`
/// (connect.c:1239, 1457, 1480), from `transport_get()` for a bundle file
/// (transport.c:1215), and from `transport_helper_init()` for every helper-backed
/// scheme, `http`/`https` included (transport-helper.c:1343). It is therefore a
/// gate on the *connection*, not on the command: `git ls-remote --get-url .` never
/// connects and never trips it, while `git clone` prints `Cloning into '<dir>'…`
/// first and trips it after.
///
/// Returns the exit code to leave with, or `None` when the transport is allowed.
pub fn transport_check_allowed(kind: &str) -> Option<ExitCode> {
    if is_transport_allowed(kind, None) {
        return None;
    }
    eprintln!("fatal: transport '{kind}' not allowed");
    Some(ExitCode::from(crate::fatal::EXIT_FATAL))
}

/// `is_transport_allowed()` (transport.c:1124-1142).
///
/// ```c
/// int is_transport_allowed(const char *type, int from_user)
/// {
///         const struct string_list *allow_list = protocol_allow_list();
///         if (allow_list)
///                 return string_list_has_string(allow_list, type);
///
///         switch (get_protocol_config(type)) {
///         case PROTOCOL_ALLOW_ALWAYS:    return 1;
///         case PROTOCOL_ALLOW_NEVER:     return 0;
///         case PROTOCOL_ALLOW_USER_ONLY:
///                 if (from_user < 0)
///                         from_user = git_env_bool("GIT_PROTOCOL_FROM_USER", 1);
///                 return from_user;
///         }
///         BUG("invalid protocol_allow_config type");
/// }
/// ```
///
/// The two halves are not a cascade: `$GIT_ALLOW_PROTOCOL` is an *override*, and
/// when it is set the configuration is not consulted at all. That is why
/// `GIT_ALLOW_PROTOCOL=ssh` refuses `file` even though `file` would otherwise be
/// allowed to a user typing the command, and why an empty `$GIT_ALLOW_PROTOCOL`
/// allows nothing rather than everything — an empty list is still a list.
///
/// `from_user` is git's tri-state `int`: `Some(_)` for a caller that knows, `None`
/// for its `-1`, which reads `$GIT_PROTOCOL_FROM_USER`. **git sets that variable
/// to `0` around everything it runs on the user's behalf rather than at their
/// request** — submodule clones, remote-helper invocations, recursive fetches — so
/// the whole point of the `user` policy is the difference between "the user typed
/// this URL" and "a `.gitmodules` file or an HTTP redirect produced it". Without
/// it, a hostile `.gitmodules` naming an `ext::` or a local path reaches the
/// transport layer as though the user had asked for it.
pub fn is_transport_allowed(kind: &str, from_user: Option<bool>) -> bool {
    // `protocol_allow_list()` (transport.c:1047-1064): `string_list_split(&allowed,
    // v, ":", -1)`, so entries are separated by `:` and compared whole.
    if let Ok(list) = std::env::var("GIT_ALLOW_PROTOCOL") {
        return list.split(':').any(|entry| entry == kind);
    }
    match protocol_config(kind) {
        ProtocolAllow::Always => true,
        ProtocolAllow::Never => false,
        ProtocolAllow::UserOnly => {
            from_user.unwrap_or_else(|| env_bool("GIT_PROTOCOL_FROM_USER", true))
        }
    }
}

/// `enum protocol_allow_config` (transport.c:1066-1070).
enum ProtocolAllow {
    Never,
    UserOnly,
    Always,
}

/// `get_protocol_config()` (transport.c:1085-1122): `protocol.<type>.allow`, then
/// `protocol.allow`, then the built-in defaults.
///
/// ```c
/// /* fallback to built-in defaults */
/// /* known safe */
/// if (!strcmp(type, "http") || !strcmp(type, "https") ||
///     !strcmp(type, "git")  || !strcmp(type, "ssh"))
///         return PROTOCOL_ALLOW_ALWAYS;
///
/// /* known scary; err on the side of caution */
/// if (!strcmp(type, "ext"))
///         return PROTOCOL_ALLOW_NEVER;
///
/// /* unknown; by default let them be used only directly by the user */
/// return PROTOCOL_ALLOW_USER_ONLY;
/// ```
///
/// `ext::` is `never` by default because its URL *is* a command line — nothing
/// short of an explicit `protocol.ext.allow` should let one arrive from a
/// `.gitmodules` file. `file` is not in the safe list: it falls through to
/// user-only, which is why the local-path form of a submodule URL is refused
/// while typing the same path by hand works.
///
/// The lookup is `repo_config_get_string(the_repository, …)`, the full
/// configuration cascade — *not* the protected subset `safe.directory` uses. A
/// repository's own `config` can therefore widen its transports, which is
/// deliberate: it is the repository you already trusted enough to run commands in.
fn protocol_config(kind: &str) -> ProtocolAllow {
    let key = format!("protocol.{kind}.allow");
    if let Some(value) = transport_config_string(&key) {
        return parse_protocol_config(&key, &value);
    }
    if let Some(value) = transport_config_string("protocol.allow") {
        return parse_protocol_config("protocol.allow", &value);
    }
    match kind {
        "http" | "https" | "git" | "ssh" => ProtocolAllow::Always,
        "ext" => ProtocolAllow::Never,
        _ => ProtocolAllow::UserOnly,
    }
}

/// One key out of the same cascade `repo_config_get_string(the_repository, …)`
/// reads: the repository's merged configuration inside one, the global cascade
/// outside — `protocol.*.allow` has to work for `git clone`, which has no
/// repository yet.
fn transport_config_string(key: &str) -> Option<String> {
    match gix::discover(".") {
        Ok(repo) => repo.config_snapshot().string(key).map(|v| v.to_string()),
        Err(_) => crate::config::global_config()
            .string(key)
            .map(|v| v.to_string()),
    }
}

/// `parse_protocol_config()` (transport.c:1072-1083): the three words, matched
/// case-insensitively, and `die()` for anything else.
fn parse_protocol_config(key: &str, value: &str) -> ProtocolAllow {
    if value.eq_ignore_ascii_case("always") {
        return ProtocolAllow::Always;
    }
    if value.eq_ignore_ascii_case("never") {
        return ProtocolAllow::Never;
    }
    if value.eq_ignore_ascii_case("user") {
        return ProtocolAllow::UserOnly;
    }
    eprintln!("fatal: unknown value for config '{key}': {value}");
    crate::hosted::exit(crate::fatal::EXIT_FATAL as i32);
}

/// The transport name a URL is checked under — git's `url_scheme_name()` for the
/// three schemes `git_connect()` handles, and the scheme itself for everything
/// routed through a remote helper.
///
/// A URL with no scheme at all (`.`, `/srv/repo.git`, `host:path`) is what
/// `parse_connect_url()` classifies: an scp-like `host:path` is `ssh`, anything
/// else local is `file`. gitoxide's parser has already made that call by the time
/// it hands back a `Scheme`.
pub fn transport_name(url: &gix::Url) -> String {
    match &url.scheme {
        gix::url::Scheme::File => "file".to_owned(),
        gix::url::Scheme::Git => "git".to_owned(),
        gix::url::Scheme::Ssh => "ssh".to_owned(),
        gix::url::Scheme::Http => "http".to_owned(),
        gix::url::Scheme::Https => "https".to_owned(),
        gix::url::Scheme::Ext(name) => name.clone(),
    }
}

/// [`transport_check_allowed`] for a URL that has already been resolved — the
/// shape every transport verb needs, since each of them holds a `gix::Url` by the
/// time it is about to connect.
pub fn check_url_allowed(url: &gix::Url) -> Option<ExitCode> {
    transport_check_allowed(&transport_name(url))
}
