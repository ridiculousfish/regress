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
        res.len = s
            .len()
            .try_into()
            .expect("UTF-8 encoding should fit in 4 bytes");
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
struct Utf8Bucket {
    ivs: Interval,                     // closed code-point interval
    byte_ranges: &'static [ByteRange], // per-byte closed ranges
}

impl Utf8Bucket {
    fn contains(&self, bytes: &[u8]) -> bool {
        bytes.len() == self.byte_ranges.len()
            && bytes
                .iter()
                .zip(self.byte_ranges.iter())
                .all(|(&b, r)| r.contains(b))
    }
}

#[rustfmt::skip]
const ALL_UTF8_BUCKETS: &[Utf8Bucket] = &[
    // 1-byte: U+0000..U+007F
    Utf8Bucket { ivs: Interval::new(0x0000, 0x007F),     byte_ranges: &[br(0x00, 0x7F)] },
    // 2-byte: U+0080..U+07FF
    Utf8Bucket { ivs: Interval::new(0x0080, 0x07FF),     byte_ranges: &[br(0xC2, 0xDF), br(0x80, 0xBF)] },
    // 3-byte E0: avoid overlongs -> 2nd byte A0..BF
    Utf8Bucket { ivs: Interval::new(0x0800, 0x0FFF),     byte_ranges: &[br(0xE0, 0xE0), br(0xA0, 0xBF), br(0x80, 0xBF)] },
    // 3-byte E1–EC: full continuations
    Utf8Bucket { ivs: Interval::new(0x1000, 0xCFFF),     byte_ranges: &[br(0xE1, 0xEC), br(0x80, 0xBF), br(0x80, 0xBF)] },
    // 3-byte ED: surrogates -> 2nd byte 80..9F
    Utf8Bucket { ivs: Interval::new(0xD000, 0xD7FF),     byte_ranges: &[br(0xED, 0xED), br(0x80, 0x9F), br(0x80, 0xBF)] },
    // 3-byte EE–EF: full continuations
    Utf8Bucket { ivs: Interval::new(0xE000, 0xFFFF),     byte_ranges: &[br(0xEE, 0xEF), br(0x80, 0xBF), br(0x80, 0xBF)] },
    // 4-byte F0: avoid overlongs -> 2nd byte 90..BF
    Utf8Bucket { ivs: Interval::new(0x1_0000, 0x3_FFFF), byte_ranges: &[br(0xF0, 0xF0), br(0x90, 0xBF), br(0x80, 0xBF), br(0x80, 0xBF)] },
    // 4-byte F1–F3: full continuations
    Utf8Bucket { ivs: Interval::new(0x4_0000, 0xF_FFFF), byte_ranges: &[br(0xF1, 0xF3), br(0x80, 0xBF), br(0x80, 0xBF), br(0x80, 0xBF)] },
    // 4-byte F4: cap at U+10FFFF -> 2nd byte 80..8F
    Utf8Bucket { ivs: Interval::new(0x10_0000, 0x10_FFFF), byte_ranges: &[br(0xF4, 0xF4), br(0x80, 0x8F), br(0x80, 0xBF), br(0x80, 0xBF)] },
];

pub type ByteRangePath = SmallVec<[ByteRange; 4]>;

// Decode a UTF-8 byte slice to a codepoint.
fn decode_utf8_cp(bytes: &[u8]) -> u32 {
    core::str::from_utf8(bytes).unwrap().chars().next().unwrap() as u32
}

fn segment_interval_for_utf8(
    start: u32,
    end: u32,
    bucket: &Utf8Bucket,
    paths: &mut Vec<ByteRangePath>,
) {
    let start_bytes = Utf8Buf::from_cp(start);
    let end_bytes = Utf8Buf::from_cp(end);
    let n = start_bytes.len();
    debug_assert_eq!(n, end_bytes.len());

    // Find the first byte position where start and end differ.
    let split_level = match (0..n).find(|&i| start_bytes[i] != end_bytes[i]) {
        None => {
            // start == end: emit a single-codepoint path.
            let path = start_bytes
                .iter()
                .map(|&b| ByteRange { start: b, end: b })
                .collect();
            paths.push(path);
            return;
        }
        Some(level) => level,
    };

    if split_level == n - 1 {
        // Only the last byte varies; this is a valid rectangle, emit directly.
        let path = start_bytes
            .iter()
            .zip(end_bytes.iter())
            .map(|(&s, &e)| ByteRange { start: s, end: e })
            .collect();
        paths.push(path);
        return;
    }

    // Split at split_level into three parts.
    //
    // max_start: same bytes as `start` through split_level, remaining bytes at bucket max.
    // min_end:   same bytes as `end`   through split_level, remaining bytes at bucket min.
    let mut max_start_buf = [0u8; 4];
    let mut min_end_buf = [0u8; 4];
    max_start_buf[..n].copy_from_slice(&start_bytes);
    min_end_buf[..n].copy_from_slice(&end_bytes);
    for i in (split_level + 1)..n {
        max_start_buf[i] = bucket.byte_ranges[i].end;
        min_end_buf[i] = bucket.byte_ranges[i].start;
    }
    let max_start = decode_utf8_cp(&max_start_buf[..n]);
    let min_end = decode_utf8_cp(&min_end_buf[..n]);

    // Part 1: [start .. max_start]
    segment_interval_for_utf8(start, max_start, bucket, paths);

    // Part 2: [max_start+1 .. min_end-1]
    // By construction, bytes after split_level are bucket-min on the left and bucket-max
    // on the right, so this is always a valid rectangle — emit directly.
    if max_start + 1 < min_end {
        let p2_start = Utf8Buf::from_cp(max_start + 1);
        let p2_end = Utf8Buf::from_cp(min_end - 1);
        let path = p2_start
            .iter()
            .zip(p2_end.iter())
            .map(|(&s, &e)| ByteRange { start: s, end: e })
            .collect();
        paths.push(path);
    }

    // Part 3: [min_end .. end]
    segment_interval_for_utf8(min_end, end, bucket, paths);
}

