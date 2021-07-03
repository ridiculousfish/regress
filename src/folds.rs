use crate::codepointset::{CodePointSet, Interval};
use crate::unicode::{FoldRange, FOLDS};
use crate::util::SliceHelp;
use std::cmp::Ordering;

impl FoldRange {
    fn first(&self) -> u32 {
        self.start as u32
    }

    fn last(&self) -> u32 {
        self.start as u32 + self.length as u32 - 1
    }

    fn add_delta(&self, cu: u32) -> u32 {
        let cs = (cu as i32) + self.delta;
        std::debug_assert!(0 <= cs && cs <= 0x10FFFF);
        cs as u32
    }

    /// \return the Interval of transformed-to code points.
    fn transformed_to(&self) -> Interval {
        Interval {
            first: self.add_delta(self.first()),
            last: self.add_delta(self.last()),
        }
    }

    /// \return the Interval of transformed-from code points.
    fn transformed_from(&self) -> Interval {
        Interval {
            first: self.first(),
            last: self.last(),
        }
    }

    fn can_apply(&self, cu: u32) -> bool {
        self.transformed_from().contains(cu)
    }

    fn apply(&self, cu: u32) -> u32 {
        debug_assert!(self.can_apply(cu), "Cannot apply to this code point");
        let offset = cu - self.first();
        if offset % (self.modulo as u32) != 0 {
            cu
        } else {
            self.add_delta(cu)
        }
    }
}

pub fn fold(c: char) -> char {
    let cu = c as u32;
    let searched = FOLDS.binary_search_by(|fr| {
        if fr.first() > cu {
            Ordering::Greater
        } else if fr.last() < cu {
            Ordering::Less
        } else {
            Ordering::Equal
        }
    });
    if let Ok(index) = searched {
        let fr: &FoldRange = if cfg!(feature = "prohibit-unsafe") {
            unsafe { FOLDS.get_unchecked(index) }
        } else {
            FOLDS.get(index).expect("Invalid index")
        };
        let cs = fr.apply(cu);
        if cfg!(feature = "prohibit-unsafe") {
            unsafe { std::char::from_u32_unchecked(cs) }
        } else {
            std::char::from_u32(cs).expect("Char should have been in bounds")
        }
    } else {
        c
    }
}

fn fold_interval(iv: Interval, recv: &mut CodePointSet) {
    // Find the range of folds which overlap our interval.
    let overlaps = FOLDS.equal_range_by(|tr| {
        if tr.first() > iv.last {
            Ordering::Greater
        } else if tr.last() < iv.first {
            Ordering::Less
        } else {
            Ordering::Equal
        }
    });
    for fr in &FOLDS[overlaps] {
        debug_assert!(
            fr.transformed_from().overlaps(iv),
            "Interval does not overlap transform"
        );
        // Find the (inclusive) range of our interval that this transform covers.
        // TODO: could walk by modulo amount.
        // TODO: optimize for cases when modulo is 1.
        let first_trans = std::cmp::max(fr.first(), iv.first);
        let last_trans = std::cmp::min(fr.last(), iv.last);
        for cu in first_trans..(last_trans + 1) {
            let cs = fr.apply(cu);
            if cs != cu {
                recv.add_one(cs)
            }
        }
    }
}

/// This is a slow linear search across all ranges.
fn unfold_interval(iv: Interval, recv: &mut CodePointSet) {
    // TODO: optimize ASCII case.
    for tr in FOLDS.iter() {
        if !iv.overlaps(tr.transformed_to()) {
            continue;
        }
        for cp in tr.transformed_from().codepoints() {
            // TODO: this can be optimized.
            let tcp = tr.apply(cp);
            if tcp != cp && iv.contains(tcp) {
                recv.add_one(cp);
            }
        }
    }
}

/// \return all the characters which fold to c's fold.
/// This is a slow linear search across all ranges.
pub fn unfold_char(c: char) -> Vec<char> {
    let mut res = vec![c];
    let fc = fold(c);
    if fc != c {
        res.push(fc)
    }
    // TODO: optimize ASCII case.
    let fcp = fc as u32;
    for tr in FOLDS.iter() {
        if !tr.transformed_to().contains(fcp) {
            continue;
        }
        for cp in tr.transformed_from().codepoints() {
            // TODO: this can be optimized.
            let tcp = tr.apply(cp);
            if tcp == fcp {
                res.push(std::char::from_u32(cp).unwrap());
            }
        }
    }
    res.sort_unstable();
    res.dedup();
    res
}

// Fold every character in \p input, then find all the prefolds.
pub fn fold_code_points(mut input: CodePointSet) -> CodePointSet {
    let mut folded = input.clone();
    for iv in input.intervals() {
        fold_interval(*iv, &mut folded)
    }

    // Reuse input storage.
    input.clone_from(&folded);
    for iv in folded.intervals() {
        unfold_interval(*iv, &mut input);
    }
    input
}
