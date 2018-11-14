use crate::codepointset::CODE_POINT_MAX;
use std::cmp::Ordering;
use std::ops::{Index, IndexMut};
use std::slice::SliceIndex;

/// A trait which performs bounds checking only in debug mode.
pub trait DebugCheckIndex<Idx>: Index<Idx> + IndexMut<Idx> {
    fn iat(&self, index: Idx) -> &Self::Output;
    fn mat(&mut self, index: Idx) -> &mut Self::Output;
}

impl<Idx, T> DebugCheckIndex<Idx> for Vec<T>
where
    Idx: SliceIndex<[T]> + Clone,
{
    #[inline(always)]
    fn iat(&self, idx: Idx) -> &Self::Output {
        debug_assert!(self.get(idx.clone()).is_some(), "Index out of bounds");
        if cfg!(feature = "prohibit-unsafe") {
            self.index(idx)
        } else {
            unsafe { self.get_unchecked(idx) }
        }
    }

    #[inline(always)]
    fn mat(&mut self, idx: Idx) -> &mut Self::Output {
        debug_assert!(self.get(idx.clone()).is_some(), "Index out of bounds");
        if cfg!(feature = "prohibit-unsafe") {
            self.index_mut(idx)
        } else {
            unsafe { self.get_unchecked_mut(idx) }
        }
    }
}

impl<Idx, T> DebugCheckIndex<Idx> for [T]
where
    Idx: SliceIndex<[T]> + Clone,
{
    #[inline(always)]
    fn iat(&self, idx: Idx) -> &Self::Output {
        debug_assert!(self.get(idx.clone()).is_some(), "Index out of bounds");
        if cfg!(feature = "prohibit-unsafe") {
            self.index(idx)
        } else {
            unsafe { self.get_unchecked(idx) }
        }
    }

    #[inline(always)]
    fn mat(&mut self, idx: Idx) -> &mut Self::Output {
        debug_assert!(self.get(idx.clone()).is_some(), "Index out of bounds");
        if cfg!(feature = "prohibit-unsafe") {
            self.index_mut(idx)
        } else {
            unsafe { self.get_unchecked_mut(idx) }
        }
    }
}

/// \return the first byte of a UTF-8 encoded code point.
/// We do not use char because we don't want to deal with failing on surrogates.
pub fn utf8_first_byte(cp: u32) -> u8 {
    debug_assert!(cp <= CODE_POINT_MAX);
    if cp < 0x80 {
        // One byte encoding.
        cp as u8
    } else if cp < 0x800 {
        // Two byte encoding.
        (cp >> 6 & 0x1F) as u8 | 0b1100_0000
    } else if cp < 0x10000 {
        // Three byte encoding.
        (cp >> 12 & 0x0F) as u8 | 0b1110_0000
    } else {
        // Four byte encoding.
        (cp >> 18 & 0x07) as u8 | 0b1111_0000
    }
}

pub trait SliceHelp {
    type Item;

    /// Given that self is sorted according to f, returns the range of indexes
    /// where f indicates equal elements.
    fn equal_range_by<'a, F>(&'a self, f: F) -> std::ops::Range<usize>
    where
        F: FnMut(&'a Self::Item) -> Ordering;
}

impl<T> SliceHelp for [T] {
    type Item = T;
    fn equal_range_by<'a, F>(&'a self, mut f: F) -> std::ops::Range<usize>
    where
        F: FnMut(&'a Self::Item) -> Ordering,
    {
        let left = self
            .binary_search_by(|v| f(v).then(Ordering::Greater))
            .unwrap_err();
        let right = self[left..]
            .binary_search_by(|v| f(v).then(Ordering::Less))
            .unwrap_err()
            + left;
        left..right
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ranges() {
        use super::SliceHelp;
        let vals = [0, 1, 2, 3, 4, 4, 4, 7, 8, 9, 9];
        let fast_er = |needle: usize| vals.equal_range_by(|v| v.cmp(&needle));
        let slow_er = |needle: usize| {
            let mut left = 0;
            while left < vals.len() && vals[left] < needle {
                left += 1
            }
            let mut right = left;
            while right < vals.len() && vals[right] == needle {
                right += 1
            }
            left..right
        };

        for i in 0..10 {
            assert_eq!(fast_er(i), slow_er(i))
        }
    }

    #[test]
    fn utf8() {
        for &cp in &[
            0x0,
            0x7,
            0xFF,
            0x80,
            0xABC,
            0x7FF,
            0x800,
            0x801,
            0xFFFF,
            0x10000,
            0x10001,
            0x1FFFF,
            super::CODE_POINT_MAX - 1,
            super::CODE_POINT_MAX,
        ] {
            let mut buff = [0; 4];
            std::char::from_u32(cp).unwrap().encode_utf8(&mut buff);
            assert_eq!(buff[0], super::utf8_first_byte(cp));
        }
    }
}
