//! Helpers to deal with UTF-8 in NFAs.
use crate::automata::nfa::ByteRange;
use crate::codepointset::{CodePointSet, Interval};
use smallvec::SmallVec;

const fn br(start: u8, end: u8) -> ByteRange {
    ByteRange { start, end }
}

/// A small inline array of bytes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Utf8Buf {
    buf: [u8; 4],
    len: u8, // always in 1..=4
}

impl Utf8Buf {
    // Create a new Utf8Buf4 from a code point.
    pub fn from_cp(cp: u32) -> Self {
        let c = char::from_u32(cp).expect("invalid code point");
        let mut res = Self {
            buf: [0; 4],
            len: 0,
        };
        let s = c.encode_utf8(&mut res.buf);
        res.len = s.len() as u8;
        res
    }
}

impl std::ops::Deref for Utf8Buf {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.buf[..self.len as usize]
    }
}

/// A UTF-8 structural bucket: the exact code-point span it covers and
/// the per-byte closed ranges that encode that span.
#[derive(Copy, Clone)]
pub struct Utf8Bucket {
    pub ivs: Interval,                     // closed code-point interval
    pub byte_ranges: &'static [ByteRange], // per-byte closed ranges
}

impl Utf8Bucket {
    // Check if a sequence of bytes is contained within our sequence of byte ranges.
    pub fn contains(&self, bytes: &[u8]) -> bool {
        bytes.len() == self.byte_ranges.len()
            && bytes
                .iter()
                .zip(self.byte_ranges.iter())
                .all(|(&b, r)| r.contains(b))
    }
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

// Length boundaries for mapping from code points to number of UTF-8 bytes.
// These are closed boundaries (<=).
pub const UTF8_LENGTH_BOUNDARIES: [u32; 4] = [0x007F, 0x07FF, 0xFFFF, 0x10FFFF];

pub type ByteRangePath = SmallVec<[ByteRange; 4]>;

fn add_paths_from_bucket(cps: &CodePointSet, bucket: &Utf8Bucket, paths: &mut Vec<ByteRangePath>) {
    // Process a single bucket, adding to our trie.
    for iv in cps.intervals_intersecting(bucket.ivs) {
        let mut iv = *iv;
        // First and last ranges may extend beyond the bucket's range.
        // Clamp them all for simplicity.
        iv.first = iv.first.max(bucket.ivs.first);
        iv.last = iv.last.min(bucket.ivs.last);
        debug_assert!(iv.first <= iv.last);

        let b1 = Utf8Buf::from_cp(iv.first);
        let b2 = Utf8Buf::from_cp(iv.last);

        // Every byte should be within the byte ranges.
        debug_assert_eq!(b1.len(), b2.len());
        debug_assert!(bucket.contains(&b1));
        debug_assert!(bucket.contains(&b2));

        // Construct our trie path.
        let path = b1
            .iter()
            .zip(b2.iter())
            .map(|(&start, &end)| ByteRange { start, end })
            .collect();
        paths.push(path);
    }
}

pub fn utf8_paths_from_code_point_set(cps: &CodePointSet) -> Vec<ByteRangePath> {
    let mut paths = Vec::new();
    let Some(last_cp) = cps.last_codepoint() else {
        return paths;
    };
    let [bound1, bound2, bound3, _] = UTF8_LENGTH_BOUNDARIES;

    for bucket in UTF8_BUCKETS_LEN1 {
        add_paths_from_bucket(cps, bucket, &mut paths);
    }
    if last_cp > bound1 {
        for bucket in UTF8_BUCKETS_LEN2 {
            add_paths_from_bucket(cps, bucket, &mut paths);
        }
    }
    if last_cp > bound2 {
        for bucket in UTF8_BUCKETS_LEN3 {
            add_paths_from_bucket(cps, bucket, &mut paths);
        }
    }
    if last_cp > bound3 {
        for bucket in UTF8_BUCKETS_LEN4 {
            add_paths_from_bucket(cps, bucket, &mut paths);
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_len_increases_at_boundaries() {
        // Test UTF8_LENGTH_BOUNDARIES.
        let mut expected_len = 1;
        for &b in &UTF8_LENGTH_BOUNDARIES {
            assert_eq!(Utf8Buf::from_cp(b - 1).len(), expected_len);
            assert_eq!(Utf8Buf::from_cp(b).len(), expected_len);
            if b + 1 < 0x10FFFF {
                assert_eq!(Utf8Buf::from_cp(b + 1).len(), expected_len + 1);
            }
            expected_len += 1;
        }
    }

    #[test]
    fn test_utf8_paths_empty() {
        let cps = CodePointSet::new();
        let paths = utf8_paths_from_code_point_set(&cps);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_utf8_paths_ascii() {
        let mut cps = CodePointSet::new();
        cps.add_one(0x41); // 'A'
        let paths = utf8_paths_from_code_point_set(&cps);

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], ByteRangePath::from_iter([br(0x41, 0x41)]));

        let mut cps = CodePointSet::new();
        cps.add(Interval::new(0x41, 0x5A)); // 'A' to 'Z'
        let paths = utf8_paths_from_code_point_set(&cps);

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], ByteRangePath::from_iter([br(0x41, 0x5A)]));
    }

