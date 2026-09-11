//! Building EWAH bitmaps, ported from git 2.55.0 `ewah/ewah_bitmap.c` and
//! `ewah/bitmap.c`.
//!
//! The decoder in the parent module reads what git writes; this writes what git
//! reads. Both halves share one on-disk shape — a bit count, a run of
//! compressed 64-bit words, and the offset of the last run-length word — so a
//! bitmap built here round-trips through [`super::decode()`] and is accepted by
//! git's `ewah_read_mmap()` unchanged.
//!
//! The port is line-faithful. The one thing Rust forces is that git's
//! `self->rlw` is a pointer into the word buffer while [`Builder::rlw`] is an
//! index into the same buffer, which is also exactly what the serialized form
//! stores.

use super::Vec as EwahVec;

/// How many bits one compressed word covers; git's `BITS_IN_EWORD`.
const BITS_IN_EWORD: usize = 64;
/// Width of the running-length field inside a run-length word.
const RLW_RUNNING_BITS: u32 = 32;
/// Width of the literal-word-count field, which takes the rest of the word
/// after the run bit and the running length.
const RLW_LITERAL_BITS: u32 = 64 - 1 - RLW_RUNNING_BITS;
const RLW_LARGEST_RUNNING_COUNT: u64 = (1 << RLW_RUNNING_BITS) - 1;
const RLW_LARGEST_LITERAL_COUNT: u64 = (1 << RLW_LITERAL_BITS) - 1;
const RLW_LARGEST_RUNNING_COUNT_SHIFT: u64 = RLW_LARGEST_RUNNING_COUNT << 1;
const RLW_RUNNING_LEN_PLUS_BIT: u64 = (1u64 << (RLW_RUNNING_BITS + 1)) - 1;

fn rlw_get_run_bit(word: u64) -> bool {
    word & 1 == 1
}

fn rlw_set_run_bit(word: &mut u64, bit: bool) {
    if bit {
        *word |= 1;
    } else {
        *word &= !1;
    }
}

fn rlw_get_running_len(word: u64) -> u64 {
    (word >> 1) & RLW_LARGEST_RUNNING_COUNT
}

fn rlw_set_running_len(word: &mut u64, len: u64) {
    *word |= RLW_LARGEST_RUNNING_COUNT_SHIFT;
    *word &= (len << 1) | !RLW_LARGEST_RUNNING_COUNT_SHIFT;
}

fn rlw_get_literal_words(word: u64) -> u64 {
    word >> (1 + RLW_RUNNING_BITS)
}

fn rlw_set_literal_words(word: &mut u64, len: u64) {
    *word |= !RLW_RUNNING_LEN_PLUS_BIT;
    *word &= (len << (RLW_RUNNING_BITS + 1)) | RLW_RUNNING_LEN_PLUS_BIT;
}

fn rlw_size(word: u64) -> u64 {
    rlw_get_running_len(word) + rlw_get_literal_words(word)
}

/// An EWAH bitmap under construction; git's `struct ewah_bitmap`.
///
/// Bits are appended in increasing order — [`set()`](Builder::set) refuses to
/// go backwards, as git's own `assert(i >= self->bit_size)` does — because the
/// encoding is a forward-only stream of runs and literal words.
#[derive(Clone)]
pub struct Builder {
    buffer: std::vec::Vec<u64>,
    bit_size: usize,
    /// Index into `buffer` of the run-length word currently being extended.
    rlw: usize,
}

impl Default for Builder {
    fn default() -> Self {
        Builder::new()
    }
}

impl Builder {
    /// An empty bitmap, git's `ewah_new()` followed by `ewah_clear()`: one
    /// zeroed run-length word and nothing else.
    pub fn new() -> Self {
        Builder {
            buffer: vec![0],
            bit_size: 0,
            rlw: 0,
        }
    }

    /// How many bits the bitmap covers, which is one past the highest bit ever
    /// set rounded up to whole words by the run and literal encoding.
    pub fn num_bits(&self) -> usize {
        self.bit_size
    }

