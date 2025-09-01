//! Helpers to deal with UTF-8 in NFAs.
use crate::automata::nfa::ByteRange;
use crate::codepointset::{CodePointSet, Interval};
use smallvec::SmallVec;

const fn br(start: u8, end: u8) -> ByteRange {
    ByteRange { start, end }
}

/// A UTF-8 structural bucket: the exact code-point span it covers and
/// the per-byte closed ranges that encode that span.
#[derive(Copy, Clone)]
pub struct Utf8Bucket {
    pub ivs: Interval,                     // closed code-point interval
    pub byte_ranges: &'static [ByteRange], // per-byte closed ranges
}

// ---- Length 1 (U+0000..U+007F) ---------------------------------------------
#[rustfmt::skip]
pub const UTF8_BUCKETS_LEN1: &[Utf8Bucket] = &[Utf8Bucket {
    ivs: Interval::new(0x0000, 0x007F),
    byte_ranges:&[br(0x00, 0x7F)],
}];

// ---- Length 2 (U+0080..U+07FF) --------------------------------------------
#[rustfmt::skip]
pub const UTF8_BUCKETS_LEN2: &[Utf8Bucket] = &[Utf8Bucket {
    ivs: Interval::new(0x0080, 0x07FF),
    byte_ranges:&[br(0xC2, 0xDF), br(0x80, 0xBF)],
}];

// ---- Length 3 (split by lead-byte buckets)
#[rustfmt::skip]
pub const UTF8_BUCKETS_LEN3: &[Utf8Bucket] = &[
    // E0: avoid overlongs -> A0..BF
    Utf8Bucket {
        ivs: Interval::new(0x0800, 0x0FFF),
        byte_ranges:&[br(0xE0, 0xE0), br(0xA0, 0xBF), br(0x80, 0xBF)],
    },
    // E1–EC: full continuations
    Utf8Bucket {
        ivs: Interval::new(0x1000, 0xCFFF),
        byte_ranges:&[br(0xE1, 0xEC), br(0x80, 0xBF), br(0x80, 0xBF)],
    },
    // ED: (surrogates) clamp second byte 80..9F
    Utf8Bucket {
        ivs: Interval::new(0xD000, 0xD7FF),
        byte_ranges:&[br(0xED, 0xED), br(0x80, 0x9F), br(0x80, 0xBF)],
    },
    // EE–EF: full continuations
    Utf8Bucket {
        ivs: Interval::new(0xE000, 0xFFFF),
        byte_ranges:&[br(0xEE, 0xEF), br(0x80, 0xBF), br(0x80, 0xBF)],
    },
];

// ---- Length 4
#[rustfmt::skip]
pub const UTF8_BUCKETS_LEN4: &[Utf8Bucket] = &[
    // F0: avoid overlongs -> 2nd byte 90..BF
    Utf8Bucket {
        ivs: Interval::new(0x1_0000, 0x3_FFFF),
        byte_ranges:&[
            br(0xF0, 0xF0), br(0x90, 0xBF), br(0x80, 0xBF), br(0x80, 0xBF)
        ],
    },
    // F1–F3: full continuations
    Utf8Bucket {
        ivs: Interval::new(0x4_0000, 0xF_FFFF),
        byte_ranges:&[
            br(0xF1, 0xF3), br(0x80, 0xBF), br(0x80, 0xBF), br(0x80, 0xBF)
        ],
    },

    // F4: cap at U+10FFFF -> 2nd byte 80..8F
    Utf8Bucket {
        ivs: Interval::new(0x10_0000, 0x10_FFFF),
        byte_ranges:&[
            br(0xF4, 0xF4), br(0x80, 0x8F), br(0x80, 0xBF), br(0x80, 0xBF)
        ],
    },
];

/// Helper to pick the table for a given UTF-8 length.
#[inline]
pub const fn utf8_buckets_for_len(len: usize) -> &'static [Utf8Bucket] {
    match len {
        1 => UTF8_BUCKETS_LEN1,
        2 => UTF8_BUCKETS_LEN2,
        3 => UTF8_BUCKETS_LEN3,
        4 => UTF8_BUCKETS_LEN4,
        _ => &[],
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
    fn process_bucket(&mut self, bucket: &Utf8Bucket) {
        // Process a single bucket, adding to our trie.
        let mut overlaps = self.cps.intervals_intersecting(bucket.ivs);
        if overlaps.is_empty() {
            return;
        }
        // Handle first and last specially, as they may extend beyond the bucket's range.
        if overlaps[0].first < bucket.ivs.first {
            let mut first_iv = overlaps[0];
            first_iv.first = bucket.ivs.first;
            first_iv.last = first_iv.last.min(bucket.ivs.last);
            self.process_iv_in_bucket(first_iv, &bucket);
            overlaps = &overlaps[1..];
        }
    }

    fn process_iv_in_bucket(&mut self, iv: Interval, bucket: &Utf8Bucket) {
        // Process an interval of code points that are all contained within a given bucket.
        let c1 = char::from_u32(iv.first).unwrap();
        let c2 = char::from_u32(iv.last).unwrap();
        let mut buf1 = [0; 4];
        let mut buf2 = [0; 4];
        let bytes1 = c1.encode_utf8(&mut buf1);
        let bytes2 = c2.encode_utf8(&mut buf2);
        debug_assert_eq!(bytes1.len(), bytes2.len());
        debug_assert_eq!(bytes1.len(), bucket.byte_ranges.len());
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
        let byte_buckets = utf8_buckets_for_len(byte_count);
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
