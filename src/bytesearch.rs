use crate::insn::MAX_CHAR_SET_LENGTH;
use core::fmt;
extern crate memchr;

/// Facilities for searching bytes.
pub trait ByteSearcher {
    /// Search for ourselves in a slice of bytes.
    /// The length of the slice is unspecified and may be 0.
    /// \return the next index of ourselves in the slice, or None.
    fn find_in(&self, rhs: &[u8]) -> Option<usize>;
}

impl ByteSearcher for [u8; 1] {
    #[inline(always)]
    fn find_in(&self, rhs: &[u8]) -> Option<usize> {
        memchr::memchr(self[0], rhs)
    }
}

impl ByteSearcher for [u8; 2] {
    #[inline(always)]
    fn find_in(&self, rhs: &[u8]) -> Option<usize> {
        memchr::memchr2(self[0], self[1], rhs)
    }
}

impl ByteSearcher for [u8; 3] {
    #[inline(always)]
    fn find_in(&self, rhs: &[u8]) -> Option<usize> {
        memchr::memchr3(self[0], self[1], self[2], rhs)
    }
}

impl ByteSearcher for memchr::memmem::Finder<'_> {
    fn find_in(&self, rhs: &[u8]) -> Option<usize> {
        self.find(rhs)
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
        // TODO.
        for (idx, byte) in rhs.iter().enumerate() {
            if self.contains(*byte) {
                return Some(idx);
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

// An iterator over the indexes of bytes.
pub struct ByteBitmapIter<'a> {
    bm: &'a ByteBitmap,
    idx: u8,
    current: u16,
}

impl Iterator for ByteBitmapIter<'_> {
    type Item = u8;

    fn next(&mut self) -> Option<Self::Item> {
        while self.current == 0 {
            self.idx += 1;
            if self.idx >= self.bm.0.len() as u8 {
                return None;
            }
            self.current = self.bm.0[self.idx as usize];
        }
        let bit = self.current.trailing_zeros() as u8;
        self.current &= !(1 << bit);
        Some((self.idx << 4) | bit)
    }
}

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

    /// Construct from a single byte.
    pub fn from_byte(byte: u8) -> ByteBitmap {
        let mut bb = ByteBitmap::default();
        bb.set(byte);
        bb
    }

    /// Iterate over the bytes.
    pub fn iter(&'_ self) -> ByteBitmapIter<'_> {
        ByteBitmapIter {
            bm: self,
            idx: 0,
            current: self.0[0],
        }
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

    /// Return ourselves as an array of a fixed length.
    /// Panics if the array is not large enough.
    #[allow(clippy::wrong_self_convention)]
    #[inline(always)]
    pub fn as_array<const N: usize>(&self) -> [u8; N] {
        let mut array = [0u8; N];
        let mut idx = 0;
        for byte in 0..=255 {
            if self.contains(byte) {
                array[idx] = byte;
                idx += 1;
            }
        }
        array
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

    /// Low-nibble lookup table for the SIMD (PSHUFB/TBL) nibble classifier, or
    /// `None` if the set contains a byte >= 0x80. `lo[l]` has bit `h` set iff
    /// byte `(h<<4)|l` is in the set; the companion `hi[h] = 1<<h` (h<8) means a
    /// byte is in the set iff `lo[b&0xf] & hi[b>>4] != 0`. Restricting to ASCII
    /// keeps the high nibble < 8 so each lane mask fits in a byte (and any input
    /// byte >= 0x80 — high nibble >= 8 — correctly reads `hi == 0`).
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn ascii_lo_table(&self) -> Option<[u8; 16]> {
        if self.0[8..16].iter().any(|&w| w != 0) {
            return None; // a non-ASCII byte is in the set
        }
        let mut lo = [0u8; 16];
        for (l, slot) in lo.iter_mut().enumerate() {
            let mut m = 0u8;
            for h in 0..8 {
                if (self.0[h] >> l) & 1 != 0 {
                    m |= 1 << h;
                }
            }
            *slot = m;
        }
        Some(lo)
    }
}

/// The constant high-nibble table: `hi[h] = 1<<h` for `h < 8`, else 0.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
const NIBBLE_HI: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 0, 0, 0, 0, 0, 0, 0, 0];

/// SIMD byte-set scan via two PSHUFB nibble lookups + AND (Geoff Langdale's
/// universal classifier). 16 bytes/step. `lo` is from [`ByteBitmap::ascii_lo_table`].
#[cfg(all(target_arch = "x86_64", feature = "std"))]
#[target_feature(enable = "ssse3")]
unsafe fn bitmap_find_ssse3(lo: &[u8; 16], bytes: &[u8]) -> Option<usize> {
    use core::arch::x86_64::*;
    let mut i = 0;
    // SAFETY: `bitmap_find_ssse3` is only called after an `ssse3` feature check,
    // and every load is bounds-checked by the `i + 16 <= len` / `i < len` guards.
    unsafe {
        let lo_tbl = _mm_loadu_si128(lo.as_ptr() as *const __m128i);
        let hi_tbl = _mm_loadu_si128(NIBBLE_HI.as_ptr() as *const __m128i);
        let low_nibble = _mm_set1_epi8(0x0f);
        let zero = _mm_setzero_si128();
        while i + 16 <= bytes.len() {
            let v = _mm_loadu_si128(bytes.as_ptr().add(i) as *const __m128i);
            // pshufb zeroes lanes whose index byte has bit7 set (b >= 0x80), so
            // the lo lookup is already 0 for non-ASCII input (ASCII-only set).
            let lo_part = _mm_shuffle_epi8(lo_tbl, v);
            let hi_idx = _mm_and_si128(_mm_srli_epi16(v, 4), low_nibble);
            let hi_part = _mm_shuffle_epi8(hi_tbl, hi_idx);
            let res = _mm_and_si128(lo_part, hi_part);
            // Bits set where a lane is non-zero (in the set).
            let mask = (_mm_movemask_epi8(_mm_cmpeq_epi8(res, zero)) as u32) ^ 0xffff;
            if mask != 0 {
                return Some(i + mask.trailing_zeros() as usize);
            }
            i += 16;
        }
    }
    while i < bytes.len() {
        let b = bytes[i];
        if lo[(b & 0x0f) as usize] & NIBBLE_HI[(b >> 4) as usize] != 0 {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// AVX2 version of [`bitmap_find_ssse3`]: 32 bytes/step. `_mm256_shuffle_epi8`
/// shuffles within each 128-bit lane, so the 16-byte tables are broadcast to both.
#[cfg(all(target_arch = "x86_64", feature = "std"))]
#[target_feature(enable = "avx2")]
unsafe fn bitmap_find_avx2(lo: &[u8; 16], bytes: &[u8]) -> Option<usize> {
    use core::arch::x86_64::*;
    let mut i = 0;
    // SAFETY: only called after an `avx2` check; loads bounded by the guard.
    unsafe {
        let lo_tbl =
            _mm256_broadcastsi128_si256(_mm_loadu_si128(lo.as_ptr() as *const __m128i));
        let hi_tbl =
            _mm256_broadcastsi128_si256(_mm_loadu_si128(NIBBLE_HI.as_ptr() as *const __m128i));
        let low_nibble = _mm256_set1_epi8(0x0f);
        let zero = _mm256_setzero_si256();
        while i + 32 <= bytes.len() {
            let v = _mm256_loadu_si256(bytes.as_ptr().add(i) as *const __m256i);
            let lo_part = _mm256_shuffle_epi8(lo_tbl, v);
            let hi_idx = _mm256_and_si256(_mm256_srli_epi16(v, 4), low_nibble);
            let hi_part = _mm256_shuffle_epi8(hi_tbl, hi_idx);
            let res = _mm256_and_si256(lo_part, hi_part);
            let mask = (_mm256_movemask_epi8(_mm256_cmpeq_epi8(res, zero)) as u32) ^ 0xffff_ffff;
            if mask != 0 {
                return Some(i + mask.trailing_zeros() as usize);
            }
            i += 32;
        }
    }
    while i < bytes.len() {
        let b = bytes[i];
        if lo[(b & 0x0f) as usize] & NIBBLE_HI[(b >> 4) as usize] != 0 {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// SIMD byte-set scan via two NEON table lookups + AND. 16 bytes/step.
#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn bitmap_find_neon(lo: &[u8; 16], bytes: &[u8]) -> Option<usize> {
    use core::arch::aarch64::*;
    let mut i = 0;
    // SAFETY: NEON is baseline on aarch64; loads are bounded by the loop guards.
    unsafe {
        let lo_tbl = vld1q_u8(lo.as_ptr());
        let hi_tbl = vld1q_u8(NIBBLE_HI.as_ptr());
        let low_nibble = vdupq_n_u8(0x0f);
        while i + 16 <= bytes.len() {
            let v = vld1q_u8(bytes.as_ptr().add(i));
            let lo_part = vqtbl1q_u8(lo_tbl, vandq_u8(v, low_nibble));
            // For b >= 0x80 the high nibble is >= 8, indexing `hi_tbl`'s zeros.
            let hi_part = vqtbl1q_u8(hi_tbl, vshrq_n_u8::<4>(v));
            let res = vandq_u8(lo_part, hi_part);
            // 4-bit-per-byte mask of non-zero (in-set) lanes (see casefold_search).
            let nz = vmvnq_u8(vceqzq_u8(res));
            let narrowed = vshrn_n_u16::<4>(vreinterpretq_u16_u8(nz));
            let bits = vget_lane_u64::<0>(vreinterpret_u64_u8(narrowed));
            if bits != 0 {
                return Some(i + (bits.trailing_zeros() as usize) / 4);
            }
            i += 16;
        }
    }
    while i < bytes.len() {
        let b = bytes[i];
        if lo[(b & 0x0f) as usize] & NIBBLE_HI[(b >> 4) as usize] != 0 {
            return Some(i);
        }
        i += 1;
    }
    None
}

impl ByteSearcher for ByteBitmap {
    #[inline(always)]
    fn find_in(&self, bytes: &[u8]) -> Option<usize> {
        if cfg!(feature = "prohibit-unsafe") {
            return bytes.iter().position(|&b| self.contains(b));
        }
        // SIMD nibble classifier for an ASCII byte set (the common `\d`/`\w`/
        // bracket prefilter case); scalar SWAR otherwise / on other targets.
        #[cfg(all(target_arch = "x86_64", feature = "std"))]
        if let Some(lo) = self.ascii_lo_table() {
            if std::is_x86_feature_detected!("avx2") {
                return unsafe { bitmap_find_avx2(&lo, bytes) };
            }
            if std::is_x86_feature_detected!("ssse3") {
                return unsafe { bitmap_find_ssse3(&lo, bytes) };
            }
        }
        #[cfg(target_arch = "aarch64")]
        if let Some(lo) = self.ascii_lo_table() {
            return unsafe { bitmap_find_neon(&lo, bytes) };
        }
        self.unsafe_find_in_slice(bytes)
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
    fn bitmap_simd_matches_scalar() {
        // Exercise the SIMD nibble classifier (ASCII set) against a scalar oracle
        // over haystacks that cross the 16- and 32-byte SIMD boundaries, include
        // non-ASCII bytes (>= 0x80, never in an ASCII set), and place the match
        // at every offset. Also covers all-`bitnot` (non-ASCII set → scalar path).
        let sets: &[&[u8]] = &[
            &[b'0', b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9'], // [0-9]
            &[b'.'],
            &[b'A', b'Z', b'_', b'a', b'z'],
        ];
        for set in sets {
            let bm = make_bitmap(set);
            // Build a 70-byte haystack: filler bytes that include high-bit bytes.
            let mut hay: Vec<u8> = (0..70).map(|i| if i % 3 == 0 { 0x80 + (i as u8 & 0x3f) } else { b'x' }).collect();
            // No match yet.
            assert_eq!(bm.find_in(&hay), scalar_find(&bm, &hay), "set={set:?} no-match");
            // Plant the set's first byte at each offset and compare to the oracle.
            for pos in 0..hay.len() {
                let save = hay[pos];
                hay[pos] = set[0];
                assert_eq!(
                    bm.find_in(&hay),
                    scalar_find(&bm, &hay),
                    "set={set:?} match at {pos}"
                );
                hay[pos] = save;
            }
        }
    }

    fn scalar_find(bm: &ByteBitmap, hay: &[u8]) -> Option<usize> {
        hay.iter().position(|&b| bm.contains(b))
    }

    #[test]
    fn test_byte_bitmap_iter() {
        let mut bm = ByteBitmap::default();
        bm.set(0b0000_0001);
        bm.set(0b0000_0010);
        bm.set(0b0100_0000);

        let mut iter = bm.iter();
        assert_eq!(iter.next(), Some(0b0000_0001));
        assert_eq!(iter.next(), Some(0b0000_0010));
        assert_eq!(iter.next(), Some(0b0100_0000));
        assert_eq!(iter.next(), None);
    }
}