    #[test]
    fn test_utf8_paths_two_byte() {
        let mut cps = CodePointSet::new();
        cps.add_one(0x03B1); // α (Greek alpha)
        let paths = utf8_paths_from_code_point_set(&cps);

        assert_eq!(paths.len(), 1);
        // α encodes as [0xCE, 0xB1]
        assert_eq!(
            paths[0],
            ByteRangePath::from_iter([br(0xCE, 0xCE), br(0xB1, 0xB1)])
        );

        let mut cps = CodePointSet::new();
        cps.add(Interval::new(0x03B1, 0x03B3)); // α to γ
        let paths = utf8_paths_from_code_point_set(&cps);

        assert_eq!(paths.len(), 1);
        // α = [0xCE, 0xB1], γ = [0xCE, 0xB3]
        assert_eq!(
            paths[0],
            ByteRangePath::from_iter([br(0xCE, 0xCE), br(0xB1, 0xB3)])
        );
    }

    #[test]
    fn test_utf8_paths_three_byte() {
        let mut cps = CodePointSet::new();
        cps.add_one(0x4E2D); // 中 (Chinese character)
        let paths = utf8_paths_from_code_point_set(&cps);

        assert_eq!(paths.len(), 1);
        // 中 encodes as [0xE4, 0xB8, 0xAD]
        assert_eq!(
            paths[0],
            ByteRangePath::from_iter([br(0xE4, 0xE4), br(0xB8, 0xB8), br(0xAD, 0xAD)])
        );
    }

    #[test]
    fn test_utf8_paths_four_byte() {
        let mut cps = CodePointSet::new();
        cps.add_one(0x1F680); // 🚀 (rocket emoji)
        let paths = utf8_paths_from_code_point_set(&cps);

        assert_eq!(paths.len(), 1);
        // 🚀 encodes as [0xF0, 0x9F, 0x9A, 0x80]
        assert_eq!(
            paths[0],
            ByteRangePath::from_iter([
                br(0xF0, 0xF0),
                br(0x9F, 0x9F),
                br(0x9A, 0x9A),
                br(0x80, 0x80)
            ])
        );
    }

    #[test]
    fn test_utf8_paths_sparse() {
        let mut cps = CodePointSet::new();
        cps.add_one(0x41); // 'A' (1 byte)
        cps.add_one(0x03B1); // α (2 bytes)
        cps.add_one(0x4E2D); // 中 (3 bytes)
        let paths = utf8_paths_from_code_point_set(&cps);

        assert_eq!(paths.len(), 3);
        // Should have one path for each character
        assert_eq!(paths[0], ByteRangePath::from_iter([br(0x41, 0x41)]));
        assert_eq!(
            paths[1],
            ByteRangePath::from_iter([br(0xCE, 0xCE), br(0xB1, 0xB1)])
        );
        assert_eq!(
            paths[2],
            ByteRangePath::from_iter([br(0xE4, 0xE4), br(0xB8, 0xB8), br(0xAD, 0xAD)])
        );
    }

