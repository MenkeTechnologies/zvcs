//! Port of `git_default_config()` (environment.c:659-716) — the callback git
//! hands every configured `<key> <value>` pair before a command does any work.
//!
//! Unlike [`crate::repo_settings`], which git builds lazily the first time some
//! code path asks for a setting, this runs while the configuration is being
//! *read*: `repo_config(r, git_default_config, NULL)` walks the configset and
//! calls the callback once per value (config.c:2334-2342 → `configset_iter`,
//! config.c:1654-1673). A value the callback refuses kills the command before it
//! prints anything of its own — which is why `git -c core.ignorecase=bogus status`
//! says nothing about the working tree, and why `git -c core.createObject=bogus
//! branch` never lists a branch.
//!
//! ```c
//! int git_default_config(const char *var, const char *value,
//!                        const struct config_context *ctx, void *cb)
//! {
//!         if (starts_with(var, "core."))
//!                 return git_default_core_config(var, value, ctx, cb);
//!         if (starts_with(var, "user.") || starts_with(var, "author.") ||
//!             starts_with(var, "committer."))
//!                 return git_ident_config(var, value, ctx, cb);
//!         if (starts_with(var, "i18n."))       return git_default_i18n_config(var, value);
//!         if (starts_with(var, "branch."))     return git_default_branch_config(var, value);
//!         if (starts_with(var, "push."))       return git_default_push_config(var, value);
//!         if (starts_with(var, "attr."))       return git_default_attr_config(var, value);
//!         if (starts_with(var, "advice.") || starts_with(var, "color.advice"))
//!                 return git_default_advice_config(var, value);
//!         if (!strcmp(var, "pager.color") || !strcmp(var, "color.pager")) { … }
//!         if (!strcmp(var, "pack.packsizelimit")) { … }
//!         if (!strcmp(var, "pack.compression")) { … }
//!         if (starts_with(var, "sparse."))     return git_default_sparse_config(var, value);
//!         return 0;
//! }
//! ```
//!
//! # The two shapes of refusal
//!
//! Every diagnostic in this module is one of exactly two shapes, and which one a
//! key uses is not a style choice — it is whether the C arm calls `die()` itself
//! or returns a negative number for `configset_iter()` to turn into a death:
//!
//! * **`die()` inside the callback.** One `fatal:` line, no origin clause, the
//!   same bytes whether the value came from `-c`, a file, or the environment.
//!   `git_config_bool`'s `bad boolean config value '%s' for '%s'`
//!   (config.c:1292-1298) and `die_bad_number`'s `bad numeric config value …`
//!   (config.c:1188-1223) are the two common ones; `bad zlib compression level`,
//!   `invalid mode for object creation` and `failed to expand user dir in` are
//!   arm-specific. The numeric one alone carries ` in file <path>`.
//! * **`return error(...)` / `return config_error_nonbool(...)`.** The `error:`
//!   text prints first, then `configset_iter()` calls `git_die_config_linenr()`
//!   (config.c:2552-2559) which names the source: `unable to parse '<key>' from
//!   command-line config`, or `bad config variable '<key>' in file '<path>' at
//!   line <n>`. [`crate::config::ValueOrigin`] carries what that needs.
//!
//! Both were measured against git 2.55.0 in all three scopes (`-c`, `.git/config`,
//! `GIT_CONFIG_KEY_0`/`GIT_CONFIG_VALUE_0`); see `tests/default_config_keys.rs`.
//!
//! # Every occurrence, in parse order
//!
//! This is not a last-value-wins read: the callback sees every occurrence, so the
//! **first** bad one is fatal even when a later line would have overridden it.
//! [`crate::config::walk_config`] reproduces that order; the contrast with the
//! targeted readers is measured there.
//!
//! # What is honored, and what is only validated
//!
//! Almost nothing here changes what a *valid* value makes this port do, and that
//! is deliberate: the settings these keys steer are read again, later and
//! elsewhere, by whichever verb needs them (`core.ignorecase` by the status walk,
//! `core.abbrev` by [`crate::abbrev`], `core.autocrlf` by the filter pipeline,
//! `push.default` by `push`). What this module adds is git's *refusal*, at git's
//! position in the run. The two exceptions that resolve a value callers can ask
//! for are [`DefaultConfig::object_creation_mode`] and
//! [`DefaultConfig::sparse_expect_files_outside_of_patterns`]; see their docs for
//! why neither has an observable effect on this substrate.
//!
//! # The three keys deliberately left out
//!
//! `core.fsync`, `core.fsyncMethod` and `core.fsyncObjectFiles` belong to
//! `git_default_core_config` (environment.c:475-501) but are **not** validated
//! here. This port already reads all three — with git's exact
//! `ignoring unknown core.fsync component`, `ignoring unknown core.fsyncMethod
//! value` and `core.fsyncObjectFiles is deprecated` diagnostics — in
//! [`crate::config::FsyncPolicy`], at the point an index or object file is about
//! to be written rather than at parse time. Validating them here as well would
//! print each warning twice for any verb that reaches both. The cost of leaving
//! them is that a command which never writes anything does not report them at
//! all, where git does; the cost of moving them would be a duplicated warning on
//! every write, which is worse. This is a known, measured gap, not an oversight.

