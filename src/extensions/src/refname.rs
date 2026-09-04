//! `refs.c`'s ref-name shortening, in one place.
//!
//! `refs_shorten_unambiguous_ref()` is what turns a full ref name back into the
//! spelling a user would type. It is *not* a prefix strip: it walks
//! `ref_rev_parse_rules` from the most specific rule down, and accepts the first
//! candidate no *other* rule could expand into an existing ref.
//!
//! ```c
//! static const char *ref_rev_parse_rules[] = {
//!         "%.*s",
//!         "refs/%.*s",
//!         "refs/tags/%.*s",
//!         "refs/heads/%.*s",
//!         "refs/remotes/%.*s",
//!         "refs/remotes/%.*s/HEAD",
//!         NULL
//! };
//! ```
//! (`refs.c:622-630`)
//!
//! Two consequences a prefix strip gets wrong, both observable in stock 2.55.0:
//!
//!   * the last rule has a *suffix*, so `refs/remotes/origin/HEAD` shortens to
//!     `origin`, not `origin/HEAD`;
//!   * a candidate that another rule could also produce is rejected, so with both
//!     `refs/heads/dup` and `refs/tags/dup` present neither shortens to `dup` —
//!     they become `heads/dup` and `tags/dup`.
//!
//! `strict` is the caller's, and the callers do not agree:
//!
//! | caller | C | strict |
//! |---|---|---|
//! | `%(refname:short)` and friends | `ref-filter.c:2231` | `core.warnAmbiguousRefs` |
//! | `rev-parse --abbrev-ref` | `builtin/rev-parse.c:170` | `core.warnAmbiguousRefs`, overridable with `=strict`/`=loose` |
//! | `%gd` (the reflog walker's selector) | `reflog-walk.c:252` | always 0 |

use gix::bstr::ByteSlice;
use std::collections::HashSet;

/// `ref_rev_parse_rules` (`refs.c:622-630`) split into the literal text either
/// side of the `%.*s`, in the same order. Index 0 is the bare name: it never
/// produces a candidate (the C loop starts at `NUM_REV_PARSE_RULES - 1` and stops
/// before 0) but it is tested for ambiguity like every other rule.
pub const REV_PARSE_RULES: [(&[u8], &[u8]); 6] = [
    (b"", b""),
    (b"refs/", b""),
    (b"refs/tags/", b""),
    (b"refs/heads/", b""),
    (b"refs/remotes/", b""),
    (b"refs/remotes/", b"/HEAD"),
];

/// `refs_shorten_unambiguous_ref()` (`refs.c:1625-1686`) with the ref lookup left
/// to the caller.
///
/// `exists` is `refs_ref_exists()`: whether that exact full name resolves through
/// to an object. [`ref_exists`] is the repository-backed spelling; `for-each-ref`
/// passes a set it already materialised.
///
/// ```c
/// /* skip first rule, it will always match */
/// for (i = NUM_REV_PARSE_RULES - 1; i > 0 ; --i) {
///         int rules_to_fail = i;
///         short_name = match_parse_rule(refname, ref_rev_parse_rules[i], &short_name_len);
///         if (!short_name)
///                 continue;
///         if (strict)
///                 rules_to_fail = NUM_REV_PARSE_RULES;
///         for (j = 0; j < rules_to_fail; j++) {
///                 if (i == j)
///                         continue;
///                 strbuf_addf(&resolved_buf, ref_rev_parse_rules[j], short_name_len, short_name);
///                 if (refs_ref_exists(refs, resolved_buf.buf))
///                         break;
///         }
///         if (j == rules_to_fail)
///                 return xmemdupz(short_name, short_name_len);
/// }
/// return xstrdup(refname);
/// ```
///
/// Non-strict tests only the rules *before* the matched one, so a candidate is
/// allowed to collide with a more specific rule; strict tests all of them.
pub fn shorten_unambiguous_with(
    refname: &[u8],
    strict: bool,
    exists: impl Fn(&[u8]) -> bool,
) -> Vec<u8> {
    for i in (1..REV_PARSE_RULES.len()).rev() {
        let (prefix, suffix) = REV_PARSE_RULES[i];
        // `match_parse_rule()` (`refs.c:1592-1623`): the literal text either side
        // of the placeholder has to match, and what is left over is the candidate.
        let Some(candidate) = refname
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(suffix))
        else {
            continue;
        };
        // `strip_suffix()` would report a match with nothing in between for
        // `refs/remotes//HEAD`; git's rules never produce an empty name.
        if candidate.is_empty() {
            continue;
        }
        let rules_to_fail = if strict { REV_PARSE_RULES.len() } else { i };
        let ambiguous = REV_PARSE_RULES[..rules_to_fail]
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .any(|(_, (p, s))| exists(&[*p, candidate, *s].concat()));
        if !ambiguous {
            return candidate.to_vec();
        }
    }
    refname.to_vec()
}

