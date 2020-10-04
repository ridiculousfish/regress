use crate::cursor;
use crate::cursor::Direction;
use crate::folds;
use crate::indexing::{ElementType, InputIndexer};
use crate::types::BracketContents;

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
        match c.as_char() {
            'a'..='z' => true,
            'A'..='Z' => true,
            '0'..='9' => true,
            '_' => true,
            _ => false,
        }
    }

    /// ES9 11.3
    fn is_line_terminator(c: Self::Element) -> bool {
        let c = c.as_char();
        c == '\u{000A}' || c == '\u{000D}' || c == '\u{2028}' || c == '\u{2029}'
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
        folds::fold(c)
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
