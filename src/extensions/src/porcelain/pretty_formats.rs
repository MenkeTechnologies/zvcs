//! `pretty.<name>` — the user-defined `--pretty`/`--format` names, and the
//! name-resolution table every command sharing git's pretty machinery looks a
//! `--pretty=<x>` up in.
//!
//! This is a port of the four static pieces at the top of `pretty.c`: the
//! `commit_formats` table (`struct cmt_fmt_map`, pretty.c:34-45), the config
//! callback that appends to it (`git_pretty_formats_config`, pretty.c:62-118),
//! the lazy initializer that seeds it with the built-ins and then reads config
//! (`setup_commit_formats`, pretty.c:120-145), and the lookup
//! (`find_commit_format_recursive` / `find_commit_format`, pretty.c:147-188).
//! [`resolve`] is `get_commit_format`'s second half (pretty.c:209-221) — the part
//! that runs once the `format:`/`tformat:`/`%` shortcuts have been ruled out.
//!
//! Three behaviours fall out of that port and are *not* obvious from the
//! documentation; each was confirmed against stock git 2.55.0 before being
//! written down here.
//!
//! **Lookup is a case-insensitive shortest-prefix match, not an equality test.**
//! `istarts_with(commit_formats[i].name, sought)` asks whether the *table entry's*
//! name starts with what was typed, and the tie-break keeps the shortest name that
//! did (pretty.c:160-171). So `--pretty=one` is `oneline`, `--pretty=r` is `raw`
//! (not `reference`, which is longer), `--pretty=f` is `full` (not `fuller`),
//! `--pretty=m` is `medium` (`mboxrd` is the same length and loses the
//! `found_match_len > match_len` strict comparison by arriving later), and
//! `--pretty=ONELINE` resolves like `--pretty=oneline`.
//!
//! **A `pretty.<name>` whose value has no `%` in it is an *alias*, not a format**
//! (pretty.c:112-115). `pretty.short = oneline` makes `--pretty=short`… still the
//! built-in `short`, because the config callback refuses to define any name that
//! collides with a built-in (pretty.c:74-77) — but `pretty.mine = oneline` makes
//! `--pretty=mine` the built-in `oneline`. Alias resolution recurses, and the guard
//! against a loop is a redirection *budget* rather than a visited-set:
//! `num_redirections >= commit_formats_len` (pretty.c:155-158), which is why an
//! empty value (`pretty.e=`) and a self-referential one (`pretty.a=A`, matched
//! case-blind against its own name) both die with the same
//! `'<name>' references an alias which points to itself`.
//!
//! **A leading `format:`/`tformat:` inside the *config value* is honoured**
//! (pretty.c:101-108), and it is what decides whether the format separates records
//! or terminates them: `pretty.f = format:%H` prints no trailing newline after the
//! last commit, while `pretty.t = tformat:%H` and the bare `pretty.p = %H` both do.
//!
//! Deliberately not ported: git's reaction to a value-less key (`[pretty] x`, or
//! `git -c pretty.x`). `git_config_string()` fails there and the whole config read
//! becomes `fatal: unable to parse 'pretty.x' from command-line config` /
//! `fatal: bad config variable 'pretty.x' in file '<f>' at line <n>` — a
//! config-origin-shaped diagnostic that belongs to the config reader rather than to
//! this table. Such a key is skipped here instead of defining a broken entry.

use gix::bstr::ByteSlice;

/// `enum cmit_fmt` (commit.h), restricted to the names that reach this table.
///
/// `CMIT_FMT_USERFORMAT` is not spelled here: an entry that renders a user format
/// carries [`CmtFmtMap::user_format`] instead, which is what
/// `get_commit_format()`'s `if (commit_format->format == CMIT_FMT_USERFORMAT)`
/// arm (pretty.c:218-221) keys on. The built-in `reference` is such an entry, and
/// is the one built-in that is one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Builtin {
    Raw,
    Medium,
    Short,
    Email,
    MboxRd,
    Fuller,
    Full,
    Oneline,
    Reference,
}

