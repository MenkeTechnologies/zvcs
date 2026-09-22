//! The diagnostics `pathspec.c` raises while parsing a pathspec element.
//!
//! git parses every pathspec on the command line before the command does any
//! work — `parse_pathspec()` walks the argument vector and `init_pathspec_item()`
//! reads each element's magic — and every way that can fail is a `die()`, so a
//! malformed pathspec is a `fatal:` and exit 128 for *every* verb that takes one.
//! The wording is part of that contract: scripts key on it, and the element is
//! quoted as the user wrote it rather than as the parser decomposed it.
//!
//! gitoxide reports the same failures through [`gix::pathspec::parse::Error`],
//! whose wording is its own. Mapping that enum back onto git's texts is the only
//! thing this module does, and it lives here rather than in each caller because
//! the mapping was previously copied into three commands and had already drifted:
//! `clean` rendered a trailing escape as `cannot use '\' for value matching`,
//! which is the *value* diagnostic, not the escape one.
//!
//! Every message below is quoted from git 2.55.0 `pathspec.c` at the cited line
//! and was read back off the binary before being written down.

use gix::bstr::{BStr, BString, ByteSlice};

/// `Invalid pathspec magic '%.*s' in '%s'` (pathspec.c:377) — a long-form keyword
/// that is not in git's `pathspec_magic` table.
pub fn invalid_magic(keyword: &BStr, elem: &BStr) -> String {
    format!("Invalid pathspec magic '{}' in '{}'", keyword.to_str_lossy(), elem.to_str_lossy())
}

/// `Missing ')' at the end of pathspec magic in '%s'` (pathspec.c:382) — a
/// long-form magic list that runs off the end of the element.
pub fn missing_closing_paren(elem: &BStr) -> String {
    format!("Missing ')' at the end of pathspec magic in '{}'", elem.to_str_lossy())
}

/// `Unimplemented pathspec magic '%c' in '%s'` (pathspec.c:420) — a short mnemonic
/// git reserves but has never assigned. Only `/`, `!` and its `^` alias are live.
pub fn unimplemented_magic(mnemonic: char, elem: &BStr) -> String {
    format!("Unimplemented pathspec magic '{mnemonic}' in '{}'", elem.to_str_lossy())
}

/// `%s: 'literal' and 'glob' are incompatible` (pathspec.c:479) — the one pair git
/// refuses, because each decides the same question (is `*` a wildcard) differently.
/// Named after the whole element, not after the keywords.
pub fn incompatible_literal_glob(elem: &BStr) -> String {
    format!("{}: 'literal' and 'glob' are incompatible", elem.to_str_lossy())
}

/// `%s: pathspec magic not supported by this command: %s` (pathspec.c:591) — magic
/// that parsed fine but is outside the mask the verb passed to `parse_pathspec()`.
/// `magic` is git's space-separated list of the offending keyword names.
pub fn magic_not_supported(elem: &BStr, magic: &str) -> String {
    format!(
        "{}: pathspec magic not supported by this command: {magic}",
        elem.to_str_lossy()
    )
}

/// `invalid attribute name %s` (pathspec.c:244) — an `attr:` entry whose name is
/// not a valid attribute. git names it bare, without quotes.
pub fn invalid_attribute_name(name: &BStr) -> String {
    format!("invalid attribute name {}", name.to_str_lossy())
}

/// `cannot use '%c' for value matching` (pathspec.c:186) — an `attr:<n>=<v>` value
/// byte outside the alphanumeric/`,-_` set.
pub fn invalid_attribute_value_char(character: char) -> String {
    format!("cannot use '{character}' for value matching")
}

/// `Escape character '\' not allowed as last character in attr value`
/// (pathspec.c:181-182) — a value ending in a backslash with nothing to escape.
pub fn trailing_escape_in_attr_value() -> String {
    r"Escape character '\' not allowed as last character in attr value".to_string()
}

/// `attr spec must not be empty` (pathspec.c:202) — `:(attr:)`.
pub fn empty_attr_spec() -> String {
    "attr spec must not be empty".to_string()
}

/// `Only one 'attr:' specification is allowed.` (pathspec.c:199) — including the
/// full stop, which git's string carries and no other message in this set does.
pub fn multiple_attr_specs() -> String {
    "Only one 'attr:' specification is allowed.".to_string()
}

/// `invalid parameter for pathspec magic 'prefix'` (pathspec.c:356) — the bytes
/// after `prefix:` that `strtol()` did not consume in full.
pub fn invalid_prefix_parameter() -> String {
    "invalid parameter for pathspec magic 'prefix'".to_string()
}

/// `empty string is not a valid pathspec. please use . instead if you meant to
/// match all paths` (pathspec.c:640). Checked over the whole argument vector
/// before any element is parsed, and left untranslated in git.
pub fn empty_pathspec() -> String {
    "empty string is not a valid pathspec. \
     please use . instead if you meant to match all paths"
        .to_string()
}

/// The `fatal:` body git prints for a pathspec gitoxide refused to parse.
///
/// The two parsers do not reject exactly the same inputs, and the difference runs
/// one way only for every variant below: gitoxide's short-magic table is narrower
/// than git's `is_pathspec_magic()`, and its `Missing ')'` search looks for the
/// paren anywhere in the element rather than before the end of the magic, so each
/// accepts specs the other rejects — but nothing gitoxide rejects here is
/// something git accepts. That is what makes translating rather than gating
/// correct: a spec that reaches this function is one git would have died on too.
///
/// `elem` is the element as written, which is what git quotes; the parser's own
/// view of it (magic stripped, escapes resolved) is never what appears in the
/// message.
pub fn parse_error_message(elem: &BStr, err: &gix::pathspec::parse::Error) -> String {
    use gix::pathspec::parse::Error as E;
    match err {
        E::EmptyString => empty_pathspec(),
        E::InvalidKeyword { keyword } => invalid_magic(keyword.as_bstr(), elem),
        E::Unimplemented { short_keyword } => unimplemented_magic(*short_keyword, elem),
        E::MissingClosingParenthesis => missing_closing_paren(elem),
        E::InvalidAttribute { attribute } => invalid_attribute_name(attribute.as_bstr()),
        E::InvalidAttributeValue { character } => invalid_attribute_value_char(*character),
        E::TrailingEscapeCharacter => trailing_escape_in_attr_value(),
        E::EmptyAttribute => empty_attr_spec(),
        E::MultipleAttributeSpecifications => multiple_attr_specs(),
        E::IncompatibleSearchModes => incompatible_literal_glob(elem),
        E::InvalidPrefixParameter => invalid_prefix_parameter(),
    }
}

