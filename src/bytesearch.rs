use crate::insn::MAX_CHAR_SET_LENGTH;
use core::fmt;
extern crate memchr;
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

/// Boyer-Moore substring search implementation for efficient literal matching.
mod boyer_moore {
    /// Compute the bad character table for Boyer-Moore algorithm.
    /// Returns an array where index i contains the rightmost position of byte i in the pattern,
    /// or -1 if the byte doesn't appear in the pattern.
    fn compute_bad_char_table(pattern: &[u8]) -> [isize; 256] {
        let mut table = [-1isize; 256];
        for (i, &byte) in pattern.iter().enumerate() {
            table[byte as usize] = i as isize;
        }
        table
    }

    /// Boyer-Moore search algorithm implementation.
    /// Returns the index of the first occurrence of pattern in text, or None if not found.
    pub fn boyer_moore_search(pattern: &[u8], text: &[u8]) -> Option<usize> {
        if pattern.is_empty() {
            return Some(0);
        }
        if text.len() < pattern.len() {
            return None;
        }

        let bad_char = compute_bad_char_table(pattern);
        let m = pattern.len();
        let n = text.len();
        let mut shift = 0;

        while shift <= n - m {
            let mut j = m - 1;

            // Check pattern from right to left
            while j < m && pattern[j] == text[shift + j] {
                if j == 0 {
                    return Some(shift);
                }
                j -= 1;
            }

            // Calculate shift using bad character rule
            let bad_char_shift = if j < m {
                let text_char = text[shift + j] as usize;
                let last_occurrence = bad_char[text_char];
                if last_occurrence < j as isize {
                    j as isize - last_occurrence
                } else {
                    1
                }
            } else {
                1
            };

            shift += bad_char_shift as usize;
        }

        None
    }
}

/// Facilities for searching bytes.
pub trait ByteSearcher {
    /// Search for ourselves in a slice of bytes.
    /// The length of the slice is unspecified and may be 0.
    /// \return the next index of ourselves in the slice, or None.
    fn find_in(&self, rhs: &[u8]) -> Option<usize>;
}

/// Helper trait that describes matching against literal bytes.
pub trait ByteSeq: ByteSearcher + core::fmt::Debug + Copy + Clone {
    /// Number of bytes.
    const LENGTH: usize;

    /// Test if a slice is equal.
    /// The slice must have exactly LENGTH bytes.
    fn equals_known_len(&self, rhs: &[u8]) -> bool;
}

extern "C" {
    fn memcmp(s1: *const u8, s2: *const u8, n: usize) -> i32;
}

impl<const N: usize> ByteSeq for [u8; N] {
    const LENGTH: usize = N;

    #[inline(always)]
    fn equals_known_len(&self, rhs: &[u8]) -> bool {
        debug_assert!(rhs.len() == Self::LENGTH, "Slice has wrong length");
        if cfg!(feature = "prohibit-unsafe") {
            // Here's what we would like to do. However this will emit an unnecessary length compare, and an unnecessary pointer compare.
            self == rhs
        } else {
            // Warning: this is delicate. We intend for the compiler to emit optimized bytewise comparisons of unaligned LENGTH bytes,
            // where LENGTH is a compile-time constant. Rust's default == on slices will perform a pointer comparison which will always be false,
            // and kill any vectorization.
            // memcmp() will be optimized to the builtin.
            unsafe { memcmp(self.as_ptr(), rhs.as_ptr(), Self::LENGTH) == 0 }
        }
    }
}

impl<const N: usize> ByteSearcher for [u8; N] {
    #[inline(always)]
    fn find_in(&self, rhs: &[u8]) -> Option<usize> {
        if N == 1 {
            return memchr::memchr(self[0], rhs);
        }
        // Use Boyer-Moore for longer patterns to get better performance
        // than the naive sliding window approach. For very short patterns
        // (2-3 bytes), the overhead might not be worth it, but for longer
        // patterns Boyer-Moore provides significant speedup.
        if N >= 5 {
            return boyer_moore::boyer_moore_search(self, rhs);
        }

        // For short patterns (2-3 bytes), still use the simple approach
        // as the Boyer-Moore overhead might not be worth it
        for win in rhs.windows(Self::LENGTH) {
            if self.equals_known_len(win) {
                // Black magic?
                return Some((win.as_ptr() as usize) - (rhs.as_ptr() as usize));
            }
        }
        None
    }
}

/// A ByteSet is any set of bytes.
pub trait ByteSet {
    /// \return whether the ByteSet contains the byte.
    fn contains(&self, b: u8) -> bool;
}

/// A ByteArraySet wraps a small array and uses linear equality.
#[derive(Copy, Clone, Debug)]
#[repr(align(4))]
pub struct ByteArraySet<ArraySet: SmallArraySet>(pub ArraySet);

