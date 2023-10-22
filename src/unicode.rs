use crate::codepointset::{CodePointSet, Interval};
use crate::indexing::ElementType;
use crate::unicodetables::{self, UnicodePropertyBinary, FOLDS};
use crate::util::SliceHelp;
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use core::cmp::Ordering;
use icu_properties::{GeneralCategory, GeneralCategoryGroup, Script};

// CodePointRange packs a code point and a length together into a u32.
// We currently do not need to store any information about code points in plane 16 (U+100000),
// which are private use, so we only need 20 bits of code point storage;
// the remaining 12 can be the length.
// The length is stored with a bias of -1, so the last codepoint may be obtained by adding the "length" and the first code point.
const CODE_POINT_BITS: u32 = 20;
const LENGTH_BITS: u32 = 32 - CODE_POINT_BITS;

#[derive(Copy, Clone, Debug)]
struct CodePointRange(u32);

// This will trigger an error in const functions if $x is false.
macro_rules! const_assert_true {
    ($x:expr $(,)*) => {
        [()][!$x as usize];
    };
}

impl CodePointRange {
    #[inline(always)]
    const fn from(start: u32, len: u32) -> Self {
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
    const fn first(self) -> u32 {
        self.0 >> LENGTH_BITS
    }

    // \return the last codepoint in the range.
    #[inline(always)]
    const fn last(self) -> u32 {
        self.first() + self.len_minus_1()
    }
}

// The "extra" field contains a predicate mask in the low bits and a signed delta amount in the high bits.
// A code point only transforms if its difference from the range base is 0 once masked.
const PREDICATE_MASK_BITS: u32 = 4;

pub(crate) struct FoldRange {
    /// The range of codepoints.
    range: CodePointRange,

