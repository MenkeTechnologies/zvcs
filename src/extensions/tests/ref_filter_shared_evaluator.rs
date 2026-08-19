//! `branch --format`, `tag --format` and `for-each-ref --format` are one engine.
//!
//! In git they are: all three build a `struct ref_filter`, hand it to
//! `filter_refs()`, and print what `format_ref_array_item()` renders. The only
//! things that differ per verb are which refs are asked for and what format
//! string is used when the user gave none. A port that grows a second, thinner
//! evaluator for `branch` or `tag` renders a handful of atoms correctly and the
//! rest *wrongly at exit 0* — silent, and normally read by a script.
//!
//! Every case here pins a distinction where the three verbs must agree, or where
//! one of them is documented to differ. Each is a place a plausible
//! implementation goes wrong without failing:
//!
//!   * A per-ref model carrying only a name and an id cannot answer
//!     `%(objecttype)`, `%(flag)` or `%(contents:subject)` — but it *can* answer
//!     them with an empty string.
//!   * `--sort` on a date atom is numeric for the bare atom and a *string* the
//!     moment any `:<format>` is spelled out — `:default` included, whose
//!     rendering is byte-identical (`grab_date()`, ref-filter.c:1690-1697).
//!     Sorting `:default` numerically is invisible until two refs straddle a
//!     month name.
//!   * `--sort` on `objectsize` / `raw:size` is `FIELD_ULONG`, so `9` sorts
//!     before `100`. Byte comparison of the rendering reverses that, and looks
//!     right on any fixture whose sizes have equal digit counts.
//!   * `branch --list` / `tag --list` patterns go through `match_pattern()`
//!     (ref-filter.c:2670-2692), which strips the namespace and does *not* set
//!     `WM_PATHNAME`; `for-each-ref` uses `match_name_as_path()`, which does
//!     neither. So `a*b` matches `refs/tags/a/b` for `tag` and not for
//!     `for-each-ref`.
//!   * `git branch`'s `* ` marker and colors live in `build_format()`
//!     (builtin/branch.c:386-443), i.e. inside the default format string. A
//!     user-supplied `--format` replaces them; a port that decorates around the
//!     format prepends two bytes to every scripted line.
//!   * `print_ref_list()` (builtin/branch.c:476-477) runs `filter_ahead_behind()`
//!     and then sorts — it never calls `filter_is_base()`, which
//!     `filter_and_format_refs()` does (ref-filter.c:3440). So the *same*
//!     `%(is-base:<x>)` over the *same* refs marks one ref under `for-each-ref`
//!     and none under `branch`.
//!   * `grab_describe_values()` is reached only from `grab_values()`'s `OBJ_TAG`
//!     and `OBJ_COMMIT` arms (ref-filter.c:2135, 2150), so `%(describe)` on a
//!     ref pointing straight at a blob or tree is empty rather than whatever
//!     `git describe <blob>` would say.
//!   * `-n0` leaves `filter->lines` at 0, so `if (filter->lines)` is false and
//!     `git tag -n0` uses the plain `%(refname:lstrip=2)` format with no
//!     `%(align:15)` padding (builtin/tag.c:59-70).
//!   * `verify_ref_format()` returns the same failure to all three verbs, and
//!     they do different things with it: `usage_with_options()` for
//!     `for-each-ref`, `die("unable to parse format string")` for the other two.
//!   * Neither `branch` nor `tag` registers `OPT_QUOTING`, so
//!     `--shell`/`--perl`/`--python`/`--tcl` are unknown options there.
//!
//! Every expectation below was measured from stock git 2.55.0 in an identical
//! throwaway repository under the same pinned environment, comparing stdout,
//! stderr and exit status separately.
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
        let root =
            std::env::temp_dir().join(format!("zvcs-reffilter-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let f = Fixture { root, work };
        f.git(&["init", "-q", "-b", "main", "."]);
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
            .env("GIT_AUTHOR_DATE", "2022-03-01T00:00:00+0000")
            .env("GIT_COMMITTER_DATE", "2022-03-01T00:00:00+0000")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("GIT_PAGER", "cat");
        c
    }

    /// Run with an overridden timestamp, so a fixture can place refs on either
    /// side of a month name.
    fn at(&self, when: &str, args: &[&str]) {
        let out = self
            .cmd(args)
            .env("GIT_AUTHOR_DATE", when)
            .env("GIT_COMMITTER_DATE", when)
            .output()
            .unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn git(&self, args: &[&str]) {
        let out = self.cmd(args).output().unwrap();
        assert!(out.status.success(), "`git {args:?}` failed: {out:?}");
    }

    fn stdout(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(
            out.status.success(),
            "`git {args:?}` failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let out = self.cmd(args).output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().expect("no signal"),
        )
    }

    /// Write `body` and store it as a loose blob, returning its id. The index is
    /// left alone, so the blob is reachable only from whatever tags it.
    fn blob(&self, name: &str, body: &[u8]) -> String {
        std::fs::write(self.work.join(name), body).unwrap();
        self.stdout(&["hash-object", "-w", name]).trim().to_string()
    }
}

/// Three commits with pairwise distinct timestamps, chosen so the chronological
/// order and the `%(committerdate:default)` byte order disagree completely:
///
/// ```text
/// early  Wed Jan 1 00:00:00 2020 +0000
/// mid    Mon Feb 1 00:00:00 2021 +0000
/// main   Tue Mar 1 00:00:00 2022 +0000
/// ```
///
/// Numerically that is early < mid < main; as strings it is mid < main < early,
/// because the rendering leads with the weekday name.
fn seeded(tag: &str) -> Fixture {
    let f = Fixture::new(tag);
    for (when, body, message, branch) in [
        ("2020-01-01T00:00:00+0000", "one", "first", Some("early")),
        ("2021-02-01T00:00:00+0000", "two", "second", Some("mid")),
        ("2022-03-01T00:00:00+0000", "three", "third", None),
    ] {
        std::fs::write(f.work.join("a"), body).unwrap();
        f.at(when, &["add", "a"]);
        f.at(when, &["commit", "-q", "-m", message]);
        if let Some(name) = branch {
            f.git(&["branch", name]);
        }
    }
    f
}

#[test]
fn branch_format_answers_object_atoms_from_the_same_per_ref_model() {
    let f = seeded("model");
    f.git(&["pack-refs", "--all"]);
    // A ref created after `pack-refs` is the loose one, which is what makes
    // `%(flag)` a value that had to be read rather than guessed.
    f.git(&["branch", "loose"]);

    let expected = "refs/heads/early|commit|packed|first\n\
                    refs/heads/loose|commit||third\n\
                    refs/heads/main|commit|packed|third\n\
                    refs/heads/mid|commit|packed|second\n";
    let got = f.stdout(&[
        "branch",
        "--format=%(refname)|%(objecttype)|%(flag)|%(contents:subject)",
    ]);
    assert_eq!(got, expected);

    // The same atoms, the same values, out of `for-each-ref` — the point being
    // that one evaluator over one per-ref model produced both.
    let fer = f.stdout(&[
        "for-each-ref",
        "--format=%(refname)|%(objecttype)|%(flag)|%(contents:subject)",
        "refs/heads/",
    ]);
    assert_eq!(fer, expected);
}

#[test]
fn a_date_sort_key_with_any_format_sorts_as_a_string_in_every_verb() {
    let f = seeded("datesort");

    let numeric = "early\nmid\nmain\n";
    let stringy = "mid\nmain\nearly\n";

    assert_eq!(
        f.stdout(&["branch", "--sort=committerdate", "--format=%(refname:short)"]),
        numeric
    );
    assert_eq!(
        f.stdout(&[
            "branch",
            "--sort=committerdate:default",
            "--format=%(refname:short)"
        ]),
        stringy
    );
    assert_eq!(
        f.stdout(&[
            "for-each-ref",
            "--sort=committerdate",
            "--format=%(refname:short)",
            "refs/heads/"
        ]),
        numeric
    );
    assert_eq!(
        f.stdout(&[
            "for-each-ref",
            "--sort=committerdate:default",
            "--format=%(refname:short)",
            "refs/heads/"
        ]),
        stringy
    );
}

#[test]
fn size_sort_keys_compare_as_numbers_not_as_rendered_digits() {
    let f = seeded("sizesort");
    let small = f.blob("b9", b"123456789");
    let big = f.blob("b100", &[b'z'; 100]);
    f.git(&["tag", "small", &small]);
    f.git(&["tag", "big", &big]);

    // 9 before 100. Comparing the rendered bytes reverses this, and no fixture
    // whose sizes share a digit count can tell the two rules apart.
    assert_eq!(
        f.stdout(&[
            "tag",
            "--sort=objectsize",
            "--format=%(objectsize)|%(refname:lstrip=2)"
        ]),
        "9|small\n100|big\n"
    );
    assert_eq!(
        f.stdout(&[
            "tag",
            "--sort=raw:size",
            "--format=%(raw:size)|%(refname:lstrip=2)"
        ]),
        "9|small\n100|big\n"
    );
    assert_eq!(
        f.stdout(&[
            "tag",
            "--sort=-objectsize",
            "--format=%(objectsize)|%(refname:lstrip=2)"
        ]),
        "100|big\n9|small\n"
    );
}

#[test]
fn list_patterns_strip_the_namespace_and_let_star_cross_a_slash() {
    let f = seeded("patterns");
    f.git(&["branch", "a/b"]);
    f.git(&["tag", "a/b"]);

    // `match_pattern()`: the pattern is written against `a/b`, and `*` crosses
    // the `/` because `WM_PATHNAME` is not set.
    assert_eq!(
        f.stdout(&["branch", "--list", "a*b", "--format=%(refname)"]),
        "refs/heads/a/b\n"
    );
    assert_eq!(
        f.stdout(&["tag", "--list", "a*b", "--format=%(refname)"]),
        "refs/tags/a/b\n"
    );
    // `for-each-ref` sets `match_as_path`, so the same shape spelled against the
    // full name matches nothing.
    assert_eq!(
        f.stdout(&["for-each-ref", "--format=%(refname)", "refs/tags/a*b"]),
        ""
    );
}

#[test]
fn a_user_format_replaces_branch_decorations_rather_than_composing_with_them() {
    let f = seeded("decorations");

    // No `* `, no `  `: `build_format()` carries those, and a `--format` is used
    // *instead of* it, never alongside it.
    assert_eq!(
        f.stdout(&["branch", "--format=%(refname:short)"]),
        "early\nmain\nmid\n"
    );
    // The default listing still has them, on the checked-out branch alone.
    assert_eq!(f.stdout(&["branch"]), "  early\n* main\n  mid\n");
    // And the marker is reachable from a format, as the ordinary atom it is.
    assert_eq!(
        f.stdout(&["branch", "--format=[%(HEAD)]%(refname:short)"]),
        "[ ]early\n[*]main\n[ ]mid\n"
    );
}

#[test]
fn the_if_container_reads_a_blank_condition_as_false() {
    let f = seeded("ifstack");

    // git's `is_empty()` is "every byte passes isspace", and `%(HEAD)` renders a
    // single *space* for a ref that is not HEAD — so a bare `%(if)%(HEAD)` is
    // false for those, not true-because-non-empty. Getting this wrong inverts
    // every `%(if)%(HEAD)` format in the wild, `git branch`'s own default
    // listing included.
    assert_eq!(
        f.stdout(&["branch", "--format=[%(if)%(HEAD)%(then)T%(else)E%(end)]"]),
        "[E]\n[T]\n[E]\n"
    );
    // With no `%(else)`, a false condition contributes nothing at all.
    assert_eq!(
        f.stdout(&["branch", "--format=[%(if)%(HEAD)%(then)T%(end)]"]),
        "[]\n[T]\n[]\n"
    );
    // An atom that renders truly empty is false too.
    assert_eq!(
        f.stdout(&["branch", "--format=[%(if)%(symref)%(then)T%(else)E%(end)]"]),
        "[E]\n[E]\n[E]\n"
    );
    // `:equals=` / `:notequals=` compare the accumulated condition bytes.
    assert_eq!(
        f.stdout(&[
            "branch",
            "--format=[%(if:equals=refs/heads/main)%(refname)%(then)EQ%(else)NE%(end)]"
        ]),
        "[NE]\n[EQ]\n[NE]\n"
    );
    // Containers nest: the inner `%(end)` feeds the outer condition.
    assert_eq!(
        f.stdout(&[
            "branch",
            "--format=[%(if)%(if)%(HEAD)%(then)x%(end)%(then)N%(else)M%(end)]"
        ]),
        "[M]\n[N]\n[M]\n"
    );
}

#[test]
fn is_base_is_marked_for_for_each_ref_but_never_for_branch() {
    let f = seeded("isbase");

    // `filter_and_format_refs()` calls `filter_is_base()`, so exactly one ref in
    // the array is marked.
    assert_eq!(
        f.stdout(&[
            "for-each-ref",
            "--format=%(refname)|[%(is-base:HEAD)]",
            "refs/heads/"
        ]),
        "refs/heads/early|[]\nrefs/heads/main|[(HEAD)]\nrefs/heads/mid|[]\n"
    );

    // `print_ref_list()` open-codes its tail and has no such call, so the
    // identical atom over the identical refs marks nothing.
    assert_eq!(
        f.stdout(&["branch", "--format=%(refname)|[%(is-base:HEAD)]"]),
        "refs/heads/early|[]\nrefs/heads/main|[]\nrefs/heads/mid|[]\n"
    );
}

#[test]
fn describe_is_empty_for_a_ref_that_points_straight_at_a_blob_or_tree() {
    let f = seeded("describe");
    f.git(&["tag", "-a", "ann", "-m", "annotated subject"]);
    let blob = f.blob("payload", b"payload");
    let tree = f.stdout(&["rev-parse", "HEAD^{tree}"]).trim().to_string();
    f.git(&["tag", "blobtag", &blob]);
    f.git(&["tag", "treetag", &tree]);

    // `grab_values()` reaches `grab_describe_values()` from its OBJ_TAG and
    // OBJ_COMMIT arms only. The commit-ish tag describes; the other two do not.
    assert_eq!(
        f.stdout(&["tag", "--format=%(refname:lstrip=2)|%(objecttype)|%(describe)"]),
        "ann|tag|ann\nblobtag|blob|\ntreetag|tree|\n"
    );
}

#[test]
fn ahead_behind_reports_non_commit_refs_before_any_output_line() {
    let f = seeded("aheadbehind");
    let blob = f.blob("payload", b"payload");
    f.git(&["tag", "blobtag", &blob]);
    f.git(&["tag", "commitish"]);

    // `filter_ahead_behind()` resolves every array item's refname with quiet=0,
    // so the complaint is emitted once per non-commit ref in the whole array,
    // ahead of the formatted output, even though the atom itself renders empty
    // for that ref.
    let (out, err, code) = f.run(&["tag", "--format=%(ahead-behind:HEAD)"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(err, format!("error: object {blob} is a blob, not a commit\n"));
    assert_eq!(out, "\n0 0\n");
}

#[test]
fn tag_dash_n_zero_is_not_an_output_mode() {
    let f = seeded("nzero");
    f.git(&["tag", "-a", "ann", "-m", "annotated subject"]);
    f.git(&["tag", "-a", "a-tag-name-well-past-fifteen", "-m", "msg"]);

    // `-n0` leaves `filter->lines` at 0, which `if (filter->lines)` reads as "no
    // `-n` at all" — the plain format, with no padding and no trailing space.
    assert_eq!(f.stdout(&["tag", "-n0"]), "a-tag-name-well-past-fifteen\nann\n");
    // `-n1` is the `%(align:15)` format plus one separating space, and
    // `%(align)` never truncates, so a longer name simply pushes its message out.
    assert_eq!(
        f.stdout(&["tag", "-n1"]),
        "a-tag-name-well-past-fifteen msg\nann             annotated subject\n"
    );
}

#[test]
fn a_detached_head_sorts_first_whatever_the_sort_key_says() {
    let f = seeded("detached");
    let short = f.stdout(&["rev-parse", "--short", "HEAD"]).trim().to_string();
    f.git(&["checkout", "-q", "--detach", "HEAD"]);
    let desc = format!("(HEAD detached at {short})");

    // `REF_SORTING_DETACHED_HEAD_FIRST` short-circuits the first key comparison
    // and is exempt from `REF_SORTING_REVERSE`, so reversing a key cannot move
    // the pseudo entry off the top.
    for key in ["refname", "-refname", "committerdate", "-committerdate"] {
        let got = f.stdout(&["branch", &format!("--sort={key}"), "--format=%(refname)"]);
        assert_eq!(got.lines().next(), Some(desc.as_str()), "--sort={key}: {got}");
    }
    // `get_refname()` (ref-filter.c:2337-2342) short-circuits ahead of
    // `show_ref()`, so the description is what *every* refname modifier yields —
    // it is neither stripped nor shortened — and `%(HEAD)` marks the entry.
    assert_eq!(
        f.stdout(&["branch", "--format=%(refname:lstrip=2)|%(refname:short)|%(HEAD)"]),
        format!("{desc}|{desc}|*\nearly|early| \nmain|main| \nmid|mid| \n")
    );
}

#[test]
fn the_detached_head_entry_is_named_after_the_switch_not_the_object() {
    // `get_head_description()` (ref-filter.c:2297-2327) does *not* name the object
    // HEAD holds. `wt_status_get_detached_from()` reads HEAD's reflog backwards
    // for the last `checkout: moving from <x> to <y>`, dwims `<y>`, and reports
    // that ref — saying `at` only while HEAD is still sitting on it. Always
    // printing `(HEAD detached at <abbrev>)` is the plausible shortcut, and it is
    // wrong in three of the five shapes below.
    for (mode, want) in [
        // Detached straight onto a branch, still there.
        ("at", "(HEAD detached at refs/heads/main)"),
        // Same, then a commit on top: the switch target no longer holds HEAD.
        ("from", "(HEAD detached from refs/heads/main)"),
        // The reflog recorded a raw object name, which dwims to nothing; the
        // expectation is computed below because the abbreviation is repo-dependent.
        ("atsha", ""),
        // A tag is reported by its short name (`refs/tags/` is skipped).
        ("tag", "(HEAD detached at ann-tag)"),
        // No reflog at all leaves `detached_from` NULL.
        ("noreflog", "(no branch)"),
    ] {
        let f = seeded(&format!("headdesc-{mode}"));
        f.git(&["tag", "-a", "ann-tag", "-m", "annotated"]);
        match mode {
            "at" => f.git(&["checkout", "-q", "--detach", "main"]),
            "from" => {
                f.git(&["checkout", "-q", "--detach", "main"]);
                f.git(&["commit", "-q", "--allow-empty", "-m", "onwards"]);
            }
            "atsha" => f.git(&["checkout", "-q", "--detach", "HEAD~1"]),
            "tag" => f.git(&["checkout", "-q", "--detach", "ann-tag"]),
            "noreflog" => {
                f.git(&["checkout", "-q", "--detach", "main"]);
                let _ = std::fs::remove_file(f.work.join(".git/logs/HEAD"));
            }
            _ => unreachable!(),
        }

        let got = f.stdout(&["branch", "--format=%(refname)"]);
        let first = got.lines().next().unwrap_or("").to_string();
        if mode == "atsha" {
            // The abbreviation is repository-dependent, so pin the shape and that
            // it is *not* a ref name.
            let short = f.stdout(&["rev-parse", "--short", "HEAD"]).trim().to_string();
            assert_eq!(first, format!("(HEAD detached at {short})"), "{mode}: {got}");
        } else {
            assert_eq!(first, want, "{mode}: {got}");
        }
        // Whatever the wording, `%(refname)`'s modifiers do not touch it and
        // `%(HEAD)` marks the entry.
        assert_eq!(
            f.stdout(&["branch", "--format=%(refname:lstrip=2)|%(HEAD)"])
                .lines()
                .next()
                .unwrap_or(""),
            format!("{first}|*"),
            "{mode}"
        );
    }
}

#[test]
fn no_sort_means_no_sort_at_all() {
    let f = seeded("nosort");
    f.git(&["checkout", "-q", "--detach", "HEAD"]);

    // `OPT_REF_SORT` is an `OPT_STRING_LIST`, so `--no-sort` clears the list git
    // seeded with `refname` before `parse_options()` ran. With it empty,
    // `ref_sorting_options()` returns NULL and `ref_array_sort()` does nothing —
    // so the array keeps `do_filter_refs()`'s order, which appends the detached
    // HEAD *after* the `refs/` walk. Substituting a refname comparison for "no
    // sort" is invisible everywhere except right here.
    let unsorted = f.stdout(&["branch", "--no-sort", "--format=%(refname)"]);
    assert_eq!(
        unsorted.lines().last(),
        unsorted.lines().find(|l| l.starts_with("(HEAD detached")),
        "{unsorted}"
    );
    assert!(unsorted.lines().next().unwrap().starts_with("refs/heads/"), "{unsorted}");

    // A `--no-sort` before a `--sort` clears only what came before it, and the
    // detached entry goes back to the top.
    let resorted = f.stdout(&["branch", "--no-sort", "--sort=refname", "--format=%(refname)"]);
    assert!(
        resorted.lines().next().unwrap().starts_with("(HEAD detached"),
        "{resorted}"
    );
}

#[test]
fn columns_apply_to_a_user_format_but_not_alongside_verbose() {
    let f = seeded("columns");

    // `print_ref_list()` renders the format first and hands the finished lines to
    // `print_columns()`, so `--column` composes with any format.
    assert_eq!(
        f.stdout(&["branch", "--column=always", "--format=%(refname:short)"]),
        "early main  mid\n"
    );

    // `cmd_branch()` rejects the pair before it ever lists.
    let (out, err, code) = f.run(&["branch", "--column=always", "-v"]);
    assert_eq!(code, 128, "{err}");
    assert_eq!(out, "");
    assert_eq!(
        err,
        "fatal: options '--column' and '--verbose' cannot be used together\n"
    );
}

#[test]
fn a_malformed_format_dies_for_branch_and_tag_and_is_a_usage_error_for_for_each_ref() {
    let f = seeded("malformed");

    // `verify_ref_format()` produces the same `error:` line for all three; what
    // the caller does with its return value is what differs.
    for verb in ["branch", "tag"] {
        let (out, err, code) = f.run(&[verb, "--format=%(refname"]);
        assert_eq!(code, 128, "{verb}: {err}");
        assert_eq!(out, "", "{verb}");
        assert_eq!(
            err,
            "error: malformed format string %(refname\nfatal: unable to parse format string\n",
            "{verb}"
        );
    }
    let (_, err, code) = f.run(&["for-each-ref", "--format=%(refname"]);
    assert_eq!(code, 129, "{err}");
    assert!(
        err.starts_with("error: malformed format string %(refname\n"),
        "{err}"
    );
    assert!(err.contains("usage: git for-each-ref "), "{err}");

    // An unknown field name is a `die()` from inside the atom parser, so it is
    // 128 with no usage block everywhere.
    for verb in ["branch", "tag", "for-each-ref"] {
        let (_, err, code) = f.run(&[verb, "--format=%(bogusfield)"]);
        assert_eq!(code, 128, "{verb}: {err}");
        assert_eq!(err, "fatal: unknown field name: bogusfield\n", "{verb}");
    }
}

#[test]
fn quoting_styles_are_unknown_options_for_branch_and_tag() {
    let f = seeded("quoting");
    for (verb, style) in [
        ("branch", "--shell"),
        ("branch", "--python"),
        ("tag", "--perl"),
        ("tag", "--tcl"),
    ] {
        let (out, err, code) = f.run(&[verb, style, "--format=%(refname)"]);
        assert_eq!(code, 129, "{verb} {style}: {err}");
        assert_eq!(out, "", "{verb} {style}");
        let name = style.trim_start_matches("--");
        assert!(
            err.starts_with(&format!("error: unknown option `{name}'\n")),
            "{verb} {style}: {err}"
        );
        assert!(err.contains(&format!("usage: git {verb} ")), "{verb} {style}");
    }
    // `for-each-ref`, whose option table does carry them, is unaffected.
    assert_eq!(
        f.stdout(&[
            "for-each-ref",
            "--shell",
            "--format=%(refname)",
            "refs/heads/main"
        ]),
        "'refs/heads/main'\n"
    );
}

#[test]
fn verify_tag_format_renders_the_operand_as_typed() {
    let f = seeded("verifyfmt");
    if let Err(reason) = install_ssh_signing(&f) {
        // Loudly, so a box skipping this shows up in the log rather than passing
        // as an ordinary green test.
        eprintln!(
            "SKIP ref_filter_shared_evaluator::verify_tag_format_renders_the_operand_as_typed: \
             no signing backend available ({reason})"
        );
        return;
    }
    f.git(&["tag", "-s", "-m", "signed tag msg", "signed"]);

    // `pretty_print_ref()` (ref-filter.c:3653-3671) builds its one array item
    // from the operand *as typed*, with a zero flag word and no symref:
    // `%(refname)` is the bare name, `%(refname:lstrip=2)` has nothing to strip,
    // and `%(flag)` is empty. The peeled id is passed as NULL, so a `*`-atom
    // peels lazily and still resolves.
    let peeled = f.stdout(&["rev-parse", "signed^{}"]).trim().to_string();
    assert_eq!(
        f.stdout(&[
            "verify-tag",
            "--format=%(refname)|%(refname:lstrip=2)|%(flag)|%(tag)|%(*objectname)",
            "signed",
        ]),
        format!("signed|||signed|{peeled}\n")
    );
    // The container atoms and the whole modifier vocabulary come along with the
    // shared evaluator; a verb-local atom table had neither.
    assert_eq!(
        f.stdout(&[
            "verify-tag",
            "--format=%(align:12)%(refname)%(end)|%(if)%(taggername)%(then)A%(else)B%(end)",
            "signed",
        ]),
        "signed      |A\n"
    );
    // `git tag -v` is the same call with a `refs/tags/` lookup in front of it.
    assert_eq!(
        f.stdout(&["tag", "-v", "--format=%(refname)|%(objecttype)", "signed"]),
        "signed|tag\n"
    );
    // The format is verified once, up front, at git's own position — so this
    // reports before any signature is checked.
    let (_, err, code) = f.run(&["verify-tag", "--format=%(bogusfield)", "signed"]);
    assert_eq!(code, 128, "{err}");
    assert_eq!(err, "fatal: unknown field name: bogusfield\n");
}

/// Configure SSH signing with a throwaway key, so the signature-dependent case
/// needs no keyring, no agent and no network.
///
/// `Err(reason)` means no backend could be set up; the caller reports that as a
/// loud skip rather than letting it read as a pass.
fn install_ssh_signing(f: &Fixture) -> Result<(), String> {
    let key = f.root.join("sign.key");
    let out = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "zvcs-test", "-f"])
        .arg(&key)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("ssh-keygen not runnable: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ssh-keygen failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let pubkey = std::fs::read_to_string(f.root.join("sign.key.pub"))
        .map_err(|e| format!("no public key: {e}"))?;
    let mut parts = pubkey.split_whitespace();
    let (algo, blob) = match (parts.next(), parts.next()) {
        (Some(a), Some(b)) => (a, b),
        _ => return Err(format!("unparsable public key: {pubkey:?}")),
    };
    let allowed = f.root.join("allowed_signers");
    std::fs::write(&allowed, format!("committer@example.com {algo} {blob}\n"))
        .map_err(|e| format!("cannot write allowed_signers: {e}"))?;

    f.git(&["config", "gpg.format", "ssh"]);
    f.git(&["config", "user.signingkey", key.to_str().expect("utf-8 tmpdir")]);
    f.git(&[
        "config",
        "gpg.ssh.allowedSignersFile",
        allowed.to_str().expect("utf-8 tmpdir"),
    ]);
    Ok(())
}