use crate::config::{ConfigValue, walk_config};

/// `enum object_creation_mode` (`environment.h`): how a finished loose object is
/// moved from its temporary file to its final name.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ObjectCreationMode {
    /// `OBJECT_CREATION_USES_RENAMES` — git's default.
    Renames,
    /// `OBJECT_CREATION_USES_HARDLINKS` — `core.createObject = link`.
    Hardlinks,
}

/// The resolved values, for callers that want them rather than just the check.
#[derive(Copy, Clone, Debug)]
pub struct DefaultConfig {
    /// `core.createObject`.
    ///
    /// It picks how a finished loose object is moved into place: `rename` (the
    /// default) or `link`, i.e. `link()` + `unlink()` for filesystems whose
    /// `rename()` cannot be trusted to be atomic (`object-file.c`'s
    /// `finalize_object_file`, reading `object_creation_mode`). gitoxide writes a
    /// loose object through `tempfile::NamedTempFile::persist`
    /// (`gix-odb/src/store_impls/loose/write.rs`), which is `rename(2)` and has no
    /// hard-link variant to select — both modes produce the same file with the
    /// same bytes, so the mode is resolved and rejected here and nowhere else.
    pub object_creation_mode: ObjectCreationMode,
    /// `sparse.expectFilesOutsideOfPatterns`.
    ///
    /// It turns *off* the scan `repo_read_index()` runs over a sparse index
    /// (`repository.c:458` → `clear_skip_worktree_from_present_files`,
    /// sparse-index.c:673-685): for every entry carrying `SKIP_WORKTREE`, git
    /// `lstat`s the path and clears the bit if the file is actually there. This
    /// port never runs that scan, so it already behaves as if the key were
    /// `true`; that is a pre-existing gap in the sparse-checkout port, and what
    /// this reader adds is git's rejection of an unreadable value.
    pub sparse_expect_files_outside_of_patterns: bool,
}

/// How git refuses one of these keys.
pub enum Rejection {
    /// A single `die()` line, with no origin clause.
    Die(String),
    /// A refusal that already wrote every one of its own lines to stderr and
    /// needs nothing more printed — `format.noprefix`'s `die_message()` plus
    /// `advise()` (builtin/log.c:1109-1122) is the one arm shaped that way.
    Silent,
    /// One or more `error()` lines, then `git_die_config_linenr()`.
    Reported {
        /// The `error:` texts, in the order the C arm emits them. `push.default`
        /// is the one arm that emits two.
        errors: Vec<String>,
        /// The `fatal:` clause `git_die_config_linenr()` builds from the origin.
        fatal: String,
    },
}

impl Rejection {
    /// Print whatever precedes the `fatal:` line and return the message the
    /// caller should die with.
    pub fn into_fatal(self) -> String {
        match self {
            Rejection::Die(msg) => msg,
            Rejection::Silent => String::new(),
            Rejection::Reported { errors, fatal } => {
                for line in errors {
                    eprintln!("error: {line}");
                }
                fatal
            }
        }
    }
}

impl Rejection {
    /// The error the dispatcher returns: git's `die()` for the two speaking
    /// shapes, and a bare exit code for the arm that already printed everything
    /// it had to say.
    pub fn into_error(self) -> anyhow::Error {
        match self {
            Rejection::Silent => anyhow::Error::new(crate::fatal::Silent(crate::fatal::EXIT_FATAL)),
            other => crate::fatal::die(other.into_fatal()),
        }
    }
}

/// `git_default_config()` over every configured value, in parse order.
///
/// `Ok` carries the values the last winning occurrence of each resolved key left
/// behind; `Err` is the first refusal, which is where git stops.
pub fn validate(repo: &gix::Repository) -> Result<DefaultConfig, Rejection> {
    let mut resolved = DefaultConfig {
        object_creation_mode: ObjectCreationMode::Renames,
        sparse_expect_files_outside_of_patterns: false,
    };
    for value in walk_config(repo) {
        git_default_config(&value, &mut resolved)?;
    }
    Ok(resolved)
}

