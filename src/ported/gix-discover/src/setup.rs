//! The git-directory tests of `setup.c` — `is_git_directory()` and
//! `read_gitfile_gently()` — which every caller that has to decide "is this a
//! repository" without opening one asks the same way: discovery, `rev-parse`, and
//! `is_submodule_modified()` (submodule.c:1892-1899) on a gitlink's `<path>/.git`.

/// ```c
/// int is_git_directory(const char *suspect)
/// {
///         /* Check worktree-related signatures */
///         … "%s/HEAD" …
///         if (validate_headref(path.buf))
///                 goto done;
///
///         strbuf_reset(&path);
///         get_common_dir(&path, suspect);
///         len = path.len;
///
///         /* Check non-worktree-related signatures */
///         if (getenv(DB_ENVIRONMENT)) {
///                 if (access(getenv(DB_ENVIRONMENT), X_OK)) goto done;
///         } else {
///                 strbuf_setlen(&path, len);
///                 strbuf_addstr(&path, "/objects");
///                 if (access(path.buf, X_OK)) goto done;
///         }
///
///         strbuf_setlen(&path, len);
///         strbuf_addstr(&path, "/refs");
///         if (access(path.buf, X_OK)) goto done;
///
///         ret = 1;
/// ```
///
/// (`setup.c:415-453`.) `HEAD` is looked for in `suspect` itself, but `objects` and
/// `refs` in the *common* directory — which is what makes a linked worktree's
/// private administrative directory (`.git/worktrees/<id>`, which has neither) a
/// git directory all the same.
pub fn is_git_directory(dir: &std::path::Path) -> bool {
    if !validate_headref(&dir.join("HEAD")) {
        return false;
    }
    let common = get_common_dir(dir);
    let objects = match std::env::var_os("GIT_OBJECT_DIRECTORY") {
        Some(v) => std::path::PathBuf::from(v),
        None => common.join("objects"),
    };
    objects.is_dir() && common.join("refs").is_dir()
}

/// ```c
/// int get_common_dir_noenv(struct strbuf *sb, const char *gitdir)
/// {
///         strbuf_addf(&path, "%s/commondir", gitdir);
///         if (file_exists(path.buf)) {
///                 … read it, strip trailing CR/LF …
///                 if (!is_absolute_path(data.buf))
///                         strbuf_addf(&path, "%s/", gitdir);
///                 strbuf_addbuf(&path, &data);
///                 strbuf_add_real_path(sb, path.buf);
///         } else {
///                 strbuf_addstr(sb, gitdir);
///         }
/// }
/// ```
///
/// (`setup.c:312-350`.) `$GIT_COMMON_DIR` wins outright when it is set.
fn get_common_dir(gitdir: &std::path::Path) -> std::path::PathBuf {
    if let Some(v) = std::env::var_os("GIT_COMMON_DIR") {
        return std::path::PathBuf::from(v);
    }
    let Ok(text) = std::fs::read_to_string(gitdir.join("commondir")) else {
        return gitdir.to_path_buf();
    };
    let target = text.trim_end_matches(['\n', '\r']);
    let path = std::path::Path::new(target);
    let joined = if path.is_absolute() { path.to_path_buf() } else { gitdir.join(path) };
    std::fs::canonicalize(&joined).unwrap_or(joined)
}