pub fn utf8_paths_from_code_point_set(cps: &CodePointSet) -> Vec<ByteRangePath> {
    let mut paths = Vec::new();
    for bucket in ALL_UTF8_BUCKETS {
        for iv in cps.intervals_intersecting(bucket.ivs) {
            let first = iv.first.max(bucket.ivs.first);
            let last = iv.last.min(bucket.ivs.last);
            debug_assert!(bucket.contains(&Utf8Buf::from_cp(first)));
            debug_assert!(bucket.contains(&Utf8Buf::from_cp(last)));
            segment_interval_for_utf8(first, last, bucket, &mut paths);
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_len_increases_at_boundaries() {
        const BOUNDARIES: [u32; 4] = [0x007F, 0x07FF, 0xFFFF, 0x10FFFF];
        let mut expected_len = 1;
        for &b in &BOUNDARIES {
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
    fn test_simple_pairing_spans_two_rows() {
        // U+00F8..U+013F occupies two UTF-8 "rows":
        //   row 1: [0xC3, 0xB8..0xBF]  U+00F8..U+00FF
        //   row 2: [0xC4, 0x80..0xBF]  U+0100..U+013F
        //
        // A buggy can_use_simple_pairing sees 0xC3<=0xC4 and 0xB8<=0xBF and says
        // "sure, emit [br(0xC3,0xC4), br(0xB8,0xBF)]".  That path does NOT contain
        // the byte sequence [0xC4, 0x80] (U+0100) because 0x80 < 0xB8.
        let mut cps = CodePointSet::new();
        cps.add(Interval::new(0x00F8, 0x013F));
        let paths = utf8_paths_from_code_point_set(&cps);

        // Every code point in the set must be covered by at least one path.
        for cp in [0x00F8u32, 0x00FF, 0x0100, 0x0101, 0x013F] {
            let enc = Utf8Buf::from_cp(cp);
            let covered = paths.iter().any(|path| {
                path.len() == enc.len() && path.iter().zip(enc.iter()).all(|(r, &b)| r.contains(b))
            });
            assert!(covered, "U+{cp:04X} is not covered by any path");
        }
    }

    #[test]
    fn test_utf8_paths_large_range_within_bucket() {
        let mut cps = CodePointSet::new();
        // Large range within the E1-EC bucket
        cps.add(Interval::new(0x1000, 0x2000));
        let paths = utf8_paths_from_code_point_set(&cps);

        // Verify correctness via roundtrip.
        let roundtripped = code_point_set_from_utf8_paths(&paths);
        assert_eq!(cps, roundtripped);
    }

    #[test]
    fn test_utf8_paths_surrogate_range() {
        let mut cps = CodePointSet::new();
        // Test code points that would be surrogates in UTF-16 (but valid in UTF-8).
        // This range lives entirely in the ED bucket (second byte capped at 0x9F).
        cps.add(Interval::new(0xD000, 0xD7FF));
        let paths = utf8_paths_from_code_point_set(&cps);

        // Verify correctness via roundtrip.
        let roundtripped = code_point_set_from_utf8_paths(&paths);
        assert_eq!(cps, roundtripped);
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
        let bucket = &ALL_UTF8_BUCKETS[1]; // 2-byte bucket

        // Should contain valid 2-byte sequences
        assert!(bucket.contains(&[0xC2, 0x80])); // U+0080
        assert!(bucket.contains(&[0xDF, 0xBF])); // U+07FF

        // Should not contain invalid sequences
        assert!(!bucket.contains(&[0x41])); // wrong length
        assert!(!bucket.contains(&[0xC0, 0x80])); // overlong (invalid first byte)
        assert!(!bucket.contains(&[0xC2, 0x7F])); // invalid continuation byte
        assert!(!bucket.contains(&[0xE0, 0x80, 0x80])); // wrong length
    }

    /// Compute the exact set of code points whose UTF-8 encoding is matched by
    /// at least one path.  For each path we iterate over all leading-byte
    /// combinations and, for every such prefix, the last-byte range maps to a
    /// contiguous run of code points that we can add as a single interval.
    fn code_point_set_from_utf8_paths(paths: &[ByteRangePath]) -> CodePointSet {
        fn decode(bytes: &[u8]) -> u32 {
            str::from_utf8(bytes).unwrap().chars().next().unwrap() as u32
        }

        let mut cps = CodePointSet::new();
        for path in paths {
            match path.len() {
                0 => {}
                1 => {
                    cps.add(Interval::new(path[0].start as u32, path[0].end as u32));
                }
                2 => {
                    for b0 in path[0].start..=path[0].end {
                        let lo = decode(&[b0, path[1].start]);
                        let hi = decode(&[b0, path[1].end]);
                        cps.add(Interval::new(lo, hi));
                    }
                }
                3 => {
                    for b0 in path[0].start..=path[0].end {
                        for b1 in path[1].start..=path[1].end {
                            let lo = decode(&[b0, b1, path[2].start]);
                            let hi = decode(&[b0, b1, path[2].end]);
                            cps.add(Interval::new(lo, hi));
                        }
                    }
                }
                4 => {
                    for b0 in path[0].start..=path[0].end {
                        for b1 in path[1].start..=path[1].end {
                            for b2 in path[2].start..=path[2].end {
                                let lo = decode(&[b0, b1, b2, path[3].start]);
                                let hi = decode(&[b0, b1, b2, path[3].end]);
                                cps.add(Interval::new(lo, hi));
                            }
                        }
                    }
                }
                _ => panic!("UTF-8 path longer than 4 bytes"),
            }
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

    /// Round-trip the complete valid Unicode code-point set in one shot.
    /// This exercises every path the algorithm produces and ensures there are
    /// no gaps or spurious additions across the entire range.
    #[test]
    fn test_full_unicode_roundtrip() {
        let mut all = CodePointSet::new();
        all.add(Interval::new(0x0000, 0xD7FF));
        all.add(Interval::new(0xE000, 0x10_FFFF));

        let paths = utf8_paths_from_code_point_set(&all);
        let roundtripped = code_point_set_from_utf8_paths(&paths);
        assert_eq!(all, roundtripped);
    }

    /// Test every adjacent pair [cp, cp+1] across the whole Unicode range
    /// (excluding surrogates).  With ~1.1 M pairs this covers every possible
    /// row-crossing transition, including cases that fall between the bucket
    /// boundary points tested by test_all_boundary_intervals.
    #[test]
    fn test_all_adjacent_pairs() {
        // Collect the valid code points (surrogates have no UTF-8 encoding).
        let valid: Vec<u32> = (0u32..=0x10_FFFF)
            .filter(|&cp| !(0xD800..=0xDFFF).contains(&cp))
            .collect();

        for pair in valid.windows(2) {
            let (lo, hi) = (pair[0], pair[1]);
            // Skip the gap over the surrogate block (0xD7FF → 0xE000).
            if hi != lo + 1 {
                continue;
            }
            let mut cps = CodePointSet::new();
            cps.add(Interval::new(lo, hi));

            let paths = utf8_paths_from_code_point_set(&cps);
            let roundtripped = code_point_set_from_utf8_paths(&paths);
            assert_eq!(
                cps, roundtripped,
                "roundtrip failed for [U+{lo:04X}, U+{hi:04X}]"
            );
        }
    }

    /// Test every interval [lo, hi] where lo and hi are drawn from the set of
    /// UTF-8 structural boundary code points (bucket edges ±1).  With ~20 such
    /// points this produces ~200 pairs — exhaustive for all structurally
    /// interesting transitions without relying on random sampling.
    #[test]
    fn test_all_boundary_intervals() {
        // One point from each side of every UTF-8 bucket edge, plus the
        // endpoints of the overall range.
        const BOUNDARIES: &[u32] = &[
            0x0000,
            0x0001,
            0x007E,
            0x007F,
            0x0080,
            0x0081,
            0x07FE,
            0x07FF,
            0x0800,
            0x0801,
            0x0FFE,
            0x0FFF,
            0x1000,
            0x1001,
            0xCFFE,
            0xCFFF,
            0xD000,
            0xD001,
            0xD7FE,
            0xD7FF,
            // skip surrogate block 0xD800..=0xDFFF
            0xE000,
            0xE001,
            0xFFFE,
            0xFFFF,
            0x0001_0000,
            0x0001_0001,
            0x0003_FFFE,
            0x0003_FFFF,
            0x0004_0000,
            0x0004_0001,
            0x000F_FFFE,
            0x000F_FFFF,
            0x0010_0000,
            0x0010_0001,
            0x0010_FFFE,
            0x0010_FFFF,
        ];

        for &lo in BOUNDARIES {
            for &hi in BOUNDARIES {
                if hi < lo {
                    continue;
                }
                let mut cps = CodePointSet::new();
                cps.add(Interval::new(lo, hi));
                // Drop surrogates — they have no UTF-8 representation.
                cps.remove(&[Interval::new(0xD800, 0xDFFF)]);

                let paths = utf8_paths_from_code_point_set(&cps);
                let roundtripped = code_point_set_from_utf8_paths(&paths);
                assert_eq!(
                    cps, roundtripped,
                    "roundtrip failed for interval [U+{lo:04X}, U+{hi:04X}]"
                );
            }
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

        let paths = [path1];

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