/// The `if (starts_with(...))` chain of `git_default_config()` itself, for one
/// value.
///
/// Exposed to the crate because it is the *tail* of every other config callback
/// in git: `git_diff_basic_config` ends `return git_default_config(...)`
/// (diff.c:562), and `git_status_config` reaches it through the diff chain. A
/// command that installs one of those callbacks must therefore run the whole
/// composed chain over each value in turn rather than run two independent walks —
/// otherwise a `diff.*` refusal and a `core.*` refusal in the same config would
/// report in the wrong order. See [`crate::diff_config`].
pub(crate) fn git_default_config(v: &ConfigValue, out: &mut DefaultConfig) -> Result<(), Rejection> {
    let key = v.key.as_str();
    if let Some(rest) = key.strip_prefix("core.") {
        return core_config(v, rest, out);
    }
    if key.starts_with("user.") || key.starts_with("author.") || key.starts_with("committer.") {
        return ident_config(v, key);
    }
    if key.starts_with("i18n.") {
        return i18n_config(v, key);
    }
    if key.starts_with("branch.") {
        return branch_config(v, key);
    }
    if key.starts_with("push.") {
        return push_config(v, key);
    }
    if key.starts_with("attr.") {
        // environment.c:645-656, `git_default_attr_config()`: `attr.tree` is the
        // only key, and it is a plain string.
        if key == "attr.tree" {
            string_value(v)?;
        }
        return Ok(());
    }
    if key.starts_with("advice.") || key.starts_with("color.advice") {
        return advice_config(v, key);
    }
    // environment.c:687-690. Two spellings of one setting, both plain booleans.
    if key == "pager.color" || key == "color.pager" {
        bool_value(v, key)?;
        return Ok(());
    }
    // environment.c:692-695.
    if key == "pack.packsizelimit" {
        ulong_value(v, key)?;
        return Ok(());
    }
    // environment.c:697-706 — `git_config_int` and then the same zlib range check
    // `core.compression` runs, with `pack` in the message instead.
    if key == "pack.compression" {
        let level = int_value(v, key)?;
        check_zlib_level(level, "bad pack compression level")?;
        return Ok(());
    }
    if key.starts_with("sparse.") {
        return sparse_config(v, key, out);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// git_default_core_config() — environment.c:293-546
// ---------------------------------------------------------------------------

/// The `core.*` arms, in the order the C chain tests them.
///
/// `name` is the key with `core.` stripped, which is what the `strcmp` chain is
/// really comparing; `v.key` is passed on to the diagnostics because git spells
/// the whole dotted name there.
fn core_config(v: &ConfigValue, name: &str, out: &mut DefaultConfig) -> Result<(), Rejection> {
    let key = v.key.as_str();
    match name {
        // environment.c:299-346, 496-546 — the plain booleans. Every one is
        // `git_config_bool(var, value)`, so a value neither the word nor the
        // integer grammar reads dies with `bad boolean config value`.
        "filemode" | "trustctime" | "quotepath" | "symlinks" | "ignorecase" | "bare"
        | "ignorestat" | "lockfilepid" | "sparsecheckout" | "sparsecheckoutcone"
        | "precomposeunicode" | "protecthfs" | "protectntfs" => {
            bool_value(v, key)?;
        }

        // environment.c:307-317:
        //
        //     if (!value) return config_error_nonbool(var);
        //     if (!strcasecmp(value, "default")) …
        //     else if (!strcasecmp(value, "minimal")) …
        //     else return error(_("invalid value for '%s': '%s'"), var, value);
        //
        // Note the missing `return 0` after the arm — a valid value falls through
        // the rest of the chain, which matches nothing and ends at
        // `platform_core_config`, a no-op off Windows. Harmless, and reproduced by
        // simply not returning early either.
        "checkstat" => {
            let raw = string_value(v)?;
            if !raw.eq_ignore_ascii_case("default") && !raw.eq_ignore_ascii_case("minimal") {
                return Err(reported(
                    v,
                    vec![format!("invalid value for '{key}': '{raw}'")],
                ));
            }
        }

        // environment.c:349-363:
        //
        //     if (!value) return config_error_nonbool(var);
        //     if (!strcasecmp(value, "auto")) default_abbrev = -1;
        //     else if (!git_parse_maybe_bool_text(value)) default_abbrev = GIT_MAX_HEXSZ;
        //     else {
        //             int abbrev = git_config_int(var, value, ctx->kvi);
        //             if (abbrev < minimum_abbrev)
        //                     return error(_("abbrev length out of range: %d"), abbrev);
        //             default_abbrev = abbrev;
        //     }
        //
        // The middle branch is the *text* boolean only, and only its false half:
        // `git_parse_maybe_bool_text` answers 0 for `false`/`no`/`off`/``, 1 for
        // `true`/`yes`/`on` and -1 for anything else, and `!1` and `!-1` are both
        // 0 — so `true` falls through to the integer reader and dies with
        // `bad numeric config value 'true' for 'core.abbrev': invalid unit`, while
        // `no` means the full hash. Both verified against git 2.55.0.
        "abbrev" => {
            let raw = string_value(v)?;
            if !raw.eq_ignore_ascii_case("auto") && crate::optint::maybe_bool_text(&raw) != Some(false)
            {
                let abbrev = int_value(v, key)?;
                if abbrev < crate::abbrev::MINIMUM_ABBREV as i64 {
                    return Err(reported(
                        v,
                        vec![format!("abbrev length out of range: {abbrev}")],
                    ));
                }
            }
        }

        // environment.c:365-366 → object-name.c:204-230, `set_disambiguate_hint_config()`.
        // The comparison is `strcasecmp` against a six-entry table.
        "disambiguate" => {
            let raw = string_value(v)?;
            const HINTS: [&str; 6] = ["none", "commit", "committish", "tree", "treeish", "blob"];
            if !HINTS.iter().any(|h| raw.eq_ignore_ascii_case(h)) {
                return Err(reported(
                    v,
                    vec![format!("unknown hint type for '{key}': {raw}")],
                ));
            }
        }

        // environment.c:368-390. Both keys read an int and then range-check it as
        // a zlib level, with `-1` standing for `Z_DEFAULT_COMPRESSION` and so
        // exempt. `core.compression` seeds the pack level too, but the check and
        // the message are identical.
        "loosecompression" | "compression" => {
            let level = int_value(v, key)?;
            check_zlib_level(level, "bad zlib compression level")?;
        }

        // environment.c:392-399 / 401-412. One extra word each, matched
        // case-insensitively, and everything else is a boolean.
        "autocrlf" => {
            if !v.value.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("input")) {
                bool_value(v, key)?;
            }
        }
        "safecrlf" => {
            if !v.value.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("warn")) {
                bool_value(v, key)?;
            }
        }

        // environment.c:413-423: `lf`/`crlf`/`native` and *anything else* is
        // `EOL_UNSET`. There is no rejection here at all, which is why
        // `core.eol=bogus` runs clean under git.
        "eol" => {}

        // environment.c:425-433, 458-461 — `git_config_string`, so the only
        // refusal is the valueless spelling.
        "checkroundtripencoding" | "editor" | "askpass" => {
            string_value(v)?;
        }

        // environment.c:334-337, 463-466 — `git_config_pathname`, which is
        // `git_config_string` plus `interpolate_path()`; see [`expand_path`].
        "attributesfile" | "excludesfile" => {
            let raw = string_value(v)?;
            expand_path(&raw)?;
        }

        // environment.c:435-456. Two spellings of one setting; the message names
        // whichever the user wrote (normalised to lowercase by the parser).
        "commentchar" | "commentstring" => {
            let raw = string_value(v)?;
            if !raw.eq_ignore_ascii_case("auto") {
                if raw.is_empty() {
                    return Err(reported(
                        v,
                        vec![format!("{key} must have at least one character")],
                    ));
                }
                if raw.contains('\n') {
                    return Err(reported(v, vec![format!("{key} cannot contain newline")]));
                }
            }
        }

        // environment.c:468-473 → ws.c:32-80, `parse_whitespace_rule()`. An
        // unknown rule name is silently ignored, an out-of-range `tabwidth=`
        // warns, and only the one contradictory pair is fatal.
        "whitespace" => {
            let raw = string_value(v)?;
            parse_whitespace_rule(&raw)?;
        }

        // `core.createObject` — environment.c:508-518:
        //
        //     if (!value) return config_error_nonbool(var);
        //     if (!strcmp(value, "rename")) …
        //     else if (!strcmp(value, "link")) …
        //     else die(_("invalid mode for object creation: %s"), value);
        //
        // The two comparisons are `strcmp`, not `strcasecmp`: `Link` and `RENAME`
        // are invalid modes, not spellings of the valid ones.
        "createobject" => {
            let raw = string_value(v)?;
            out.object_creation_mode = match raw.as_str() {
                "rename" => ObjectCreationMode::Renames,
                "link" => ObjectCreationMode::Hardlinks,
                other => {
                    return Err(Rejection::Die(format!(
                        "invalid mode for object creation: {other}"
                    )))
                }
            };
        }

        // environment.c:475-501: read by `crate::config::FsyncPolicy` at the write
        // site instead — see the module header for why they are not here.
        "fsync" | "fsyncmethod" | "fsyncobjectfiles" => {}

        // Everything else falls through to `platform_core_config()`, which is a
        // no-op on every platform but Windows (`git-compat-util.h` defines it as
        // `noop_core_config` unless `compat/mingw.h` overrode it).
        _ => {}
    }
    Ok(())
}

