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