/// git's `parse_pathspec()` gate, for a verb that would otherwise meet a bad
/// element only once gitoxide is already matching with it.
///
/// Returns the `fatal:` body for the first element git would die on, in argument
/// order — git parses left to right and stops at the first failure — or `None`
/// when every element parses. Callers print `fatal: {msg}` and exit 128.
///
/// `defaults` must be the same `Defaults` the command's real matcher is built
/// with (`repo.pathspec_defaults_inherit_ignore_case()`, or
/// `Defaults::from_environment()` outside a repository). With
/// `GIT_LITERAL_PATHSPECS` set they carry `literal: true`, no element is parsed
/// for magic at all, and this gate correctly finds nothing to reject.
pub fn first_magic_fatal<S: AsRef<[u8]>>(
    specs: &[S],
    defaults: gix::pathspec::Defaults,
) -> Option<String> {
    specs.iter().find_map(|spec| {
        let elem: &BStr = spec.as_ref().as_bstr();
        gix::pathspec::parse(spec.as_ref(), defaults)
            .err()
            .map(|err| parse_error_message(elem, &err))
    })
}

/// `init_pathspec_magic()`'s two `die()`s (`pathspec.c`), which fire before any
/// element is looked at.
///
/// ```c
/// literal_global = git_env_bool(GIT_LITERAL_PATHSPECS_ENVIRONMENT, 0);
/// glob_global    = git_env_bool(GIT_GLOB_PATHSPECS_ENVIRONMENT, 0);
/// noglob_global  = git_env_bool(GIT_NOGLOB_PATHSPECS_ENVIRONMENT, 0);
/// icase_global   = git_env_bool(GIT_ICASE_PATHSPECS_ENVIRONMENT, 0);
/// if (literal_global && (glob_global || noglob_global || icase_global))
///         die(_("global 'literal' pathspec setting is incompatible "
///               "with all other global pathspec settings"));
/// if (glob_global && noglob_global)
///         die(_("global 'glob' and 'noglob' pathspec settings are incompatible"));
/// ```
///
/// The four variables are how git carries `--literal-pathspecs`,
/// `--glob-pathspecs`, `--noglob-pathspecs` and `--icase-pathspecs` from the
/// command line as well, so this gate covers both spellings. `git_env_bool` reads
/// the *value*, so a variable set to `0`, `false` or the empty string is off and
/// conflicts with nothing.
///
/// Returns the `fatal:` body, in git's order, or `None`. Callers print
/// `fatal: {msg}` and exit 128.
pub fn global_magic_fatal() -> Option<String> {
    // `git_env_bool()` (parse.c:197-208), not gitoxide's boolean: git's grammar
    // has the base-0 integer fallback `git_parse_maybe_bool()` provides, so
    // `0x10` and `1k` are true, and a value that is neither a word nor an
    // integer is `fatal: bad boolean environment value '<v>' for '<k>'` rather
    // than a silent false.
    let env_bool = |name: &str| crate::setup::git_env_bool(name, false);
    let literal = env_bool("GIT_LITERAL_PATHSPECS");
    let glob = env_bool("GIT_GLOB_PATHSPECS");
    let noglob = env_bool("GIT_NOGLOB_PATHSPECS");
    let icase = env_bool("GIT_ICASE_PATHSPECS");
    if literal && (glob || noglob || icase) {
        return Some(
            "global 'literal' pathspec setting is incompatible with all other global pathspec \
             settings"
                .into(),
        );
    }
    if glob && noglob {
        return Some("global 'glob' and 'noglob' pathspec settings are incompatible".into());
    }
    None
}

/// `init_pathspec_item()`'s *other* `die()` — the one that fires after the magic
/// parsed cleanly and the path itself turned out to point out of the repository
/// (`pathspec.c:489-502`):
///
/// ```c
/// match = prefix_path_gently(the_repository, prefix, prefixlen,
///                            &prefixlen, copyfrom);
/// if (!match) {
///         const char *hint_path;
///
///         if ((flags & PATHSPEC_NO_REPOSITORY) || !have_git_dir())
///                 die(_("'%s' is outside the directory tree"), copyfrom);
///         hint_path = repo_get_work_tree(the_repository);
///         if (!hint_path)
///                 hint_path = repo_get_git_dir(the_repository);
///         die(_("%s: '%s' is outside repository at '%s'"), elt,
///             copyfrom, absolute_path(hint_path));
/// }
/// ```
///
/// Two operands, and they are not the same string: `elt` is the element as
/// written, magic and all, while `copyfrom` is the path left after the magic was
/// stripped. For a bare `..` they coincide, which is exactly why that shape is a
/// poor test of the rendering and `:(icase)../x` is the one that separates them.
///
/// `hint_path` is the working tree, and git prints it through `absolute_path()`
/// — but the path it holds came from `setup_work_tree()`'s `xgetcwd()`, so
/// symlinks are already resolved and `/var/…` appears as `/private/var/…` on
/// macOS. Resolving it here is what reproduces that.
///
/// This is a *separate* gate from [`first_magic_fatal`] because git raises it at
/// a different point and with a different message; a caller that takes a
/// pathspec runs both, in this order, before it does any work. Returns the
/// `fatal:` body for the first element git would die on, in argument order.
pub fn first_outside_repository_fatal<S: AsRef<[u8]>>(
    repo: &gix::Repository,
    specs: &[S],
    defaults: gix::pathspec::Defaults,
) -> Option<String> {
    let Some(workdir) = repo.workdir() else {
        return bare_outside_repository_fatal(repo, specs, defaults);
    };
    let root = gix::path::realpath(workdir).unwrap_or_else(|_| workdir.to_owned());
    // `prefix_path_gently()` is handed `revs->prefix`, the CWD as seen from the
    // working tree. Outside one there is nothing to be outside *of*.
    let prefix = repo.prefix().ok().flatten().unwrap_or_else(|| std::path::Path::new("")).to_owned();
    specs.iter().find_map(|spec| {
        let elt: &BStr = spec.as_ref().as_bstr();
        let mut pattern = gix::pathspec::parse(spec.as_ref(), defaults).ok()?;
        // `copyfrom`: the path with the magic already taken off.
        let copyfrom = pattern.path().to_owned();
        pattern.normalize(&prefix, &root).err().map(|_| {
            format!(
                "{}: '{}' is outside repository at '{}'",
                elt.to_str_lossy(),
                copyfrom.to_str_lossy(),
                root.display()
            )
        })
    })
}

