//! Changed-path Bloom filters, ported from git 2.55.0 `bloom.c` / `bloom.h`.
//!
//! # What a filter answers
//!
//! One filter per commit records the set of paths that commit changed relative
//! to its first parent, so `git log -- <path>` can skip a commit without
//! reading either of its trees. It is a Bloom filter, so "no" is certain and
//! "yes" may be wrong; a reader that gets "yes" still does the tree diff.
//!
//! Every changed path is entered together with each of its leading
//! directories — `dir/sub/file` also enters `dir/sub` and `dir` — which is what
//! makes the filter answer for a directory pathspec and not just a file.
//!
//! # Why two hash versions exist
//!
//! `murmur3_seeded_v1` reads each byte through C's `char`, which is signed on
//! most platforms, so bytes `>= 0x80` sign-extend and land on different bits
//! than the algorithm calls for. Version 2 casts through `unsigned char` and is
//! correct. Both are kept because version 1 filters exist in repositories
//! already and a reader must reproduce the bug exactly to match them; that is
//! why [`murmur3_seeded_v1`] deliberately sign-extends via `i8`.
//!
//! # Sizes and the two truncations
//!
//! A filter is `ceil(n * bits_per_entry / 8)` bytes for `n` distinct paths. Two
//! cases do not get a real filter, and both are one byte so the format's
//! per-commit lengths stay meaningful:
//!
//! * a commit that changed nothing gets `0x00`, which answers "no" to
//!   everything, and
//! * a commit that changed more than `max_changed_paths` paths gets `0xff`,
//!   which answers "yes" to everything and so costs a reader only the diff it
//!   would have done anyway.

/// git's `BITS_PER_WORD`: filter data is addressed a byte at a time.
const BITS_PER_WORD: u32 = 8;

/// git's `DEFAULT_BLOOM_MAX_CHANGES`.
pub const DEFAULT_MAX_CHANGED_PATHS: u32 = 512;

/// The two seeds from `bloom_key_fill()`, which the format documents so that
/// any reader can reproduce them.
const SEED0: u32 = 0x293a_e76f;
const SEED1: u32 = 0x7e64_6e2c;

/// git's `struct bloom_filter_settings`, and the three fields of it that the
/// `BDAT` chunk header carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// Which murmur3 to hash with: 1 reproduces the sign-extension bug, 2 does
    /// not. Nothing else is writable.
    pub hash_version: u32,
    /// How many bit positions one path sets, git's `num_hashes`.
    pub num_hashes: u32,
    /// How many bits each path is given when sizing a filter.
    pub bits_per_entry: u32,
    /// Above this many changed paths a commit gets the all-ones filter instead.
    /// Not stored in the file.
    pub max_changed_paths: u32,
}

impl Default for Settings {
    /// git's `DEFAULT_BLOOM_FILTER_SETTINGS`.
    fn default() -> Self {
        Settings {
            hash_version: 1,
            num_hashes: 7,
            bits_per_entry: 10,
            max_changed_paths: DEFAULT_MAX_CHANGED_PATHS,
        }
    }
}

/// git's `rotate_left()`, which masks the count as C's shift operators do not.
fn rotate_left(value: u32, count: u32) -> u32 {
    value.rotate_left(count & 31)
}

/// The tail-and-finalize half of murmur3, identical between the two versions.
fn finalize(mut seed: u32, len: usize) -> u32 {
    seed ^= len as u32;
    seed ^= seed >> 16;
    seed = seed.wrapping_mul(0x85eb_ca6b);
    seed ^= seed >> 13;
    seed = seed.wrapping_mul(0xc2b2_ae35);
    seed ^= seed >> 16;
    seed
}

/// git's `murmur3_seeded_v2()`: 32-bit murmur3 over `data`, reading bytes
/// unsigned, which is what the algorithm actually specifies.
pub fn murmur3_seeded_v2(mut seed: u32, data: &[u8]) -> u32 {
    const C1: u32 = 0xcc9e_2d51;
    const C2: u32 = 0x1b87_3593;

    let len4 = data.len() / 4;
    for i in 0..len4 {
        let mut k = u32::from_le_bytes([data[4 * i], data[4 * i + 1], data[4 * i + 2], data[4 * i + 3]]);
        k = k.wrapping_mul(C1);
        k = rotate_left(k, 15);
        k = k.wrapping_mul(C2);
        seed ^= k;
        seed = rotate_left(seed, 13).wrapping_mul(5).wrapping_add(0xe654_6b64);
    }

    let tail = &data[len4 * 4..];
    let mut k1 = 0u32;
    if !tail.is_empty() {
        if tail.len() >= 3 {
            k1 ^= u32::from(tail[2]) << 16;
        }
        if tail.len() >= 2 {
            k1 ^= u32::from(tail[1]) << 8;
        }
        k1 ^= u32::from(tail[0]);
        k1 = k1.wrapping_mul(C1);
        k1 = rotate_left(k1, 15);
        k1 = k1.wrapping_mul(C2);
        seed ^= k1;
    }

    finalize(seed, data.len())
}

