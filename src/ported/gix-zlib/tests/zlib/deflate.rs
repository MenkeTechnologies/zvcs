//! Byte-identity tests for the zlib transcription in [`gix_zlib::deflate`].
//!
//! Every expected value here was produced by zlib itself — `deflateInit(&s, level)`
//! followed by one `deflate(&s, Z_FINISH)`, the exact sequence `git`'s
//! `git_deflate_init()` / `git_deflate()` perform. zlib 1.2.12 (what macOS ships and
//! what a Homebrew `git` links), 1.3.1 and 1.3.2 were checked against each other
//! first and agree byte for byte at every level, so "zlib" is one target rather
//! than three.
//!
//! These are spec-conformance vectors, not snapshots of this crate's behaviour: if
//! a change here makes one fail, the encoder diverged from zlib and the encoder is
//! what is wrong.

use gix_zlib::deflate::{compress, crc32, Deflate, Wrap, Z_FINISH, Z_STREAM_END};
use gix_zlib::stream::deflate as stream_deflate;
use gix_zlib::{Compression, Decompress, FlushDecompress, Status};

fn unhex(s: &str) -> Vec<u8> {
    assert_eq!(s.len() % 2, 0, "a hex vector has an even number of digits");
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).expect("hex digits"))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A deterministic stand-in for incompressible data: xorshift32, which is stable
/// across platforms and needs no fixture file.
fn pseudo(n: usize) -> Vec<u8> {
    let mut s: u32 = 0x2545_F491;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s >> 24) as u8
        })
        .collect()
}

/// Compressible text with a short vocabulary, which is what most git objects look
/// like: enough repetition to exercise the hash chains and the lazy match.
fn texty(n: usize) -> Vec<u8> {
    const WORDS: [&str; 5] = ["alpha", "beta", "gamma", "delta", "epsilon"];
    let mut out = Vec::with_capacity(n + 16);
    let mut i = 0usize;
    while out.len() < n {
        out.extend_from_slice(WORDS[i % WORDS.len()].as_bytes());
        out.push(if i % 7 == 0 { b'\n' } else { b' ' });
        i += 1;
    }
    out.truncate(n);
    out
}

fn inflate(stream: &[u8], expected_len: usize) -> Vec<u8> {
    let mut state = Decompress::new();
    let mut out = vec![0; expected_len + 1];
    let status = state
        .decompress(stream, &mut out, FlushDecompress::Finish)
        .expect("the encoder produced a valid stream");
    assert_eq!(status, Status::StreamEnd, "the stream is complete");
    out.truncate(state.total_out() as usize);
    out
}

/// Short streams, written out in full, so a divergence points at the byte that moved.
///
/// The `tree` case is the object that first exposed the gap: 67 bytes that zlib
/// packs into 76 and that `zlib-rs` emitted as a 78-byte *stored* block, which is a
/// legal deflate stream and the wrong one.
#[test]
fn short_streams_match_zlib_byte_for_byte() {
    let tree = unhex(
        "31303036343420524541444d452e6d64009741694d75caeb49d3b7c1f59451c0c56bf6216c\
         343030303020737263003dd22691d588a2b3cfc3a3524d8f550bca7d32ac",
    );
    let runs = vec![b'a'; 64];
    let fox = b"the quick brown fox jumps over the lazy dog\n".repeat(4);

    let cases: [(&str, &[u8], [&str; 3]); 5] = [
        (
            "empty",
            b"",
            [
                "7801030000000001",
                "789c030000000001",
                "78da030000000001",
            ],
        ),
        (
            "hello",
            b"hello, deflate",
            [
                "7801cb48cdc9c9d75148494dcb492c49050026ad0536",
                "789ccb48cdc9c9d75148494dcb492c49050026ad0536",
                "78dacb48cdc9c9d75148494dcb492c49050026ad0536",
            ],
        ),
        (
            "runs",
            &runs,
            [
                "78014b4ca40c0000148d1841",
                "789c4b4ca40c0000148d1841",
                "78da4b4ca40c0000148d1841",
            ],
        ),
        // Level 1 diverges from 6 and 9 here: `deflate_fast` takes the first match
        // where `deflate_slow` looks one byte further. A vector that agreed at every
        // level would not be able to tell the two coders apart.
        (
            "fox",
            &fox,
            [
                "78012bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a773813803ad16004926400d",
                "789c2bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a773950c02b5004926400d",
                "78da2bc94855282ccd4cce56482aca2fcf5348cbaf50c82acd2d2856c82f4b2d5228014ae72456552aa4e4a773950c02b5004926400d",
            ],
        ),
        (
            "tree",
            &tree,
            [
                "78013334303033315108727574f175d5cb4d6198ee98e95b7aeab5e7e5ed07bf4e093c7034fb9b628e8901102814172533d85e529b78b563d1e6f3871707f9f687729faa355a030069751c52",
                "789c3334303033315108727574f175d5cb4d6198ee98e95b7aeab5e7e5ed07bf4e093c7034fb9b628e8901102814172533d85e529b78b563d1e6f3871707f9f687729faa355a030069751c52",
                "78da3334303033315108727574f175d5cb4d6198ee98e95b7aeab5e7e5ed07bf4e093c7034fb9b628e8901102814172533d85e529b78b563d1e6f3871707f9f687729faa355a030069751c52",
            ],
        ),
    ];

    for (name, input, expected) in cases {
        for (level, want) in [1, 6, 9].into_iter().zip(expected) {
            let got = compress(input, level, Wrap::Zlib);
            assert_eq!(hex(&got), want, "{name} at level {level}");
            assert_eq!(inflate(&got, input.len()), input, "{name} at level {level} round-trips");
        }
    }
}

