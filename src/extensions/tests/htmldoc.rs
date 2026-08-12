//! The HTML documentation set behind `git help -w`.
//!
//! `show_html_page()` stats `<html-path>/<page>.html` and dies when it is
//! missing, so the set is only useful if the page name it writes is byte-equal
//! to the one the lookup asks for. That mapping is `cmd_to_page()` and it is
//! not obvious — `add` becomes `git-add`, `revisions` becomes `gitrevisions`,
//! `gitk` stays `gitk` — so the coverage test drives the real function rather
//! than restating the rule.
//!
//! Nothing here spawns `man`, a browser, or the binary: the set is generated
//! from tables compiled into the crate, so these run anywhere.

use std::collections::BTreeSet;

use zvcs::dispatch::SUPERSET_VERBS;
use zvcs::porcelain::help::{cmd_to_page, topics};
use zvcs::superset::htmldoc;
use zvcs::superset::manpage::DOCS;

/// Every topic `git help` can be asked about must resolve to a page the set
/// renders. A topic with no page is a `fatal: … documentation file not found.`
/// that stock git does not produce.
#[test]
fn every_documented_topic_has_a_page() {
    let pages: BTreeSet<String> = htmldoc::page_names().into_iter().collect();

    for topic in topics() {
        let page = cmd_to_page(&topic.name, topic.is_command);
        assert_eq!(page, topic.page, "`{}` disagrees with cmd_to_page()", topic.name);
        assert!(
            pages.contains(&page),
            "`git help -w {}` looks for {page}.html, which the set does not render",
            topic.name
        );
        assert!(htmldoc::render(&page).is_some(), "{page} is listed but renders nothing");
    }

    // The superset verbs reach the same lookup: they are builtins of this
    // binary, so `cmd_to_page` treats them as commands.
    for verb in SUPERSET_VERBS {
        let page = cmd_to_page(verb, true);
        assert_eq!(page, format!("git-{verb}"));
        assert!(pages.contains(&page), "`git help -w {verb}` has no {page}.html");
    }

    // `cmd_to_page(NULL)` is "git" — the topic `git help -w git` opens.
    assert!(pages.contains("git"), "the set has no git.html index page");
}

/// Page names are unique. Two entries writing the same file would make one of
/// them silently unreachable, and the collision is invisible on disk.
#[test]
fn page_names_are_unique() {
    let names = htmldoc::page_names();
    let unique: BTreeSet<&String> = names.iter().collect();
    assert_eq!(unique.len(), names.len(), "duplicate page names in the set");
}

/// Each page is a self-contained HTML document that actually carries the entry's
/// content — the failure this guards is a page that exists (so the stat passes)
/// but says nothing, which no test of the stat alone would catch.
#[test]
fn pages_are_self_contained_documents_carrying_their_content() {
    for topic in topics() {
        let html = htmldoc::render(&topic.page).expect("page renders");
        assert!(html.starts_with("<!doctype html>"), "{}: not a document", topic.page);
        assert!(html.ends_with("</body></html>\n"), "{}: truncated", topic.page);
        assert!(html.contains("<style>"), "{}: no inlined stylesheet", topic.page);
        assert!(
            html.contains(&format!("<title>{}</title>", topic.page)),
            "{}: title is not the page name",
            topic.page
        );
        // git's own one-line description, and nothing HTML-unsafe left raw.
        let summary = topic.summary.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        assert!(html.contains(&summary), "{}: summary missing from the page", topic.page);
        assert_eq!(
            html.matches("<code>").count(),
            html.matches("</code>").count(),
            "{}: unbalanced <code> spans",
            topic.page
        );
    }
}

/// A superset verb's page carries its whole manual — synopsis and every
/// description paragraph — not just the summary line the stock commands get.
#[test]
fn superset_pages_carry_the_full_manual() {
    for doc in DOCS {
        let html = htmldoc::render(&format!("git-{}", doc.verb)).expect("verb page renders");
        assert!(html.contains("<h2>SYNOPSIS</h2>"), "{}: no synopsis section", doc.verb);
        for para in doc.desc {
            // The renderer resolves the roff em-dash and the backtick spans, so
            // compare on a fragment that survives both.
            let plain = para.split("\\(em").next().expect("non-empty paragraph");
            let fragment: String = plain.chars().take_while(|c| *c != '`' && *c != '<').collect();
            assert!(
                html.contains(fragment.trim()),
                "{}: description paragraph missing from the page",
                doc.verb
            );
        }
    }
}

/// The index links every page in the set and nothing else, so a page added to
/// the catalogue cannot end up unreachable from it.
#[test]
fn the_index_links_every_page() {
    let index = htmldoc::render("git").expect("index renders");
    for page in htmldoc::page_names() {
        if page == "git" {
            continue; // the index does not link itself
        }
        assert!(
            index.contains(&format!("href=\"{page}.html\"")),
            "the index does not link {page}.html"
        );
    }
}

/// `install_all` writes the whole set under the reported html path, and a repeat
/// run is a no-op — the installers call it every time, so it must not churn
/// several hundred mtimes.
#[test]
fn install_all_writes_the_set_and_reinstalls_without_rewriting() {
    let scratch = std::env::temp_dir().join(format!("zvcs-htmldoc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    // `html_dir()` reads ZVCS_HOME at call time; set it before installing.
    std::env::set_var("ZVCS_HOME", &scratch);

    let n = htmldoc::install_all().expect("install_all");
    let dir = htmldoc::html_dir();
    assert_eq!(n, htmldoc::page_names().len(), "install_all should write one file per page");
    assert_eq!(std::fs::read_dir(&dir).expect("read set").count(), n);

    let stamp = |page: &str| {
        std::fs::metadata(dir.join(format!("{page}.html")))
            .and_then(|m| m.modified())
            .expect("mtime")
    };
    let before = stamp("git-status");
    htmldoc::install_all().expect("reinstall");
    assert_eq!(before, stamp("git-status"), "an unchanged page was rewritten");

    std::env::remove_var("ZVCS_HOME");
    let _ = std::fs::remove_dir_all(&scratch);
}