    /// How many compressed words the bitmap occupies, git's `buffer_size`. The
    /// delta search over XOR candidates compares exactly this.
    pub fn word_count(&self) -> usize {
        self.buffer.len()
    }

    fn buffer_push(&mut self, value: u64) {
        self.buffer.push(value);
    }

    fn buffer_push_rlw(&mut self, value: u64) {
        self.buffer.push(value);
        self.rlw = self.buffer.len() - 1;
    }

    /// git's `add_empty_words()`: extend the current run, or start new
    /// run-length words until `number` empty words of value `v` are recorded.
    fn add_empty_words_inner(&mut self, v: bool, mut number: u64) {
        let rlw = self.buffer[self.rlw];
        if rlw_get_run_bit(rlw) != v && rlw_size(rlw) == 0 {
            rlw_set_run_bit(&mut self.buffer[self.rlw], v);
        } else if rlw_get_literal_words(rlw) != 0 || rlw_get_run_bit(rlw) != v {
            self.buffer_push_rlw(0);
            if v {
                rlw_set_run_bit(&mut self.buffer[self.rlw], v);
            }
        }

        let runlen = rlw_get_running_len(self.buffer[self.rlw]);
        let can_add = number.min(RLW_LARGEST_RUNNING_COUNT - runlen);
        rlw_set_running_len(&mut self.buffer[self.rlw], runlen + can_add);
        number -= can_add;

        while number >= RLW_LARGEST_RUNNING_COUNT {
            self.buffer_push_rlw(0);
            if v {
                rlw_set_run_bit(&mut self.buffer[self.rlw], v);
            }
            rlw_set_running_len(&mut self.buffer[self.rlw], RLW_LARGEST_RUNNING_COUNT);
            number -= RLW_LARGEST_RUNNING_COUNT;
        }

        if number > 0 {
            self.buffer_push_rlw(0);
            if v {
                rlw_set_run_bit(&mut self.buffer[self.rlw], v);
            }
            rlw_set_running_len(&mut self.buffer[self.rlw], number);
        }
    }

    /// Append `number` words that are all-zero (`v` false) or all-one, git's
    /// `ewah_add_empty_words()`.
    pub fn add_empty_words(&mut self, v: bool, number: u64) {
        if number == 0 {
            return;
        }
        self.bit_size += (number as usize) * BITS_IN_EWORD;
        self.add_empty_words_inner(v, number);
    }

    /// git's `add_literal()`: record one word verbatim behind the current
    /// run-length word, starting a new one once the literal count saturates.
    fn add_literal(&mut self, new_data: u64) {
        let current = rlw_get_literal_words(self.buffer[self.rlw]);
        if current >= RLW_LARGEST_LITERAL_COUNT {
            self.buffer_push_rlw(0);
            rlw_set_literal_words(&mut self.buffer[self.rlw], 1);
            self.buffer_push(new_data);
            return;
        }
        rlw_set_literal_words(&mut self.buffer[self.rlw], current + 1);
        self.buffer_push(new_data);
    }

    /// git's `add_empty_word()`, the single-word case of a run.
    fn add_empty_word(&mut self, v: bool) {
        let rlw = self.buffer[self.rlw];
        let no_literal = rlw_get_literal_words(rlw) == 0;
        let run_len = rlw_get_running_len(rlw);

        if no_literal && run_len == 0 {
            rlw_set_run_bit(&mut self.buffer[self.rlw], v);
        }

        if no_literal && rlw_get_run_bit(self.buffer[self.rlw]) == v && run_len < RLW_LARGEST_RUNNING_COUNT
        {
            rlw_set_running_len(&mut self.buffer[self.rlw], run_len + 1);
        } else {
            self.buffer_push_rlw(0);
            rlw_set_run_bit(&mut self.buffer[self.rlw], v);
            rlw_set_running_len(&mut self.buffer[self.rlw], 1);
        }
    }

