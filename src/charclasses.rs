use crate::codepointset::Interval;

// Character classes like \d or \S.

/// Construct an interval from an inclusive range of char.
const fn r(first: char, last: char) -> Interval {
    Interval {
        first: first as u32,
        last: last as u32,
    }
}

/// Construct an interval from a single char.
const fn r1(c: char) -> Interval {
    Interval {
        first: c as u32,
        last: c as u32,
    }
}

// Note all of these are sorted.

/// ES9 21.2.2.6.1.
pub const WORD_CHARS: [Interval; 4] = [r('0', '9'), r('A', 'Z'), r1('_'), r('a', 'z')];

/// ES9 21.2.2.12
pub const DIGITS: [Interval; 1] = [r('0', '9')];

/// ES9 11.2
pub const WHITESPACE: [Interval; 8] = [
    r('\u{0009}', '\u{000C}'),
    r1('\u{0020}'),
    r1('\u{1680}'),
    r('\u{2000}', '\u{200A}'),
    r1('\u{202F}'),
    r1('\u{205F}'),
    r1('\u{3000}'),
    r1('\u{FEFF}'),
];

/// ES9 11.3
pub const LINE_TERMINATOR: [Interval; 3] =
    [r1('\u{000A}'), r1('\u{000D}'), r('\u{2028}', '\u{2029}')];
