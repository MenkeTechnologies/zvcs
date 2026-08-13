use crate::spec::parse::{parse, try_parse};

/// Epoch seconds right now, the reference `approxidate()` resolves a relative date against.
fn seconds_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after the unix epoch")
        .as_secs() as i64
}

/// Assert that a recorded `<seconds> +0000` reflog date is `now - shift`, where `now` is pinned by
/// bracketing the clock around the `parse()` call rather than by a tolerance.
///
/// `approxidate()` computes relative dates by subtracting from the epoch value of `now`, so
/// `shift` is exact across DST transitions.
fn assert_reflog_date_is_now_minus(entry: Option<&String>, before: i64, after: i64, shift: i64, spec: &str) {
    let entry = entry.unwrap_or_else(|| panic!("{spec}: a reflog date lookup should have been recorded"));
    let (seconds, offset) = entry
        .split_once(' ')
        .unwrap_or_else(|| panic!("{spec}: recorded as `<seconds> <offset>`, got {entry:?}"));
    assert_eq!(offset, "+0000", "{spec}: approxidate yields a UTC instant");
    let seconds: i64 = seconds.parse().expect("the recorded timestamp is a number");
    let window = (before - shift)..=(after - shift);
    assert!(
        window.contains(&seconds),
        "{spec}: expected now-{shift}s, i.e. within {window:?}, got {seconds}"
    );
}

#[test]
fn braces_must_be_closed() {
    for unclosed_spec in ["@{something", "@{", "@{..@"] {
        let err = try_parse(unclosed_spec).unwrap_err();
        assert_eq!(
            err.input.as_ref().map(std::convert::AsRef::as_ref),
            Some(&unclosed_spec.as_bytes()[1..])
        );
    }
}

#[test]
#[cfg(target_pointer_width = "64")] // Only works this way on 64-bit systems.
fn fuzzed() {
    let rec = parse("@{-9223372036854775808}");
    assert_eq!(rec.nth_checked_out_branch, [Some(9223372036854775808), None]);
}

#[test]
fn reflog_by_entry_for_current_branch() {
    for (spec, expected_entry) in [("@{0}", 0), ("@{42}", 42), ("@{00100}", 100)] {
        let rec = parse(spec);

        assert!(rec.kind.is_none());
        assert_eq!(rec.find_ref[0], None);
        assert_eq!(
            rec.prefix[0], None,
            "neither ref nor prefixes are set, straight to navigation"
        );
        assert_eq!(rec.current_branch_reflog_entry[0], Some(expected_entry.to_string()));
        assert_eq!(rec.calls, 1);
    }
}

#[test]
fn reflog_by_date_for_current_branch() {
    // `object-name.c:780` runs the selector through `approxidate_careful()`. Expectations below
    // are what stock git 2.55.0 answered for the same strings:
    //   TZ=UTC GIT_TEST_DATE_NOW=1698400800 git rev-parse --since=<sel>
    //     "42 +0030"   -> --max-age=1698400800   (== now, i.e. shift 0)
    //     "2.days.ago" -> --max-age=1698228000   (== now - 172800)
    //     "2 days ago" -> --max-age=1698228000
    // `42 +0030` reads as a raw commit-header date to a format-list parser, but approxidate is a
    // tokenizer: `42` is neither a day-of-month nor a year, and `+0030` is zero-padded past two
    // digits so `approxidate_digit()` discards it. Nothing is left, and the answer is `now`.
    for (spec, shift) in [("@{42 +0030}", 0), ("@{2.days.ago}", 2 * 86_400), ("@{2 days ago}", 2 * 86_400)] {
        let before = seconds_now();
        let rec = parse(spec);
        let after = seconds_now();

        assert!(rec.kind.is_none());
        assert_eq!(rec.find_ref[0], None);
        assert_eq!(
            rec.prefix[0], None,
            "neither ref nor prefixes are set, straight to navigation"
        );
        assert_reflog_date_is_now_minus(rec.current_branch_reflog_entry[0].as_ref(), before, after, shift, spec);
        assert_eq!(rec.calls, 1);
    }
}

#[test]
fn reflog_by_unix_timestamp_for_current_branch() {
    let rec = parse("@{100000000}");

    assert!(rec.kind.is_none());
    assert_eq!(rec.find_ref[0], None);
    assert_eq!(
        rec.prefix[0], None,
        "neither ref nor prefixes are set, straight to navigation"
    );
    assert_eq!(
        rec.current_branch_reflog_entry[0],
        Some("100000000 +0000".to_string()),
        "This number is the first to count as date"
    );
    assert_eq!(rec.calls, 1);

    let rec = parse("@{99999999}");
    assert_eq!(
        rec.current_branch_reflog_entry[0],
        Some("99999999".to_string()),
        "one less is an offset though"
    );
}

#[test]
fn reflog_by_date_with_date_parse_failure() {
    let err = try_parse("@{foo}").unwrap_err();
    insta::assert_snapshot!(err, @"could not parse time for reflog lookup: \"foo\"");
}

#[test]
fn reflog_by_date_for_hash_is_invalid() {
    for (spec, full_name) in [
        ("1234@{42 +0030}", "1234"),
        ("abcd-dirty@{42 +0030}", "abcd-dirty"),
        ("v1.2.3-0-g1234@{42 +0030}", "v1.2.3-0-g1234"),
    ] {
        let err = try_parse(spec).unwrap_err();
        assert_eq!(err.input.as_ref().map(AsRef::as_ref), Some(full_name.as_bytes()));
        assert!(err.message.contains("reflog entries require a ref name"));
    }
}