/// [`first_outside_repository_fatal`] in a repository without a working tree.
///
/// `prefix_path_gently()` still runs there (setup.c:120-147) with a `NULL`
/// prefix: a relative element fails only when `normalize_path_copy_len()` climbs
/// above the top, and an absolute one always fails, because
/// `abspath_part_inside_repo()` returns -1 when `repo_get_work_tree()` is `NULL`
/// (setup.c:56-60). The hint then falls back to `repo_get_git_dir()` and goes
/// through `absolute_path()` unnormalised, so standing in the bare repository
/// itself — where the stored git dir is `.` — prints `<cwd>/.`.
fn bare_outside_repository_fatal<S: AsRef<[u8]>>(
    repo: &gix::Repository,
    specs: &[S],
    defaults: gix::pathspec::Defaults,
) -> Option<String> {
    let bad = specs.iter().find_map(|spec| {
        let mut pattern = gix::pathspec::parse(spec.as_ref(), defaults).ok()?;
        let copyfrom = pattern.path().to_owned();
        let outside = gix::path::is_absolute(gix::path::from_bstr(copyfrom.as_bstr()))
            || pattern.normalize(std::path::Path::new(""), std::path::Path::new("")).is_err();
        outside.then(|| (spec.as_ref().as_bstr().to_owned(), copyfrom))
    })?;
    let hint = absolute_path(&crate::porcelain::rev_parse::repo_get_git_dir(repo));
    Some(format!(
        "{}: '{}' is outside repository at '{}'",
        bad.0.to_str_lossy(),
        bad.1.to_str_lossy(),
        hint.display()
    ))
}

/// `strbuf_add_absolute_path()` (abspath.c): a relative path is appended to the
/// current directory — spelled as `$PWD` when that names the same directory as
/// `getcwd()` — with one `/` between, and nothing is normalised.
fn absolute_path(path: &std::path::Path) -> std::path::PathBuf {
    if path.is_absolute() {
        return path.to_owned();
    }
    let Ok(cwd) = std::env::current_dir() else {
        return path.to_owned();
    };
    let base = match std::env::var_os("PWD").map(std::path::PathBuf::from) {
        Some(pwd) if pwd != cwd && same_inode(&pwd, &cwd) => pwd,
        _ => cwd,
    };
    let mut out = base.into_os_string();
    if !out.as_encoded_bytes().ends_with(b"/") {
        out.push("/");
    }
    out.push(path.as_os_str());
    out.into()
}

