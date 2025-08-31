//! Helpers to deal with UTF-8 in NFAs.
use crate::automata::nfa::ByteRange;
use crate::codepointset::{CodePointSet, Interval};
use smallvec::SmallVec;

const fn br(start: u8, end: u8) -> ByteRange {
    ByteRange { start, end }
}

// Closed byte ranges for valid UTF-8 sequences of length 1.
const UTF8_BUCKETS_LEN1: &[&[ByteRange]] = &[&[br(0x00, 0x7F)]];

// Closed byte ranges for valid UTF-8 sequences of length 2.
const UTF8_BUCKETS_LEN2: &[&[ByteRange]] = &[&[br(0xC2, 0xDF), br(0x80, 0xBF)]];

// Closed byte ranges for valid UTF-8 sequences of length 3.
#[rustfmt::skip]
const UTF8_BUCKETS_LEN3: &[&[ByteRange]] = &[
    // E0: avoid overlongs -> 2nd byte A0..BF
    &[br(0xE0, 0xE0), br(0xA0, 0xBF), br(0x80, 0xBF)],
    // E1–EC, EE–EF: full continuation ranges
    &[br(0xE1, 0xEC), br(0x80, 0xBF), br(0x80, 0xBF)],
    &[br(0xEE, 0xEF), br(0x80, 0xBF), br(0x80, 0xBF)],
    // ED: exclude surrogates -> 2nd byte 80..9F
    &[br(0xED, 0xED), br(0x80, 0x9F), br(0x80, 0xBF)],
];

// Closed byte ranges for valid UTF-8 sequences of length 4.
#[rustfmt::skip]
const UTF8_BUCKETS_LEN4: &[&[ByteRange]] = &[
    // F0: avoid overlongs -> 2nd byte 90..BF
    &[br(0xF0, 0xF0), br(0x90, 0xBF), br(0x80, 0xBF), br(0x80, 0xBF)],
    // F1–F3: full continuation ranges
    &[br(0xF1, 0xF3), br(0x80, 0xBF), br(0x80, 0xBF), br(0x80, 0xBF)],
    // F4: cap at U+10FFFF -> 2nd byte 80..8F
    &[br(0xF4, 0xF4), br(0x80, 0x8F), br(0x80, 0xBF), br(0x80, 0xBF)],
];

#[inline]
fn buckets_for_len(len: usize) -> &'static [&'static [ByteRange]] {
    match len {
        1 => UTF8_BUCKETS_LEN1,
        2 => UTF8_BUCKETS_LEN2,
        3 => UTF8_BUCKETS_LEN3,
        4 => UTF8_BUCKETS_LEN4,
        _ => panic!("Invalid UTF-8 length"),
    }
}

// Length boundaries for mapping from code points to number of UTF-8 bytes.
// These are closed boundaries (<=).
pub const UTF8_LENGTH_BOUNDARIES: [u32; 4] = [0x007F, 0x07FF, 0xFFFF, 0x10FFFF];

struct Trie<'a> {
    // Code point set we're processing.
    cps: &'a CodePointSet,

    // Disjoint sequence of sequences of byte ranges.
    // Each interior sequence of bytes ranges encodes successful paths through the NFA.
    paths: Vec<SmallVec<[ByteRange; 4]>>,
}

impl Trie<'_> {
    fn process_bucket(&mut self, bucket: &[ByteRange]) {
        // Process a single bucket.
    }
}

pub(super) fn code_point_set_to_trie(cps: &CodePointSet) {
    let mut trie = Trie {
        cps,
        paths: Vec::new(),
    };
    let mut interval_start = 0;
    for (idx, &interval_end) in UTF8_LENGTH_BOUNDARIES.iter().enumerate() {
        // Construct an interval of all of the code points with the given byte count.
        let byte_count = idx + 1;
        let interval = Interval::new(interval_start, interval_end);
        let byte_buckets = buckets_for_len(byte_count);
        for bucket in byte_buckets {
            trie.process_bucket(bucket);
        }

        interval_start = interval_end + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_len_increases_at_boundaries() {
        // Test UTF8_LENGTH_BOUNDARIES.
        fn utf8_len(cp: u32) -> usize {
            char::from_u32(cp).expect("valid scalar").len_utf8()
        }

        let mut expected_len = 1;
        for &b in &UTF8_LENGTH_BOUNDARIES {
            assert_eq!(utf8_len(b - 1), expected_len);
            assert_eq!(utf8_len(b), expected_len);
            assert_eq!(utf8_len(b + 1), expected_len + 1);
            expected_len += 1;
        }
    }
}