/// `parse_whitespace_rule()` (ws.c:32-80), reduced to the two things it can say.
///
/// The rule names themselves are not validated — an unrecognised token is simply
/// skipped, which is why `core.whitespace=bogus` runs clean. What *can* stop a
/// command is the contradiction check at ws.c:78-79, and what can warn is a
/// `tabwidth=` outside `(0, 0100)`:
///
/// ```text
/// $ git -c core.whitespace=tab-in-indent,indent-with-non-tab status -s
/// fatal: cannot enforce both tab-in-indent and indent-with-non-tab
/// $ git -c core.whitespace=tabwidth=99 status -s
/// warning: tabwidth 99 out of range
/// ```
///
/// The warning prints the argument *as written*, from the `%.*s` that spans to
/// the next comma, so `tabwidth=99x` warns `tabwidth 99x out of range`.
fn parse_whitespace_rule(raw: &str) -> Result<(), Rejection> {
    let mut tab_in_indent = false;
    let mut indent_with_non_tab = false;
    for token in raw.split(',') {
        // `string + strspn(string, ", \t\n\r")`: the separators are stripped from
        // the front of every token, and a `-` prefix negates.
        let token = token.trim_matches([' ', '\t', '\n', '\r']);
        let (negated, name) = match token.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, token),
        };
        if name.is_empty() {
            continue;
        }
        // ws.c:54-63 uses `strncmp(rule_name, string, len)`, i.e. the configured
        // token is a *prefix* match against the table entry.
        match name {
            "tab-in-indent" => tab_in_indent = !negated,
            "indent-with-non-tab" => indent_with_non_tab = !negated,
            _ => {}
        }
        if let Some(arg) = name.strip_prefix("tabwidth=") {
            // `atoi`, so leading digits win and junk reads as 0.
            let width: u32 = arg
                .as_bytes()
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .fold(0u32, |acc, b| acc.saturating_mul(10) + u32::from(b - b'0'));
            if width == 0 || width >= 0o100 {
                eprintln!("warning: tabwidth {arg} out of range");
            }
        }
    }
    if tab_in_indent && indent_with_non_tab {
        return Err(Rejection::Die(
            "cannot enforce both tab-in-indent and indent-with-non-tab".to_string(),
        ));
    }
    Ok(())
}