fn same_inode(a: &std::path::Path, b: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// Both of `init_pathspec_item()`'s `die()`s, in git's order, for a command that
/// has finished collecting its pathspec list.
///
/// git runs `parse_pathspec()` once, inside `setup_revisions()`, over the whole
/// list and before the command does any work — so a bad element is fatal even on
/// a code path that would never have consulted the set. A port that instead
/// waits for its matcher to be built inherits gitoxide's wording and exit 1, and
/// misses the check entirely wherever it builds no matcher at all: `git diff`
/// hands its patterns straight to the index/worktree status iterator, so
/// `git diff -- ..` reported gitoxide's `Could not obtain the repository prefix`
/// where git reports `fatal: ..: '..' is outside repository at '<worktree>'`.
///
/// Returns the `fatal:` body for the first element git would die on. Callers
/// print `fatal: {msg}` and exit 128.
pub fn parse_pathspec_fatal<S: AsRef<[u8]>>(
    repo: &gix::Repository,
    specs: &[S],
) -> Option<String> {
    parse_pathspec_fatal_masked(repo, specs, 0)
}

/// [`parse_pathspec_fatal`] for a verb that passes a non-zero `magic_mask`.
///
/// git interleaves the three `die()`s *per element* (pathspec.c:650-668):
/// `init_pathspec_item()` parses element `i`'s magic and then resolves its path,
/// and only then is element `i`'s magic measured against the mask. So a single
/// element that is both outside the repository and carries unsupported magic
/// reports the path, while a later element's bad magic never masks an earlier
/// element's bad path. A gate that ran the three checks as three passes over the
/// list would get both of those backwards.
///
/// `magic_mask` is the set of bits the verb does *not* accept — git's own
/// spelling. Almost every verb passes `0`; `ls-tree` and `check-ignore` are the
/// two in-tree exceptions.
pub fn parse_pathspec_fatal_masked<S: AsRef<[u8]>>(
    repo: &gix::Repository,
    specs: &[S],
    magic_mask: u32,
) -> Option<String> {
    if specs.is_empty() {
        return None;
    }
    if let Some(msg) = empty_element_fatal(specs) {
        return Some(msg);
    }
    let defaults = repo.pathspec_defaults_inherit_ignore_case(false).ok()?;
    specs.iter().find_map(|spec| {
        let elt: &BStr = spec.as_ref().as_bstr();
        let element = match parse_element_magic(elt) {
            Ok(element) => element,
            Err(msg) => return Some(msg),
        };
        // `:(top)` and `:(prefix:<n>)` take `copyfrom` verbatim and never reach
        // `prefix_path_gently()` (pathspec.c:482-487), so neither can be "outside
        // repository": `git status -- ':(top)../x'` is a no-op match, not a fatal.
        if !element.rooted() {
            if let Some(msg) =
                first_outside_repository_fatal(repo, std::slice::from_ref(spec), defaults)
            {
                return Some(msg);
            }
        }
        let bad = element.magic & magic_mask;
        (bad != 0).then(|| unsupported_magic(elt, bad))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gix::pathspec::parse::Error as E;

    /// One case per `die()` in `pathspec.c`, verbatim. These strings are the
    /// interface: a verb that renders one differently is the bug, not the test.
    #[test]
    fn renders_every_pathspec_c_diagnostic() {
        assert_eq!(
            parse_error_message(":(icase".into(), &E::MissingClosingParenthesis),
            "Missing ')' at the end of pathspec magic in ':(icase'"
        );
        assert_eq!(
            parse_error_message(
                ":(bogus)x".into(),
                &E::InvalidKeyword { keyword: "bogus".into() }
            ),
            "Invalid pathspec magic 'bogus' in ':(bogus)x'"
        );
        assert_eq!(
            parse_error_message(":%x".into(), &E::Unimplemented { short_keyword: '%' }),
            "Unimplemented pathspec magic '%' in ':%x'"
        );
        assert_eq!(
            parse_error_message(":(icase,literal,glob)x".into(), &E::IncompatibleSearchModes),
            ":(icase,literal,glob)x: 'literal' and 'glob' are incompatible"
        );
        assert_eq!(
            parse_error_message(":(attr:)x".into(), &E::EmptyAttribute),
            "attr spec must not be empty"
        );
        assert_eq!(
            parse_error_message(":(attr:a,attr:b)x".into(), &E::MultipleAttributeSpecifications),
            "Only one 'attr:' specification is allowed."
        );
        assert_eq!(
            parse_error_message(
                ":(attr:\u{e9})x".into(),
                &E::InvalidAttribute { attribute: "\u{e9}".into() }
            ),
            "invalid attribute name \u{e9}",
            "git names the attribute bare — gitoxide's own text quotes it"
        );
        assert_eq!(
            parse_error_message(":(attr:a=b*c)x".into(), &E::InvalidAttributeValue { character: '*' }),
            "cannot use '*' for value matching"
        );
        assert_eq!(
            parse_error_message(":(attr:x=y\\".into(), &E::TrailingEscapeCharacter),
            r"Escape character '\' not allowed as last character in attr value",
            "the trailing-escape die is its own message, not the value one"
        );
        assert_eq!(
            parse_error_message("".into(), &E::EmptyString),
            "empty string is not a valid pathspec. \
             please use . instead if you meant to match all paths"
        );
        // The mask rejection has no gitoxide error to map from — `ls-tree` reaches
        // it after a successful parse — so it is only reachable as a constructor.
        assert_eq!(
            magic_not_supported(":(icase)x".into(), "icase"),
            ":(icase)x: pathspec magic not supported by this command: icase"
        );
    }

    /// `normalize_path_copy_len()`'s answers, including the `-1` that becomes the
    /// "is outside repository" die. The `None` cases are the whole reason this is
    /// a port rather than a `Path::components()` fold — that clamps at the root
    /// and reports success for a path git refuses.
    #[test]
    fn normalize_path_folds_and_fails_where_git_does() {
        let n = |s: &str| normalize_path(s.into()).map(|b| b.to_string());
        assert_eq!(n("a/b/../c").as_deref(), Some("a/c"));
        assert_eq!(n("./a").as_deref(), Some("a"));
        assert_eq!(n("a//b").as_deref(), Some("a/b"));
        assert_eq!(n("a/.").as_deref(), Some("a/"), "a trailing '.' leaves the separator");
        assert_eq!(n("a/..").as_deref(), Some(""), "back to the top, but still inside");
        assert_eq!(n("").as_deref(), Some(""));
        assert_eq!(n(".").as_deref(), Some(""));
        // A trailing slash survives, which is what makes `git add dir/` mean a
        // directory to everything downstream.
        assert_eq!(n("dir/").as_deref(), Some("dir/"));
        // `..x` is an ordinary component, not two dots and a name.
        assert_eq!(n("..x/y").as_deref(), Some("..x/y"));
        // The failures: a relative path that climbs above its own start, and the
        // absolute `/..` that climbs above `/`.
        assert_eq!(n(".."), None);
        assert_eq!(n("../x"), None);
        assert_eq!(n("a/../../x"), None);
        assert_eq!(n("/.."), None);
        assert_eq!(n("/a/../b").as_deref(), Some("/b"));
    }

    /// `parse_element_magic()` against every shape in `pathspec.c`'s two magic
    /// grammars, checking both halves of its answer: the bits and where the path
    /// begins. A `path_start` one byte out re-emits the magic into the filename or
    /// eats the first character of it, and neither is visible in the bits alone.
    #[test]
    fn parse_element_magic_splits_both_grammars() {
        let parse = |s: &str| {
            let element = parse_element_magic(s.into()).expect(s);
            (element.magic, element.path(s.into()).to_string(), element.rooted())
        };
        assert_eq!(parse("README.md"), (0, "README.md".into(), false));
        assert_eq!(parse(":!src/x"), (MAGIC_EXCLUDE, "src/x".into(), false));
        assert_eq!(parse(":^src/x"), (MAGIC_EXCLUDE, "src/x".into(), false), "'^' aliases '!'");
        assert_eq!(parse(":/x"), (MAGIC_TOP, "x".into(), true));
        // `parse_short_magic` steps over the `:` that terminates the mnemonics,
        // which is what lets a path start with one of them.
        assert_eq!(parse("::x"), (0, "x".into(), false));
        assert_eq!(parse(":!:x"), (MAGIC_EXCLUDE, "x".into(), false));
        assert_eq!(parse(":(icase,glob)X"), (MAGIC_ICASE | MAGIC_GLOB, "X".into(), false));
        assert_eq!(parse(":(attr:lbl)x"), (MAGIC_ATTR, "x".into(), false));
        assert_eq!(parse(":(prefix:2)x"), (0, "x".into(), true), "prefix magic carries no bit");
        assert_eq!(parse(":(top)"), (MAGIC_TOP, String::new(), true));

        // And the refusals, each naming the element as written.
        let err = |s: &str| parse_element_magic(s.into()).unwrap_err();
        assert_eq!(err(":(bogus)x"), "Invalid pathspec magic 'bogus' in ':(bogus)x'");
        assert_eq!(err(":(icase"), "Missing ')' at the end of pathspec magic in ':(icase'");
        assert_eq!(err(":%x"), "Unimplemented pathspec magic '%' in ':%x'");
        assert_eq!(err(":(literal,glob)x"), ":(literal,glob)x: 'literal' and 'glob' are incompatible");
        assert_eq!(err(":(attr:)x"), "attr spec must not be empty");
        assert_eq!(err(":(attr:a,attr:b)x"), "Only one 'attr:' specification is allowed.");
        assert_eq!(err(":(attr:a=b*c)x"), "cannot use '*' for value matching");
        assert_eq!(err(":(prefix:z)x"), "invalid parameter for pathspec magic 'prefix'");
        // A space does not separate keywords — `strcspn_escaped(pos, ",)")` — so
        // this is one `attr:` whose body names the attribute `attr:b`, and `:` is
        // not a legal attribute-name byte.
        assert_eq!(err(":(attr:a attr:b)x"), "invalid attribute name attr:b");
    }

    /// `pathspec_magic_names()` walks `pathspec_magic[]`, not the user's keyword
    /// order, and spells out the mnemonic where the table has one.
    #[test]
    fn magic_names_follow_the_table_not_the_user() {
        assert_eq!(magic_names(MAGIC_ICASE | MAGIC_GLOB), "'glob', 'icase'");
        assert_eq!(magic_names(MAGIC_EXCLUDE), "'exclude' (mnemonic: '!')");
        assert_eq!(magic_names(MAGIC_TOP), "'top' (mnemonic: '/')");
        assert_eq!(magic_names(MAGIC_ATTR), "'attr'");
        assert_eq!(magic_names(0), "");
    }

    /// The gate stops at the first bad element and names *that* one, because
    /// git's parse loop dies on it before reaching the rest.
    #[test]
    fn gate_reports_the_first_failure_in_argument_order() {
        let defaults = gix::pathspec::Defaults::default();
        let specs = ["ok.txt".to_string(), ":(icase".to_string(), ":(bogus)y".to_string()];
        assert_eq!(
            first_magic_fatal(&specs, defaults).as_deref(),
            Some("Missing ')' at the end of pathspec magic in ':(icase'")
        );
        assert_eq!(first_magic_fatal(&["a.txt".to_string()], defaults), None);
        // `--literal-pathspecs` turns the magic off entirely; nothing is rejected.
        let literal = gix::pathspec::Defaults { literal: true, ..Default::default() };
        assert_eq!(first_magic_fatal(&[":(icase".to_string()], literal), None);
    }
}

// ---------------------------------------------------------------------------
// `parse_pathspec()` itself, ported rather than translated.
//
// Everything above maps gitoxide's parse errors onto git's wording, which is
// enough for a verb whose only question is "would git have died here". It is
// not enough for a verb that passes a `magic_mask`: `unsupported_magic()`
// (pathspec.c:581-593) names the *bits* git resolved, in `pathspec_magic[]`
// order, and it fires at a point in `parse_pathspec()`'s loop
// (pathspec.c:657-658) that sits after two other `die()`s. A verb that answers
// the mask question from its own reading of the element — `check-ignore` had
// one, `ls-tree` has another — gets the order, the spelling and the keyword
// validation wrong in the same three ways each time, because the information
// it needs is the parse result and it never parsed.
//
// So this is `parse_element_magic()` (pathspec.c:333-442), the attr body
// grammar it calls into (pathspec.c:165-255), `pathspec_magic_names()`
// (pathspec.c:563-579) and the loop that orders them (pathspec.c:637-668).
// ---------------------------------------------------------------------------

/// `PATHSPEC_FROMTOP`, `:(top)`, mnemonic `/`.
pub const MAGIC_TOP: u32 = 1 << 0;
/// `PATHSPEC_LITERAL`, `:(literal)`.
pub const MAGIC_LITERAL: u32 = 1 << 1;
/// `PATHSPEC_GLOB`, `:(glob)`.
pub const MAGIC_GLOB: u32 = 1 << 2;
/// `PATHSPEC_ICASE`, `:(icase)`.
pub const MAGIC_ICASE: u32 = 1 << 3;
/// `PATHSPEC_EXCLUDE`, `:(exclude)`, mnemonic `!` (and its `^` alias).
pub const MAGIC_EXCLUDE: u32 = 1 << 4;
/// `PATHSPEC_ATTR`, `:(attr:<spec>)`.
pub const MAGIC_ATTR: u32 = 1 << 5;

/// `PATHSPEC_ALL_MAGIC` (pathspec.h:14-21), the set a verb subtracts from to
/// spell "everything but these": `ls-tree` passes
/// `PATHSPEC_ALL_MAGIC & ~(PATHSPEC_FROMTOP | PATHSPEC_LITERAL)`
/// (builtin/ls-tree.c:420-423).
///
/// git's macro also carries `PATHSPEC_MAXDEPTH`, which no element can ask for —
/// `--max-depth` sets it on the parsed set afterwards — so it is left out here
/// rather than given a bit that nothing could ever raise.
pub const MAGIC_ALL: u32 =
    MAGIC_TOP | MAGIC_LITERAL | MAGIC_GLOB | MAGIC_ICASE | MAGIC_EXCLUDE | MAGIC_ATTR;

/// `pathspec_magic[]` (pathspec.c:101-112), in git's declaration order — which
/// is the order [`magic_names`] renders in and so is load-bearing, not a detail:
///
/// ```c
/// } pathspec_magic[] = {
///         { PATHSPEC_FROMTOP,  '/', "top" },
///         { PATHSPEC_LITERAL, '\0', "literal" },
///         { PATHSPEC_GLOB,    '\0', "glob" },
///         { PATHSPEC_ICASE,   '\0', "icase" },
///         { PATHSPEC_EXCLUDE,  '!', "exclude" },
///         { PATHSPEC_ATTR,    '\0', "attr" },
/// };
/// ```
const MAGIC_TABLE: [(u32, Option<char>, &str); 6] = [
    (MAGIC_TOP, Some('/'), "top"),
    (MAGIC_LITERAL, None, "literal"),
    (MAGIC_GLOB, None, "glob"),
    (MAGIC_ICASE, None, "icase"),
    (MAGIC_EXCLUDE, Some('!'), "exclude"),
    (MAGIC_ATTR, None, "attr"),
];

/// `is_pathspec_magic()` — `sane_istest(x, GIT_PATHSPEC_MAGIC)`
/// (sane-ctype.h:55), whose member set `t/unit-tests/u-ctype.c:66` pins as
/// ``!"#%&',-/:;<=>@_`~``.
///
/// Only `/` and `!` are assigned; the rest are reserved, and a reserved byte is
/// what separates `Unimplemented pathspec magic` from "the path starts here".
fn is_pathspec_magic(ch: u8) -> bool {
    matches!(
        ch,
        b'!' | b'"' | b'#' | b'%' | b'&' | b'\'' | b',' | b'-' | b'/' | b':' | b';' | b'<'
            | b'=' | b'>' | b'@' | b'_' | b'`' | b'~'
    )
}

/// `pathspec_magic_names()` (pathspec.c:563-579): every set bit, in
/// `pathspec_magic[]` order, joined with `", "`, and named
/// `'<name>' (mnemonic: '<c>')` when the table gives it a mnemonic.
///
/// The order is the table's, not the order the user wrote the keywords in, so
/// `:(icase,glob)` is reported as `'glob', 'icase'`.
pub fn magic_names(magic: u32) -> String {
    let mut out = String::new();
    for (bit, mnemonic, name) in MAGIC_TABLE {
        if magic & bit == 0 {
            continue;
        }
        if !out.is_empty() {
            out.push_str(", ");
        }
        match mnemonic {
            Some(ch) => out.push_str(&format!("'{name}' (mnemonic: '{ch}')")),
            None => out.push_str(&format!("'{name}'")),
        }
    }
    out
}

/// `unsupported_magic()` (pathspec.c:581-593). `magic` is already masked down to
/// the offending bits by the caller, exactly as `parse_pathspec()` does at
/// pathspec.c:657-658 — passing the item's whole magic would name supported
/// keywords too.
pub fn unsupported_magic(pattern: &BStr, magic: u32) -> String {
    magic_not_supported(pattern, &magic_names(magic))
}

/// One element's magic and where its path begins, i.e. `init_pathspec_item()`'s
/// `magic` and `copyfrom` (pathspec.c:451-470).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Element {
    /// The `PATHSPEC_*` bits the element asked for.
    pub magic: u32,
    /// Byte offset into the element at which `copyfrom` starts.
    pub path_start: usize,
    /// `pathspec_prefix >= 0`, i.e. the element carried `:(prefix:<n>)`. git
    /// keeps `copyfrom` verbatim for those exactly as it does for `:(top)`
    /// (pathspec.c:482-487), so it is the second half of "is this element
    /// already rooted".
    pub prefix_magic: bool,
}

impl Element {
    /// Whether `init_pathspec_item()` skips `prefix_path_gently()` for this
    /// element — `pathspec_prefix >= 0 || (magic & PATHSPEC_FROMTOP)`
    /// (pathspec.c:482-487). A rooted element is neither joined to the prefix
    /// nor able to raise "is outside repository".
    pub fn rooted(&self) -> bool {
        self.prefix_magic || self.magic & MAGIC_TOP != 0
    }

    /// `copyfrom` — the element with its magic taken off.
    pub fn path<'a>(&self, elt: &'a BStr) -> &'a BStr {
        elt[self.path_start..].as_bstr()
    }
}