    /// Append one whole word, git's `ewah_add()`: all-zero and all-one words
    /// become runs, everything else a literal.
    pub fn add(&mut self, word: u64) {
        self.bit_size += BITS_IN_EWORD;
        if word == 0 {
            self.add_empty_word(false);
        } else if word == u64::MAX {
            self.add_empty_word(true);
        } else {
            self.add_literal(word);
        }
    }

    /// Set bit `i`, git's `ewah_set()`.
    ///
    /// Bits must be set in increasing order; a bit at or below the current
    /// [`num_bits()`](Builder::num_bits) is ignored rather than corrupting the
    /// stream, which is where git asserts.
    pub fn set(&mut self, i: usize) {
        if i < self.bit_size {
            return;
        }
        let dist = (i + 1).div_ceil(BITS_IN_EWORD) - self.bit_size.div_ceil(BITS_IN_EWORD);
        self.bit_size = i + 1;

        if dist > 0 {
            if dist > 1 {
                self.add_empty_words_inner(false, dist as u64 - 1);
            }
            self.add_literal(1u64 << (i % BITS_IN_EWORD));
            return;
        }

        if rlw_get_literal_words(self.buffer[self.rlw]) == 0 {
            let running = rlw_get_running_len(self.buffer[self.rlw]);
            rlw_set_running_len(&mut self.buffer[self.rlw], running - 1);
            self.add_literal(1u64 << (i % BITS_IN_EWORD));
            return;
        }

        let last = self.buffer.len() - 1;
        self.buffer[last] |= 1u64 << (i % BITS_IN_EWORD);

        // Check if we just completed a stream of 1s.
        if self.buffer[last] == u64::MAX {
            self.buffer.truncate(last);
            let literals = rlw_get_literal_words(self.buffer[self.rlw]);
            rlw_set_literal_words(&mut self.buffer[self.rlw], literals - 1);
            self.add_empty_word(true);
        }
    }

    /// Compress a plain bitmap given as its raw 64-bit words, git's
    /// `bitmap_to_ewah()`.
    ///
    /// Trailing all-zero words are dropped, so the result covers only as many
    /// bits as the highest set one needs — plus, exactly as in git, one final
    /// empty word when the input is all zeroes.
    pub fn from_bitmap_words(words: &[u64]) -> Self {
        let mut ewah = Builder::new();
        let mut running_empty_words: u64 = 0;
        let mut last_word: u64 = 0;

        for &word in words {
            if word == 0 {
                running_empty_words += 1;
                continue;
            }
            if last_word != 0 {
                ewah.add(last_word);
            }
            if running_empty_words > 0 {
                ewah.add_empty_words(false, running_empty_words);
                running_empty_words = 0;
            }
            last_word = word;
        }

        ewah.add(last_word);
        ewah
    }

    /// Serialize as git's `ewah_serialize_to()` does, appending to `out`.
    pub fn write_to(&self, out: &mut std::vec::Vec<u8>) {
        out.extend_from_slice(&(self.bit_size as u32).to_be_bytes());
        out.extend_from_slice(&(self.buffer.len() as u32).to_be_bytes());
        for word in &self.buffer {
            out.extend_from_slice(&word.to_be_bytes());
        }
        out.extend_from_slice(&(self.rlw as u32).to_be_bytes());
    }

    /// The same bitmap as the decoder's type, so a freshly built bitmap can be
    /// read back with the crate's own accessors.
    pub fn to_vec(&self) -> EwahVec {
        EwahVec {
            num_bits: self.bit_size as u32,
            bits: self.buffer.clone(),
            rlw: self.rlw as u64,
        }
    }

    /// A bitmap that was read back off disk, in the form the builder operates
    /// on — git keeps one `struct ewah_bitmap` for both directions, and so must
    /// anything that XORs a stored bitmap against another.
    pub fn from_decoded(decoded: &EwahVec) -> Self {
        Builder {
            buffer: decoded.bits.clone(),
            bit_size: decoded.num_bits as usize,
            rlw: decoded.rlw as usize,
        }
    }

    /// The compressed words themselves, git's `buffer`.
    pub fn words(&self) -> &[u64] {
        &self.buffer
    }

