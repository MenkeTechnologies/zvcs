use gix_config_value::Boolean;

#[test]
fn from_utf8_str() -> crate::Result {
    assert_eq!(
        Boolean::try_from("yes")?,
        Boolean(true),
        "UTF-8 strings use the same boolean parser as byte strings"
    );
    Ok(())
}

#[test]
fn from_str_false() -> crate::Result {
    assert!(!Boolean::try_from("no")?.0);
    assert!(!Boolean::try_from("off")?.0);
    assert!(!Boolean::try_from("false")?.0);
    assert!(!Boolean::try_from("0")?.0);
    assert!(!Boolean::try_from("")?.0);
    Ok(())
}

#[test]
fn from_str_true() -> crate::Result {
    assert_eq!(Boolean::try_from("yes").map(Into::into), Ok(true));
    assert_eq!(Boolean::try_from("on"), Ok(Boolean(true)));
    assert_eq!(Boolean::try_from("true"), Ok(Boolean(true)));
    assert!(Boolean::try_from("1")?.0);
    assert!(Boolean::try_from("+10")?.0);
    assert!(Boolean::try_from("-1")?.0);
    Ok(())
}

#[test]
fn ignores_case() {
    // Random subset
    for word in &["no", "yes", "on", "off", "true", "false"] {
        let first: bool = Boolean::try_from(*word).unwrap().into();
        let second: bool = Boolean::try_from(word.to_uppercase().as_str()).unwrap().into();
        assert_eq!(first, second);
    }
}

#[test]
fn from_str_err() {
    assert!(Boolean::try_from("yesn't").is_err());
    assert!(Boolean::try_from("yesno").is_err());
}

#[test]
fn integers_are_read_like_git_parse_int() {
    // `git_parse_maybe_bool()` falls back to `git_parse_int()`: base 0 (hex, octal) and a
    // `k`/`m`/`g` unit, non-zero meaning true.
    for truthy in ["0x10", "0X1f", "010", "1k", "1g", " 7", "+0x1"] {
        assert_eq!(Boolean::try_from(truthy), Ok(Boolean(true)), "{truthy:?}");
    }
    for falsy in ["0x0", "00", "0k", "-0"] {
        assert_eq!(Boolean::try_from(falsy), Ok(Boolean(false)), "{falsy:?}");
    }
    // Trailing junk, a digit outside the base, a bare `0x`, a second unit and anything past
    // `INT_MAX` are `git_parse_int()` failures.
    for invalid in ["0x", "08", "1kk", "1x", "2147483648", "3g", "99999999999999999999", "0x10z"] {
        assert!(Boolean::try_from(invalid).is_err(), "{invalid:?}");
    }
}
