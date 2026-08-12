//! The HTML documentation set `git help -w` opens and `git --html-path` reports.
//!
//! `show_html_page()` stats `<html-path>/<page>.html` and dies when the file is
//! missing, so in stock git `git help -w status` works only because `make
//! install` laid an HTML manual down beside the binary. This port has no install
//! step and no asciidoc toolchain, so the set is generated here — from the
//! tables that are already the source of truth for the rest of `git help`, never
//! from a second list:
//!
//!   * [`crate::porcelain::help::topics`] — every command and documentation
//!     topic in the `command-list` blocks `git help -a` and `git help -g` print,
//!     with the one-line description and the section heading git files it under.
//!     The page name comes from `cmd_to_page()`, the same function the lookup
//!     uses, so a page can never be written under a name `help -w` would not ask
//!     for.
//!   * [`crate::superset::manpage::DOCS`] — the full manual for each superset
//!     (`z*`) verb, the same table `git help <zverb>` renders to roff and
//!     `docs/reference.html` is generated from.
//!
//! What each page carries follows from that: a `z*` verb's page is its complete
//! manual, and a stock command's page is its command-list entry — the summary,
//! the category, whether this build dispatches it, and the cross-reference to
//! the man page that holds the prose. git's own asciidoc manual is not
//! reproduced and no page claims to be it.
//!
//! Pages are written under [`html_dir`] on demand by `git help -w`, and all at
//! once by `git zshadow` / `git zdashed`, exactly as the man pages are. Nothing
//! is generated at startup, and nothing is read until a page is asked for.

use std::io;
use std::path::PathBuf;

use crate::porcelain::help;
use crate::superset::manpage::{self, html_esc as esc, html_inline as inline};

/// The page every other page links back to, and what `git help -w git` opens.
const INDEX: &str = "git";

/// One page of the set.
struct Entry {
    /// The page's base name — `git-status`, `gitrevisions`, `git-zsync`.
    page: String,
    /// The heading, i.e. the page name as a title.
    title: String,
    /// The one-line description under the title.
    summary: String,
    /// The category the page is filed under on the index.
    section: String,
    /// The command line, for the verbs that have one recorded.
    synopsis: Option<String>,
    /// Body paragraphs, already plain text (roff escapes resolved).
    body: Vec<String>,
}

/// `system_path(GIT_HTML_PATH)` — `<prefix>/share/doc/git-doc`, the directory
/// `git --html-path` reports. This port's prefix is `$ZVCS_HOME`, the same one
/// the man pages and the info tree live under.
pub fn html_dir() -> PathBuf {
    crate::superset::zdaemon::zvcs_home()
        .join("share")
        .join("doc")
        .join("git-doc")
}

/// Every page in the set, index last so it can count the rest.
fn entries() -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();

    // The stock command list and the documentation topics.
    for topic in help::topics() {
        let dispatched = crate::dispatch::is_verb(&topic.name);
        let mut body = vec![format!(
            "Listed under \u{201c}{}\u{201d} in the table `{}` prints.",
            topic.section, topic.listing
        )];
        if topic.is_command {
            body.push(if dispatched {
                format!(
                    "This build dispatches `{name}`: run `git {name}` to use it and `git {name} -h` \
                     for its usage line and option list.",
                    name = topic.name
                )
            } else {
                format!(
                    "This build does not dispatch `{name}`. An executable `git-{name}` found on \
                     `PATH` still runs, as it does under stock git.",
                    name = topic.name
                )
            });
            body.push(format!(
                "`git help {}` opens the full manual page for this command.",
                topic.name
            ));
        } else {
            body.push(format!(
                "`{}` is a documentation topic rather than a command. `git help {}` opens the full \
                 manual page for it.",
                topic.name, topic.name
            ));
        }
        out.push(Entry {
            title: topic.page.clone(),
            page: topic.page,
            summary: topic.summary,
            section: topic.section,
            synopsis: None,
            body,
        });
    }

    // The superset verbs, with their complete manual.
    for doc in manpage::DOCS {
        out.push(Entry {
            page: format!("git-{}", doc.verb),
            title: format!("git-{}", doc.verb),
            summary: doc.summary.to_string(),
            section: "zvcs superset verbs".to_string(),
            synopsis: Some(doc.synopsis.to_string()),
            body: doc.desc.iter().map(|p| (*p).to_string()).collect(),
        });
    }

    out
}

/// The name of every page the set contains, for callers that only need the list.
pub fn page_names() -> Vec<String> {
    let mut names: Vec<String> = entries().into_iter().map(|e| e.page).collect();
    names.push(INDEX.to_string());
    names
}