    /// git's `ewah_checksum()` (`ewah/ewah_bitmap.c`:498-508, v2.55.0), the
    /// value `rev-list --test-bitmap` prints for the commit it was handed.
    ///
    /// git runs it over `buffer`'s bytes *in memory*, which are host order and
    /// not the big-endian order the same words take on disk, so this reads each
    /// word back out in native order to agree with it.
    pub fn checksum(&self) -> u32 {
        let mut crc = self.bit_size as u32;
        for word in &self.buffer {
            for byte in word.to_ne_bytes() {
                crc = crc.wrapping_shl(5).wrapping_sub(crc).wrapping_add(u32::from(byte));
            }
        }
        crc
    }

    /// git's `ewah_to_bitmap()` (`ewah/bitmap.c`:102-118, v2.55.0): expand the
    /// compressed stream into the plain 64-bit words a `struct bitmap` holds.
    pub fn to_bitmap_words(&self) -> std::vec::Vec<u64> {
        let mut words = std::vec::Vec::new();
        let mut iter = Iter::new(&self.buffer);
        while let Some(word) = iter.next_word() {
            words.push(word);
        }
        words
    }

    /// git's `ewah_add_dirty_words()`: append `number` words verbatim (or
    /// inverted) as literals, without the all-zero/all-one folding
    /// [`add()`](Builder::add) performs.
    fn add_dirty_words(&mut self, buffer: &[u64], mut number: usize, negate: bool) {
        let mut at = 0usize;
        loop {
            let literals = rlw_get_literal_words(self.buffer[self.rlw]);
            let can_add = (number as u64).min(RLW_LARGEST_LITERAL_COUNT - literals);
            rlw_set_literal_words(&mut self.buffer[self.rlw], literals + can_add);

            for offset in 0..can_add as usize {
                let word = buffer[at + offset];
                self.buffer.push(if negate { !word } else { word });
            }

            self.bit_size += can_add as usize * BITS_IN_EWORD;

            if number as u64 - can_add == 0 {
                break;
            }

            self.buffer_push_rlw(0);
            at += can_add as usize;
            number -= can_add as usize;
        }
    }
}

/// git's `bitmap_equals()` (`ewah/bitmap.c`:237-261, v2.55.0): two plain
/// bitmaps are equal when they agree on every word the shorter one has and the
/// longer one is zero past that point.
pub fn bitmap_words_equal(a: &[u64], b: &[u64]) -> bool {
    let (small, big) = if a.len() < b.len() { (a, b) } else { (b, a) };
    small.iter().zip(big).all(|(l, r)| l == r) && big[small.len()..].iter().all(|word| *word == 0)
}

/// How many bits [`Builder::from_bitmap_words`] ends up covering for `words`,
/// without building the bitmap: everything up to the last word that has a bit
/// set, and one empty word for an all-zero input.
fn bits_for(words: &[u64]) -> usize {
    match words.iter().rposition(|&word| word != 0) {
        Some(at) => (at + 1) * BITS_IN_EWORD,
        None => BITS_IN_EWORD,
    }
}

/// git's `ewah_xor()`, computed on the plain word form the two bitmaps were
/// built from rather than on their compressed streams.
///
/// Word for word the result is the same, since XOR distributes over the
/// encoding; what differs is that git's version can leave trailing all-zero
/// words in the output where this drops them. Both decode to the same set of
/// bits, and the bit count is git's `max(bit_size)` either way, so a reader
/// XOR-ing this back against `a` recovers `b` exactly.
pub fn xor_of_bitmap_words(a: &[u64], b: &[u64]) -> Builder {
    let len = a.len().max(b.len());
    let words: std::vec::Vec<u64> = (0..len)
        .map(|at| a.get(at).copied().unwrap_or(0) ^ b.get(at).copied().unwrap_or(0))
        .collect();
    let mut out = Builder::from_bitmap_words(&words);
    out.bit_size = out.bit_size.max(bits_for(a).max(bits_for(b)));
    out
}

#[cfg(test)]
mod tests {
    use super::Builder;