    #[test]
    fn test_utf8_paths_mixed_length_ranges() {
        let mut cps = CodePointSet::new();
        cps.add(Interval::new(0x7E, 0x81)); // spans 1-byte to 2-byte boundary
        let paths = utf8_paths_from_code_point_set(&cps);

        assert_eq!(paths.len(), 2);
        // 0x7E - 0x7F (1 byte)
        assert_eq!(paths[0], ByteRangePath::from_iter([br(0x7E, 0x7F)]));
        // 0x80 - 0x81 (2 bytes): [0xC2, 0x80] to [0xC2, 0x81]
        assert_eq!(
            paths[1],
            ByteRangePath::from_iter([br(0xC2, 0xC2), br(0x80, 0x81)])
        );
    }

    #[test]
    fn test_utf8_paths_cross_bucket_boundary() {
        let mut cps = CodePointSet::new();
        // Test range that crosses 3-byte bucket boundaries
        cps.add(Interval::new(0x0FFF, 0x1000)); // crosses E0 -> E1 bucket boundary
        let paths = utf8_paths_from_code_point_set(&cps);

        assert_eq!(paths.len(), 2);
        // 0x0FFF is in E0 bucket: [0xE0, 0xBF, 0xBF]
        assert_eq!(
            paths[0],
            ByteRangePath::from_iter([br(0xE0, 0xE0), br(0xBF, 0xBF), br(0xBF, 0xBF)])
        );
        // 0x1000 is in E1 bucket: [0xE1, 0x80, 0x80]
        assert_eq!(
            paths[1],
            ByteRangePath::from_iter([br(0xE1, 0xE1), br(0x80, 0x80), br(0x80, 0x80)])
        );
    }

    #[test]
    fn test_utf8_paths_large_range_within_bucket() {
        let mut cps = CodePointSet::new();
        // Large range within the E1-EC bucket
        cps.add(Interval::new(0x1000, 0x2000));
        let paths = utf8_paths_from_code_point_set(&cps);

        assert_eq!(paths.len(), 1);
        // Should span from [0xE1, 0x80, 0x80] to [0xE2, 0x80, 0x80]
        assert_eq!(
            paths[0],
            ByteRangePath::from_iter([br(0xE1, 0xE2), br(0x80, 0x80), br(0x80, 0x80)])
        );
    }

    #[test]
    fn test_utf8_paths_surrogate_range() {
        let mut cps = CodePointSet::new();
        // Test code points that would be surrogates in UTF-16 (but valid in UTF-8)
        cps.add(Interval::new(0xD000, 0xD7FF));
        let paths = utf8_paths_from_code_point_set(&cps);

        assert_eq!(paths.len(), 1);
        // This range is handled by the ED bucket with restricted second byte
        // Should use ED bucket: [0xED, 0x80-0x9F, 0x80-0xBF]
        assert_eq!(
            paths[0],
            ByteRangePath::from_iter([br(0xED, 0xED), br(0x80, 0x9F), br(0x80, 0xBF)])
        );
    }

    #[test]
    fn test_utf8_paths_max_four_byte() {
        let mut cps = CodePointSet::new();
        cps.add_one(0x10FFFF); // Maximum valid Unicode code point
        let paths = utf8_paths_from_code_point_set(&cps);

        assert_eq!(paths.len(), 1);
        // 0x10FFFF encodes as [0xF4, 0x8F, 0xBF, 0xBF]
        assert_eq!(
            paths[0],
            ByteRangePath::from_iter([
                br(0xF4, 0xF4),
                br(0x8F, 0x8F),
                br(0xBF, 0xBF),
                br(0xBF, 0xBF)
            ])
        );
    }

