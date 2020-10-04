use crate::cursor::Cursorable;
use crate::folds;
use crate::indexing::{ElementType, Position};
use crate::types::{BracketContents, Range};

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

/// Check whether the reference to \p range within \p cursor matches position \p
/// pos.
pub fn backref<Cursor: Cursorable>(
    orig_range: Range,
    position: &mut Position,
    cursor: Cursor,
) -> bool {
    cursor.subrange_eq(position, orig_range)
}

pub fn backref_icase<Cursor: Cursorable>(
    orig_range: Range,
    position: &mut Position,
    cursor: Cursor,
) -> bool {
    let ref_cursor = cursor.subcursor(orig_range.clone());
    let mut ref_pos = if Cursor::FORWARD {
        Position(0)
    } else {
        Position(orig_range.end - orig_range.start)
    };
    while let Some(c1) = ref_cursor.next(&mut ref_pos) {
        let mut matched = false;
        if let Some(c2) = cursor.next(position) {
            matched = Cursor::CharProps::fold_equals(c1, c2)
        }
        if !matched {
            return false;
        }
    }
    true
}