/// git's `murmur3_seeded_v1()`: the same hash reading bytes through a signed
/// `char`, so anything `>= 0x80` sign-extends into the high bits.
///
/// The `i8` casts are the bug, reproduced on purpose: a version 1 filter in an
/// existing repository was built this way, and a reader that hashed correctly
/// would miss every path with a byte above 0x7f in it.
pub fn murmur3_seeded_v1(mut seed: u32, data: &[u8]) -> u32 {
    const C1: u32 = 0xcc9e_2d51;
    const C2: u32 = 0x1b87_3593;

    /// One byte as C's signed `char` widened to `uint32_t`.
    fn signed(byte: u8) -> u32 {
        byte as i8 as i32 as u32
    }

    let len4 = data.len() / 4;
    for i in 0..len4 {
        let k = signed(data[4 * i])
            | signed(data[4 * i + 1]) << 8
            | signed(data[4 * i + 2]) << 16
            | signed(data[4 * i + 3]) << 24;
        let mut k = k.wrapping_mul(C1);
        k = rotate_left(k, 15);
        k = k.wrapping_mul(C2);
        seed ^= k;
        seed = rotate_left(seed, 13).wrapping_mul(5).wrapping_add(0xe654_6b64);
    }

    let tail = &data[len4 * 4..];
    let mut k1 = 0u32;
    if !tail.is_empty() {
        if tail.len() >= 3 {
            k1 ^= signed(tail[2]) << 16;
        }
        if tail.len() >= 2 {
            k1 ^= signed(tail[1]) << 8;
        }
        k1 ^= signed(tail[0]);
        k1 = k1.wrapping_mul(C1);
        k1 = rotate_left(k1, 15);
        k1 = k1.wrapping_mul(C2);
        seed ^= k1;
    }

    finalize(seed, data.len())
}

/// The `num_hashes` bit positions one path claims; git's `struct bloom_key`
/// filled by `bloom_key_fill()`.
///
/// git derives all of them from two hashes as `hash0 + i * hash1`, the double
/// hashing of Kirsch and Mitzenmacher, so a path costs two murmur3 passes
/// rather than `num_hashes` of them.
#[derive(Debug, Clone)]
pub struct Key {
    hashes: Vec<u32>,
}

impl Key {
    /// git's `bloom_key_fill()`.
    pub fn fill(data: &[u8], settings: &Settings) -> Self {
        let (hash0, hash1) = if settings.hash_version == 2 {
            (murmur3_seeded_v2(SEED0, data), murmur3_seeded_v2(SEED1, data))
        } else {
            (murmur3_seeded_v1(SEED0, data), murmur3_seeded_v1(SEED1, data))
        };
        Key {
            hashes: (0..settings.num_hashes)
                .map(|i| hash0.wrapping_add(i.wrapping_mul(hash1)))
                .collect(),
        }
    }
}

/// One commit's filter; git's `struct bloom_filter`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    /// The bits, one byte at a time, exactly as they go into `BDAT`.
    pub data: Vec<u8>,
}

impl Filter {
    /// git's `init_truncated_large_filter()`: one all-ones byte, which answers
    /// "maybe" for every path and so never hides a commit from a reader.
    pub fn truncated_large() -> Self {
        Filter { data: vec![0xff] }
    }

    /// git's `BLOOM_TRUNC_EMPTY` case: one all-zero byte, which answers "no"
    /// for every path.
    pub fn empty() -> Self {
        Filter { data: vec![0] }
    }

    /// git's `add_key_to_filter()`.
    fn add_key(&mut self, key: &Key) {
        let modulus = self.data.len() as u64 * u64::from(BITS_PER_WORD);
        for &hash in &key.hashes {
            let hash_mod = u64::from(hash) % modulus;
            let block = (hash_mod / u64::from(BITS_PER_WORD)) as usize;
            self.data[block] |= 1u8 << (hash_mod % u64::from(BITS_PER_WORD));
        }
    }

    /// git's `bloom_filter_contains()`: `false` is certain, `true` may be a
    /// false positive.
    pub fn contains(&self, key: &Key) -> bool {
        let modulus = self.data.len() as u64 * u64::from(BITS_PER_WORD);
        if modulus == 0 {
            return true;
        }
        key.hashes.iter().all(|&hash| {
            let hash_mod = u64::from(hash) % modulus;
            let block = (hash_mod / u64::from(BITS_PER_WORD)) as usize;
            self.data[block] & (1u8 << (hash_mod % u64::from(BITS_PER_WORD))) != 0
        })
    }
}

