//! The `core.commentChar=auto` deprecation notice: a port of the
//! `comment_char_config` bookkeeping in config.c:2019-2290 that
//! `repo_read_config()` runs alongside every configset load, and of
//! `check_auto_comment_char_config()` (config.c:2255-2278), which turns it into a
//! `warning()` plus two `advise()` blocks.
//!
//! git arms the check with `warn_on_auto_comment_char = true` in exactly five
//! builtins — `commit` (builtin/commit.c:1803), `merge` (builtin/merge.c:1384),
//! `rebase` (builtin/rebase.c:1259), `revert` and `cherry-pick`
//! (builtin/revert.c:290, :310) — just before their first repository config
//! read, so the notice comes out ahead of anything those commands say about
//! their options or their work. [`warns_on_auto_comment_char`] names them for the
//! dispatcher, which calls [`check_auto_comment_char_config`] at that point.

use std::path::{Path, PathBuf};

use crate::config::{ValueOrigin, walk_config};

/// `comment_key_name()` (config.c:2066-2077), indexed by `key_id`.
const COMMENT_KEY_NAME: [&str; 2] = ["core.commentChar", "core.commentString"];

/// `DEPRECATED_CONFIG_ENV` (config.c:2259-2260): set once the warning is given,
/// so no child process repeats it.
const DEPRECATED_CONFIG_ENV: &str = "GIT_AUTO_COMMENT_CHAR_CONFIG_WARNING_GIVEN";

/// The builtins that set `warn_on_auto_comment_char` before reading config.
pub(crate) fn warns_on_auto_comment_char(sub: &str) -> bool {
    matches!(sub, "commit" | "merge" | "rebase" | "revert" | "cherry-pick")
}

/// `struct comment_char_config_item` (config.c:2025-2029). `scope_system` stands
/// for `item->scope == CONFIG_SCOPE_SYSTEM`, the only scope test made of it.
struct Item {
    key_id: usize,
    path: String,
    scope_system: bool,
}

/// `struct comment_char_config` (config.c:2019-2031). `key_flags` is the
/// `strintmap` from a file path to two bits per key id: `KEY_SEEN_ONCE` (1) or
/// `KEY_SEEN_TWICE` (2) (config.c:2044-2064).
#[derive(Default)]
struct CommentCharConfig {
    last_key_id: usize,
    auto_set: bool,
    auto_set_in_file: bool,
    key_flags: Vec<(String, usize, u8)>,
    item: Vec<Item>,
}

impl CommentCharConfig {
    fn get_flags(&self, path: &str, id: usize) -> u8 {
        self.key_flags
            .iter()
            .find(|(p, k, _)| p == path && *k == id)
            .map_or(0, |(_, _, v)| *v)
    }

    fn set_flags(&mut self, path: &str, id: usize, value: u8) {
        match self.key_flags.iter_mut().find(|(p, k, _)| p == path && *k == id) {
            Some(slot) => slot.2 = value,
            None => self.key_flags.push((path.to_owned(), id, value)),
        }
    }

    /// `comment_char_callback()` (config.c:2079-2115).
    ///
    /// ```c
    /// config->last_key_id = key_id;
    /// config->auto_set = value && !strcmp(value, "auto");
    /// if (kvi->origin_type != CONFIG_ORIGIN_FILE) {
    ///         return;
    /// } else if (get_comment_key_flags(config, kvi->filename, key_id)) {
    ///         set_comment_key_flags(config, kvi->filename, key_id, KEY_SEEN_TWICE);
    /// } else {
    ///         ... append { key_id, kvi->scope, kvi->filename } ...
    ///         set_comment_key_flags(config, kvi->filename, key_id, KEY_SEEN_ONCE);
    /// }
    /// config->auto_set_in_file = config->auto_set;
    /// ```
    ///
    /// The `auto` test is a case-sensitive `strcmp`, unlike the `strcasecmp` that
    /// picks `#` in environment.c:469, so `AUTO` behaves as `auto` without a word.
    fn callback(&mut self, key_id: usize, value: Option<&str>, file: Option<(&str, bool)>) {
        self.last_key_id = key_id;
        self.auto_set = value == Some("auto");
        let Some((path, scope_system)) = file else {
            return;
        };
        if self.get_flags(path, key_id) != 0 {
            self.set_flags(path, key_id, 2);
        } else {
            self.item.push(Item {
                key_id,
                path: path.to_owned(),
                scope_system,
            });
            self.set_flags(path, key_id, 1);
        }
        self.auto_set_in_file = self.auto_set;
    }
}

/// `check_auto_comment_char_config()` for the repository `repo`, in the
/// `!WITH_BREAKING_CHANGES` build (config.c:2255-2278):
///
/// ```c
/// if (!config->auto_set || !warn_on_auto_comment_char)
///         return;
/// if (git_env_bool(DEPRECATED_CONFIG_ENV, false))
///         return;
/// setenv(DEPRECATED_CONFIG_ENV, "true", true);
/// warning(_("Support for '%s=auto' is deprecated and will be removed in "
///           "Git 3.0"), comment_key_name(config->last_key_id));
/// add_comment_char_advice(repo, config);
/// ```
pub(crate) fn check_auto_comment_char_config(repo: &gix::Repository) {
    let scopes = system_scoped_files(repo);
    let mut config = CommentCharConfig::default();
    for v in walk_config(repo) {
        let key_id = match v.key.as_str() {
            "core.commentchar" => 0,
            "core.commentstring" => 1,
            _ => continue,
        };
        let file = match &v.origin {
            ValueOrigin::File { path, .. } => Some((path.as_str(), scopes.iter().any(|p| p == path))),
            ValueOrigin::CommandLine => None,
        };
        config.callback(key_id, v.value.as_deref(), file);
    }

    if !config.auto_set {
        return;
    }
    if crate::setup::git_env_bool(DEPRECATED_CONFIG_ENV, false) {
        return;
    }
    std::env::set_var(DEPRECATED_CONFIG_ENV, "true");
    eprintln!(
        "warning: Support for '{}=auto' is deprecated and will be removed in Git 3.0",
        COMMENT_KEY_NAME[config.last_key_id]
    );
    add_comment_char_advice(repo, &config);
}