/// The first two bytes of a zlib stream encode the window size and a level hint, and
/// have to make the pair a multiple of 31. These are the four values zlib produces
/// for `windowBits = 15`, and getting them wrong would make every stream we write
/// differ from git's in its very first byte.
#[test]
fn zlib_header_bytes_match_zlib_at_every_level() {
    let expected = [
        (0, [0x78, 0x01]),
        (1, [0x78, 0x01]),
        (2, [0x78, 0x5e]),
        (3, [0x78, 0x5e]),
        (4, [0x78, 0x5e]),
        (5, [0x78, 0x5e]),
        (6, [0x78, 0x9c]),
        (7, [0x78, 0xda]),
        (8, [0x78, 0xda]),
        (9, [0x78, 0xda]),
    ];
    for (level, want) in expected {
        let got = compress(b"header check", level, Wrap::Zlib);
        assert_eq!(&got[..2], &want, "zlib header at level {level}");
        assert_eq!(
            u16::from_be_bytes([got[0], got[1]]) % 31,
            0,
            "the header pair is a multiple of 31 at level {level}"
        );
    }
    // -1 is what `git` passes for an unconfigured level, and zlib maps it to 6.
    assert_eq!(
        compress(b"header check", -1, Wrap::Zlib),
        compress(b"header check", 6, Wrap::Zlib),
        "level -1 is level 6"
    );
}

/// Inputs long enough to slide the window, run multiple blocks and, for the
/// incompressible one, fall back to stored blocks. Written as the output length plus
/// its CRC-32 rather than as megabytes of hex; a single flipped bit in a hundred
/// kilobytes of output still fails.
#[test]
fn long_inputs_match_zlib_at_every_level() {
    let corpora: [(&str, Vec<u8>, [(usize, u32); 10]); 4] = [
        (
            "pseudo_4k",
            pseudo(4096),
            [
                (4107, 0x8e81_0e76),
                (4107, 0x8e81_0e76),
                (4107, 0x26d7_e65d),
                (4107, 0x26d7_e65d),
                (4107, 0x26d7_e65d),
                (4107, 0x26d7_e65d),
                (4107, 0x853d_7186),
                (4107, 0x5e8d_2409),
                (4107, 0x5e8d_2409),
                (4107, 0x5e8d_2409),
            ],
        ),
        (
            "pseudo_200k",
            pseudo(200_000),
            [
                (200_026, 0x8fc8_f7f6),
                (200_071, 0x1348_750f),
                (200_071, 0x3f53_a03b),
                (200_071, 0x3f53_a03b),
                (200_071, 0xd65a_5b6b),
                (200_071, 0xd65a_5b6b),
                (200_071, 0x8f31_728f),
                (200_071, 0x7deb_47ba),
                (200_071, 0x7deb_47ba),
                (200_071, 0x7deb_47ba),
            ],
        ),
        (
            "texty_40k",
            texty(40_000),
            [
                (40_011, 0x2b6f_22d0),
                (490, 0x0c67_1bd5),
                (473, 0x4774_338a),
                (468, 0x8ece_a8ed),
                (265, 0x2a1c_021c),
                (256, 0xe0ad_fc9c),
                (236, 0x25ef_2819),
                (236, 0x58b5_d332),
                (236, 0x58b5_d332),
                (236, 0x58b5_d332),
            ],
        ),
        (
            "texty_300k",
            texty(300_000),
            [
                (300_031, 0x1164_99fd),
                (2744, 0x2a07_363d),
                (2908, 0x6eb1_8740),
                (2736, 0x676e_b2db),
                (1436, 0x8e8c_d849),
                (1365, 0xc899_7dcf),
                (1243, 0xccb4_e56c),
                (1243, 0xe1f5_ea28),
                (1243, 0xe1f5_ea28),
                (1243, 0xe1f5_ea28),
            ],
        ),
    ];

    for (name, input, expected) in corpora {
        for (level, (want_len, want_crc)) in expected.into_iter().enumerate() {
            let got = compress(&input, level as i32, Wrap::Zlib);
            assert_eq!(got.len(), want_len, "{name} at level {level}: output length");
            assert_eq!(
                crc32(0, &got),
                want_crc,
                "{name} at level {level}: output bytes (length matched, contents did not)"
            );
            assert_eq!(inflate(&got, input.len()), input, "{name} at level {level} round-trips");
        }
    }
}