/// Cover over contains() to avoid bumping into native contains call.
impl<ArraySet: SmallArraySet> ByteArraySet<ArraySet> {
    #[inline(always)]
    pub fn contains(self, b: u8) -> bool {
        self.0.contains(b)
    }
}

impl<ArraySet: SmallArraySet> ByteSearcher for ByteArraySet<ArraySet> {
    #[inline(always)]
    fn find_in(&self, rhs: &[u8]) -> Option<usize> {
        self.0.find_in(rhs)
    }
}

/// A SmallArraySet is a set implemented as a small byte array.
pub trait SmallArraySet: Copy {
    fn contains(self, b: u8) -> bool;

    fn find_in(self, rhs: &[u8]) -> Option<usize>;
}

// Beware: Rust is cranky about loop unrolling.
// Do not try to be too clever here.
impl SmallArraySet for [u8; 2] {
    #[inline(always)]
    fn contains(self, b: u8) -> bool {
        b == self[0] || b == self[1]
    }

    #[inline(always)]
    fn find_in(self, rhs: &[u8]) -> Option<usize> {
        memchr::memchr2(self[0], self[1], rhs)
    }
}
impl SmallArraySet for [u8; 3] {
    #[inline(always)]
    fn contains(self, b: u8) -> bool {
        b == self[0] || b == self[1] || b == self[2]
    }

    #[inline(always)]
    fn find_in(self, rhs: &[u8]) -> Option<usize> {
        memchr::memchr3(self[0], self[1], self[2], rhs)
    }
}
impl SmallArraySet for [u8; 4] {
    #[inline(always)]
    fn contains(self, b: u8) -> bool {
        b == self[0] || b == self[1] || b == self[2] || b == self[3]
    }

    #[inline(always)]
    fn find_in(self, rhs: &[u8]) -> Option<usize> {
        // For [u8; 4], we should do literal sequence search, not character set search
        // Since 4 < 5, we use simple sliding window (Boyer-Moore threshold is 5)
        for win in rhs.windows(4) {
            if self == *win {
                return Some((win.as_ptr() as usize) - (rhs.as_ptr() as usize));
            }
        }
        None
    }
}

// CharSet helper. Avoid branching in the loop to get good unrolling.
#[allow(unused_parens)]
#[inline(always)]
pub fn charset_contains(set: &[u32; MAX_CHAR_SET_LENGTH], c: u32) -> bool {
    let mut result = false;
    for &v in set.iter() {
        result |= (v == c);
    }
    result
}

/// A helper function for formatting bitmaps, using - ranges.
fn format_bitmap<Func>(name: &str, f: &mut fmt::Formatter<'_>, contains: Func) -> fmt::Result
where
    Func: Fn(u8) -> bool,
{
    write!(f, "{}[", name)?;
    let mut idx = 0;
    let mut maybe_space = "";
    while idx <= 256 {
        // Compute the next value not contained.
        let mut end = idx;
        while end <= 256 && contains(end as u8) {
            end += 1;
        }
        match end - idx {
            0 => (),
            1 => write!(f, "{}{}", maybe_space, idx)?,
            _ => write!(f, "{}{}-{}", maybe_space, idx, end - 1)?,
        };
        if end > idx {
            maybe_space = " ";
        }
        idx = end + 1
    }
    write!(f, "]")?;
    Ok(())
}

/// A bitmap covering ASCII characters.
#[derive(Default, Copy, Clone)]
#[repr(align(4))]
pub struct AsciiBitmap(pub [u8; 16]);

impl AsciiBitmap {
    /// Set a byte val in this bitmap.
    #[inline(always)]
    pub fn set(&mut self, val: u8) {
        debug_assert!(val <= 127, "Value should be ASCII");
        self.0[(val >> 3) as usize] |= 1 << (val & 0x7);
    }
}

impl fmt::Debug for AsciiBitmap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        format_bitmap("AsciiBitmap", f, |v| self.contains(v))
    }
}

impl ByteSet for AsciiBitmap {
    /// \return whether this bitmap contains a given value.
    /// The value does NOT have to be ASCII.
    #[inline(always)]
    fn contains(&self, val: u8) -> bool {
        // Delicate tricks to avoid branches.
        // In general we want to compute the byte via /8, and then mask into the
        // byte. But if the value is not ASCII then the byte could be too large.
        // So mask off the MSB so that the byte is always in range.
        let byte = (val & 0x7F) >> 3;
        let bit = val & 0x7;

        // Now probe the bitmap. If our sign bit was set, we want the mask to be 0;
        // otherwise we want to set only the 'bit' offset.
        // Invert the sign bit and reuse it.
        let mask = ((val >> 7) ^ 1) << bit;

        // Probe the bitmap. We expect the compiler to elide the bounds check.
        (self.0[byte as usize] & mask) != 0
    }
}