impl Builtin {
    /// The canonical name the table holds, which is also the spelling every
    /// caller's own `match` arm already uses for the exact-match case.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Builtin::Raw => "raw",
            Builtin::Medium => "medium",
            Builtin::Short => "short",
            Builtin::Email => "email",
            Builtin::MboxRd => "mboxrd",
            Builtin::Fuller => "fuller",
            Builtin::Full => "full",
            Builtin::Oneline => "oneline",
            Builtin::Reference => "reference",
        }
    }
}

/// `struct cmt_fmt_map` (pretty.c:34-42).
///
/// `expand_tabs_in_log` and `default_date_mode_type` are the two fields
/// `get_commit_format()` copies into `rev` alongside the format itself
/// (pretty.c:215-217); they belong to built-in entries only, since the config
/// callback zeroes them for every user entry it appends (pretty.c:90).
#[derive(Clone, Debug)]
pub(crate) struct CmtFmtMap {
    /// The name looked up. For a built-in this is the fixed spelling; for a
    /// `pretty.<name>` entry it is everything after `pretty.`, with the key
    /// lower-cased the way git's config parser lower-cases it and any subsection
    /// left in its original case (`pretty.Sub.x` is the name `Sub.x`).
    pub(crate) name: String,
    /// The built-in this entry *is*, or `None` for a `pretty.<name>` entry —
    /// which is `CMIT_FMT_USERFORMAT` in git either way it is used.
    pub(crate) builtin: Option<Builtin>,
    /// `is_tformat`: the format terminates each record rather than separating
    /// them, so the last commit gets a trailing newline too.
    pub(crate) is_tformat: bool,
    /// `is_alias`: the value named another format instead of being one
    /// (pretty.c:112-115). [`find_commit_format`] follows it.
    pub(crate) is_alias: bool,
    /// `user_format`: the `%`-placeholder string this entry expands to, for the
    /// `CMIT_FMT_USERFORMAT` entries. Doubles as the alias target when
    /// [`CmtFmtMap::is_alias`] is set, exactly as it does in git.
    pub(crate) user_format: Option<String>,
}

/// `builtin_formats[]` (pretty.c:122-137), in table order — which matters, because
/// [`find_commit_format`]'s tie-break is `found_match_len > match_len`, a strict
/// comparison that keeps whichever equal-length name was seen first. That is why
/// `--pretty=m` is `medium` and not `mboxrd`.
///
/// ```c
/// { "raw",       CMIT_FMT_RAW,        0,  0 },
/// { "medium",    CMIT_FMT_MEDIUM,     0,  8 },
/// { "short",     CMIT_FMT_SHORT,      0,  0 },
/// { "email",     CMIT_FMT_EMAIL,      0,  0 },
/// { "mboxrd",    CMIT_FMT_MBOXRD,     0,  0 },
/// { "fuller",    CMIT_FMT_FULLER,     0,  8 },
/// { "full",      CMIT_FMT_FULL,       0,  8 },
/// { "oneline",   CMIT_FMT_ONELINE,    1,  0 },
/// { "reference", CMIT_FMT_USERFORMAT, 1,  0, 0, DATE_SHORT, "%C(auto)%h (%s, %ad)" },
/// ```
const BUILTIN_FORMATS: &[(Builtin, bool)] = &[
    (Builtin::Raw, false),
    (Builtin::Medium, false),
    (Builtin::Short, false),
    (Builtin::Email, false),
    (Builtin::MboxRd, false),
    (Builtin::Fuller, false),
    (Builtin::Full, false),
    (Builtin::Oneline, true),
    (Builtin::Reference, true),
];

