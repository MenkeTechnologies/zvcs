//! `git tag --trailer` amends `TAG_EDITMSG` before the editor sees it.
//!
//! `create_tag()` (builtin/tag.c:325-366) writes the template — a given
//! non-empty message, newline-terminated, else the body of the tag the ref
//! names, else the commented prompt worded for the cleanup mode — runs
//! `amend_file_with_trailers()` on it, and only then launches the editor, so
//! the editor is shown the trailers and they land above the template; zvcs
//! added them to whatever the editor left. Around it:
//!
//! * an empty `-m ''` with `--trailer` seeds the file from the previous tag or
//!   the template, which `--cleanup=verbatim` then keeps;
//! * `-m` chunks are joined by `parse_msg_arg()` (builtin/tag.c:439-452) with a
//!   blank line and no trailing newline, which `verbatim` keeps as is;
//! * the template says lines will be kept unless the mode is `strip`
//!   (builtin/tag.c:205-207);
//! * `--cleanup` is parsed for a lightweight tag too, and `default` is not a
//!   mode (builtin/tag.c:672-679).
#![cfg(unix)]

#[path = "support/stock_git.rs"]
mod stock_git;
use stock_git::stock_git;

use std::path::{Path, PathBuf};
use std::process::Command;

const ZVCS: &str = env!("CARGO_BIN_EXE_git");

/// One commit, an editor that shows the buffer it was given on stderr, and a
/// message file without a final newline.
const FIXTURE: &str = r#"
set -e
"$GIT" init -q -b main .
"$GIT" commit -q --allow-empty -m c1
printf '#!/bin/sh\necho "--- editor:" >&2\ncat "$1" >&2\necho "--- end" >&2\n' > ../ed.sh
chmod +x ../ed.sh
printf 'no newline' > ../m.txt
"#;

struct Twin {
    root: PathBuf,
}

impl Drop for Twin {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Twin {
    fn new(label: &str) -> Twin {
        let root = std::env::temp_dir().join(format!("zvcs-tag-trailer-ed-{label}-{}", std::process::id()));
        Twin { root }
    }

    /// Build the fixture afresh on both sides, so every case starts clean.
    fn reset(&self, stock: &str) {
        let _ = std::fs::remove_dir_all(&self.root);
        for side in ["stock", "zvcs"] {
            let dir = self.root.join(side).join("r");
            std::fs::create_dir_all(&dir).unwrap();
            let out = env(&mut Command::new("sh"), &self.root)
                .args(["-c", FIXTURE])
                .env("GIT", stock)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "fixture: {}", String::from_utf8_lossy(&out.stderr));
        }
    }

    /// Run `script` under `sh` with `$GIT` the binary for `side`.
    fn sh(&self, bin: &str, side: &str, script: &str) -> (String, String, i32) {
        let out = env(&mut Command::new("sh"), &self.root)
            .args(["-c", script])
            .env("GIT", bin)
            .env("STOCK", stock_git().unwrap())
            .env("GIT_EDITOR", self.root.join(side).join("ed.sh"))
            .current_dir(self.root.join(side).join("r"))
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().unwrap_or(-1),
        )
    }

    /// The script's output, every tag object byte for byte, and whether
    /// `TAG_EDITMSG` was left behind.
    fn same(&self, stock: &str, script: &str) {
        self.reset(stock);
        let state = r#"; echo "rc=$?"; for t in $("$STOCK" tag); do echo "== $t"; "$STOCK" cat-file tag $t | od -c; done; ls .git/TAG_EDITMSG 2>&1; true"#;
        let script = format!("{script}{state}");
        assert_eq!(self.sh(ZVCS, "zvcs", &script), self.sh(stock, "stock", &script), "{script}");
    }
}

fn env<'a>(cmd: &'a mut Command, root: &Path) -> &'a mut Command {
    cmd.env("HOME", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "maintenance.auto")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1800000000 +0000")
        .env("GIT_COMMITTER_DATE", "1800000000 +0000")
        .env("ZVCS_HOME", root.join("zvcs-home"))
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("VISUAL")
        .env_remove("EDITOR")
}

#[test]
fn the_editor_is_shown_the_trailers() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new("editor");
    t.same(stock, r#""$GIT" tag -a t1 --trailer 'Foo: bar'"#);
    t.same(stock, r#""$GIT" tag -a t1 -e -m msg --trailer 'Foo: bar' --trailer 'X=y'"#);
    t.same(stock, r#""$GIT" tag -a t1 -e -F ../m.txt --trailer 'Foo: bar'"#);
    t.same(stock, r#""$GIT" tag -a t1 --cleanup=whitespace --trailer 'Foo: bar'"#);
    t.same(stock, r#""$GIT" -c core.commentChar=';' tag -a t1 --trailer 'Foo: bar'"#);
    t.same(stock, r#""$GIT" tag -a t1 -m msg --trailer 'Foo: bar'; "$GIT" tag -f -a t1 --trailer 'X: y'"#);
    t.same(stock, r#""$GIT" tag t1; "$GIT" tag -f -a t1 --trailer 'X: y'"#);
    t.same(stock, r#""$GIT" tag -a t1 -e -m msg --trailer ''"#);
}

#[test]
fn without_an_editor_the_template_still_takes_the_trailers() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new("noedit");
    t.same(stock, r#""$GIT" tag -a t1 -m '' --trailer 'Foo: bar'"#);
    t.same(stock, r#""$GIT" tag -a t1 -m '' --cleanup=verbatim --trailer 'Foo: bar'"#);
    t.same(stock, r#""$GIT" tag -a t1 -m msg; "$GIT" tag -f -a t1 -m '' --trailer 'X: z'"#);
    t.same(stock, r#""$GIT" tag -a t1 -F ../m.txt --trailer 'Foo: bar'"#);
    t.same(stock, r#""$GIT" tag -a t1 -m 'a ' -m '' -m c --cleanup=verbatim --trailer 'K: v'"#);
}

#[test]
fn messages_and_cleanup_modes_follow_cmd_tag() {
    let Some(stock) = stock_git() else { return };
    let t = Twin::new("cleanup");
    t.same(stock, r#""$GIT" tag -a t1 --cleanup=verbatim -m a -m b"#);
    t.same(stock, r#""$GIT" tag -a t1 --cleanup=verbatim -m '' -m b"#);
    t.same(stock, r#""$GIT" tag -a t1 --cleanup=verbatim"#);
    t.same(stock, r#""$GIT" tag -a t1 --cleanup=default -m m"#);
    t.same(stock, r#""$GIT" tag --cleanup=bogus t1"#);
}