/// Every path a filter must hold for one commit: each changed path plus each of
/// its leading directories, de-duplicated.
///
/// This is git's inner `do { ... } while (*path)` loop in
/// `get_or_compute_bloom_filter()`, which truncates the path at its last `/`
/// and re-inserts until nothing is left. Directories go in without a trailing
/// slash.
pub fn paths_with_parents<'a>(changed: impl IntoIterator<Item = &'a [u8]>) -> Vec<Vec<u8>> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for path in changed {
        let mut path = path;
        loop {
            if seen.insert(path.to_vec()) {
                out.push(path.to_vec());
            }
            match path.iter().rposition(|&b| b == b'/') {
                Some(at) => path = &path[..at],
                None => break,
            }
        }
    }
    out
}

/// Build the filter for one commit from the paths it changed; the tail of git's
/// `get_or_compute_bloom_filter()` once the diff has been queued.
///
/// `changed` is the diff's post-image paths, without leading directories, which
/// this adds. Both truncations of the doc comment on this module are applied
/// here, and `None` never comes back: every commit gets a filter, even if it is
/// one byte.
pub fn compute(changed: &[&[u8]], settings: &Settings) -> Filter {
    // git checks the raw diff count before expanding directories, then the
    // expanded count after, and truncates on either.
    if changed.len() > settings.max_changed_paths as usize {
        return Filter::truncated_large();
    }
    let paths = paths_with_parents(changed.iter().copied());
    if paths.len() > settings.max_changed_paths as usize {
        return Filter::truncated_large();
    }

    let len = (paths.len() * settings.bits_per_entry as usize + BITS_PER_WORD as usize - 1)
        / BITS_PER_WORD as usize;
    if len == 0 {
        return Filter::empty();
    }

    let mut filter = Filter { data: vec![0; len] };
    for path in &paths {
        filter.add_key(&Key::fill(path, settings));
    }
    filter
}

#[cfg(test)]
mod tests {
    use super::{compute, murmur3_seeded_v1, murmur3_seeded_v2, paths_with_parents, Filter, Key, Settings};

    /// The four `get_murmur3` vectors from git's `t0095-bloom.sh`, which the
    /// test helper produces with `version = 2`.
    ///
    /// The last one is seven bytes that all have their high bit set, and is in
    /// git's suite precisely because it is what separates the two versions.
    #[test]
    fn murmur3_v2_matches_gits_published_vectors() {
        assert_eq!(murmur3_seeded_v2(0, b""), 0x0000_0000);
        assert_eq!(murmur3_seeded_v2(0, b"Hello world!"), 0x627b_0c2c);
        assert_eq!(
            murmur3_seeded_v2(0, b"The quick brown fox jumps over the lazy dog"),
            0x2e4f_f723
        );
        assert_eq!(
            murmur3_seeded_v2(0, b"\x99\xaa\xbb\xcc\xdd\xee\xff"),
            0xa183_ccfd,
            "seven high-bit bytes, git's get_murmur3_seven_highbit"
        );
    }

    /// The `generate_filter` vectors from `t0095-bloom.sh`. The helper uses
    /// git's default settings, so the filter is a fixed two bytes and the keys
    /// are the seven `hash0 + i * hash1` positions.
    #[test]
    fn bloom_keys_match_gits_published_vectors() {
        let settings = Settings::default();
        let case = |data: &[u8], hashes: [u32; 7], bytes: [u8; 2]| {
            let key = Key::fill(data, &settings);
            assert_eq!(key.hashes, hashes, "hashes for {:?}", String::from_utf8_lossy(data));
            let mut filter = Filter {
                data: vec![0; (settings.bits_per_entry as usize + 7) / 8],
            };
            filter.add_key(&key);
            assert_eq!(
                filter.data,
                bytes,
                "filter bytes for {:?}",
                String::from_utf8_lossy(data)
            );
        };
        case(
            b"",
            [
                0x5615_800c, 0x5b96_6560, 0x6117_4ab4, 0x6698_3008, 0x6c19_155c, 0x7199_fab0,
                0x771a_e004,
            ],
            [0x11, 0x11],
        );
        case(
            b" ",
            [
                0xf178_874c, 0x5f3d_6eb6, 0xcd02_5620, 0x3ac7_3d8a, 0xa88c_24f4, 0x1651_0c5e,
                0x8415_f3c8,
            ],
            [0x51, 0x55],
        );
        case(
            b"Hello world!",
            [
                0xb270_de9b, 0x1bb6_f26e, 0x84fd_0641, 0xee43_1a14, 0x5789_2de7, 0xc0cf_41ba,
                0x2a15_558d,
            ],
            [0x92, 0x6c],
        );
        case(
            b"file.txt",
            [
                0x20ab_385b, 0xf523_7fe2, 0xc99b_c769, 0x9e14_0ef0, 0x728c_5677, 0x4704_9dfe,
                0x1b7c_e585,
            ],
            [0xa5, 0x4a],
        );
    }