/// What `--pretty=<x>` resolved to, once the table has been walked.
#[derive(Clone, Debug)]
pub(crate) enum Resolved {
    /// One of `builtin_formats[]`. Callers map the name onto their own format
    /// enum exactly as they did when the match was an equality test.
    Builtin(Builtin),
    /// A `pretty.<name>` entry: `save_user_format(rev, commit_format->user_format,
    /// commit_format->is_tformat)` (pretty.c:219-220).
    User {
        /// The `%`-placeholder string, with any `format:`/`tformat:` prefix from
        /// the config value already stripped.
        format: String,
        /// `rev->use_terminator`.
        is_tformat: bool,
    },
}

/// `die("invalid --pretty format: '%s' references an alias which points to
/// itself", original)` (pretty.c:156-158). Carries `original` — the name as it was
/// *typed*, not the link the walk gave up on.
#[derive(Debug)]
pub(crate) struct AliasCycle(pub(crate) String);

impl AliasCycle {
    /// The message git dies with, without the `fatal: ` prefix the renderer adds.
    pub(crate) fn message(&self) -> String {
        format!(
            "invalid --pretty format: '{}' references an alias which points to itself",
            self.0
        )
    }
}

impl From<AliasCycle> for crate::fatal::Fatal {
    fn from(c: AliasCycle) -> Self {
        crate::fatal::Fatal(c.message())
    }
}

impl From<AliasCycle> for anyhow::Error {
    fn from(c: AliasCycle) -> Self {
        anyhow::Error::new(crate::fatal::Fatal::from(c))
    }
}

/// `setup_commit_formats()` (pretty.c:120-145): the built-in table, then every
/// `pretty.<name>` the merged config defines, appended in first-definition order.
///
/// `repo` is `the_repository`, whose config `repo_config()` reads (pretty.c:144).
/// `None` is the caller that has not discovered a repository yet — git's own lazy
/// initialization happens inside `find_commit_format()`, after `cmd_main()` has
/// already set the repository up, so the discovery is done here instead of
/// threading a handle through every option parser.
pub(crate) fn commit_formats(repo: Option<&gix::Repository>) -> Vec<CmtFmtMap> {
    let formats: Vec<CmtFmtMap> = BUILTIN_FORMATS
        .iter()
        .map(|&(builtin, is_tformat)| CmtFmtMap {
            name: builtin.name().to_string(),
            builtin: Some(builtin),
            is_tformat,
            is_alias: false,
            // Only `reference` is a `CMIT_FMT_USERFORMAT` built-in; the rest are
            // rendered by their own printers and carry no format string.
            user_format: (builtin == Builtin::Reference)
                .then(|| "%C(auto)%h (%s, %ad)".to_string()),
        })
        .collect();
    let builtin_len = formats.len();

    // `repo_config()` reads the whole cascade, and outside a repository that is
    // still the system + `~/.gitconfig` + `GIT_CONFIG_*` layers — which is why
    // `git -c pretty.x=%s shortlog --pretty=x` resolves with no repository in
    // sight. The merged file is cloned out rather than borrowed so the discovered
    // repository does not have to outlive the snapshot.
    let file: gix::config::File = match repo {
        Some(r) => r.config_snapshot().plumbing().clone(),
        None => match gix::discover(".") {
            Ok(r) => r.config_snapshot().plumbing().clone(),
            Err(_) => crate::config::global_config(),
        },
    };
    with_user_formats(formats, builtin_len, &file)
}

/// The `pretty.<name>` half of [`commit_formats`], split out so it can be reached
/// with either a repository's merged config or the global cascade.
fn with_user_formats(
    mut formats: Vec<CmtFmtMap>,
    builtin_len: usize,
    file: &gix::config::File,
) -> Vec<CmtFmtMap> {
    for section in file.sections() {
        let header = section.header();
        if !header.name().eq_ignore_ascii_case(b"pretty") {
            continue;
        }
        // git's config parser lower-cases the section and the value name but keeps
        // a subsection verbatim, so `skip_prefix(var, "pretty.")` yields
        // `Sub.x` for `[pretty "Sub"] X = …` and `x` for `[pretty] X = …`.
        let subsection = header
            .subsection_name()
            .map(|s| s.to_str_lossy().into_owned());
        for value_name in section.value_names() {
            let name = match &subsection {
                Some(sub) => format!("{sub}.{}", value_name.to_ascii_lowercase()),
                None => value_name.to_ascii_lowercase(),
            };
            // `git_config_string()` fails on a value-less key; see the module doc
            // for why that diagnostic is not reproduced here.
            let Some(value) = section.value(&value_name) else {
                continue;
            };
            let value = value.to_str_lossy().into_owned();
            add_user_format(&mut formats, builtin_len, name, value);
        }
    }
    formats
}