/// `interpolate_path(value, 0)` (path.c), as far as `git_config_pathname` needs
/// it: the expansions that can *fail*, since a successful one lands in a variable
/// this port does not read.
///
/// ```c
/// if (path[0] == '~') {
///         const char *first_slash = strchrnul(path, '/');
///         size_t username_len = first_slash - (path + 1);
///         if (username_len == 0) {
///                 const char *home = getenv("HOME");
///                 if (!home) goto return_null;
///                 …
///         } else {
///                 struct passwd *pw = getpw_str(path + 1, username_len);
///                 if (!pw) goto return_null;
///                 …
///         }
/// }
/// ```
///
/// and `git_config_pathname` turns the `NULL` into
/// `die(_("failed to expand user dir in: '%s'"), value)`. Verified against git
/// 2.55.0: `-c core.excludesfile=~nosuchuser/x status -s` dies with exactly that.
///
/// `:(optional)` is stripped before interpolation (config.c:1315), and a
/// `%(prefix)/` path is resolved against the installation prefix rather than a
/// user directory, so neither can produce this failure.
pub(crate) fn expand_path(raw: &str) -> Result<(), Rejection> {
    let path = raw.strip_prefix(":(optional)").unwrap_or(raw);
    if path.starts_with("%(prefix)/") {
        return Ok(());
    }
    let Some(after_tilde) = path.strip_prefix('~') else {
        return Ok(());
    };
    let user = after_tilde.split('/').next().unwrap_or("");
    let ok = if user.is_empty() {
        std::env::var_os("HOME").is_some()
    } else {
        home_of(user).is_some()
    };
    if ok {
        return Ok(());
    }
    Err(Rejection::Die(format!(
        "failed to expand user dir in: '{path}'"
    )))
}