/// `strcspn_escaped()` (pathspec.c:148-163): the offset of the first byte in
/// `stop` that is not preceded by a backslash, or the length.
fn strcspn_escaped(s: &[u8], stop: &[u8]) -> usize {
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'\\' && i + 1 < s.len() {
            i += 2;
            continue;
        }
        if stop.contains(&s[i]) {
            return i;
        }
        i += 1;
    }
    s.len()
}

/// `strtol(nptr, &endptr, 10)` as far as `parse_long_magic()` uses it
/// (pathspec.c:354-355): the value, and how many bytes `endptr` advanced.
///
/// Leading whitespace and a sign are consumed; a run with no digits converts
/// nothing, leaving `endptr == nptr` and a value of `0`. The saturation stands
/// in for `strtol`'s `LONG_MAX`/`LONG_MIN` clamp.
///
/// This is the same port as `gix_pathspec`'s, kept here for the same reason
/// every other piece of `parse_long_magic()` is: this module answers before the
/// matcher is built and so cannot borrow the matcher's parser.
fn strtol(input: &[u8]) -> (i64, usize) {
    let mut i = 0;
    while input.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    let negative = match input.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let digits_at = i;
    let mut value: i64 = 0;
    while let Some(&b) = input.get(i).filter(|b| b.is_ascii_digit()) {
        value = value.saturating_mul(10).saturating_add(i64::from(b - b'0'));
        i += 1;
    }
    if i == digits_at {
        return (0, 0);
    }
    (if negative { -value } else { value }, i)
}