/// `git_pretty_formats_config()`'s body (pretty.c:66-117) for one `pretty.<name>`.
///
/// ```c
/// for (i = 0; i < builtin_formats_len; i++)
///         if (!strcmp(commit_formats[i].name, name))
///                 return 0;
/// …
/// if (skip_prefix(fmt, "format:", &stripped)) {
///         commit_format->is_tformat = 0;
///         commit_format->user_format = xstrdup(stripped);
/// } else if (skip_prefix(fmt, "tformat:", &stripped)) {
///         commit_format->is_tformat = 1;
///         commit_format->user_format = xstrdup(stripped);
/// } else if (strchr(fmt, '%')) {
///         commit_format->is_tformat = 1;
///         commit_format->user_format = fmt;
/// } else {
///         commit_format->is_alias = 1;
///         commit_format->user_format = fmt;
/// }
/// ```
///
/// A name that collides with a built-in is dropped outright, which is why
/// `pretty.oneline = SHADOW %s` leaves `--pretty=oneline` alone. A name already
/// defined is *overwritten in place*, so a later definition supplies the value
/// while the earlier one keeps the table slot — the slot being what decides an
/// equal-length tie in [`find_commit_format`].
fn add_user_format(formats: &mut Vec<CmtFmtMap>, builtin_len: usize, name: String, value: String) {
    if formats[..builtin_len]
        .iter()
        .any(|f| f.name.eq_ignore_ascii_case(&name))
    {
        return;
    }
    let (is_tformat, is_alias, user_format) =
        if let Some(stripped) = value.strip_prefix("format:") {
            (false, false, stripped.to_string())
        } else if let Some(stripped) = value.strip_prefix("tformat:") {
            (true, false, stripped.to_string())
        } else if value.contains('%') {
            (true, false, value)
        } else {
            (false, true, value)
        };

    let entry = CmtFmtMap {
        name,
        builtin: None,
        is_tformat,
        is_alias,
        user_format: Some(user_format),
    };
    match formats[builtin_len..].iter_mut().find(|f| f.name == entry.name) {
        Some(slot) => *slot = entry,
        None => formats.push(entry),
    }
}

/// `find_commit_format_recursive()` / `find_commit_format()` (pretty.c:147-188).
///
/// ```c
/// if (num_redirections >= commit_formats_len)
///         die("invalid --pretty format: "
///             "'%s' references an alias which points to itself", original);
///
/// for (i = 0; i < commit_formats_len; i++) {
///         size_t match_len;
///         if (!istarts_with(commit_formats[i].name, sought))
///                 continue;
///         match_len = strlen(commit_formats[i].name);
///         if (found == NULL || found_match_len > match_len) {
///                 found = &commit_formats[i];
///                 found_match_len = match_len;
///         }
/// }
/// if (found && found->is_alias)
///         found = find_commit_format_recursive(found->user_format, original,
///                                              num_redirections+1);
/// ```
///
/// The budget is the table's own length, so it grows with the number of
/// `pretty.<name>` keys — a legitimate chain can be as long as the table.
pub(crate) fn find_commit_format<'a>(
    formats: &'a [CmtFmtMap],
    sought: &str,
) -> Result<Option<&'a CmtFmtMap>, AliasCycle> {
    fn recurse<'a>(
        formats: &'a [CmtFmtMap],
        sought: &str,
        original: &str,
        num_redirections: usize,
    ) -> Result<Option<&'a CmtFmtMap>, AliasCycle> {
        if num_redirections >= formats.len() {
            return Err(AliasCycle(original.to_string()));
        }
        let mut found: Option<&CmtFmtMap> = None;
        let mut found_match_len = 0usize;
        for candidate in formats {
            if !starts_with_ignore_ascii_case(&candidate.name, sought) {
                continue;
            }
            let match_len = candidate.name.len();
            if found.is_none() || found_match_len > match_len {
                found = Some(candidate);
                found_match_len = match_len;
            }
        }
        match found {
            Some(f) if f.is_alias => {
                let target = f.user_format.as_deref().unwrap_or_default();
                recurse(formats, target, original, num_redirections + 1)
            }
            other => Ok(other),
        }
    }
    recurse(formats, sought, sought, 0)
}