/// `getpw_str()`: whether `getpwnam(3)` knows this user.
fn home_of(user: &str) -> Option<()> {
    let name = std::ffi::CString::new(user).ok()?;
    // SAFETY: `getpwnam` takes a NUL-terminated string and returns a pointer into
    // a static buffer or NULL. Only nullness is inspected, so the buffer's
    // lifetime and the call's lack of thread safety do not matter here.
    let pw = unsafe { libc::getpwnam(name.as_ptr()) };
    (!pw.is_null()).then_some(())
}

/// The zlib range check both compression keys run (environment.c:371-374,
/// 382-385, 700-703): `-1` is `Z_DEFAULT_COMPRESSION` and exempt, anything
/// outside `[0, Z_BEST_COMPRESSION]` is fatal. `message` is `bad zlib
/// compression level` for the two `core.*` keys and `bad pack compression level`
/// for `pack.compression`.
fn check_zlib_level(level: i64, message: &str) -> Result<(), Rejection> {
    /// `Z_BEST_COMPRESSION` (zlib.h).
    const Z_BEST_COMPRESSION: i64 = 9;
    if level == -1 || (0..=Z_BEST_COMPRESSION).contains(&level) {
        return Ok(());
    }
    Err(Rejection::Die(format!("{message} {level}")))
}

// ---------------------------------------------------------------------------
// git_ident_config() / set_ident() — ident.c:610-687
// ---------------------------------------------------------------------------