/// [`shorten_unambiguous_with`] against the repository's own ref store.
pub fn shorten_unambiguous(repo: &gix::Repository, refname: &[u8], strict: bool) -> Vec<u8> {
    shorten_unambiguous_with(refname, strict, |name| ref_exists(repo, name))
}

/// [`shorten_unambiguous`] for the common `str`-in/`String`-out caller.
pub fn shorten_unambiguous_str(repo: &gix::Repository, refname: &str, strict: bool) -> String {
    String::from_utf8_lossy(&shorten_unambiguous(repo, refname.as_bytes(), strict)).into_owned()
}

/// [`shorten_unambiguous_with`] backed by a set of full ref names that has already
/// been materialised, falling back to the ref store for the names a `refs/`
/// enumeration cannot contain.
///
/// The fallback is not an optimisation detail. Rule 0 tests the bare candidate,
/// and the root refs (`HEAD`, `ORIG_HEAD`, `MERGE_HEAD`, …) live directly under
/// `$GIT_DIR` rather than under `refs/`, so a set built from `refs/` alone misses
/// them: stock 2.55.0 shortens `refs/heads/ORIG_HEAD` to `heads/ORIG_HEAD`
/// precisely because `$GIT_DIR/ORIG_HEAD` exists.
pub fn shorten_unambiguous_in_set(
    repo: &gix::Repository,
    refname: &[u8],
    strict: bool,
    all: &HashSet<Vec<u8>>,
) -> Vec<u8> {
    shorten_unambiguous_with(refname, strict, |name| {
        all.contains(name)
            || (!name.contains(&b'/') && ref_exists(repo, name))
    })
}

/// `refs_ref_exists()` (`refs.c:469-473`): whether `name`, taken as a full ref
/// name, resolves for reading.
///
/// gitoxide's `try_find` takes a *partial* name and dwims `refs/tags/`,
/// `refs/heads/` and `refs/remotes/` on its own; every caller here is already
/// walking git's rule list, so a lookup that answered under a different name is
/// discarded — otherwise every candidate would "exist" and nothing would ever
/// shorten.
pub fn ref_exists(repo: &gix::Repository, name: &[u8]) -> bool {
    let Ok(name) = std::str::from_utf8(name) else {
        return false;
    };
    resolve_ref_reading(repo, name).is_some()
}

/// `refs_resolve_ref_unsafe(…, RESOLVE_REF_READING, …)`: the name `path` finally
/// resolves to after following symrefs, or `None` when `path` names no reference
/// under exactly that name, or the chain does not end at an object.
pub fn resolve_ref_reading(repo: &gix::Repository, path: &str) -> Option<String> {
    // git's `SYMREF_MAXDEPTH`.
    const MAX_DEPTH: usize = 5;
    let mut name = path.to_owned();
    for _ in 0..MAX_DEPTH {
        let found = repo.refs.try_find(name.as_str()).ok().flatten()?;
        if found.name.as_bstr() != name.as_str() {
            return None;
        }
        match found.target {
            gix::refs::Target::Object(_) => return Some(name),
            gix::refs::Target::Symbolic(next) => {
                name = next.as_bstr().to_str_lossy().into_owned()
            }
        }
    }
    None
}

/// `refs.c`'s `refname_match()` (`refs.c:632-643`): could `abbrev_name` have meant
/// `full_name`?
///
/// ```c
/// int refname_match(const char *abbrev_name, const char *full_name)
/// {
///         const char **p;
///         const int abbrev_name_len = strlen(abbrev_name);
///         const int num_rules = NUM_REV_PARSE_RULES;
///
///         for (p = ref_rev_parse_rules; *p; p++)
///                 if (!strcmp(full_name, mkpath(*p, abbrev_name_len, abbrev_name)))
///                         return &ref_rev_parse_rules[num_rules] - p;
///
///         return 0;
/// }
/// ```
///
/// The return value is the rule's rank counted from the end of
/// [`REV_PARSE_RULES`], so an earlier rule scores higher and `0` means no rule
/// matched. Callers that only ask "does it match at all" compare against zero;
/// `find_ref_by_name_abbrev()` keeps the highest-ranked candidate.
pub fn refname_match(abbrev_name: &str, full_name: &str) -> usize {
    REV_PARSE_RULES
        .iter()
        .position(|(prefix, suffix)| {
            full_name.len() == prefix.len() + abbrev_name.len() + suffix.len()
                && full_name.as_bytes().starts_with(prefix)
                && full_name.as_bytes().ends_with(suffix)
                && full_name[prefix.len()..full_name.len() - suffix.len()] == *abbrev_name
        })
        .map_or(0, |i| REV_PARSE_RULES.len() - i)
}

