use gix_config_value::{Integer, integer::Suffix};

#[test]
fn from_utf8_str() -> crate::Result {
    assert_eq!(
        Integer::try_from("1k")?,
        Integer {
            value: 1,
            suffix: Some(Suffix::Kibi),
        },
        "UTF-8 strings use the same integer parser as byte strings"
    );
    Ok(())
}

#[test]
fn from_str_no_suffix() {
    assert_eq!(Integer::try_from("1").unwrap(), Integer { value: 1, suffix: None });

    assert_eq!(
        Integer::try_from("-1").unwrap(),
        Integer {
            value: -1,
            suffix: None
        }
    );
}

#[test]
fn from_str_with_suffix() {
    assert_eq!(
        Integer::try_from("1k").unwrap(),
        Integer {
            value: 1,
            suffix: Some(Suffix::Kibi),
        }
    );

    assert_eq!(
        Integer::try_from("1m").unwrap(),
        Integer {
            value: 1,
            suffix: Some(Suffix::Mebi),
        }
    );

    assert_eq!(
        Integer::try_from("1g").unwrap(),
        Integer {
            value: 1,
            suffix: Some(Suffix::Gibi),
        }
    );
}

#[test]
fn invalid_from_str() {
    assert!(Integer::try_from("").is_err());
    assert!(Integer::try_from("-").is_err());
    assert!(Integer::try_from("k").is_err());
    assert!(Integer::try_from("m").is_err());
    assert!(Integer::try_from("g").is_err());
    assert!(Integer::try_from("123123123123123123123123").is_err());
    assert!(Integer::try_from("gg").is_err());
    assert!(Integer::try_from("™️🤦‍♂️").is_err());
}

#[test]
fn as_decimal() {
    fn decimal(input: &str) -> Option<i64> {
        Integer::try_from(input).unwrap().to_decimal()
    }

    assert_eq!(decimal("12"), Some(12), "works without suffix");
    assert_eq!(decimal("13k"), Some(13 * 1024), "works with kilobyte suffix");
    assert_eq!(decimal("13K"), Some(13 * 1024), "works with Kilobyte suffix");
    assert_eq!(decimal("14m"), Some(14 * 1_048_576), "works with megabyte suffix");
    assert_eq!(decimal("14M"), Some(14 * 1_048_576), "works with Megabyte suffix");
    assert_eq!(decimal("15g"), Some(15 * 1_073_741_824), "works with gigabyte suffix");
    assert_eq!(decimal("15G"), Some(15 * 1_073_741_824), "works with Gigabyte suffix");

    assert_eq!(decimal(&format!("{}g", i64::MAX)), None, "overflow results in None");
    assert_eq!(decimal(&format!("{}g", i64::MIN)), None, "underflow results in None");
}

#[test]
fn read_like_strtoimax_with_base_zero() {
    let parse = |input: &str| Integer::try_from(input).ok();
    let plain = |value| Some(Integer { value, suffix: None });
    assert_eq!(parse(" 1"), plain(1), "leading whitespace is skipped");
    assert_eq!(parse("\t\n+7"), plain(7), "whitespace then an explicit plus sign");
    assert_eq!(parse("0x1F"), plain(31), "0x selects hex");
    assert_eq!(parse("010"), plain(8), "a leading zero selects octal");
    assert_eq!(parse("-0x10"), plain(-16));
    assert_eq!(
        parse("0x1k"),
        Some(Integer {
            value: 1,
            suffix: Some(Suffix::Kibi)
        }),
        "the unit applies after the base prefix"
    );
    assert_eq!(parse("08"), None, "8 is no octal digit, so `8` is left over and is no unit");
    assert_eq!(parse("1 "), None, "trailing whitespace is not a unit");
    assert_eq!(parse("0x"), None, "no hex digit after the prefix");
    assert_eq!(parse("9223372036854775808"), None, "ERANGE");
}