/// `attr_name_valid()` (attr.c:199-216): non-empty, not starting with `-`, and
/// drawn from `[-A-Za-z0-9_.]`.
fn attr_name_valid(name: &[u8]) -> bool {
    if name.is_empty() || name[0] == b'-' {
        return false;
    }
    name.iter().all(|&ch| {
        ch == b'-' || ch == b'.' || ch == b'_' || ch.is_ascii_digit() || ch.is_ascii_lowercase()
            || ch.is_ascii_uppercase()
    })
}

/// `attr_value_unescape()` (pathspec.c:172-191) — run for its `die()`s only,
/// since nothing here needs the unescaped value.
fn check_attr_value(value: &[u8]) -> Result<(), String> {
    let mut i = 0;
    while i < value.len() {
        if value[i] == b'\\' {
            if i + 1 == value.len() {
                return Err(trailing_escape_in_attr_value());
            }
            i += 1;
        }
        let ch = value[i];
        // `invalid_value_char()` (pathspec.c:165-170): `isalnum()` or one of `,-_`.
        if !(ch.is_ascii_alphanumeric() || ch == b',' || ch == b'-' || ch == b'_') {
            return Err(invalid_attribute_value_char(ch as char));
        }
        i += 1;
    }
    Ok(())
}

/// `parse_pathspec_attr_match()` (pathspec.c:193-255), for its diagnostics.
///
/// `already` is `item->attr_check || item->attr_match`, i.e. whether an earlier
/// `attr:` in the same element already claimed the slot — the one check that
/// depends on state outside the body.
fn check_attr_body(body: &[u8], already: bool) -> Result<(), String> {
    if already {
        return Err(multiple_attr_specs());
    }
    if body.is_empty() {
        return Err(empty_attr_spec());
    }
    for attr in body.split(|&b| b == b' ').filter(|s| !s.is_empty()) {
        let (name, value) = match attr[0] {
            // `!` and `-` take the whole remainder as the name; only the
            // default arm looks for `=`.
            b'!' | b'-' => (&attr[1..], None),
            _ => match attr.iter().position(|&b| b == b'=') {
                Some(eq) => (&attr[..eq], Some(&attr[eq + 1..])),
                None => (&attr[..], None),
            },
        };
        if let Some(value) = value {
            check_attr_value(value)?;
        }
        if !attr_name_valid(name) {
            return Err(invalid_attribute_name(name.as_bstr()));
        }
    }
    Ok(())
}