/// A bitmap covering all bytes.
#[derive(Default, Copy, Clone, PartialEq, Eq)]
#[repr(align(4))]
pub struct ByteBitmap([u16; 16]);

// TODO: the codegen here is pretty horrible; LLVM is emitting a sequence of
// halfword instructions. Consider using a union?
impl ByteBitmap {
    /// Construct from a sequence of bytes.
    pub fn new(bytes: &[u8]) -> ByteBitmap {
        let mut bb = ByteBitmap::default();
        for &b in bytes {
            bb.set(b)
        }
        bb
    }

    /// \return whether this bitmap contains a given byte val.
    #[inline(always)]
    pub fn contains(&self, val: u8) -> bool {
        let byte = val >> 4;
        let bit = val & 0xF;
        (self.0[byte as usize] & (1 << bit)) != 0
    }

    /// Set a bit in this bitmap.
    #[inline(always)]
    pub fn set(&mut self, val: u8) {
        let byte = val >> 4;
        let bit = val & 0xF;
        self.0[byte as usize] |= 1 << bit;
    }

    /// Update ourselves from another bitmap, in place.
    pub fn bitor(&mut self, rhs: &ByteBitmap) {
        for idx in 0..self.0.len() {
            self.0[idx] |= rhs.0[idx];
        }
    }

    /// Invert our bits, in place.
    pub fn bitnot(&mut self) -> &mut Self {
        for val in self.0.iter_mut() {
            *val = !*val;
        }
        self
    }

    /// Count number of set bits.
    pub fn count_bits(&self) -> u32 {
        self.0.iter().map(|v| v.count_ones()).sum()
    }

    /// \return all set bytes, as a vec.
    #[allow(clippy::wrong_self_convention)]
    pub fn to_vec(&self) -> Vec<u8> {
        (0..=255).filter(|b| self.contains(*b)).collect()
    }

    /// \return the index of the first byte in the slice that is present in this
    /// bitmap, using some unsafe tricks.
    #[inline(always)]
    fn unsafe_find_in_slice(&self, bytes: &[u8]) -> Option<usize> {
        type Chunk = u32;
        let bm = &self.0;

        let mut offset = 0;
        let (prefix, body, suffix) = unsafe { bytes.align_to::<Chunk>() };
        for &byte in prefix.iter() {
            if self.contains(byte) {
                return Some(offset);
            }
            offset += 1;
        }

        for &chunk in body {
            // Use LE. Here index 0 is the earliest address.
            let byte_idxs = ((chunk >> 4) & 0x0F0F0F0F).to_le_bytes();
            let bit_idxs = (chunk & 0x0F0F0F0F).to_le_bytes();
            if (bm[byte_idxs[0] as usize] & (1 << bit_idxs[0])) != 0 {
                return Some(offset);
            }
            if (bm[byte_idxs[1] as usize] & (1 << bit_idxs[1])) != 0 {
                return Some(offset + 1);
            }
            if (bm[byte_idxs[2] as usize] & (1 << bit_idxs[2])) != 0 {
                return Some(offset + 2);
            }
            if (bm[byte_idxs[3] as usize] & (1 << bit_idxs[3])) != 0 {
                return Some(offset + 3);
            }
            offset += 4;
        }

        for &byte in suffix.iter() {
            if self.contains(byte) {
                return Some(offset);
            }
            offset += 1;
        }
        None
    }
}

impl ByteSearcher for ByteBitmap {
    #[inline(always)]
    fn find_in(&self, bytes: &[u8]) -> Option<usize> {
        if cfg!(feature = "prohibit-unsafe") {
            for (idx, byte) in bytes.iter().enumerate() {
                if self.contains(*byte) {
                    return Some(idx);
                }
            }
            None
        } else {
            self.unsafe_find_in_slice(bytes)
        }
    }
}

impl fmt::Debug for ByteBitmap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        format_bitmap("ByteBitmap", f, |v| self.contains(v))
    }
}

/// A trivial ByteSearcher corresponding to the empty string.
#[derive(Debug, Copy, Clone)]
pub struct EmptyString {}

