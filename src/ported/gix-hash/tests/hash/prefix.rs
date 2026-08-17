mod cmp_oid {
    use std::cmp::Ordering;

    use crate::hex_to_id;

    #[test]
    fn it_detects_inequality_sha1() {
        let prefix = gix_hash::Prefix::new(&hex_to_id("b920bbb055e1efb9080592a409d3975738b6efb3"), 7).unwrap();
        assert_eq!(
            prefix.cmp_oid(&hex_to_id("a920bbb055e1efb9080592a409d3975738b6efb3")),
            Ordering::Greater
        );
        assert_eq!(
            prefix.cmp_oid(&hex_to_id("b920bbf055e1efb9080592a409d3975738b6efb3")),
            Ordering::Less
        );
        assert_eq!(prefix.to_string(), "b920bbb");
    }

    #[test]
    #[cfg(feature = "sha256")]
    fn it_detects_inequality_sha256() {
        let prefix = gix_hash::Prefix::new(
            &hex_to_id("b920bbb055e1efb9080592a409d3975738b6efb338b6efb338b6efb338b6efb3"),
            7,
        )
        .unwrap();
        assert_eq!(
            prefix.cmp_oid(&hex_to_id(
                "a920bbb055e1efb9080592a409d3975738b6efb338b6efb338b6efb338b6efb3"
            )),
            Ordering::Greater
        );
        assert_eq!(
            prefix.cmp_oid(&hex_to_id(
                "b920bbf055e1efb9080592a409d3975738b6efb338b6efb338b6efb338b6efb3"
            )),
            Ordering::Less
        );
        assert_eq!(prefix.to_string(), "b920bbb");
    }

    #[test]
    #[cfg(all(feature = "sha1", feature = "sha256"))]
    fn it_detects_inequality_sha1_and_sha256() {
        let len = 7;
        let prefix_sha1 = gix_hash::Prefix::new(&hex_to_id("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), len).unwrap();
        let prefix_sha256 = gix_hash::Prefix::new(
            &hex_to_id("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            len,
        )
        .unwrap();
        assert_eq!(
            prefix_sha256.cmp(&prefix_sha1),
            Ordering::Greater,
            "prefixes of larger hashes are always larger"
        );
        assert_eq!(
            prefix_sha1.to_string(),
            prefix_sha256.to_string(),
            "even though they look the same"
        );
    }

    #[test]
    fn it_detects_equality_sha1() {
        let id = hex_to_id("a920bbb055e1efb9080592a409d3975738b6efb3");
        let prefix = gix_hash::Prefix::new(&id, 6).unwrap();
        assert_eq!(prefix.cmp_oid(&id), Ordering::Equal);
        assert_eq!(
            prefix.cmp_oid(&hex_to_id("a920bbffffffffffffffffffffffffffffffffff")),
            Ordering::Equal
        );
        assert_eq!(prefix.to_string(), "a920bb");
    }

    #[test]
    #[cfg(feature = "sha256")]
    fn it_detects_equality_sha256() {
        let id = hex_to_id("a920bbb055e1efb9080592a409d3975738b6efb338b6efb338b6efb338b6efb3");
        let prefix = gix_hash::Prefix::new(&id, 6).unwrap();
        assert_eq!(prefix.cmp_oid(&id), Ordering::Equal);

        let sha1 = hex_to_id("a920bbffffffffffffffffffffffffffffffffff");
        assert_eq!(
            prefix.cmp_oid(&sha1),
            Ordering::Equal,
            "cmp_oid specifies that it only looks at the prefix, ignoring everything past that.\
            This is why it compares against a sha1 as well, which shouldn't matter in practice."
        );

        let sha256 = hex_to_id("a920bbffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");
        assert_eq!(prefix.cmp_oid(&sha256), Ordering::Equal);
        assert_eq!(prefix.to_string(), "a920bb");
    }

    /// A hex length of 41 to 64 makes `Kind::from_hex_len` answer `Sha256`, so a name a user types
    /// into a SHA-1 repository produces a 32-byte prefix compared against 20-byte object ids.
    ///
    /// `git` stores an object id in a fixed `hash[GIT_MAX_RAWSZ]` array that is zero past the
    /// algorithm's own length, and `match_hash()` compares straight through that padding. So
    /// `git rev-parse <40-hex-of-an-object>0` resolves to that object, while appending any non-zero
    /// hex character does not, and the whole range up to 64 characters behaves the same way.
    /// Measured against `git version 2.55.0`.
    ///
    /// Before this was handled, every one of these indexed past the end of the 20-byte candidate
    /// and aborted the process.
    #[test]
    #[cfg(all(feature = "sha1", feature = "sha256"))]
    fn a_prefix_wider_than_the_candidate_reads_the_padding_git_would_read() {
        let id = hex_to_id("a920bbb055e1efb9080592a409d3975738b6efb3");
        let full_hex = id.to_hex().to_string();

        for extra in 1..=24 {
            let zeros = "0".repeat(extra);
            let prefix = gix_hash::Prefix::from_hex(&format!("{full_hex}{zeros}")).expect("41..=64 is in range");
            assert_eq!(
                prefix.hex_len(),
                40 + extra,
                "the prefix keeps the full width that was typed"
            );
            assert_eq!(
                prefix.cmp_oid(&id),
                Ordering::Equal,
                "{extra} trailing zeros still name the object, as they do in git"
            );

            let mut nonzero = zeros.clone();
            nonzero.replace_range(extra - 1..extra, "1");
            let prefix = gix_hash::Prefix::from_hex(&format!("{full_hex}{nonzero}")).expect("41..=64 is in range");
            assert_eq!(
                prefix.cmp_oid(&id),
                Ordering::Greater,
                "a non-zero at position {} is past the object's own hash, so it cannot match",
                40 + extra
            );
        }
    }

    /// The over-wide comparison has to stay a usable ordering, because `gix-pack` bisects a
    /// pack index with it. Zero-extending every candidate to the prefix's width preserves the
    /// order the index is sorted in, so the search still converges instead of walking off.
    #[test]
    #[cfg(all(feature = "sha1", feature = "sha256"))]
    fn a_prefix_wider_than_the_candidate_still_orders_candidates() {
        let low = hex_to_id("1111111111111111111111111111111111111111");
        let high = hex_to_id("9999999999999999999999999999999999999999");
        let prefix = gix_hash::Prefix::from_hex("5555555555555555555555555555555555555555000").expect("43 chars");

        assert_eq!(prefix.cmp_oid(&low), Ordering::Greater);
        assert_eq!(prefix.cmp_oid(&high), Ordering::Less);
    }
}

mod new {
    use std::cmp::Ordering;

    use gix_hash::{Kind, ObjectId};

    use crate::hex_to_id;

    #[test]
    fn various_valid_inputs_sha1() {
        let oid_hex = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
        let oid = hex_to_id(oid_hex);

        for hex_len in 4..oid.kind().len_in_hex() {
            let mut expected = String::from(&oid_hex[..hex_len]);
            let num_of_zeros = oid.kind().len_in_hex() - hex_len;
            expected.extend(std::iter::repeat_n('0', num_of_zeros));
            let prefix = gix_hash::Prefix::new(&oid, hex_len).unwrap();
            assert_eq!(prefix.as_oid().to_hex().to_string(), expected, "{hex_len}");
            assert_eq!(prefix.hex_len(), hex_len);
            assert_eq!(prefix.cmp_oid(&oid), Ordering::Equal);
        }
    }

    #[test]
    #[cfg(feature = "sha256")]
    fn various_valid_inputs_sha256() {
        let oid_hex = "abcdefabcdefabcdefabcdefabcdefabcdefabcdedabcdedabcdedabcdedabcd";
        let oid = hex_to_id(oid_hex);

        for hex_len in 4..oid.kind().len_in_hex() {
            let mut expected = String::from(&oid_hex[..hex_len]);
            let num_of_zeros = oid.kind().len_in_hex() - hex_len;
            expected.extend(std::iter::repeat_n('0', num_of_zeros));
            let prefix = gix_hash::Prefix::new(&oid, hex_len).unwrap();
            assert_eq!(prefix.as_oid().to_hex().to_string(), expected, "{hex_len}");
            assert_eq!(prefix.hex_len(), hex_len);
            assert_eq!(prefix.cmp_oid(&oid), Ordering::Equal);
        }
    }

    #[test]
    fn errors_if_hex_len_is_longer_than_oid_len_in_hex() {
        let kind = Kind::Sha1;
        assert!(matches!(
            gix_hash::Prefix::new(&ObjectId::null(kind), kind.len_in_hex() + 1),
            Err(gix_hash::prefix::Error::TooLong { .. })
        ));
    }

    #[test]
    fn errors_if_hex_len_is_too_short() {
        let kind = Kind::Sha1;
        assert!(matches!(
            gix_hash::Prefix::new(&ObjectId::null(kind), 3),
            Err(gix_hash::prefix::Error::TooShort { .. })
        ));
    }
}

mod try_from {
    use std::cmp::Ordering;

    use gix_hash::{Prefix, prefix::from_hex::Error};

    use crate::hex_to_id;

    #[test]
    fn id_6_chars() {
        let oid_hex = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
        let input = "abcdef";

        let expected = hex_to_id(oid_hex);
        let actual = Prefix::try_from(input).expect("No errors");
        assert_eq!(actual.cmp_oid(&expected), Ordering::Equal);
    }

    #[test]
    fn id_7_chars() {
        let oid_hex = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
        let input = "abcdefa";

        let expected = hex_to_id(oid_hex);
        let actual = Prefix::try_from(input).expect("No errors");
        assert_eq!(actual.cmp_oid(&expected), Ordering::Equal);
    }
    #[test]
    fn id_to_short() {
        let input = "ab";
        let expected = Error::TooShort { hex_len: 2 };
        let actual = Prefix::try_from(input).unwrap_err();
        assert_eq!(actual, expected);
    }

    #[test]
    #[cfg(all(not(feature = "sha256"), feature = "sha1"))]
    fn id_too_long() {
        let input = "abcdefabcdefabcdefabcdefabcdefabcdefabcd123123123123123123";
        let expected = Error::TooLong { hex_len: 58 };
        let actual = Prefix::try_from(input).unwrap_err();
        assert_eq!(actual, expected);
    }

    #[test]
    fn id_always_too_long() {
        let input = "abcdefabcdefabcdefabcdefabcdefabcdefabcd123123123123123123123123123123";
        let expected = Error::TooLong { hex_len: 70 };
        let actual = Prefix::try_from(input).unwrap_err();
        assert_eq!(actual, expected);
    }

    #[test]
    fn invalid_chars() {
        let input = "abcdfOsd";
        let expected = Error::Invalid;
        let actual = Prefix::try_from(input).unwrap_err();
        assert_eq!(actual, expected);
    }
}

mod from_hex_nonempty {
    use std::cmp::Ordering;

    use gix_hash::{Prefix, prefix::from_hex::Error};

    use crate::hex_to_id;

    #[test]
    fn id_6_chars() {
        let oid_hex = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
        let input = "abcdef";

        let expected = hex_to_id(oid_hex);
        let actual = Prefix::from_hex_nonempty(input).expect("No errors");
        assert_eq!(actual.cmp_oid(&expected), Ordering::Equal);
    }

    #[test]
    fn id_7_chars() {
        let oid_hex = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
        let input = "abcdefa";

        let expected = hex_to_id(oid_hex);
        let actual = Prefix::from_hex_nonempty(input).expect("No errors");
        assert_eq!(actual.cmp_oid(&expected), Ordering::Equal);
    }

    #[test]
    fn id_2_chars_and_less() {
        let oid_hex = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";

        let oid = hex_to_id(oid_hex);
        let actual = Prefix::from_hex_nonempty("ab").expect("no errors");
        assert_eq!(actual.cmp_oid(&oid), Ordering::Equal);

        let actual = Prefix::from_hex_nonempty("a").expect("no errors");
        assert_eq!(actual.cmp_oid(&oid), Ordering::Equal);
    }

    #[test]
    fn id_empty() {
        let input = "";
        let expected = Error::TooShort { hex_len: 0 };
        let actual = Prefix::from_hex_nonempty(input).unwrap_err();
        assert_eq!(actual, expected);
    }

    #[test]
    #[cfg(all(not(feature = "sha256"), feature = "sha1"))]
    fn id_too_long() {
        let input = "abcdefabcdefabcdefabcdefabcdefabcdefabcd123123123123123123";
        let expected = Error::TooLong { hex_len: 58 };
        let actual = Prefix::from_hex_nonempty(input).unwrap_err();
        assert_eq!(actual, expected);
    }

    #[test]
    fn id_always_too_long() {
        let input = "abcdefabcdefabcdefabcdefabcdefabcdefabcd123123123123123123123123123123";
        let expected = Error::TooLong { hex_len: 70 };
        let actual = Prefix::from_hex_nonempty(input).unwrap_err();
        assert_eq!(actual, expected);
    }
}