/// `istarts_with()` (git-compat-util.h): does `haystack` begin with `prefix`,
/// compared case-insensitively? Byte-wise ASCII folding, as git's `tolower()` on a
/// `char` is.
fn starts_with_ignore_ascii_case(haystack: &str, prefix: &str) -> bool {
    haystack.len() >= prefix.len()
        && haystack.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

/// `get_commit_format()`'s tail (pretty.c:209-221), for a `--pretty=<arg>` that is
/// none of the three shortcuts its head handles (`format:` / `tformat:` / a `%`).
///
/// ```c
/// commit_format = find_commit_format(arg);
/// if (!commit_format)
///         die("invalid --pretty format: %s", arg);
/// rev->commit_format = commit_format->format;
/// rev->use_terminator = commit_format->is_tformat;
/// …
/// if (commit_format->format == CMIT_FMT_USERFORMAT)
///         save_user_format(rev, commit_format->user_format,
///                          commit_format->is_tformat);
/// ```
///
/// `Ok(None)` is the `die()` — reported by the caller, which spells the message
/// (`invalid --pretty format: <arg>`) with its own exit path. `Err` is the alias
/// loop, whose message names the format rather than the option value.
pub(crate) fn resolve(
    repo: Option<&gix::Repository>,
    sought: &str,
) -> Result<Option<Resolved>, AliasCycle> {
    let formats = commit_formats(repo);
    let Some(found) = find_commit_format(&formats, sought)? else {
        return Ok(None);
    };
    Ok(Some(match found.builtin {
        Some(builtin) => Resolved::Builtin(builtin),
        None => Resolved::User {
            format: found.user_format.clone().unwrap_or_default(),
            is_tformat: found.is_tformat,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtins() -> Vec<CmtFmtMap> {
        commit_formats_from(&[])
    }

    /// The built-in table plus the given `pretty.<name> = <value>` pairs, in order
    /// — the same construction [`commit_formats`] performs after reading config,
    /// without needing a repository on disk.
    fn commit_formats_from(user: &[(&str, &str)]) -> Vec<CmtFmtMap> {
        let mut formats: Vec<CmtFmtMap> = BUILTIN_FORMATS
            .iter()
            .map(|&(builtin, is_tformat)| CmtFmtMap {
                name: builtin.name().to_string(),
                builtin: Some(builtin),
                is_tformat,
                is_alias: false,
                user_format: (builtin == Builtin::Reference)
                    .then(|| "%C(auto)%h (%s, %ad)".to_string()),
            })
            .collect();
        let builtin_len = formats.len();
        for (name, value) in user {
            add_user_format(&mut formats, builtin_len, name.to_string(), value.to_string());
        }
        formats
    }

    fn found_name(formats: &[CmtFmtMap], sought: &str) -> Option<String> {
        find_commit_format(formats, sought).unwrap().map(|f| f.name.clone())
    }

    #[test]
    fn shortest_prefix_match_wins_case_blind() {
        let f = builtins();
        // Captured from stock git 2.55.0: `--pretty=r` is `raw`, not `reference`;
        // `--pretty=f` is `full`, not `fuller`; `--pretty=m` is `medium`, because
        // `mboxrd` ties on length and the strict `>` keeps the earlier entry.
        assert_eq!(found_name(&f, "r").as_deref(), Some("raw"));
        assert_eq!(found_name(&f, "f").as_deref(), Some("full"));
        assert_eq!(found_name(&f, "m").as_deref(), Some("medium"));
        assert_eq!(found_name(&f, "one").as_deref(), Some("oneline"));
        assert_eq!(found_name(&f, "ONELINE").as_deref(), Some("oneline"));
        assert_eq!(found_name(&f, "customx"), None);
    }

    #[test]
    fn a_user_name_shorter_than_a_builtin_shadows_the_prefix_not_the_builtin() {
        // `pretty.o` is one character, so `--pretty=o` prefers it over `oneline`;
        // the full built-in name still resolves to the built-in.
        let f = commit_formats_from(&[("o", "USER %s")]);
        assert_eq!(found_name(&f, "o").as_deref(), Some("o"));
        assert_eq!(found_name(&f, "oneline").as_deref(), Some("oneline"));
    }

    #[test]
    fn builtin_names_cannot_be_redefined() {
        let f = commit_formats_from(&[("oneline", "SHADOW %s"), ("medium", "SHADOW %s")]);
        let one = find_commit_format(&f, "oneline").unwrap().unwrap();
        assert_eq!(one.builtin, Some(Builtin::Oneline));
        assert!(one.user_format.is_none(), "built-in must keep its own printer");
    }

    #[test]
    fn config_value_prefixes_pick_terminator_or_separator() {
        let f = commit_formats_from(&[
            ("sep", "format:%H"),
            ("term", "tformat:%H"),
            ("plain", "%H"),
        ]);
        let get = |n: &str| {
            let e = find_commit_format(&f, n).unwrap().unwrap();
            (e.is_tformat, e.user_format.clone().unwrap())
        };
        assert_eq!(get("sep"), (false, "%H".to_string()));
        assert_eq!(get("term"), (true, "%H".to_string()));
        assert_eq!(get("plain"), (true, "%H".to_string()));
    }

    #[test]
    fn a_percent_less_value_is_an_alias_and_is_followed() {
        let f = commit_formats_from(&[("a", "b"), ("b", "c"), ("c", "%s")]);
        let e = find_commit_format(&f, "a").unwrap().unwrap();
        assert_eq!(e.name, "c");
        assert_eq!(e.user_format.as_deref(), Some("%s"));

        // An alias may name a built-in.
        let f = commit_formats_from(&[("mine", "oneline")]);
        assert_eq!(found_name(&f, "mine").as_deref(), Some("oneline"));
    }

    #[test]
    fn alias_loops_and_empty_values_die_with_the_self_reference_message() {
        for user in [
            vec![("a", "b"), ("b", "a")],
            vec![("a", "a")],
            // `pretty.a=A` matches its own name case-blind.
            vec![("a", "A")],
            // An empty value is an alias to "", and every name starts with "".
            vec![("e", "")],
        ] {
            let f = commit_formats_from(&user);
            let sought = user[0].0;
            let err = find_commit_format(&f, sought).expect_err("must be a cycle");
            assert_eq!(
                err.message(),
                format!(
                    "invalid --pretty format: '{sought}' references an alias which points to itself"
                )
            );
        }
    }

    #[test]
    fn a_later_definition_supplies_the_value_and_the_earlier_keeps_the_slot() {
        let f = commit_formats_from(&[("x", "%H"), ("x", "%s")]);
        let e = find_commit_format(&f, "x").unwrap().unwrap();
        assert_eq!(e.user_format.as_deref(), Some("%s"));
        assert_eq!(f.iter().filter(|c| c.name == "x").count(), 1);
    }
}