/// `user.*`, `author.*` and `committer.*`.
///
/// `user.useconfigonly` is the one boolean (ident.c:681-684); the four identity
/// strings and their two `user.` spellings are plain strings, so the only refusal
/// is the valueless form. Every other key under these three sections falls off
/// the end of `set_ident()` and returns 0 — `user.signingKey` included, which is
/// read by `gpg-interface.c` rather than here.
fn ident_config(v: &ConfigValue, key: &str) -> Result<(), Rejection> {
    match key {
        "user.useconfigonly" => {
            bool_value(v, key)?;
        }
        "author.name" | "author.email" | "committer.name" | "committer.email" | "user.name"
        | "user.email" => {
            string_value(v)?;
        }
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// git_default_i18n_config() — environment.c:562-576
// ---------------------------------------------------------------------------

/// Both `i18n.*` keys are `git_config_string`.
fn i18n_config(v: &ConfigValue, key: &str) -> Result<(), Rejection> {
    if key == "i18n.commitencoding" || key == "i18n.logoutputencoding" {
        string_value(v)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// git_default_branch_config() — environment.c:578-614
// ---------------------------------------------------------------------------

/// The two `branch.*` keys `git_default_config` owns.
///
/// `branch.autoSetupMerge` takes three extra words — compared with `strcmp`, so
/// `Always` is not one of them — and falls back to a boolean:
///
/// ```text
/// $ git -c branch.autosetupmerge=Always status -s
/// fatal: bad boolean config value 'Always' for 'branch.autosetupmerge'
/// ```
///
/// `branch.autoSetupRebase` is a closed set with no boolean fallback, and its
/// `error()` names only the variable, not the value:
///
/// ```text
/// $ git -c branch.autosetuprebase=Never status -s
/// error: malformed value for branch.autosetuprebase
/// fatal: unable to parse 'branch.autosetuprebase' from command-line config
/// ```
///
/// Note that the valueless spelling reaches `config_error_nonbool` for
/// `autoSetupRebase` but *not* for `autoSetupMerge`, whose three `strcmp`s are
/// guarded by `value &&` and whose fallback `git_config_bool(var, NULL)` is true.
fn branch_config(v: &ConfigValue, key: &str) -> Result<(), Rejection> {
    match key {
        "branch.autosetupmerge" => {
            // The three `strcmp`s are guarded by `value &&`, so the valueless
            // spelling skips them and lands on `git_config_bool(var, NULL)`,
            // which is true rather than an error.
            let word = v.value.as_deref().unwrap_or("");
            if v.value.is_none() || !matches!(word, "always" | "inherit" | "simple") {
                bool_value(v, key)?;
            }
        }
        "branch.autosetuprebase" => {
            let raw = string_value(v)?;
            if !matches!(raw.as_str(), "never" | "local" | "remote" | "always") {
                return Err(reported(v, vec![format!("malformed value for {key}")]));
            }
        }
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// git_default_push_config() — environment.c:616-643
// ---------------------------------------------------------------------------

/// `push.default`: a closed set compared with `strcmp`, and the only arm in
/// `git_default_config` that emits **two** `error()` lines before the fatal.
///
/// ```text
/// $ git -c push.default=bogus status -s
/// error: malformed value for push.default: bogus
/// error: must be one of nothing, matching, simple, upstream or current
/// fatal: unable to parse 'push.default' from command-line config
/// ```
///
/// `tracking` is accepted as a deprecated spelling of `upstream` and says
/// nothing.
fn push_config(v: &ConfigValue, key: &str) -> Result<(), Rejection> {
    if key != "push.default" {
        return Ok(());
    }
    let raw = string_value(v)?;
    const MODES: [&str; 6] = [
        "nothing",
        "matching",
        "simple",
        "upstream",
        "tracking",
        "current",
    ];
    if MODES.contains(&raw.as_str()) {
        return Ok(());
    }
    Err(reported(
        v,
        vec![
            format!("malformed value for {key}: {raw}"),
            "must be one of nothing, matching, simple, upstream or current".to_string(),
        ],
    ))
}

// ---------------------------------------------------------------------------
// git_default_advice_config() — advice.c:162-192
// ---------------------------------------------------------------------------

/// `color.advice`, `color.advice.<slot>` and `advice.<slot>`.
///
/// * `color.advice` is `git_config_colorbool` (color.c:383-403): `never`,
///   `always` and `auto` are words, and everything else is handed to
///   `git_config_bool` and dies there.
/// * `color.advice.<slot>` is `color_parse`, but only for a slot the table
///   knows — `reset` and `hint`. An unknown slot returns 0 before the value is
///   even looked at, so `color.advice.nosuchslot=nosuchcolor` runs clean.
/// * `advice.<slot>` is `git_config_bool`, again only for a key in the table
///   (advice.c:47-94); an unknown `advice.*` key is silently ignored.
///
/// The last point is why [`ADVICE_KEYS`] has to be git's list exactly rather than
/// the subset [`crate::advice`] can print: git refuses a bad boolean for every
/// entry in it and for none outside it, and both halves of that are observable.
fn advice_config(v: &ConfigValue, key: &str) -> Result<(), Rejection> {
    if key == "color.advice" {
        // `git_config_colorbool` checks its three words with `strcasecmp` and
        // only then falls through to `git_config_bool`, which is what can die.
        // The words are guarded by `if (value)`, so the valueless spelling skips
        // them and reaches the boolean — where `NULL` is true, not an error.
        let word = v.value.as_deref().unwrap_or("");
        let is_word = ["never", "always", "auto"]
            .iter()
            .any(|w| v.value.is_some() && word.eq_ignore_ascii_case(w));
        if !is_word {
            bool_value(v, key)?;
        }
        return Ok(());
    }
    if let Some(slot) = key.strip_prefix("color.advice.") {
        if !slot.eq_ignore_ascii_case("reset") && !slot.eq_ignore_ascii_case("hint") {
            return Ok(());
        }
        let raw = string_value(v)?;
        if crate::porcelain::color::parse_color_spec(&raw).is_none() {
            return Err(reported(v, vec![format!("invalid color value: {raw}")]));
        }
        return Ok(());
    }
    if let Some(slot) = key.strip_prefix("advice.") {
        if ADVICE_KEYS.iter().any(|k| k.eq_ignore_ascii_case(slot)) {
            bool_value(v, key)?;
        }
    }
    Ok(())
}

/// `advice_setting[]` (advice.c:47-94) — every `advice.<slot>` git recognises,
/// spelled as the table spells them (the comparison is `strcasecmp`, so the case
/// here is documentation rather than behaviour).
///
/// A key outside this list is *not* validated: `advice.nosuchadvice=bogus` runs
/// clean under git 2.55.0, and so it must here.
const ADVICE_KEYS: &[&str] = &[
    "addEmbeddedRepo",
    "addEmptyPathspec",
    "addIgnoredFile",
    "ambiguousFetchRefspec",
    "amWorkDir",
    "checkoutAmbiguousRemoteBranchName",
    "commitBeforeMerge",
    "defaultBranchName",
    "detachedHead",
    "diverging",
    "fetchRemoteHEADWarn",
    "fetchShowForcedUpdates",
    "forceDeleteBranch",
    "graftFileDeprecated",
    "ignoredHook",
    "implicitIdentity",
    "mergeConflict",
    "nestedTag",
    "objectNameWarning",
    "pushAlreadyExists",
    "pushFetchFirst",
    "pushNeedsForce",
    "pushNonFFCurrent",
    "pushNonFFMatching",
    "pushRefNeedsUpdate",
    "pushUnqualifiedRefName",
    "pushUpdateRejected",
    "pushNonFastForward",
    "rebaseTodoError",
    "refSyntax",
    "resetNoRefresh",
    "resolveConflict",
    "rmHints",
    "sequencerInUse",
    "setUpstreamFailure",
    "skippedCherryPicks",
    "sparseIndexExpanded",
    "statusAheadBehindWarning",
    "statusHints",
    "statusUoption",
    "submodulesNotUpdated",
    "submoduleAlternateErrorStrategyDie",
    "submoduleMergeConflict",
    "suggestDetachingHead",
    "updateSparsePath",
    "waitingForEditor",
    "worktreeAddOrphan",
];

// ---------------------------------------------------------------------------
// git_default_sparse_config() — environment.c:549-559
// ---------------------------------------------------------------------------

/// `sparse.expectFilesOutsideOfPatterns`, the section's only key.
fn sparse_config(v: &ConfigValue, key: &str, out: &mut DefaultConfig) -> Result<(), Rejection> {
    if key == "sparse.expectfilesoutsideofpatterns" {
        out.sparse_expect_files_outside_of_patterns = bool_value(v, key)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The four value readers, and the two refusal shapes they raise
// ---------------------------------------------------------------------------

/// `git_config_bool(var, value)` (config.c:1292-1298).
///
/// The valueless spelling is git's `NULL`, and `git_parse_maybe_bool_text`
/// answers **1** for it before it looks at any text (parse.c:168-169) — a
/// different answer from the empty string's 0. Everything else goes through the
/// word grammar and then the integer grammar, so `0x10` is true and `1k` is true.
fn bool_value(v: &ConfigValue, key: &str) -> Result<bool, Rejection> {
    let Some(raw) = v.value.as_deref() else {
        return Ok(true);
    };
    crate::optint::maybe_bool(raw).ok_or_else(|| {
        Rejection::Die(format!("bad boolean config value '{raw}' for '{key}'"))
    })
}

/// `git_config_string(&dest, var, value)` (config.c:1300-1306): the value itself,
/// or `config_error_nonbool` for the valueless spelling.
fn string_value(v: &ConfigValue) -> Result<String, Rejection> {
    match v.value.as_deref() {
        Some(raw) => Ok(raw.to_string()),
        None => Err(nonbool(v)),
    }
}

/// `git_config_int(var, value, kvi)` (config.c:1226-1232), dying through
/// `die_bad_number` with the ` in file <path>` clause a file-backed value earns.
///
/// A valueless key reaches this as C's `NULL`, which `git_parse_signed` rejects
/// at once and `die_bad_number` renders as the empty string — the arms that would
/// mind have already called [`string_value`] first.
fn int_value(v: &ConfigValue, key: &str) -> Result<i64, Rejection> {
    let raw = v.value.as_deref().unwrap_or("");
    crate::config::parse_config_int(raw).map_err(|reason| bad_number(v, key, raw, reason))
}

/// `git_config_ulong(var, value, kvi)` (config.c:1253-1259), the unsigned
/// counterpart of [`int_value`].
fn ulong_value(v: &ConfigValue, key: &str) -> Result<u64, Rejection> {
    let raw = v.value.as_deref().unwrap_or("");
    crate::config::parse_config_ulong(raw).map_err(|reason| bad_number(v, key, raw, reason))
}

/// `die_bad_number()` (config.c:1188-1223), minus the `fatal: ` prefix.
fn bad_number(v: &ConfigValue, key: &str, raw: &str, reason: &str) -> Rejection {
    Rejection::Die(format!(
        "bad numeric config value '{raw}' for '{key}'{}: {reason}",
        v.origin.bad_number_clause()
    ))
}

/// `config_error_nonbool()` (config.c:3552-3555) followed by the
/// `git_die_config_linenr()` the negative return earns.
fn nonbool(v: &ConfigValue) -> Rejection {
    reported(v, vec![format!("missing value for '{}'", v.key)])
}

/// Any `return error(...)` arm: the `error:` lines, then the origin-named fatal.
fn reported(v: &ConfigValue, errors: Vec<String>) -> Rejection {
    Rejection::Reported {
        errors,
        fatal: v.origin.die_linenr(&v.key),
    }
}