/// `parse_long_magic()` (pathspec.c:333-387).
fn parse_long_magic(elt: &BStr) -> Result<Element, String> {
    let mut magic = 0u32;
    let mut prefix_magic = false;
    let mut pos = 2usize;
    while pos < elt.len() && elt[pos] != b')' {
        let len = strcspn_escaped(&elt[pos..], b",)");
        let nextat = if elt.get(pos + len) == Some(&b',') { pos + len + 1 } else { pos + len };
        let kw = &elt[pos..pos + len];
        pos = nextat;
        if len == 0 {
            continue;
        }
        if kw.starts_with(b"prefix:") {
            // ```c
            // if (starts_with(pos, "prefix:")) {
            //         char *endptr;
            //         *prefix_len = strtol(pos + 7, &endptr, 10);
            //         if ((size_t)(endptr - pos) != len)
            //                 die(_("invalid parameter for pathspec magic 'prefix'"));
            //         continue;
            // }
            // ```
            //
            // (pathspec.c:352-358.) The test is on `strtol`'s own `endptr`, so
            // the three things that die are the three things `strtol` leaves
            // behind: `:(prefix:0x)` stops at the `x`, `:(prefix:x)` converts
            // nothing at all, and only a parameter consumed in full survives.
            // `:(prefix:)` is *accepted* — `endptr == pos + 7 == pos + len`,
            // nothing was left over — and reading the grammar as "one or more
            // digits, optionally signed" instead refused it, and refused the
            // leading whitespace and `+` that `strtol` also takes.
            let (value, consumed) = strtol(&kw[7..]);
            if consumed != len - 7 {
                return Err(invalid_prefix_parameter());
            }
            // `pathspec_prefix >= 0` (pathspec.c:474, :482): a negative
            // parameter parses, but leaves the element ordinary — so
            // `:(prefix:-1)../x` from a subdirectory is still "is outside
            // repository", where `:(prefix:0)../x` is not.
            prefix_magic = value >= 0;
            continue;
        }
        if kw.starts_with(b"attr:") {
            check_attr_body(&kw[5..], magic & MAGIC_ATTR != 0)?;
            magic |= MAGIC_ATTR;
            continue;
        }
        match MAGIC_TABLE.iter().find(|(_, _, name)| name.as_bytes() == kw) {
            Some((bit, _, _)) => magic |= bit,
            None => return Err(invalid_magic(kw.as_bstr(), elt)),
        }
    }
    if elt.get(pos) != Some(&b')') {
        return Err(missing_closing_paren(elt));
    }
    Ok(Element { magic, path_start: pos + 1, prefix_magic })
}

/// `parse_short_magic()` (pathspec.c:395-428).
fn parse_short_magic(elt: &BStr) -> Result<Element, String> {
    let mut magic = 0u32;
    let mut pos = 1usize;
    while pos < elt.len() && elt[pos] != b':' {
        let ch = elt[pos];
        if ch == b'^' {
            // "Special case alias for '!'" (pathspec.c:403-407) — and note it is
            // handled *before* `is_pathspec_magic()`, which does not list `^`.
            magic |= MAGIC_EXCLUDE;
            pos += 1;
            continue;
        }
        if !is_pathspec_magic(ch) {
            break;
        }
        match MAGIC_TABLE.iter().find(|(_, m, _)| *m == Some(ch as char)) {
            Some((bit, _, _)) => magic |= bit,
            None => return Err(unimplemented_magic(ch as char, elt)),
        }
        pos += 1;
    }
    if elt.get(pos) == Some(&b':') {
        pos += 1;
    }
    Ok(Element { magic, path_start: pos, prefix_magic: false })
}

/// `parse_element_magic()` (pathspec.c:430-442) followed by
/// `init_pathspec_item()`'s `literal`/`glob` check (pathspec.c:478-479), which
/// is the one `die()` that needs the whole element's magic rather than one
/// keyword.
///
/// `GIT_LITERAL_PATHSPECS` (`get_literal_global()`) short-circuits the whole
/// thing at pathspec.c:434, so an element that would otherwise be rejected is
/// simply a path with a funny name.
pub fn parse_element_magic(elt: &BStr) -> Result<Element, String> {
    if std::env::var_os("GIT_LITERAL_PATHSPECS").is_some_and(|v| {
        gix::config::Boolean::try_from(v).map(|b| b.0).unwrap_or(false)
    }) {
        return Ok(Element { magic: MAGIC_LITERAL, path_start: 0, prefix_magic: false });
    }
    let element = if elt.first() != Some(&b':') {
        Element { magic: 0, path_start: 0, prefix_magic: false }
    } else if elt.get(1) == Some(&b'(') {
        parse_long_magic(elt)?
    } else {
        parse_short_magic(elt)?
    };
    // `get_global_magic()` folds `--glob-pathspecs`/`--icase-pathspecs` in here
    // too, but those bits are always *supported* wherever they can be set, so
    // they never change a mask verdict and are left out deliberately.
    if element.magic & MAGIC_LITERAL != 0 && element.magic & MAGIC_GLOB != 0 {
        return Err(incompatible_literal_glob(elt));
    }
    Ok(element)
}

/// `parse_pathspec()`'s argv scan for an empty element (pathspec.c:637-643).
///
/// It runs over *all* of argv before a single element is parsed, so
/// `git check-ignore -- ':(bogus)x' ''` reports the empty string, not the bad
/// magic — the one ordering rule that cannot be recovered from a per-element
/// loop.
pub fn empty_element_fatal<S: AsRef<[u8]>>(specs: &[S]) -> Option<String> {
    specs.iter().any(|s| s.as_ref().is_empty()).then(empty_pathspec)
}