/// Levels 1 through 9 must not care how the caller splits the input, because git's
/// callers split it differently than we do. Level 0 is excluded on purpose:
/// `deflate_stored()` sizes its blocks from `avail_in` and `avail_out`, so there the
/// framing genuinely is part of the output.
#[test]
fn chunking_does_not_change_the_stream() {
    use std::io::Write;

    for input in [texty(40_000), pseudo(9_000), b"short".to_vec(), Vec::new()] {
        for level in 1..=9 {
            let one_shot = compress(&input, level, Wrap::Zlib);
            for chunk in [1usize, 13, 4096, 100_000] {
                let mut w = stream_deflate::Write::new(
                    Vec::new(),
                    Compression::new(level).expect("0..=9 is a valid level"),
                );
                for piece in input.chunks(chunk).filter(|c| !c.is_empty()) {
                    w.write_all(piece).expect("in-memory writes never fail");
                }
                w.flush().expect("in-memory flushes never fail");
                assert_eq!(
                    w.into_inner(),
                    one_shot,
                    "level {level} with {chunk}-byte writes over {} bytes",
                    input.len()
                );
            }
        }
    }
}

/// The three framings differ only in what surrounds the deflate blocks, so the
/// blocks themselves must be identical and the wrappers must be the right size.
#[test]
fn wrappers_frame_the_same_blocks() {
    let input = texty(20_000);
    for level in [1, 6, 9] {
        let raw = compress(&input, level, Wrap::Raw);
        let zlib = compress(&input, level, Wrap::Zlib);
        let gzip = compress(&input, level, Wrap::Gzip);

        assert_eq!(&zlib[2..zlib.len() - 4], &raw[..], "zlib wraps the raw blocks in 2 + 4 bytes");
        assert_eq!(
            &gzip[10..gzip.len() - 8],
            &raw[..],
            "gzip wraps the raw blocks in 10 + 8 bytes"
        );

        // The gzip trailer git writes: CRC-32 then the input length, both little-endian.
        let tail = &gzip[gzip.len() - 8..];
        assert_eq!(u32::from_le_bytes(tail[..4].try_into().unwrap()), crc32(0, &input));
        assert_eq!(u32::from_le_bytes(tail[4..].try_into().unwrap()), input.len() as u32);
        // `deflateSetHeader(&gzhead)` with git's `{ .os = 3 }` and MTIME zero.
        assert_eq!(&gzip[..4], &[0x1f, 0x8b, 0x08, 0x00], "gzip magic, deflate, no optional fields");
        assert_eq!(&gzip[4..8], &[0, 0, 0, 0], "MTIME is zero");
        assert_eq!(gzip[9], 3, "OS is Unix");
    }
}

/// CRC-32's published check value, so the table is pinned by something other than
/// our own output.
#[test]
fn crc32_matches_the_published_check_value() {
    assert_eq!(crc32(0, b"123456789"), 0xcbf4_3926);
    assert_eq!(crc32(0, b""), 0);
}

/// A caller that drains a few bytes at a time must get the same stream as one that
/// hands over a buffer big enough for all of it — the `z_stream` contract that
/// `Compress` and the archive writer both rely on.
#[test]
fn a_tiny_output_buffer_produces_the_same_stream() {
    let input = texty(30_000);
    for level in [1, 6, 9] {
        let want = compress(&input, level, Wrap::Zlib);

        let mut z = Deflate::new(level, Wrap::Zlib);
        let mut got = Vec::new();
        let mut small = [0u8; 7];
        let mut consumed = 0usize;
        loop {
            z.set_input(input.len() - consumed);
            z.set_output(small.len());
            let status = z.step(&input[consumed..], &mut small, Z_FINISH);
            got.extend_from_slice(&small[..z.out_pos()]);
            consumed = input.len() - z.avail_in();
            if status == Z_STREAM_END {
                break;
            }
        }
        assert_eq!(got, want, "level {level} through a 7-byte output buffer");
    }
}