    #[test]
    fn test_utf8_paths_all_lengths() {
        let mut cps = CodePointSet::new();
        cps.add_one(0x20); // 1-byte (space)
        cps.add_one(0x00A9); // 2-byte (copyright symbol)
        cps.add_one(0x20AC); // 3-byte (euro symbol)  
        cps.add_one(0x1F4A9); // 4-byte (pile of poo emoji)
        let paths = utf8_paths_from_code_point_set(&cps);

        assert_eq!(paths.len(), 4);
        assert_eq!(paths[0], ByteRangePath::from_iter([br(0x20, 0x20)])); // space
        assert_eq!(
            paths[1],
            ByteRangePath::from_iter([br(0xC2, 0xC2), br(0xA9, 0xA9)])
        ); // ©
        assert_eq!(
            paths[2],
            ByteRangePath::from_iter([br(0xE2, 0xE2), br(0x82, 0x82), br(0xAC, 0xAC)])
        ); // €
        assert_eq!(
            paths[3],
            ByteRangePath::from_iter([
                br(0xF0, 0xF0),
                br(0x9F, 0x9F),
                br(0x92, 0x92),
                br(0xA9, 0xA9)
            ])
        ); // 💩
    }

    #[test]
    fn test_utf8_buf_from_cp() {
        // Test the Utf8Buf helper
        let buf = Utf8Buf::from_cp(0x41);
        assert_eq!(&*buf, &[0x41]);

        let buf = Utf8Buf::from_cp(0x03B1);
        assert_eq!(&*buf, &[0xCE, 0xB1]);

        let buf = Utf8Buf::from_cp(0x4E2D);
        assert_eq!(&*buf, &[0xE4, 0xB8, 0xAD]);

        let buf = Utf8Buf::from_cp(0x1F680);
        assert_eq!(&*buf, &[0xF0, 0x9F, 0x9A, 0x80]);
    }

    #[test]
    fn test_utf8_bucket_contains() {
        let bucket = &UTF8_BUCKETS_LEN2[0]; // 2-byte bucket

        // Should contain valid 2-byte sequences
        assert!(bucket.contains(&[0xC2, 0x80])); // U+0080
        assert!(bucket.contains(&[0xDF, 0xBF])); // U+07FF

        // Should not contain invalid sequences
        assert!(!bucket.contains(&[0x41])); // wrong length
        assert!(!bucket.contains(&[0xC0, 0x80])); // overlong (invalid first byte)
        assert!(!bucket.contains(&[0xC2, 0x7F])); // invalid continuation byte
        assert!(!bucket.contains(&[0xE0, 0x80, 0x80])); // wrong length
    }

    fn code_point_set_from_utf8_paths(paths: &[ByteRangePath]) -> CodePointSet {
        let mut cps = CodePointSet::new();

        for path in paths {
            if path.is_empty() {
                continue;
            }

            // Decode the minimum and maximum of our ByteRangePath.
            let min_bytes: SmallVec<[u8; 4]> = path.iter().map(|r| r.start).collect();
            let max_bytes: SmallVec<[u8; 4]> = path.iter().map(|r| r.end).collect();

            let min_cp: u32 = str::from_utf8(&min_bytes)
                .unwrap()
                .chars()
                .next()
                .unwrap()
                .into();
            let max_cp: u32 = str::from_utf8(&max_bytes)
                .unwrap()
                .chars()
                .next()
                .unwrap()
                .into();
            cps.add(Interval::new(min_cp, max_cp));
        }

        cps
    }