/// Render `page`, or `None` when it is not one this set documents.
pub fn render(page: &str) -> Option<String> {
    let all = entries();
    if page == INDEX {
        return Some(render_index(&all));
    }
    all.iter().find(|e| e.page == page).map(render_entry)
}

/// Write `page` under [`html_dir`] unless it is already there with the same
/// content. `Ok(false)` means the set does not document it, which leaves the
/// caller's `stat` to fail exactly as it would against a stock install.
pub fn ensure_page(page: &str) -> io::Result<bool> {
    let Some(html) = render(page) else {
        return Ok(false);
    };
    let dir = html_dir();
    std::fs::create_dir_all(&dir)?;
    write_if_changed(&dir.join(format!("{page}.html")), &html)?;
    Ok(true)
}

/// Write the whole set under [`html_dir`], returning how many pages it holds.
/// Called by the installers beside `manpage::install_all()`.
pub fn install_all() -> io::Result<usize> {
    let all = entries();
    let dir = html_dir();
    std::fs::create_dir_all(&dir)?;
    for entry in &all {
        write_if_changed(&dir.join(format!("{}.html", entry.page)), &render_entry(entry))?;
    }
    write_if_changed(&dir.join(format!("{INDEX}.html")), &render_index(&all))?;
    Ok(all.len() + 1)
}

/// Write `html` only when the file differs, so a repeat install does not churn
/// several hundred files' mtimes.
fn write_if_changed(path: &std::path::Path, html: &str) -> io::Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|on_disk| on_disk == html) {
        return Ok(());
    }
    std::fs::write(path, html)
}

/// One command's or verb's page.
fn render_entry(entry: &Entry) -> String {
    let mut s = head(&entry.title);
    s.push_str(&format!("<h1>{}</h1>\n", esc(&entry.title)));
    s.push_str(&format!("<p class=\"sub\">{}</p>\n", esc(&entry.summary)));
    if let Some(synopsis) = &entry.synopsis {
        s.push_str("<section>\n<h2>SYNOPSIS</h2>\n");
        s.push_str(&format!("<pre class=\"synopsis\">{}</pre>\n</section>\n", esc(synopsis)));
    }
    s.push_str("<section>\n<h2>DESCRIPTION</h2>\n");
    for para in &entry.body {
        s.push_str(&format!("<p>{}</p>\n", inline(para)));
    }
    s.push_str("</section>\n");
    s.push_str(&format!(
        "<section>\n<h2>SEE ALSO</h2>\n<p><a href=\"{INDEX}.html\">{INDEX}(1)</a> \u{2014} the \
         index of this documentation set.</p>\n{}</section>\n",
        provenance()
    ));
    s.push_str("</body></html>\n");
    s
}

/// The index page: every documented page, grouped by the section it is listed
/// under, in the order the command list gives them.
fn render_index(all: &[Entry]) -> String {
    let mut s = head(INDEX);
    s.push_str("<h1>git</h1>\n");
    s.push_str(&format!(
        "<p class=\"sub\">zvcs HTML documentation set \u{2014} {} pages, one per command, \
         documentation topic and superset verb.</p>\n",
        all.len() + 1
    ));
    let mut section = "";
    for entry in all {
        if entry.section != section {
            if !section.is_empty() {
                s.push_str("</nav>\n</section>\n");
            }
            section = &entry.section;
            s.push_str(&format!("<section>\n<h2>{}</h2>\n<nav class=\"toc\">\n", esc(section)));
        }
        s.push_str(&format!(
            "<a href=\"{page}.html\" title=\"{summary}\"><code>{page}</code></a>\n",
            page = esc(&entry.page),
            summary = esc(&entry.summary)
        ));
    }
    if !section.is_empty() {
        s.push_str("</nav>\n</section>\n");
    }
    s.push_str(&format!("<section>\n{}</section>\n", provenance()));
    s.push_str("</body></html>\n");
    s
}

/// The document prologue, sharing the man pages' stylesheet so the set matches
/// `git zverbs --html` and `docs/reference.html`.
fn head(title: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\"><head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{}</title>\n{}</head><body>\n",
        esc(title),
        manpage::HTML_STYLE
    )
}

/// The one paragraph every page ends with: what generated it and what it is not.
fn provenance() -> String {
    "<p>Generated by zvcs from the command-list and manual tables compiled into the \
     <code>git</code> binary \u{2014} the same tables <code>git help -a</code> and \
     <code>git help &lt;verb&gt;</code> read. It carries each command's summary and \
     cross-references, not git's asciidoc manual.</p>\n"
        .to_string()
}