/// The files the merged read took in at `CONFIG_SCOPE_SYSTEM`: the system file
/// itself and anything it included, whose values git also tags with the
/// including file's scope. Spelled as [`walk_config`] spells file origins.
fn system_scoped_files(repo: &gix::Repository) -> Vec<String> {
    let snapshot = repo.config_snapshot();
    snapshot
        .plumbing()
        .sections()
        .filter(|s| s.meta().source == gix::config::Source::System)
        .filter_map(|s| s.meta().path.as_deref().map(crate::config::shown_path))
        .collect()
}

/// `add_comment_char_advice()` (config.c:2194-2220).
fn add_comment_char_advice(repo: &gix::Repository, config: &CommentCharConfig) {
    // "If auto is set in the last file that we saw advise the user how to update
    // their config."
    if !config.auto_set_in_file {
        return;
    }
    add_unset_auto_comment_char_advice(repo, config);
    let Some(item) = config.item.last() else {
        return;
    };
    let placeholder = "<comment string>";
    let buf = format!(
        "    git config set {}{} {placeholder}\n",
        config_scope_arg(repo, item),
        COMMENT_KEY_NAME[item.key_id]
    );
    crate::advice::print_hint(&format!(
        "\nTo set a custom comment string please run\n\n{buf}\nwhere '{placeholder}' is the string you wish to use.\n"
    ));
}

/// `add_unset_auto_comment_char_advice()` (config.c:2172-2192), with
/// `can_unset_comment_char_config()` (config.c:2159-2170): no unset advice at all
/// when any of the files is a system one the user cannot write.
fn add_unset_auto_comment_char_advice(repo: &gix::Repository, config: &CommentCharConfig) {
    if config.item.iter().any(|i| i.scope_system && !writable(&i.path)) {
        return;
    }
    let mut buf = String::new();
    for item in &config.item {
        buf.push_str("    git config unset ");
        buf.push_str(&config_scope_arg(repo, item));
        if config.get_flags(&item.path, item.key_id) == 2 {
            buf.push_str("--all ");
        }
        buf.push_str(COMMENT_KEY_NAME[item.key_id]);
        buf.push('\n');
    }
    crate::advice::print_hint(&format!(
        "\nTo use the default comment string (#) please run\n\n{buf}"
    ));
}

/// `add_config_scope_arg()` (config.c:2117-2157): the `git config` option that
/// names `item`'s file, with its trailing space, or nothing for the repository's
/// own config file (`--local` is the default).
///
/// git compares the spellings with `fspatheq()`; both sides there come from the
/// same `$GIT_DIR` and `$HOME` strings. gitoxide spells the same files its own
/// way (`./.git/config`), so the comparison here is between the resolved paths.
fn config_scope_arg(repo: &gix::Repository, item: &Item) -> String {
    if item.scope_system && !writable(&item.path) {
        // "If the user cannot write to the system config recommend setting the
        // global config instead."
        return "--global ".to_owned();
    }
    let system = std::env::var_os("GIT_CONFIG_SYSTEM")
        .map(PathBuf::from)
        .or_else(crate::config::system_path_etc_gitconfig);
    if system.is_some_and(|s| same_file(&item.path, &s)) {
        return "--system ".to_owned();
    }
    let global = crate::porcelain::maintenance::global_config_path().ok();
    if global.is_some_and(|g| same_file(&item.path, &g)) {
        return "--global ".to_owned();
    }
    if same_file(&item.path, &repo.git_dir().join("config")) {
        return String::new();
    }
    if same_file(&item.path, &repo.common_dir().join("config.worktree")) {
        return "--worktree ".to_owned();
    }
    let shown = path_from_top(repo, &item.path);
    let mut path = shown.as_str();
    let mut out = String::from("--file ");
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy().into_owned();
        if let Some(rest) = path.strip_prefix(home.as_str()) {
            path = rest.strip_prefix('/').unwrap_or(rest);
            out.push_str("~/");
        }
    }
    out.push_str(&crate::setup::sq_quote_pretty(path));
    out.push(' ');
    out
}

/// `kvi->filename` as git spells it. All five builtins that warn are
/// `NEED_WORK_TREE`, so `setup_work_tree()` has moved git to the top of the
/// work tree and a discovered `$GIT_DIR` reads `.git` there: a file found
/// through it is named from the top (`.git/extra`), not from the directory the
/// command was started in (`../.git/extra`).
fn path_from_top(repo: &gix::Repository, path: &str) -> String {
    let p = Path::new(path);
    if p.is_absolute() {
        return path.to_owned();
    }
    let (Some(top), Ok(file)) = (repo.workdir(), std::fs::canonicalize(p)) else {
        return path.to_owned();
    };
    match std::fs::canonicalize(top).ok().and_then(|t| file.strip_prefix(t).ok().map(Path::to_path_buf)) {
        Some(inside) => inside.to_string_lossy().into_owned(),
        None => path.to_owned(),
    }
}

/// `fspatheq()` over two spellings of a config file's path.
fn same_file(a: &str, b: &Path) -> bool {
    let a = Path::new(a);
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// `!access(path, W_OK)`.
fn writable(path: &str) -> bool {
    let Ok(c) = std::ffi::CString::new(path) else {
        return false;
    };
    // SAFETY: `c` is a valid NUL-terminated string for the duration of the call.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}
