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

use gix::bstr::{BStr, ByteSlice};

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
    let workdir = repo.workdir()?;
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
    if specs.is_empty() {
        return None;
    }
    let defaults = repo.pathspec_defaults_inherit_ignore_case(false).ok()?;
    first_magic_fatal(specs, defaults)
        .or_else(|| first_outside_repository_fatal(repo, specs, defaults))
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
