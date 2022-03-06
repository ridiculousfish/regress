use crate::cursor;
use crate::cursor::Direction;
use crate::indexing::{ElementType, InputIndexer};
use crate::types::BracketContents;
use crate::unicode;

pub trait CharProperties {
    type Element: ElementType;

    /// Case-fold an element.
    fn fold(c: Self::Element) -> Self::Element;

    /// \return whether these two elements fold to the same value.
    fn fold_equals(c1: Self::Element, c2: Self::Element) -> bool {
        c1 == c2 || Self::fold(c1) == Self::fold(c2)
    }

    /// \return whether this is a word char.
    /// ES9 21.2.2.6.2.
    fn is_word_char(c: Self::Element) -> bool {
        let c = c.as_u32();
        'a' as u32 <= c && c <= 'z' as u32
            || 'A' as u32 <= c && c <= 'Z' as u32
            || '0' as u32 <= c && c <= '9' as u32
            || c == '_' as u32
    }

    /// ES9 11.3
    fn is_line_terminator(c: Self::Element) -> bool {
        match c.as_u32() {
            0x000A | 0x000D | 0x2028 | 0x2029 => true,
            _ => false,
        }
    }

    /// \return whether the bracket \p bc matches the given character \p c,
    /// respecting case. Respects 'invert'.
    #[inline(always)]
    fn bracket(bc: &BracketContents, c: Self::Element) -> bool {
        let cp = c.into();
        let contained = bc.cps.intervals().iter().any(|r| r.contains(cp));
        contained ^ bc.invert
    }
}

pub struct UTF8CharProperties {}

impl CharProperties for UTF8CharProperties {
    type Element = char;

    fn fold(c: Self::Element) -> Self::Element {
        char::from_u32(unicode::fold(c.as_u32())).unwrap()
    }
}

pub struct ASCIICharProperties {}
impl CharProperties for ASCIICharProperties {
    type Element = u8;

    fn fold(c: Self::Element) -> Self::Element {
        c.to_ascii_lowercase()
    }
}

/// Check whether the \p orig_range within \p cursor matches position \p pos.
pub fn backref<Input: InputIndexer, Dir: Direction>(
    input: &Input,
    dir: Dir,
    orig_range: std::ops::Range<Input::Position>,
    pos: &mut Input::Position,
) -> bool {
    cursor::subrange_eq(input, dir, pos, orig_range.start, orig_range.end)
}

pub fn backref_icase<Input: InputIndexer, Dir: Direction>(
    input: &Input,
    dir: Dir,
    orig_range: std::ops::Range<Input::Position>,
    pos: &mut Input::Position,
) -> bool {
    let ref_input = input.subinput(orig_range);
    let mut ref_pos = if Dir::FORWARD {
        ref_input.left_end()
    } else {
        ref_input.right_end()
    };
    while let Some(c1) = cursor::next(&ref_input, dir, &mut ref_pos) {
        let mut matched = false;
        if let Some(c2) = cursor::next(input, dir, pos) {
            matched = Input::CharProps::fold_equals(c1, c2)
        }
        if !matched {
            return false;
        }
    }
    true
}
