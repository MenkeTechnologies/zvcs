//! Every superset (`z*`) verb must have a man page, so `git help <zverb>` never
//! falls through to a missing `git<verb>(1)`. These tests pin the two invariants
//! that keep that true without depending on `man` being installed in CI: the doc
//! table covers exactly the dispatch verb set, and rendering + installing them
//! produces one roff file per verb.

use zvcs::dispatch::SUPERSET_VERBS;
use zvcs::superset::manpage::{self, DOCS};

#[test]
fn docs_cover_exactly_the_superset_verbs() {
    use std::collections::BTreeSet;
    let verbs: BTreeSet<&str> = SUPERSET_VERBS.iter().copied().collect();
    let documented: BTreeSet<&str> = DOCS.iter().map(|d| d.verb).collect();

    let missing: Vec<_> = verbs.difference(&documented).collect();
    assert!(missing.is_empty(), "verbs with no man page (add a DOCS entry): {missing:?}");

    let extra: Vec<_> = documented.difference(&verbs).collect();
    assert!(extra.is_empty(), "DOCS entries for non-existent verbs: {extra:?}");
}

#[test]
fn roff_has_the_mandatory_sections() {
    for doc in DOCS {
        let page = manpage::roff(doc);
        let title = format!("GIT-{}", doc.verb.to_uppercase());
        assert!(page.contains(&format!(".TH \"{title}\" 1")), "{}: missing .TH", doc.verb);
        assert!(page.contains(".SH NAME"), "{}: missing NAME", doc.verb);
        assert!(page.contains(".SH SYNOPSIS"), "{}: missing SYNOPSIS", doc.verb);
        assert!(page.contains(".SH DESCRIPTION"), "{}: missing DESCRIPTION", doc.verb);
        assert!(
            page.contains(&format!("git\\-{} \\-", doc.verb)),
            "{}: NAME line should read `git-<verb> - <summary>`",
            doc.verb
        );
        assert!(!doc.desc.is_empty(), "{}: empty description", doc.verb);
    }
}

#[test]
fn install_all_writes_one_page_per_verb() {
    let scratch = std::env::temp_dir().join(format!("zvcs-man-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    // `man_dir()` reads ZVCS_HOME at call time; set it before installing.
    std::env::set_var("ZVCS_HOME", &scratch);

    let n = manpage::install_all().expect("install_all");
    assert_eq!(n, DOCS.len(), "install_all should write one page per doc");

    let man1 = manpage::man_dir().join("man1");
    for doc in DOCS {
        let page = man1.join(format!("git-{}.1", doc.verb));
        assert!(page.exists(), "missing installed page: {}", page.display());
    }

    std::env::remove_var("ZVCS_HOME");
    let _ = std::fs::remove_dir_all(&scratch);
}