    /// Every bit the builder was told about, recovered through the decoder.
    fn round_trip(builder: &Builder) -> std::vec::Vec<usize> {
        let mut bytes = std::vec::Vec::new();
        builder.write_to(&mut bytes);
        let (decoded, rest) = crate::ewah::decode(&bytes).expect("what we write, we can read");
        assert!(rest.is_empty(), "the encoding is self-delimiting");
        assert_eq!(decoded.num_bits(), builder.num_bits(), "bit count survives the round trip");
        let mut set = std::vec::Vec::new();
        decoded
            .for_each_set_bit(|at| {
                set.push(at);
                Some(())
            })
            .expect("a bitmap we built is well-formed");
        set
    }

    #[test]
    fn set_bits_round_trip_through_the_decoder() {
        for positions in [
            vec![0usize],
            vec![63],
            vec![64],
            vec![0, 1, 2, 3],
            vec![0, 5000],
            (0..64).collect::<std::vec::Vec<_>>(),
            (0..1000).filter(|n| n % 7 == 0).collect(),
            vec![100_000],
        ] {
            let mut builder = Builder::new();
            for &at in &positions {
                builder.set(at);
            }
            assert_eq!(round_trip(&builder), positions, "positions {positions:?}");
        }
    }

    #[test]
    fn a_full_word_of_ones_becomes_a_run() {
        let mut builder = Builder::new();
        for at in 0..128 {
            builder.set(at);
        }
        assert_eq!(round_trip(&builder), (0..128).collect::<std::vec::Vec<_>>());
        assert_eq!(
            builder.word_count(),
            1,
            "two all-one words collapse into a single run-length word"
        );
    }

    #[test]
    fn long_gaps_become_runs_rather_than_literals() {
        let mut builder = Builder::new();
        builder.set(0);
        builder.set(1_000_000);
        assert_eq!(round_trip(&builder), vec![0, 1_000_000]);
        assert!(
            builder.word_count() < 8,
            "a million-bit gap costs a handful of words, not {}",
            builder.word_count()
        );
    }

    #[test]
    fn from_bitmap_words_matches_setting_the_same_bits() {
        let mut words = vec![0u64; 40];
        let mut positions = std::vec::Vec::new();
        for at in [3usize, 64, 65, 130, 1000, 1001, 2000] {
            words[at / 64] |= 1 << (at % 64);
            positions.push(at);
        }
        // A whole word of ones, to exercise the run path.
        words[20] = u64::MAX;
        for bit in 0..64 {
            positions.push(20 * 64 + bit);
        }
        positions.sort_unstable();

        let built = Builder::from_bitmap_words(&words);
        assert_eq!(round_trip(&built), positions);
    }

    #[test]
    fn xor_is_reversible_which_is_what_the_reader_relies_on() {
        // Two overlapping reachability bitmaps of the shape the writer produces:
        // a later commit's bitmap is a superset of an earlier one's.
        let mut earlier = vec![0u64; 30];
        for at in [1usize, 70, 71, 500, 1000] {
            earlier[at / 64] |= 1 << (at % 64);
        }
        let mut later = earlier.clone();
        for at in [3usize, 1500, 1600] {
            later[at / 64] |= 1 << (at % 64);
        }

        let stored = super::xor_of_bitmap_words(&earlier, &later);
        // What a reader does: XOR the stored entry back against its base.
        let recovered = super::xor_of_bitmap_words(
            &{
                let mut words = std::vec::Vec::new();
                let mut ewah = std::vec::Vec::new();
                stored.write_to(&mut ewah);
                let (decoded, _) = crate::ewah::decode(&ewah).expect("readable");
                decoded
                    .for_each_set_bit(|at| {
                        if words.len() <= at / 64 {
                            words.resize(at / 64 + 1, 0u64);
                        }
                        words[at / 64] |= 1 << (at % 64);
                        Some(())
                    })
                    .expect("well-formed");
                words
            },
            &earlier,
        );
        assert_eq!(round_trip(&recovered), round_trip(&Builder::from_bitmap_words(&later)));
    }