/// `validate_headref()` (`setup.c:352-402`): a `refs/…` symlink, a file whose text
/// is `ref: refs/…`, or a file holding a bare object name.
fn validate_headref(path: &std::path::Path) -> bool {
    if let Ok(target) = std::fs::read_link(path) {
        return target.to_string_lossy().starts_with("refs/");
    }
    let Ok(data) = std::fs::read(path) else { return false };
    // `read_in_full(fd, buffer, sizeof(buffer)-1)` with a 256-byte buffer.
    let text = String::from_utf8_lossy(&data[..data.len().min(255)]).into_owned();
    if let Some(refname) = text.strip_prefix("ref:") {
        if refname.trim_start().starts_with("refs/") {
            return true;
        }
    }
    // `get_oid_hex_any()`: any of the hash algorithms git knows, so SHA-1's 40 and
    // SHA-256's 64 hex characters both answer.
    let head = text.trim_end_matches(['\n', '\r']);
    matches!(head.len(), 40 | 64) && head.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `read_gitfile_gently()`'s error codes, in the wording
/// `read_gitfile_error_die()` (`setup.c:927-952`, v2.56.0) gives each of them.
///
/// 2.56 names the gitfile, not the directory it points at, when that directory is
/// not a repository: `die(_("gitfile does not point to a valid repository: %s"),
/// path)` replaced 2.55's `die(_("not a git repository: %s"), dir)`, whose `dir`
/// the discovery walk passed as `NULL`.
///
/// `READ_GITFILE_ERR_STAT_FAILED` and `READ_GITFILE_ERR_NOT_A_FILE` are absent
/// deliberately: `read_gitfile_error_die()` breaks out of its switch for those two
/// without dying, because "there is no gitfile here" is the ordinary answer for a
/// directory or a path that does not exist. They are `Ok(None)`.
pub fn gitfile_error_message(file: &std::path::Path, err: GitfileError) -> String {
    let path = file.display();
    match err {
        GitfileError::OpenFailed(e) => format!("error opening '{path}': {e}"),
        GitfileError::TooLarge => format!("too large to be a .git file: '{path}'"),
        GitfileError::ReadFailed => format!("error reading {path}"),
        GitfileError::InvalidFormat => format!("invalid gitfile format: {path}"),
        GitfileError::NoPath => format!("no path in gitfile: {path}"),
        GitfileError::NotARepo => format!("gitfile does not point to a valid repository: {path}"),
    }
}

/// The `READ_GITFILE_ERR_*` codes that `read_gitfile_error_die()` turns into a
/// `die()`.
#[derive(Debug)]
pub enum GitfileError {
    /// `READ_GITFILE_ERR_OPEN_FAILED`.
    OpenFailed(std::io::Error),
    /// `READ_GITFILE_ERR_TOO_LARGE`.
    TooLarge,
    /// `READ_GITFILE_ERR_READ_FAILED`.
    ReadFailed,
    /// `READ_GITFILE_ERR_INVALID_FORMAT`.
    InvalidFormat,
    /// `READ_GITFILE_ERR_NO_PATH`.
    NoPath,
    /// `READ_GITFILE_ERR_NOT_A_REPO`.
    NotARepo,
}

/// `read_gitfile_gently()` (`setup.c:956-1035`) with its error code kept rather
/// than collapsed into "no gitfile": the caller that passes `NULL` for
/// `return_error_code` dies on six of the eight outcomes, and the two it does not
/// die on are the ones that mean "this is not a gitfile at all".
pub fn read_gitfile_gently(
    file: &std::path::Path,
) -> Result<Option<std::path::PathBuf>, GitfileError> {
    // `if (stat(path, &st))` / `if (!S_ISREG(st.st_mode))`: both are non-fatal.
    let Ok(meta) = std::fs::metadata(file) else { return Ok(None) };
    if !meta.is_file() {
        return Ok(None);
    }
    // `const int max_file_size = 1 << 20;`
    if meta.len() > (1 << 20) {
        return Err(GitfileError::TooLarge);
    }
    let data = std::fs::read(file).map_err(GitfileError::OpenFailed)?;
    let Some(rest) = data.strip_prefix(b"gitdir: ") else {
        return Err(GitfileError::InvalidFormat);
    };
    let target = rest
        .iter()
        .rposition(|b| !matches!(b, b'\n' | b'\r'))
        .map_or(&[][..], |end| &rest[..=end]);
    // `if (len < 9)`, i.e. nothing after the eight-byte prefix.
    if target.is_empty() {
        return Err(GitfileError::NoPath);
    }
    let mut path = std::path::PathBuf::from(std::ffi::OsString::from(
        String::from_utf8_lossy(target).into_owned(),
    ));
    if path.is_relative() {
        match file.parent() {
            Some(parent) => path = parent.join(path),
            None => return Err(GitfileError::NotARepo),
        }
    }
    if !is_git_directory(&path) {
        return Err(GitfileError::NotARepo);
    }
    Ok(Some(std::fs::canonicalize(&path).unwrap_or(path)))
}