/// `parse_pathspec()` (pathspec.c:637-668) as far as its `die()`s go, for a verb
/// that passes `magic_mask`.
///
/// git's order, and every one of these has been the bug somewhere:
///
/// 1. the empty-element scan over all of argv (pathspec.c:637-643);
/// 2. per element, `init_pathspec_item()` — magic parse, then
///    `'literal' and 'glob' are incompatible` (pathspec.c:478-479);
/// 3. `unsupported_magic()` against the mask (pathspec.c:657-658), which is
///    therefore *after* every parse diagnostic, not before it.
///
/// The out-of-repository `die()` sits inside step 2, between the two, but needs
/// a repository and a prefix; a verb that has them runs
/// [`first_outside_repository_fatal`] as well. `magic_mask` is the set of bits
/// the verb does *not* accept, which is how git spells it.
pub fn magic_mask_fatal<S: AsRef<[u8]>>(specs: &[S], magic_mask: u32) -> Option<String> {
    if let Some(msg) = empty_element_fatal(specs) {
        return Some(msg);
    }
    specs.iter().find_map(|spec| {
        let elt: &BStr = spec.as_ref().as_bstr();
        match parse_element_magic(elt) {
            Err(msg) => Some(msg),
            Ok(element) => {
                let bad = element.magic & magic_mask;
                (bad != 0).then(|| unsupported_magic(elt, bad))
            }
        }
    })
}

// ---------------------------------------------------------------------------
// `prefix_path()` (setup.c:149-160), the *other* way a verb turns an argument
// into a repository-relative path.
//
// `blame`, `mv` and friends never call `parse_pathspec()`: they take plain
// paths, so no magic is read, but the same "is outside repository" die applies
// — with two operands rather than three, since there is no `elt` to name.
// ---------------------------------------------------------------------------

/// `is_dir_sep()` on a platform with one separator.
fn is_dir_sep(c: u8) -> bool {
    c == b'/'
}

/// `normalize_path_copy_len()` (path.c:1121-1204): fold `.` and `..`, collapse
/// runs of `/`, and fail — git's `-1` — when a `..` climbs above the start of a
/// relative path.
///
/// The failure is the whole point: it is what makes `prefix_path_gently()`
/// return `NULL` and so what raises the die. A port that normalised with
/// `Path::components()` instead silently clamps at the root and never fails.
pub fn normalize_path(src: &BStr) -> Option<BString> {
    // `offset_1st_component()`: just the leading `/` outside Windows.
    let mut i = 0usize;
    let mut dst: Vec<u8> = Vec::with_capacity(src.len());
    if src.first().is_some_and(|&c| is_dir_sep(c)) {
        dst.push(b'/');
        i = 1;
    }
    let dst0 = dst.len();
    while src.get(i).is_some_and(|&c| is_dir_sep(c)) {
        i += 1;
    }
    loop {
        let at = |k: usize| src.get(i + k).copied();
        if at(0) == Some(b'.') {
            match at(1) {
                None => i += 1,
                Some(c) if is_dir_sep(c) => {
                    i += 2;
                    while src.get(i).is_some_and(|&c| is_dir_sep(c)) {
                        i += 1;
                    }
                    continue;
                }
                // (3) ".." and ends, and (4) "../" — both `goto up_one`; a
                // component like "..x" is an ordinary name and falls through.
                Some(b'.') if at(2).is_none_or(|c| is_dir_sep(c)) => {
                    i += if at(2).is_none() { 2 } else { 3 };
                    while src.get(i).is_some_and(|&c| is_dir_sep(c)) {
                        i += 1;
                    }
                    // `up_one`: strip the last component, failing when there is
                    // none left to strip (path.c:1193-1195) — the `-1` that makes
                    // `prefix_path_gently()` return NULL.
                    if dst.len() <= dst0 {
                        return None;
                    }
                    dst.pop(); // the trailing '/'
                    while dst.len() > dst0 && dst[dst.len() - 1] != b'/' {
                        dst.pop();
                    }
                    continue;
                }
                _ => {}
            }
        }
        // "copy up to the next '/', and eat all '/'" (path.c:1174-1184).
        let mut hit_sep = false;
        while let Some(&c) = src.get(i) {
            i += 1;
            if is_dir_sep(c) {
                hit_sep = true;
                break;
            }
            dst.push(c);
        }
        if hit_sep {
            dst.push(b'/');
            while src.get(i).is_some_and(|&c| is_dir_sep(c)) {
                i += 1;
            }
        } else {
            break;
        }
    }
    Some(BString::from(dst))
}

/// `prefix_path()` (setup.c:149-160) for a verb that takes a plain path rather
/// than a pathspec.
///
/// `Ok` is the repository-relative, normalised path; `Err` is the `die()` body
/// `'%s' is outside repository at '%s'` — note the *two* operands, against
/// [`first_outside_repository_fatal`]'s three: `parse_pathspec()` has an element
/// to name first and this does not.
///
/// git's `prefix` is directory-terminated, and the path is joined to it *before*
/// normalisation, which is what lets `../x` from `sub/` mean `x` rather than
/// failing.
pub fn prefix_path(repo: &gix::Repository, path: &BStr) -> Result<BString, String> {
    let prefix = repo
        .prefix()
        .ok()
        .flatten()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .filter(|p| !p.is_empty())
        .map(|p| if p.ends_with('/') { p } else { format!("{p}/") })
        .unwrap_or_default();
    let joined = if gix::path::is_absolute(gix::path::from_bstr(path)) {
        BString::from(path.to_vec())
    } else {
        let mut joined = BString::from(prefix.into_bytes());
        joined.extend_from_slice(path);
        joined
    };
    let outside = |()| {
        let hint = match repo.workdir() {
            Some(workdir) => gix::path::realpath(workdir).unwrap_or_else(|_| workdir.to_owned()),
            None => absolute_path(&crate::porcelain::rev_parse::repo_get_git_dir(repo)),
        };
        format!("'{}' is outside repository at '{}'", path.to_str_lossy(), hint.display())
    };
    let normalized = normalize_path(joined.as_bstr()).ok_or(()).map_err(outside)?;
    if !gix::path::is_absolute(gix::path::from_bstr(path)) {
        return Ok(normalized);
    }
    // `abspath_part_inside_repo()` (setup.c:56-106) measures an absolute path
    // against the work tree's realpath and returns the remainder.
    let workdir = repo.workdir().ok_or(()).map_err(outside)?;
    let root = gix::path::realpath(workdir).unwrap_or_else(|_| workdir.to_owned());
    let root = BString::from(root.to_string_lossy().replace('\\', "/").into_bytes());
    let rest = normalized
        .strip_prefix(root.as_slice())
        .filter(|rest| rest.is_empty() || rest[0] == b'/')
        .ok_or(())
        .map_err(outside)?;
    Ok(BString::from(rest.strip_prefix(b"/".as_slice()).unwrap_or(rest).to_vec()))
}