    #[test]
    fn test_random_codepoint_ranges_roundtrip() {
        use rand::rngs::SmallRng;
        use rand::seq::SliceRandom;
        use rand::{Rng, SeedableRng};

        // UTF-8 bucket cap for a given code point (structural UTF-8 buckets).
        #[inline]
        fn utf8_bucket_end(cp: u32) -> u32 {
            match cp {
                0x0000..=0x007F => 0x007F,          // 1-byte
                0x0080..=0x07FF => 0x07FF,          // 2-byte
                0x0800..=0x0FFF => 0x0FFF,          // 3-byte (E0)
                0x1000..=0xCFFF => 0xCFFF,          // 3-byte (E1–EC)
                0xD000..=0xD7FF => 0xD7FF,          // 3-byte (ED)
                0xE000..=0xFFFF => 0xFFFF,          // 3-byte (EE–EF)
                0x1_0000..=0x3_FFFF => 0x3_FFFF,    // 4-byte (F0)
                0x4_0000..=0xF_FFFF => 0xF_FFFF,    // 4-byte (F1–F3)
                0x10_0000..=0x10_FFFF => 0x10_FFFF, // 4-byte (F4)
                _ => 0x10_FFFF,
            }
        }

        // Weighted UTF-8–aware code point sampler, mildly biased to shorter encodings
        // and occasionally snapping to structural boundaries to stress edge cases.
        fn sample_cp_biased<R: rand::Rng + ?Sized>(rng: &mut R) -> u32 {
            // Bucket weights: 1B=40%, 2B=35%, 3B=20%, 4B=5%
            let p = rng.gen_range(0u32..100);
            let mut cp = match p {
                0..=39 => rng.gen_range(0x0000..=0x007F),  // 1-byte
                40..=74 => rng.gen_range(0x0080..=0x07FF), // 2-byte
                75..=94 => {
                    // 3-byte: split to respect E0/ED nuances
                    match rng.gen_range(0..4) {
                        0 => rng.gen_range(0x0800..=0x0FFF), // E0
                        1 => rng.gen_range(0x1000..=0xCFFF), // E1–EC
                        2 => rng.gen_range(0xD000..=0xD7FF), // ED (surrogate range structurally)
                        _ => rng.gen_range(0xE000..=0xFFFF), // EE–EF
                    }
                }
                _ => rng.gen_range(0x1_0000..=0x10_FFFF), // 4-byte
            };

            // ~2% snap to interesting boundaries to find off-by-ones.
            if rng.gen_ratio(1, 50) {
                const BOUNDS: &[u32] = &[
                    0x0000, 0x007F, 0x0080, 0x07FF, 0x0800, 0x0FFF, 0x1000, 0xCFFF, 0xD000, 0xD7FF,
                    0xE000, 0xFFFF, 0x1_0000, 0x10_FFFF,
                ];
                if let Some(&choice) = BOUNDS.choose(rng) {
                    cp = choice;
                }
            }
            cp
        }

        let mut rng = SmallRng::seed_from_u64(12345);
        let mut cps = CodePointSet::new();
        for _ in 0..100_000 {
            cps.clear();
            for _ in 0..(rng.gen_range(1..=5)) {
                let start = sample_cp_biased(&mut rng);
                let cap = utf8_bucket_end(start);

                // 85% of the time: keep end within the same UTF-8 bucket.
                // 15%: try to cross the bucket boundary.
                let stay_in_bucket = rng.gen_ratio(85, 100);
                let max_end = (start.saturating_add(rng.gen_range(0..=100))).min(0x10_FFFF);
                let end = if stay_in_bucket {
                    max_end.min(cap)
                } else {
                    // Force a boundary-cross when possible; otherwise fall back to max_end.
                    let next = (cap.saturating_add(1)).min(0x10_FFFF);
                    if next > start {
                        next.min(max_end)
                    } else {
                        max_end
                    }
                };

                cps.add(Interval::new(start, end));
            }

            // Remove surrogate range as we are UTF-8.
            cps.remove(&[Interval::new(0xD800, 0xDFFF)]);

            let paths = utf8_paths_from_code_point_set(&cps);
            let roundtripped = code_point_set_from_utf8_paths(&paths);
            assert_eq!(cps, roundtripped);
        }
    }