    /// The plain 64-bit words a bitmap covers, which is what a reader gets back
    /// out of git's `ewah_to_bitmap()`.
    fn words_of(builder: &Builder) -> std::vec::Vec<u64> {
        builder.to_bitmap_words()
    }

    /// Two bitmaps of the shape a pack's entries take: a shared body of
    /// scattered literal words, long runs between them, and a small delta.
    fn pair() -> (Builder, Builder) {
        let mut earlier = vec![0u64; 24];
        for at in (0..12).step_by(2) {
            earlier[at] = 0x0f0f_0f0f_0f0f_0f0f;
        }
        earlier[20] = u64::MAX;
        let mut later = earlier.clone();
        later[13] = 0x00ff_00ff_00ff_00ff;
        later[21] = u64::MAX;
        (
            Builder::from_bitmap_words(&earlier),
            Builder::from_bitmap_words(&later),
        )
    }

    #[test]
    fn xor_over_the_compressed_streams_agrees_with_xor_over_the_words() {
        let (a, b) = pair();
        let combined = super::xor(&a, &b);
        let (left, right) = (words_of(&a), words_of(&b));
        let expected: std::vec::Vec<u64> = (0..left.len().max(right.len()))
            .map(|at| left.get(at).copied().unwrap_or(0) ^ right.get(at).copied().unwrap_or(0))
            .collect();
        assert!(
            super::bitmap_words_equal(&words_of(&combined), &expected),
            "the run/literal walk must produce the same bits as XOR-ing word by word"
        );
        assert_eq!(
            combined.num_bits(),
            a.num_bits().max(b.num_bits()),
            "git ends with out->bit_size = max(bit_size)"
        );
    }

    #[test]
    fn xor_undoes_itself_which_is_how_a_chained_entry_is_read_back() {
        let (a, b) = pair();
        let stored = super::xor(&a, &b);
        let recovered = super::xor(&stored, &b);
        assert!(
            super::bitmap_words_equal(&words_of(&recovered), &words_of(&a)),
            "XOR-ing a stored entry back against its base recovers the original"
        );
    }

    #[test]
    fn xor_against_an_empty_bitmap_is_the_bitmap_itself() {
        let (a, _) = pair();
        let empty = Builder::from_bitmap_words(&[0u64; 24]);
        assert!(
            super::bitmap_words_equal(&words_of(&super::xor(&a, &empty)), &words_of(&a)),
            "nothing to flip leaves every bit where it was"
        );
    }

    #[test]
    fn xor_of_bitmaps_of_different_lengths_keeps_the_longer_tail() {
        let short = Builder::from_bitmap_words(&{
            let mut words = vec![0u64; 3];
            words[1] = 0x5555_5555_5555_5555;
            words
        });
        let long = Builder::from_bitmap_words(&{
            let mut words = vec![0u64; 40];
            words[1] = 0x5555_5555_5555_5555;
            words[30] = u64::MAX;
            words[39] = 1;
            words
        });
        let combined = super::xor(&short, &long);
        let mut expected = vec![0u64; 40];
        expected[30] = u64::MAX;
        expected[39] = 1;
        assert!(
            super::bitmap_words_equal(&words_of(&combined), &expected),
            "the tail past the shorter stream survives the discharge"
        );
    }

    #[test]
    fn a_bitmap_read_back_off_disk_can_be_xored_like_a_built_one() {
        let (a, b) = pair();
        let mut bytes = std::vec::Vec::new();
        super::xor(&a, &b).write_to(&mut bytes);
        let (decoded, rest) = crate::ewah::decode(&bytes).expect("what we write, we can read");
        assert!(rest.is_empty());
        let stored = Builder::from_decoded(&decoded);
        assert_eq!(
            stored.checksum(),
            super::xor(&a, &b).checksum(),
            "the checksum a reader reports is the one the bytes carry"
        );
        assert!(super::bitmap_words_equal(
            &words_of(&super::xor(&stored, &b)),
            &words_of(&a)
        ));
    }

