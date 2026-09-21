//! `range-diff --dual-color`: the reset between the outer sign and the inner line.
//!
//! Under `dual_color_diffed_diffs` the outer `+`/`-` keeps the move switch's own
//! color while the rest of the line is re-tinted from the *inner* diff's marker
//! (diff.c:1485-1498, :1528-1542), and `emit_line_ws_markup()` hands both to
//! `emit_line_0()` as `set_sign` and `set` (diff.c:1385).
//!
//! `emit_line_0()` separates them with `if (set_sign && set != set_sign)
//! fputs(reset, file)` (diff.c:793). That comparison is on the `const char *`
//! each `diff_get_color_opt()` returned — the address of a slot in
//! `diff_colors[]` — so it asks whether the two came from *different slots*, not
//! whether they spell the same escape sequence. The dual-color chooser never
//! re-picks `set` from the sign's own slot, so the reset is unconditional; an
//! outer-added line carrying an inner `+` is where that bites, because
//! `DIFF_FILE_NEW` and `DIFF_FILE_NEW_BOLD` both render as `\x1b[1;32m` here and
//! a value comparison would drop the reset.
//!
//! Expectations measured against stock git 2.55.0.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git");

struct Fixture {
    root: PathBuf,
    work: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("zvcs-rddual-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
        f.git(&["config", "user.name", "C O Mitter"]);
        f.git(&["config", "user.email", "committer@example.com"]);
        f
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "2005-04-07T15:13:13-0700")
            .env("GIT_COMMITTER_DATE", "2005-04-07T15:13:13-0700");
        c
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "setup `git {args:?}` failed: {out:?}");
    }

    fn write_commit(&self, body: &str, subject: &str) {
        std::fs::write(self.work.join("file"), body).unwrap();
        self.git(&["add", "file"]);
        self.git(&["commit", "-q", "-m", subject]);
    }
}

/// The outer `+` and the inner `+extra` it precedes are separated by a reset,
/// even though both sides resolve to the same bold-green escape.
#[test]
fn dual_color_resets_between_the_outer_sign_and_the_inner_line() {
    let f = Fixture::new("plus");
    f.write_commit("0\n", "base");
    f.git(&["tag", "base"]);
    f.write_commit("0\n1\n", "topic one");
    f.write_commit("0\n1\n2\n", "topic two");
    f.git(&["branch", "-f", "old"]);

    // The rewritten series adds one more line to the second patch, so the
    // diff-of-diffs reports an outer-added line whose own first byte is `+`.
    f.git(&["reset", "-q", "--hard", "base"]);
    f.write_commit("0\n1\n", "topic one");
    f.write_commit("0\n1\n2\nextra\n", "topic two");
    f.git(&["branch", "-f", "new"]);

    // `color.diff.new` defaults to plain green while `color.diff.newBold` is bold
    // green, so the two slots spell different escapes and any comparison at all
    // would emit the reset. Painting `new` bold makes both slots read
    // `\x1b[1;32m`, which is what separates git's slot-identity test from a
    // comparison of the escapes themselves.
    let out = f
        .cmd(&[
            "-c",
            "color.diff.new=bold green",
            "range-diff",
            "--dual-color",
            "base",
            "old",
            "new",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let page = String::from_utf8(out.stdout).unwrap();

    // `\x1b[7m` reverse, the sign color, `+`, the reset, the content color, the
    // inner line, the closing reset.
    let want = "\u{1b}[7m\u{1b}[1;32m+\u{1b}[m\u{1b}[1;32m+extra\u{1b}[m";
    assert!(
        page.contains(want),
        "outer-added line needs the reset after its sign: {page:?}"
    );
    // The run-together form a value comparison would produce must not appear.
    assert!(
        !page.contains("\u{1b}[7m\u{1b}[1;32m+\u{1b}[1;32m"),
        "the sign and the content must not share one escape run: {page:?}"
    );

    // `--no-dual-color` paints the whole line in one color and writes no sign
    // color at all, so neither shape shows up there.
    let plain = f
        .cmd(&[
            "-c",
            "color.diff.new=bold green",
            "range-diff",
            "--no-dual-color",
            "base",
            "old",
            "new",
        ])
        .output()
        .unwrap();
    assert!(plain.status.success(), "{plain:?}");
    let plain = String::from_utf8(plain.stdout).unwrap();
    assert!(!plain.contains("\u{1b}[7m"), "{plain:?}");
    assert!(plain.contains("+extra"), "{plain:?}");
}