/// `remote.c`'s `count_refspec_match()`: how many of `refs` the abbreviated
/// `pattern` could name, and which one git would use.
///
/// ```c
///         if (namelen != patlen &&
///             patlen != namelen - 5 &&
///             !starts_with(name, "refs/heads/") &&
///             !starts_with(name, "refs/tags/")) {
///                 matched_weak = refs;
///                 weak_match++;
///         }
///         else {
///                 matched = refs;
///                 match++;
///         }
/// ```
///
/// A match is "weak" when it is with a ref outside `refs/heads/` and `refs/tags/`
/// that the pattern named neither in full nor from `refs/`; git's comment gives
/// the reason — otherwise `git push $URL master` would be ambiguous between
/// `refs/remotes/origin/master` and `refs/heads/master`. One strong match with any
/// number of weak ones beside it is still unique, and the weak set is consulted
/// only when there is no strong match at all.
pub fn count_refspec_match<'a>(pattern: &str, refs: &'a [String]) -> (usize, Option<&'a String>) {
    let patlen = pattern.len();
    let (mut weak_match, mut strong_match) = (0usize, 0usize);
    let (mut matched_weak, mut matched) = (None, None);
    for name in refs {
        if refname_match(pattern, name) == 0 {
            continue;
        }
        let namelen = name.len();
        if namelen != patlen
            && patlen + 5 != namelen
            && !name.starts_with("refs/heads/")
            && !name.starts_with("refs/tags/")
        {
            matched_weak = Some(name);
            weak_match += 1;
        } else {
            matched = Some(name);
            strong_match += 1;
        }
    }
    match matched {
        Some(_) => (strong_match, matched),
        None => (weak_match, matched_weak),
    }
}

/// The ref names `count_refspec_match()` is run over: `refs_for_each_ref()`, which
/// walks all of `refs/` — remote-tracking refs and tags included, which is the
/// reason a "weak" match exists at all.
pub fn all_ref_names(repo: &gix::Repository) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(platform) = repo.references() else { return out };
    let Ok(iter) = platform.all() else { return out };
    for reference in iter {
        let Ok(reference) = reference else { continue };
        let name = reference.name().as_bstr().to_str_lossy().into_owned();
        let Some(rest) = name.strip_prefix("refs/") else { continue };
        if gix::validate::reference::name(rest.into()).is_err() {
            continue;
        }
        out.push(name);
    }
    out
}

/// `core.warnAmbiguousRefs`, the `strict` argument most callers pass.
/// `repo_settings_get_warn_ambiguous_refs()` (`repo-settings.c:196-202`) defaults
/// it to 1, so only an explicit false turns it off.
pub fn warn_ambiguous_refs(repo: &gix::Repository) -> bool {
    repo.config_snapshot().boolean("core.warnAmbiguousRefs") != Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shorten(refname: &str, strict: bool, refs: &[&str]) -> String {
        let set: HashSet<&[u8]> = refs.iter().map(|r| r.as_bytes()).collect();
        String::from_utf8(shorten_unambiguous_with(refname.as_bytes(), strict, |n| {
            set.contains(n)
        }))
        .unwrap()
    }

    #[test]
    fn remotes_head_loses_the_head_component() {
        // The rule with a suffix is what a prefix strip cannot express.
        let refs = ["refs/remotes/origin/HEAD", "refs/remotes/origin/main"];
        assert_eq!(shorten("refs/remotes/origin/HEAD", true, &refs), "origin");
        assert_eq!(shorten("refs/remotes/origin/HEAD", false, &refs), "origin");
    }

    #[test]
    fn strict_rejects_a_candidate_a_later_rule_could_also_produce() {
        let refs = ["refs/heads/dup", "refs/tags/dup"];
        // Non-strict only looks at the rules *before* the matched one, so the tag
        // (rule 2) is invisible to the branch (rule 3) but not the other way round.
        assert_eq!(shorten("refs/tags/dup", false, &refs), "dup");
        assert_eq!(shorten("refs/heads/dup", false, &refs), "heads/dup");
        // Strict looks at all of them, so neither name is shortenable.
        assert_eq!(shorten("refs/tags/dup", true, &refs), "tags/dup");
        assert_eq!(shorten("refs/heads/dup", true, &refs), "heads/dup");
    }

    #[test]
    fn rule_zero_is_tested_but_never_produced() {
        // A root ref sharing the branch's name makes the bare candidate ambiguous.
        let refs = ["refs/heads/ORIG_HEAD", "ORIG_HEAD"];
        assert_eq!(shorten("refs/heads/ORIG_HEAD", true, &refs), "heads/ORIG_HEAD");
        // …and `refs/heads/x` never shortens to `refs/heads/x` itself.
        assert_eq!(shorten("refs/heads/main", true, &["refs/heads/main"]), "main");
    }

    #[test]
    fn a_name_no_rule_matches_is_returned_whole() {
        assert_eq!(shorten("HEAD", true, &["HEAD"]), "HEAD");
        assert_eq!(shorten("refs/stash", true, &["refs/stash"]), "stash");
    }
}