    #[test]
    fn an_all_zero_bitmap_is_one_empty_word() {
        let built = Builder::from_bitmap_words(&[0u64; 16]);
        assert_eq!(round_trip(&built), std::vec::Vec::<usize>::new());
        assert_eq!(built.num_bits(), 64, "git emits one empty word for an empty bitmap");
    }
}

/// git's `struct ewah_iterator`: walks a compressed stream one uncompressed
/// 64-bit word at a time, expanding runs as it goes (`ewah/ewah_bitmap.c`
/// :305-372, v2.55.0).
struct Iter<'a> {
    buffer: &'a [u64],
    buffer_size: usize,
    pointer: usize,
    /// Literal words in the current run-length word, and how many are spent.
    lw: u64,
    literals: u64,
    /// Running length of the current run-length word, and how much is spent.
    rl: u64,
    compressed: u64,
    /// The value the run repeats.
    b: bool,
}

impl<'a> Iter<'a> {
    /// git's `ewah_iterator_init()`.
    fn new(buffer: &'a [u64]) -> Self {
        let mut it = Iter {
            buffer,
            buffer_size: buffer.len(),
            pointer: 0,
            lw: 0,
            literals: 0,
            rl: 0,
            compressed: 0,
            b: false,
        };
        if it.pointer < it.buffer_size {
            it.read_new_rlw();
        }
        it
    }

    /// git's `read_new_rlw()`: skip run-length words that describe nothing.
    fn read_new_rlw(&mut self) {
        self.literals = 0;
        self.compressed = 0;
        loop {
            let word = self.buffer[self.pointer];
            self.rl = rlw_get_running_len(word);
            self.lw = rlw_get_literal_words(word);
            self.b = rlw_get_run_bit(word);

            if self.rl != 0 || self.lw != 0 {
                return;
            }
            if self.pointer < self.buffer_size - 1 {
                self.pointer += 1;
            } else {
                self.pointer = self.buffer_size;
                return;
            }
        }
    }

    /// git's `ewah_iterator_next()`.
    fn next_word(&mut self) -> Option<u64> {
        if self.pointer >= self.buffer_size {
            return None;
        }

        let next = if self.compressed < self.rl {
            self.compressed += 1;
            if self.b { u64::MAX } else { 0 }
        } else {
            self.literals += 1;
            self.pointer += 1;
            self.buffer[self.pointer]
        };

        if self.compressed == self.rl && self.literals == self.lw {
            self.pointer += 1;
            if self.pointer < self.buffer_size {
                self.read_new_rlw();
            }
        }

        Some(next)
    }
}

/// git's `struct rlw_iterator` (`ewah/ewok_rlw.h`:84-112, v2.55.0), which walks
/// the compressed stream in whole run-length words rather than expanding it.
struct RlwIter<'a> {
    buffer: &'a [u64],
    size: usize,
    pointer: usize,
    literal_word_start: usize,
    literal_words: u64,
    running_len: u64,
    literal_word_offset: usize,
    running_bit: bool,
}

impl<'a> RlwIter<'a> {
    /// git's `rlwit_init()`.
    fn new(buffer: &'a [u64]) -> Self {
        let mut it = RlwIter {
            buffer,
            size: buffer.len(),
            pointer: 0,
            literal_word_start: 0,
            literal_words: 0,
            running_len: 0,
            literal_word_offset: 0,
            running_bit: false,
        };
        it.next_word();
        it.literal_word_start = it.literal_words_start() + it.literal_word_offset;
        it
    }

    /// git's `rlwit_literal_words()`: where the literal words of the current
    /// run-length word begin.
    fn literal_words_start(&self) -> usize {
        self.pointer - self.literal_words as usize
    }

    /// git's `rlwit_word_size()`.
    fn word_size(&self) -> u64 {
        self.running_len + self.literal_words
    }