    /// git's "get bloom filter for commit with 10 changes": ten files in one
    /// directory, which is eleven paths once the directory itself is added.
    #[test]
    fn a_commits_filter_matches_gits_published_vector() {
        let owned: Vec<Vec<u8>> = (0..10).map(|n| format!("smallDir/{n}").into_bytes()).collect();
        let changed: Vec<&[u8]> = owned.iter().map(|p| p.as_slice()).collect();
        let filter = compute(&changed, &Settings::default());
        assert_eq!(
            filter.data,
            vec![0x02, 0xb3, 0xc4, 0xa0, 0x34, 0xe7, 0xfe, 0xeb, 0xcb, 0x47, 0xfe, 0xa0, 0xe8, 0x72],
            "byte-for-byte what git's t0095-bloom.sh expects"
        );
    }

    /// The whole reason two versions exist: a byte above 0x7f sign-extends
    /// under version 1 and does not under version 2.
    #[test]
    fn the_versions_disagree_exactly_when_a_byte_has_its_high_bit_set() {
        assert_eq!(
            murmur3_seeded_v1(0, b"plain/ascii"),
            murmur3_seeded_v2(0, b"plain/ascii"),
            "no byte is above 0x7f, so the sign extension never fires"
        );
        assert_ne!(
            murmur3_seeded_v1(0, "café".as_bytes()),
            murmur3_seeded_v2(0, "café".as_bytes()),
            "0xc3 0xa9 sign-extend under version 1"
        );
    }

    #[test]
    fn every_leading_directory_joins_the_path_that_named_it() {
        let paths = paths_with_parents([&b"dir/sub/file"[..]]);
        assert_eq!(
            paths,
            vec![b"dir/sub/file".to_vec(), b"dir/sub".to_vec(), b"dir".to_vec()],
            "directories enter without a trailing slash, most specific first"
        );
    }

    #[test]
    fn a_shared_parent_is_entered_once() {
        let paths = paths_with_parents([&b"dir/a"[..], &b"dir/b"[..]]);
        assert_eq!(paths, vec![b"dir/a".to_vec(), b"dir".to_vec(), b"dir/b".to_vec()]);
    }

    #[test]
    fn a_path_that_was_added_is_found_and_one_that_was_not_is_usually_absent() {
        let settings = Settings::default();
        let filter = compute(&[b"dir/sub/file"], &settings);
        for present in [&b"dir/sub/file"[..], &b"dir/sub"[..], &b"dir"[..]] {
            assert!(
                filter.contains(&Key::fill(present, &settings)),
                "{} was entered so it must answer yes",
                String::from_utf8_lossy(present)
            );
        }
        let absent = (0..64)
            .filter(|n| {
                let path = format!("other/path{n}");
                !filter.contains(&Key::fill(path.as_bytes(), &settings))
            })
            .count();
        assert!(absent > 32, "a 3-path filter must reject most unrelated paths");
    }

    #[test]
    fn a_commit_that_changed_nothing_rejects_everything() {
        let settings = Settings::default();
        let filter = compute(&[], &settings);
        assert_eq!(filter, Filter::empty(), "one all-zero byte");
        assert!(!filter.contains(&Key::fill(b"anything", &settings)));
    }

    #[test]
    fn a_commit_over_the_limit_accepts_everything_in_one_byte() {
        let settings = Settings {
            max_changed_paths: 4,
            ..Settings::default()
        };
        let owned: Vec<Vec<u8>> = (0..10).map(|n| format!("f{n}").into_bytes()).collect();
        let changed: Vec<&[u8]> = owned.iter().map(|p| p.as_slice()).collect();
        let filter = compute(&changed, &settings);
        assert_eq!(filter, Filter::truncated_large(), "one all-ones byte");
        assert!(
            filter.contains(&Key::fill(b"never/changed", &settings)),
            "the all-ones filter must never hide a commit from a reader"
        );
    }

    #[test]
    fn a_filter_is_sized_at_bits_per_entry_per_distinct_path() {
        let settings = Settings::default();
        // Three distinct paths after directories are added, at 10 bits each,
        // is 30 bits, which is four bytes.
        assert_eq!(compute(&[b"dir/sub/file"], &settings).data.len(), 4);
        // One path with no directory is 10 bits, which is two bytes.
        assert_eq!(compute(&[b"file"], &settings).data.len(), 2);
    }
}