    /// Combination of the signed delta amount and predicate mask.
    extra: i32,
}

impl FoldRange {
    #[inline(always)]
    pub(crate) const fn from(start: u32, length: u32, delta: i32, modulo: u8) -> Self {
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
        core::debug_assert!(0 <= cs && cs <= 0x10FFFF);
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

pub(crate) fn fold(cu: u32) -> u32 {
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
        fr.apply(cu)
    } else {
        cu
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
        let first_trans = core::cmp::max(fr.first(), iv.first);
        let last_trans = core::cmp::min(fr.last(), iv.last);
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
pub(crate) fn unfold_char(c: u32) -> Vec<u32> {
    let mut res = vec![c];
    let fcp = fold(c);
    if fcp != c {
        res.push(fcp);
    }
    // TODO: optimize ASCII case.
    for tr in FOLDS.iter() {
        if !tr.transformed_to().contains(fcp) {
            continue;
        }
        for cp in tr.transformed_from().codepoints() {
            // TODO: this can be optimized.
            let tcp = tr.apply(cp);
            if tcp == fcp {
                res.push(cp);
            }
        }
    }
    res.sort_unstable();
    res.dedup();
    res
}

// Fold every character in \p input, then find all the prefolds.
pub(crate) fn fold_code_points(mut input: CodePointSet) -> CodePointSet {
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

#[derive(Debug, Copy, Clone)]
pub struct PropertyEscape {
    pub(crate) name: Option<UnicodePropertyName>,
    pub(crate) value: UnicodePropertyValue,
}

#[derive(Debug, Copy, Clone)]
pub(crate) enum UnicodePropertyName {
    GeneralCategory,
    Script,
    ScriptExtensions,
}

pub(crate) fn unicode_property_name_from_str(s: &str) -> Option<UnicodePropertyName> {
    use UnicodePropertyName::*;

    match s {
        "General_Category" | "gc" => Some(GeneralCategory),
        "Script" | "sc" => Some(Script),
        "Script_Extensions" | "scx" => Some(ScriptExtensions),
        _ => None,
    }
}

#[derive(Debug, Copy, Clone)]
pub(crate) enum UnicodePropertyValue {
    Binary(UnicodePropertyBinary),
    GeneralCategory(GeneralCategoryGroup),
    Script(Script),
}

pub(crate) fn unicode_property_value_from_str(s: &str) -> Option<UnicodePropertyValue> {
    if let Some(t) = unicodetables::unicode_property_binary_from_str(s) {
        Some(UnicodePropertyValue::Binary(t))
    } else if let Some(t) = GeneralCategoryGroup::name_to_enum_mapper().get_strict(s) {
        Some(UnicodePropertyValue::GeneralCategory(t))
    } else {
        Script::name_to_enum_mapper()
            .get_strict(s)
            .map(UnicodePropertyValue::Script)
    }
}

pub(crate) fn is_character_class(c: u32, property_escape: &PropertyEscape) -> bool {
    if let Some(c) = char::from_u32(c) {
        match property_escape.name {
            Some(UnicodePropertyName::GeneralCategory) => match &property_escape.value {
                UnicodePropertyValue::GeneralCategory(t) => {
                    t.contains(icu_properties::maps::general_category().get(c))
                }
                _ => false,
            },
            Some(UnicodePropertyName::Script) => match &property_escape.value {
                UnicodePropertyValue::Script(t) => icu_properties::maps::script().get(c) == *t,
                _ => false,
            },
            Some(UnicodePropertyName::ScriptExtensions) => match &property_escape.value {
                UnicodePropertyValue::Script(t) => icu_properties::maps::script().get(c) == *t,
                _ => false,
            },
            None => match &property_escape.value {
                UnicodePropertyValue::Binary(t) => tatat(c, *t),
                UnicodePropertyValue::GeneralCategory(t) => {
                    t.contains(icu_properties::maps::general_category().get(c))
                }
                UnicodePropertyValue::Script(t) => icu_properties::maps::script().get(c) == *t,
            },
        }
    } else {
        false
    }
}

fn tatat(ch: char, t: UnicodePropertyBinary) -> bool {
    match t {
        UnicodePropertyBinary::Alphabetic => icu_properties::sets::alphabetic().contains(ch),
        UnicodePropertyBinary::CaseIgnorable => icu_properties::sets::case_ignorable().contains(ch),
        UnicodePropertyBinary::Cased => icu_properties::sets::cased().contains(ch),
        UnicodePropertyBinary::ChangesWhenCasefolded => {
            icu_properties::sets::changes_when_casefolded().contains(ch)
        }
        UnicodePropertyBinary::ChangesWhenCasemapped => {
            icu_properties::sets::changes_when_casemapped().contains(ch)
        }
        UnicodePropertyBinary::ChangesWhenLowercased => {
            icu_properties::sets::changes_when_lowercased().contains(ch)
        }
        UnicodePropertyBinary::ChangesWhenTitlecased => {
            icu_properties::sets::changes_when_titlecased().contains(ch)
        }
        UnicodePropertyBinary::ChangesWhenUppercased => {
            icu_properties::sets::changes_when_uppercased().contains(ch)
        }
        UnicodePropertyBinary::DefaultIgnorableCodePoint => {
            icu_properties::sets::default_ignorable_code_point().contains(ch)
        }
        UnicodePropertyBinary::GraphemeBase => icu_properties::sets::grapheme_base().contains(ch),
        UnicodePropertyBinary::GraphemeExtend => {
            icu_properties::sets::grapheme_extend().contains(ch)
        }
        UnicodePropertyBinary::IDContinue => icu_properties::sets::id_continue().contains(ch),
        UnicodePropertyBinary::IDStart => icu_properties::sets::id_start().contains(ch),
        UnicodePropertyBinary::Math => icu_properties::sets::math().contains(ch),
        UnicodePropertyBinary::XIDContinue => icu_properties::sets::xid_continue().contains(ch),
        UnicodePropertyBinary::XIDStart => icu_properties::sets::xid_start().contains(ch),
        UnicodePropertyBinary::ASCIIHexDigit => {
            icu_properties::sets::ascii_hex_digit().contains(ch)
        }
        UnicodePropertyBinary::BidiControl => icu_properties::sets::bidi_control().contains(ch),
        UnicodePropertyBinary::Dash => icu_properties::sets::dash().contains(ch),
        UnicodePropertyBinary::Deprecated => icu_properties::sets::deprecated().contains(ch),
        UnicodePropertyBinary::Diacritic => icu_properties::sets::diacritic().contains(ch),
        UnicodePropertyBinary::Extender => icu_properties::sets::extender().contains(ch),
        UnicodePropertyBinary::HexDigit => icu_properties::sets::hex_digit().contains(ch),
        UnicodePropertyBinary::IDSBinaryOperator => {
            icu_properties::sets::ids_binary_operator().contains(ch)
        }
        UnicodePropertyBinary::IDSTrinaryOperator => {
            icu_properties::sets::ids_trinary_operator().contains(ch)
        }
        UnicodePropertyBinary::Ideographic => icu_properties::sets::ideographic().contains(ch),
        UnicodePropertyBinary::JoinControl => icu_properties::sets::join_control().contains(ch),
        UnicodePropertyBinary::LogicalOrderException => {
            icu_properties::sets::logical_order_exception().contains(ch)
        }
        UnicodePropertyBinary::Lowercase => icu_properties::sets::lowercase().contains(ch),
        UnicodePropertyBinary::NoncharacterCodePoint => {
            icu_properties::sets::noncharacter_code_point().contains(ch)
        }
        UnicodePropertyBinary::PatternSyntax => icu_properties::sets::pattern_syntax().contains(ch),
        UnicodePropertyBinary::PatternWhiteSpace => {
            icu_properties::sets::pattern_white_space().contains(ch)
        }
        UnicodePropertyBinary::QuotationMark => icu_properties::sets::quotation_mark().contains(ch),
        UnicodePropertyBinary::Radical => icu_properties::sets::radical().contains(ch),
        UnicodePropertyBinary::RegionalIndicator => {
            icu_properties::sets::regional_indicator().contains(ch)
        }
        UnicodePropertyBinary::SentenceTerminal => {
            icu_properties::sets::sentence_terminal().contains(ch)
        }
        UnicodePropertyBinary::SoftDotted => icu_properties::sets::soft_dotted().contains(ch),
        UnicodePropertyBinary::TerminalPunctuation => {
            icu_properties::sets::terminal_punctuation().contains(ch)
        }
        UnicodePropertyBinary::UnifiedIdeograph => {
            icu_properties::sets::unified_ideograph().contains(ch)
        }
        UnicodePropertyBinary::Uppercase => icu_properties::sets::uppercase().contains(ch),
        UnicodePropertyBinary::VariationSelector => {
            icu_properties::sets::variation_selector().contains(ch)
        }
        UnicodePropertyBinary::WhiteSpace => icu_properties::sets::white_space().contains(ch),
        UnicodePropertyBinary::Emoji => icu_properties::sets::emoji().contains(ch),
        UnicodePropertyBinary::EmojiComponent => {
            icu_properties::sets::emoji_component().contains(ch)
        }
        UnicodePropertyBinary::EmojiModifier => icu_properties::sets::emoji_modifier().contains(ch),
        UnicodePropertyBinary::EmojiModifierBase => {
            icu_properties::sets::emoji_modifier_base().contains(ch)
        }
        UnicodePropertyBinary::EmojiPresentation => {
            icu_properties::sets::emoji_presentation().contains(ch)
        }
        UnicodePropertyBinary::ExtendedPictographic => {
            icu_properties::sets::extended_pictographic().contains(ch)
        }
        UnicodePropertyBinary::ChangesWhenNFKCCasefolded => {
            icu_properties::sets::changes_when_nfkc_casefolded().contains(ch)
        }
        UnicodePropertyBinary::BidiMirrored => icu_properties::sets::bidi_mirrored().contains(ch),
        UnicodePropertyBinary::Ascii => ch.is_ascii(),
        UnicodePropertyBinary::Any => ch.as_u32() <= 0x10FFFF,
        UnicodePropertyBinary::Assigned => {
            icu_properties::maps::general_category().get(ch) != GeneralCategory::Unassigned
        }
    }
}