    /// git's `next_word()`; false once the stream is exhausted, leaving the
    /// current run-length word's fields untouched exactly as the C does.
    fn next_word(&mut self) -> bool {
        if self.pointer >= self.size {
            return false;
        }
        let word = self.buffer[self.pointer];
        self.pointer += rlw_get_literal_words(word) as usize + 1;
        self.literal_words = rlw_get_literal_words(word);
        self.running_len = rlw_get_running_len(word);
        self.running_bit = rlw_get_run_bit(word);
        self.literal_word_offset = 0;
        true
    }

    /// git's `rlwit_discard_first_words()`.
    fn discard_first_words(&mut self, mut x: u64) {
        while x > 0 {
            if self.running_len > x {
                self.running_len -= x;
                return;
            }

            x -= self.running_len;
            self.running_len = 0;

            let discard = x.min(self.literal_words);
            self.literal_word_start += discard as usize;
            self.literal_words -= discard;
            x -= discard;

            if x > 0 || self.word_size() == 0 {
                if !self.next_word() {
                    break;
                }
                self.literal_word_start = self.literal_words_start() + self.literal_word_offset;
            }
        }
    }

    /// git's `rlwit_discharge()`: copy at most `max` words into `out`,
    /// optionally inverted, and report how many were copied.
    fn discharge(&mut self, out: &mut Builder, max: u64, negate: bool) -> u64 {
        let mut index = 0u64;

        while index < max && self.word_size() > 0 {
            let mut pl = self.running_len;
            if index + pl > max {
                pl = max - index;
            }

            out.add_empty_words(self.running_bit ^ negate, pl);
            index += pl;

            let mut pd = self.literal_words;
            if pd + index > max {
                pd = max - index;
            }

            out.add_dirty_words(&self.buffer[self.literal_word_start..], pd as usize, negate);

            self.discard_first_words(pd + pl);
            index += pd;
        }

        index
    }
}

/// git's `ewah_xor()` (`ewah/ewah_bitmap.c`:407-469, v2.55.0), computed on the
/// two compressed streams exactly as git computes it.
///
/// The compressed shape of the result — and so its word count, and so the
/// `ewah_checksum()` that `rev-list --test-bitmap` prints — depends on which
/// branch of the run/literal interplay produced each word, which is why this
/// walks the streams rather than XOR-ing the expanded words: a literal word
/// discharged against a run stays a literal here even when it is all-zero,
/// where re-compressing would fold it into a run.
pub fn xor(a: &Builder, b: &Builder) -> Builder {
    let mut out = Builder::new();
    let mut rlw_i = RlwIter::new(&a.buffer);
    let mut rlw_j = RlwIter::new(&b.buffer);

    while rlw_i.word_size() > 0 && rlw_j.word_size() > 0 {
        while rlw_i.running_len > 0 || rlw_j.running_len > 0 {
            let i_is_prey = rlw_i.running_len < rlw_j.running_len;
            let (predator_running_len, negate_words) = if i_is_prey {
                (rlw_j.running_len, rlw_j.running_bit)
            } else {
                (rlw_i.running_len, rlw_i.running_bit)
            };

            let index = if i_is_prey {
                rlw_i.discharge(&mut out, predator_running_len, negate_words)
            } else {
                rlw_j.discharge(&mut out, predator_running_len, negate_words)
            };

            out.add_empty_words(negate_words, predator_running_len - index);

            if i_is_prey {
                rlw_j.discard_first_words(predator_running_len);
            } else {
                rlw_i.discard_first_words(predator_running_len);
            }
        }

        let literals = rlw_i.literal_words.min(rlw_j.literal_words);

        if literals > 0 {
            for k in 0..literals as usize {
                out.add(rlw_i.buffer[rlw_i.literal_word_start + k] ^ rlw_j.buffer[rlw_j.literal_word_start + k]);
            }

            rlw_i.discard_first_words(literals);
            rlw_j.discard_first_words(literals);
        }
    }

    if rlw_i.word_size() > 0 {
        rlw_i.discharge(&mut out, u64::MAX, false);
    } else {
        rlw_j.discharge(&mut out, u64::MAX, false);
    }

    out.bit_size = a.bit_size.max(b.bit_size);
    out
}
