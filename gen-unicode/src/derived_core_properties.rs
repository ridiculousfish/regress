use crate::{MAX_CODE_POINT, MAX_LENGTH};
use std::fs::File;
use std::io::{self, BufRead};

// Parse line from `DerivedCoreProperties.txt` with the following syntax:
// `0061..007A    ; ID_Start # L&  [26] LATIN SMALL LETTER A..LATIN SMALL LETTER Z`
// `00AA          ; ID_Start # Lo       FEMININE ORDINAL INDICATOR`
fn parse_line(line: &str, chars: &mut Vec<(u32, u32)>, property: &str) {
    let split_str = format!("; {}", property);
    let mut line_iter = line.split(&split_str);

    if let Some(codepoint_hexes) = line_iter.next() {
        if line_iter.next().is_none() {
            return;
        }

        let mut iter = codepoint_hexes.split("..");
        if let Some(first_hex) = iter.next() {
            if let Some(second_hex) = iter.next() {
                let first_int = u32::from_str_radix(first_hex.trim(), 16).unwrap();
                let second_int = u32::from_str_radix(second_hex.trim(), 16).unwrap();
                chars.push((first_int, second_int));
            } else {
                let i = u32::from_str_radix(first_hex.trim(), 16).unwrap();
                chars.push((i, i))
            }
        }
    }
}

// Given a list of inclusive ranges of code points, return a list of strings creating corresponding CodePointRange.
// If a range is too big, it is split into smaller abutting ranges.
fn chars_to_code_point_ranges(chars: &[(u32, u32)]) -> Vec<String> {
    chars
        .iter()
        .flat_map(|p| {
            let (mut start, end) = *p;
            assert!(end >= start, "Range out of order");
            assert!(
                end <= MAX_CODE_POINT,
                "end exceeds bits allocated for code point"
            );
            let mut res = Vec::new();
            let mut len = end - start + 1;
            while len > 0 {
                let amt = std::cmp::min(len, MAX_LENGTH);
                res.push(format!("CodePointRange::from({}, {}),", start, amt));
                start += amt;
                len -= amt;
            }
            res
        })
        .collect()
}

pub(crate) fn generate_id_start() -> String {
    let file =
        File::open("DerivedCoreProperties.txt").expect("could not open DerivedCoreProperties.txt");
    let lines = io::BufReader::new(file).lines();
    let mut chars = Vec::new();

    for line in lines {
        parse_line(&line.unwrap(), &mut chars, "ID_Start");
    }
    let ranges = chars_to_code_point_ranges(&chars);

    let result = format!(
        r#"
pub(crate) const ID_START: [CodePointRange; {}] = [
    {}
];
"#,
        ranges.len(),
        ranges.join("\n    ")
    );

    result
}

pub(crate) fn generate_id_continue() -> String {
    let file =
        File::open("DerivedCoreProperties.txt").expect("could not open DerivedCoreProperties.txt");
    let lines = io::BufReader::new(file).lines();
    let mut chars = Vec::new();

    for line in lines {
        parse_line(&line.unwrap(), &mut chars, "ID_Continue");
    }
    let ranges = chars_to_code_point_ranges(&chars);

    let result = format!(
        r#"pub(crate) const ID_CONTINUE: [CodePointRange; {}] = [
    {}
];
"#,
        ranges.len(),
        ranges.join("\n    ")
    );

    result
}