impl ByteSearcher for EmptyString {
    #[inline(always)]
    fn find_in(&self, _bytes: &[u8]) -> Option<usize> {
        Some(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_bitmap(bytes: &[u8]) -> ByteBitmap {
        let mut bm = ByteBitmap::default();
        for &b in bytes {
            bm.set(b)
        }
        bm
    }

    #[test]
    fn empty_search() {
        assert_eq!(EmptyString {}.find_in(&[1, 2, 3]), Some(0));
        assert_eq!(EmptyString {}.find_in(&[]), Some(0));
    }

    #[test]
    fn bitmap_search() {
        assert_eq!(make_bitmap(&[]).find_in(&[1, 2, 3]), None);
        assert_eq!(make_bitmap(&[]).bitnot().find_in(&[1, 2, 3]), Some(0));
        assert_eq!(make_bitmap(&[1]).bitnot().find_in(&[1, 2, 3]), Some(1));
        assert_eq!(make_bitmap(&[2]).bitnot().find_in(&[1, 2, 3]), Some(0));
        assert_eq!(
            make_bitmap(&[4, 5, 6, 7]).find_in(&[8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20]),
            None
        );
        assert_eq!(
            make_bitmap(&[4, 5, 6, 7]).find_in(&[8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20]),
            None
        );
        assert_eq!(
            make_bitmap(&[4, 5, 6, 7])
                .find_in(&[8, 9, 10, 11, 12, 13, 4, 14, 6, 15, 7, 16, 17, 18, 19, 20]),
            Some(6)
        );
    }

    #[test]
    fn literal_search() {
        assert_eq!([0, 1, 2, 3].find_in(&[4, 5, 6, 7]), None);
        assert_eq!([0, 1, 2, 3].find_in(&[]), None);
    }

    #[test]
    fn boyer_moore_search() {
        // Test Boyer-Moore implementation directly
        assert_eq!(
            super::boyer_moore::boyer_moore_search(b"abc", b"abcdef"),
            Some(0)
        );
        assert_eq!(
            super::boyer_moore::boyer_moore_search(b"def", b"abcdef"),
            Some(3)
        );
        assert_eq!(
            super::boyer_moore::boyer_moore_search(b"xyz", b"abcdef"),
            None
        );
        assert_eq!(
            super::boyer_moore::boyer_moore_search(b"", b"abcdef"),
            Some(0)
        );
        assert_eq!(
            super::boyer_moore::boyer_moore_search(b"abcdefg", b"abcdef"),
            None
        );

        // Test with longer patterns that will use Boyer-Moore through ByteSearcher
        let pattern = [b'h', b'e', b'l', b'l', b'o'];
        let text = b"hello world, hello again";
        assert_eq!(pattern.find_in(text), Some(0));

        let pattern2 = [b'w', b'o', b'r', b'l', b'd'];
        assert_eq!(pattern2.find_in(text), Some(6));

        let pattern3 = [b'a', b'g', b'a', b'i', b'n'];
        assert_eq!(pattern3.find_in(text), Some(19));

        // Test case where pattern doesn't exist
        let pattern4 = [b'x', b'y', b'z', b'a', b'b'];
        assert_eq!(pattern4.find_in(text), None);

        // Test repeated characters
        let pattern5 = [b'l', b'l', b'o', b' '];
        assert_eq!(pattern5.find_in(text), Some(2));
    }

    #[test]
    fn boyer_moore_performance_characteristics() {
        // Create a pathological case for naive search but good for Boyer-Moore:
        // A long text with many false starts
        let mut text = Vec::new();
        for _ in 0..1000 {
            text.extend_from_slice(b"abcdef");
        }
        text.extend_from_slice(b"abcdefghijk"); // The actual pattern appears at the end

        let pattern = [
            b'a', b'b', b'c', b'd', b'e', b'f', b'g', b'h', b'i', b'j', b'k',
        ];

        // This should find the pattern at the very end
        let expected_pos = text.len() - 11;
        assert_eq!(pattern.find_in(&text), Some(expected_pos));

        // Test that both shorter and longer patterns work correctly
        let short_pattern = [b'a', b'b', b'c']; // Uses naive search
        assert_eq!(short_pattern.find_in(&text), Some(0));

        let long_pattern = [b'f', b'g', b'h', b'i', b'j', b'k']; // Uses Boyer-Moore
        assert_eq!(long_pattern.find_in(&text), Some(expected_pos + 5));
    }

    #[test]
    fn boyer_moore_edge_cases() {
        // Test edge cases for Boyer-Moore
        let pattern = [b'a', b'a', b'a', b'a']; // Repeated character pattern
        let text = b"baaaaaaaaab";
        assert_eq!(pattern.find_in(text), Some(1));

        // Pattern at the very beginning
        let pattern2 = [b'b', b'a', b'a', b'a'];
        assert_eq!(pattern2.find_in(text), Some(0));

        // Use a longer pattern to ensure Boyer-Moore is used
        let pattern3 = [b'a', b'a', b'a', b'a', b'b']; // 5 bytes, will use Boyer-Moore
        let text3 = b"baaaaaaaaaab"; // Extended text to have the pattern
        assert_eq!(pattern3.find_in(text3), Some(7));

        // Single character repeated throughout - use Boyer-Moore
        let pattern4 = [b'a'; 5];
        assert_eq!(pattern4.find_in(text3), Some(1));
    }
}