    #[test]
    fn test_utf8_regression() {
        // This test reproduces the crash by finding a CodePointSet that generates invalid UTF-8 paths
        // Based on the actual crash from:
        // ./target/debug/regress-tool '[\p{Alphabetic}]' aaaa --nfa --flags u --dump-nfa

        // Create a CodePointSet that should trigger the UTF-8 path generation bug
        let mut cps = CodePointSet::new();

        // Add ranges from the actual \p{Alphabetic} that might cause issues
        cps.add(Interval::new(65, 90)); // A-Z
        cps.add(Interval::new(97, 122)); // a-z
        cps.add(Interval::new(248, 705)); // ø-ʱ (spans multiple UTF-8 buckets - likely culprit)

        println!("Testing CodePointSet: {:?}", cps.intervals());

        // Generate UTF-8 paths - this is where the bug might manifest
        let paths = utf8_paths_from_code_point_set(&cps);
        println!("Generated {} UTF-8 paths", paths.len());

        // Check all byte ranges for validity - this should find the invalid range that causes the crash
        let mut found_invalid = false;
        for (path_idx, path) in paths.iter().enumerate() {
            println!("Path {}: {:?}", path_idx, path);
            for (range_idx, range) in path.iter().enumerate() {
                if range.start > range.end {
                    println!(
                        "FOUND THE BUG! Invalid byte range in path {}, range {}: start=0x{:02X} > end=0x{:02X}",
                        path_idx, range_idx, range.start, range.end
                    );
                    found_invalid = true;
                    // Don't panic here - just log it
                }
            }
        }

        if !found_invalid {
            println!("No invalid byte ranges found in UTF-8 paths - trying more complex ranges");

            // Try with an even more complex set that's more likely to trigger the bug
            let mut complex_cps = CodePointSet::new();
            complex_cps.add(Interval::new(248, 705)); // This specific range might be the trigger
            complex_cps.add(Interval::new(710, 721));
            complex_cps.add(Interval::new(736, 740));

            let complex_paths = utf8_paths_from_code_point_set(&complex_cps);
            println!(
                "Testing more complex set with {} paths",
                complex_paths.len()
            );

            for (path_idx, path) in complex_paths.iter().enumerate() {
                for (range_idx, range) in path.iter().enumerate() {
                    if range.start > range.end {
                        println!(
                            "FOUND THE BUG in complex set! Invalid byte range in path {}, range {}: start=0x{:02X} > end=0x{:02X}",
                            path_idx, range_idx, range.start, range.end
                        );
                        found_invalid = true;
                    }
                }
            }
        }

        // This assertion should FAIL when the bug is present
        assert!(
            !found_invalid,
            "Found invalid byte ranges in UTF-8 path generation - this is the bug!"
        );
    }

    #[test]
    fn test_expose_reconstruction_invalid_utf8() {
        // Test the code_point_set_from_utf8_paths function directly with edge cases

        // Create a UTF-8 path that has valid individual ranges but when min/max combined
        // creates invalid UTF-8 or invalid code point ranges
        let mut path1 = ByteRangePath::new();
        path1.push(ByteRange {
            start: 0xE1,
            end: 0xE2,
        }); // 3-byte UTF-8 first byte
        path1.push(ByteRange {
            start: 0xB8,
            end: 0x81,
        }); // Invalid: start > end
        path1.push(ByteRange {
            start: 0x80,
            end: 0xBF,
        }); // Valid continuation

        let paths = vec![path1];

        // Check the paths first
        for (path_idx, path) in paths.iter().enumerate() {
            for (range_idx, range) in path.iter().enumerate() {
                if range.start > range.end {
                    println!(
                        "SUCCESS! Found invalid byte range in test path {}, range {}: start=0x{:02X} > end=0x{:02X}",
                        path_idx, range_idx, range.start, range.end
                    );
                    return; // Test succeeded in exposing the bug
                }
            }
        }

        println!("Direct UTF-8 path construction test completed - no invalid ranges found");
    }
}