#[test]
fn reflog_by_date_for_given_ref_name() {
    // Same `approxidate_careful()` rule as above (`object-name.c:780`), just behind a ref name.
    // Stock git 2.55.0, TZ=UTC GIT_TEST_DATE_NOW=1698400800:
    //   git rev-parse --since="42 +0030"   -> --max-age=1698400800  (== now)
    //   git rev-parse --since="2.days.ago" -> --max-age=1698228000  (== now - 172800)
    for (spec, expected_ref, shift) in [
        ("main@{42 +0030}", "main", 0),
        ("refs/heads/other@{42 +0030}", "refs/heads/other", 0),
        ("refs/worktree/feature/a@{42 +0030}", "refs/worktree/feature/a", 0),
        ("main@{2.days.ago}", "main", 2 * 86_400),
    ] {
        let before = seconds_now();
        let rec = parse(spec);
        let after = seconds_now();

        assert!(rec.kind.is_none());
        assert_eq!(rec.get_ref(0), expected_ref);
        assert_eq!(rec.prefix[0], None);
        assert_reflog_date_is_now_minus(rec.current_branch_reflog_entry[0].as_ref(), before, after, shift, spec);
        assert_eq!(rec.calls, 2, "first the ref, then the reflog entry");
    }
}

#[test]
fn reflog_by_entry_for_given_ref_name() {
    for (spec, expected_ref, expected_entry) in [
        ("main@{0}", "main", 0),
        ("refs/heads/other@{42}", "refs/heads/other", 42),
        ("refs/worktree/feature/a@{00100}", "refs/worktree/feature/a", 100),
    ] {
        let rec = parse(spec);

        assert!(rec.kind.is_none());
        assert_eq!(rec.get_ref(0), expected_ref);
        assert_eq!(rec.prefix[0], None);
        assert_eq!(rec.current_branch_reflog_entry[0], Some(expected_entry.to_string()));
        assert_eq!(rec.calls, 2, "first the ref, then the reflog entry");
    }
}

#[test]
fn reflog_by_entry_for_hash_is_invalid() {
    for (spec, full_name) in [
        ("1234@{0}", "1234"),
        ("abcd-dirty@{1}", "abcd-dirty"),
        ("v1.2.3-0-g1234@{2}", "v1.2.3-0-g1234"),
    ] {
        let err = try_parse(spec).unwrap_err();
        assert_eq!(err.input.as_ref().map(AsRef::as_ref), Some(full_name.as_bytes()));
        assert!(err.message.contains("reflog entries require a ref name"));
    }
}

#[test]
fn sibling_branch_current_branch() {
    for (spec, kind_name) in [("@{u}", "Upstream"), ("@{push}", "Push"), ("@{UPSTREAM}", "Upstream")] {
        let rec = parse(spec);

        assert!(rec.kind.is_none());
        assert_eq!(rec.find_ref[0], None);
        assert_eq!(rec.prefix[0], None, "neither ref nor prefix are explicitly set");
        assert_eq!(rec.sibling_branch[0].as_deref(), Some(kind_name));
        assert_eq!(rec.calls, 1);
    }
}

#[test]
fn sibling_branch_for_branch_name() {
    for (spec, ref_name, kind_name) in [
        ("r1@{U}", "r1", "Upstream"),
        ("refs/heads/main@{Push}", "refs/heads/main", "Push"),
        ("refs/worktree/private@{UpStreaM}", "refs/worktree/private", "Upstream"),
    ] {
        let rec = parse(spec);

        assert!(rec.kind.is_none());
        assert_eq!(rec.get_ref(0), ref_name);
        assert_eq!(rec.prefix[0], None, "neither ref nor prefix are explicitly set");
        assert_eq!(
            rec.sibling_branch[0].as_deref(),
            Some(kind_name),
            "note that we do not know if something is a branch or not and make the call even if it would not be allowed. Configuration decides"
        );
        assert_eq!(rec.calls, 2);
    }
}

#[test]
fn sibling_branch_for_hash_is_invalid() {
    for (spec, full_name) in [
        ("1234@{u}", "1234"),
        ("abcd-dirty@{push}", "abcd-dirty"),
        ("v1.2.3-0-g1234@{upstream}", "v1.2.3-0-g1234"),
    ] {
        let err = try_parse(spec).unwrap_err();
        assert_eq!(err.input.as_ref().map(AsRef::as_ref), Some(full_name.as_bytes()));
        assert!(err.message.contains("sibling branches"));
    }
}

#[test]
fn nth_checked_out_branch_for_refname_is_invalid() {
    let err = try_parse("r1@{-1}").unwrap_err();
    // its undefined how to handle negative numbers and specified ref names
    insta::assert_snapshot!(err, @"reference name must be followed by positive numbers in @{n}: \"-1\"");
}

#[test]
fn nth_checked_out_branch() {
    for (spec, expected_branch) in [("@{-1}", 1), ("@{-42}", 42), ("@{-00100}", 100)] {
        let rec = parse(spec);

        assert!(rec.kind.is_none());
        assert_eq!(rec.find_ref[0], None);
        assert_eq!(
            rec.prefix[0], None,
            "neither ref nor prefixes are set, straight to navigation"
        );
        assert_eq!(rec.nth_checked_out_branch[0], Some(expected_branch));
        assert_eq!(rec.calls, 1);
    }
}

#[test]
fn numbers_within_braces_cannot_be_negative_zero() {
    let err = try_parse("@{-0}").unwrap_err();
    // negative zero is not accepted, even though it could easily be defaulted to 0 which is a valid value
    insta::assert_snapshot!(err, @"negative zero is invalid - remove the minus sign: \"-0\"");
}

#[test]
fn numbers_within_braces_can_be_positive_zero() {
    assert_eq!(
        parse("@{+0}"),
        parse("@{0}"),
        "+ prefixes are allowed though and the same as without it"
    );
}
