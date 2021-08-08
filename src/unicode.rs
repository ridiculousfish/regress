use crate::codepointset::{CodePointSet, Interval};
use crate::unicodetables::{self, FOLDS, ID_CONTINUE, ID_START};
use crate::util::SliceHelp;
use std::cmp::Ordering;

// CodePointRange packs a code point and a length together into a u32.
// We currently do not need to store any information about code points in plane 16 (U+100000),
// which are private use, so we only need 20 bits of code point storage;
// the remaining 12 can be the length.
// The length is stored with a bias of -1, so the last codepoint may be obtained by adding the "length" and the first code point.
const CODE_POINT_BITS: u32 = 20;
const LENGTH_BITS: u32 = 32 - CODE_POINT_BITS;

#[derive(Copy, Clone, Debug)]
pub struct CodePointRange(u32);

// This will trigger an error in const functions if $x is false.
macro_rules! const_assert_true {
    ($x:expr $(,)*) => {
        let _ = [()][!$x as usize];
    };
}

impl CodePointRange {
    #[inline(always)]
    pub const fn from(start: u32, len: u32) -> Self {
        const_assert_true!(start < (1 << CODE_POINT_BITS));
        const_assert_true!(len > 0 && len <= (1 << LENGTH_BITS));
        const_assert_true!((start + len - 1) < ((1 << CODE_POINT_BITS) - 1));
        CodePointRange((start << LENGTH_BITS) | (len - 1))
    }

    #[inline(always)]
    const fn len_minus_1(self) -> u32 {
        self.0 & ((1 << LENGTH_BITS) - 1)
    }

    // \return the first codepoint in the range.
    #[inline(always)]
    pub const fn first(self) -> u32 {
        self.0 >> LENGTH_BITS
    }

    // \return the last codepoint in the range.
    #[inline(always)]
    pub const fn last(self) -> u32 {
        self.first() + self.len_minus_1() as u32
    }

    /// \return whether this range is strictly less than, contains, or strictly greater than a given code point.
    #[inline(always)]
    pub fn compare(self, cp: u32) -> Ordering {
        if self.first() > cp {
            Ordering::Greater
        } else if self.last() < cp {
            Ordering::Less
        } else {
            Ordering::Equal
        }
    }
}

/// CodePointRangeUnpacked is used when the max of CodePointRange would be exceeded.
#[derive(Copy, Clone, Debug)]
pub struct CodePointRangeUnpacked(u32, u32);

impl CodePointRangeUnpacked {
    #[inline(always)]
    pub const fn from(start: u32, end: u32) -> Self {
        CodePointRangeUnpacked(start, end)
    }

    // Compares the range to a single codepoint.
    #[inline(always)]
    pub fn compare(self, cp: u32) -> Ordering {
        if cp < self.0 {
            core::cmp::Ordering::Greater
        } else if cp > self.1 {
            core::cmp::Ordering::Less
        } else {
            core::cmp::Ordering::Equal
        }
    }
}

// The "extra" field contains a predicate mask in the low bits and a signed delta amount in the high bits.
// A code point only transforms if its difference from the range base is 0 once masked.
const PREDICATE_MASK_BITS: u32 = 4;

pub(crate) struct FoldRange {
    /// The range of codepoints.
    pub(crate) range: CodePointRange,

    /// Combination of the signed delta amount and predicate mask.
    pub(crate) extra: i32,
}

impl FoldRange {
    #[inline(always)]
    pub const fn from(start: u32, length: u32, delta: i32, modulo: u8) -> Self {
        let mask = (1 << modulo) - 1;
        const_assert_true!(mask < (1 << PREDICATE_MASK_BITS));
        const_assert_true!(((delta << PREDICATE_MASK_BITS) >> PREDICATE_MASK_BITS) == delta);
        let extra = mask | (delta << PREDICATE_MASK_BITS);
        FoldRange {
            range: CodePointRange::from(start, length),
            extra,
        }
    }
    #[inline(always)]
    fn first(&self) -> u32 {
        self.range.first()
    }

    #[inline(always)]
    fn last(&self) -> u32 {
        self.range.last()
    }

    #[inline(always)]
    fn delta(&self) -> i32 {
        self.extra >> PREDICATE_MASK_BITS
    }

    #[inline(always)]
    fn predicate_mask(&self) -> u32 {
        (self.extra as u32) & PREDICATE_MASK_BITS
    }

    fn add_delta(&self, cu: u32) -> u32 {
        let cs = (cu as i32) + self.delta();
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
        if (offset & self.predicate_mask()) == 0 {
            self.add_delta(cu)
        } else {
            cu
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

/// \return whether c has the 'ID_Start' Unicode property.
pub(crate) fn is_id_start(c: char) -> bool {
    let i = c as u32;
    ID_START.binary_search_by(|&cpr| cpr.compare(i)).is_ok()
}

/// \return whether c has the 'ID_Continue' Unicode property.
pub(crate) fn is_id_continue(c: char) -> bool {
    let i = c as u32;
    ID_CONTINUE.binary_search_by(|&cpr| cpr.compare(i)).is_ok()
}

#[derive(Debug, Clone)]
pub struct PropertyEscape {
    pub name: Option<UnicodePropertyName>,
    pub value: UnicodePropertyValue,
}

#[derive(Debug, Clone)]
pub enum UnicodePropertyName {
    GeneralCategory,
    Script,
    ScriptExtensions,
}

pub fn unicode_property_name_from_str(s: &str) -> Option<UnicodePropertyName> {
    use UnicodePropertyName::*;

    match s {
        "General_Category" | "gc" => Some(GeneralCategory),
        "Script" | "sc" => Some(Script),
        "Script_Extensions" | "scx" => Some(ScriptExtensions),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub enum UnicodePropertyValue {
    Binary(unicodetables::UnicodePropertyBinary),
    GeneralCategory(unicodetables::UnicodePropertyValueGeneralCategory),
    Script(unicodetables::UnicodePropertyValueScript),
}

pub fn unicode_property_value_from_str(s: &str) -> Option<UnicodePropertyValue> {
    if let Some(t) = unicodetables::unicode_property_binary_from_str(s) {
        Some(UnicodePropertyValue::Binary(t))
    } else if let Some(t) = unicodetables::unicode_property_value_general_category_from_str(s) {
        Some(UnicodePropertyValue::GeneralCategory(t))
    } else {
        unicodetables::unicode_property_value_script_from_str(s).map(UnicodePropertyValue::Script)
    }
}

pub(crate) fn is_character_class(c: char, property_escape: &PropertyEscape) -> bool {
    match property_escape.name {
        Some(UnicodePropertyName::GeneralCategory) => match &property_escape.value {
            UnicodePropertyValue::GeneralCategory(t) => {
                unicodetables::is_property_value_general_category(c, t)
            }
            _ => false,
        },
        Some(UnicodePropertyName::Script) => match &property_escape.value {
            UnicodePropertyValue::Script(t) => unicodetables::is_property_value_script(c, t),
            _ => false,
        },
        Some(UnicodePropertyName::ScriptExtensions) => match &property_escape.value {
            UnicodePropertyValue::Script(t) => unicodetables::is_property_value_script(c, t),
            _ => false,
        },
        None => match &property_escape.value {
            UnicodePropertyValue::Binary(t) => unicodetables::is_property_binary(c, t),
            UnicodePropertyValue::GeneralCategory(t) => {
                unicodetables::is_property_value_general_category(c, t)
            }
            UnicodePropertyValue::Script(t) => unicodetables::is_property_value_script(c, t),
        },
    }
}
