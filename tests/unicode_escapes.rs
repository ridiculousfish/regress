// Work around dead code warnings: rust-lang issue #46379
pub mod common;

// Work around dead code warnings: rust-lang issue #46379
use common::*;

#[test]
fn property_escapes_invalid() {
    // From 262 test/built-ins/RegExp/property-escapes/
    test_parse_fails(r#"\P{ASCII=F}"#);
    test_parse_fails(r#"\p{ASCII=F}"#);
    test_parse_fails(r#"\P{ASCII=Invalid}"#);
    test_parse_fails(r#"\p{ASCII=Invalid}"#);
    test_parse_fails(r#"\P{ASCII=N}"#);
    test_parse_fails(r#"\p{ASCII=N}"#);
    test_parse_fails(r#"\P{ASCII=No}"#);
    test_parse_fails(r#"\p{ASCII=No}"#);
    test_parse_fails(r#"\P{ASCII=T}"#);
    test_parse_fails(r#"\p{ASCII=T}"#);
    test_parse_fails(r#"\P{ASCII=Y}"#);
    test_parse_fails(r#"\p{ASCII=Y}"#);
    test_parse_fails(r#"\P{ASCII=Yes}"#);
    test_parse_fails(r#"\p{ASCII=Yes}"#);
    // TODO: implement property escape in brackets
    //test_parse_fails(r#"[--\p{Hex}]"#);
    //test_parse_fails(r#"[\uFFFF-\p{Hex}]"#);
    //test_parse_fails(r#"[\p{Hex}-\uFFFF]"#);
    //test_parse_fails(r#"[\p{Hex}--]"#);
    test_parse_fails(r#"\P{^General_Category=Letter}"#);
    test_parse_fails(r#"\p{^General_Category=Letter}"#);
    // TODO: implement property escape in brackets
    //test_parse_fails(r#"[\p{}]"#);
    //test_parse_fails(r#"[\P{}]"#);
    test_parse_fails(r#"\P{InAdlam}"#);
    test_parse_fails(r#"\p{InAdlam}"#);
    test_parse_fails(r#"\P{InAdlam}"#);
    test_parse_fails(r#"\p{InAdlam}"#);
    test_parse_fails(r#"\P{InScript=Adlam}"#);
    test_parse_fails(r#"\p{InScript=Adlam}"#);
    // TODO: implement property escape in brackets
    //test_parse_fails(r#"[\P{invalid}]"#);
    //test_parse_fails(r#"[\p{invalid}]"#);
    test_parse_fails(r#"\P{IsScript=Adlam}"#);
    test_parse_fails(r#"\p{IsScript=Adlam}"#);
    test_parse_fails(r#"\P"#);
    test_parse_fails(r#"\PL"#);
    test_parse_fails(r#"\pL"#);
    test_parse_fails(r#"\p"#);
    test_parse_fails(r#"\P{=Letter}"#);
    test_parse_fails(r#"\p{=Letter}"#);
    test_parse_fails(r#"\P{General_Category:Letter}"#);
    test_parse_fails(r#"\P{=}"#);
    test_parse_fails(r#"\p{=}"#);
    test_parse_fails(r#"\p{General_Category:Letter}"#);
    test_parse_fails(r#"\P{"#);
    test_parse_fails(r#"\p{"#);
    test_parse_fails(r#"\P}"#);
    test_parse_fails(r#"\p}"#);
    test_parse_fails(r#"\P{ General_Category=Uppercase_Letter }"#);
    test_parse_fails(r#"\p{ General_Category=Uppercase_Letter }"#);
    test_parse_fails(r#"\P{ Lowercase }"#);
    test_parse_fails(r#"\p{ Lowercase }"#);
    test_parse_fails(r#"\P{ANY}"#);
    test_parse_fails(r#"\p{ANY}"#);
    test_parse_fails(r#"\P{ASSIGNED}"#);
    test_parse_fails(r#"\p{ASSIGNED}"#);
    test_parse_fails(r#"\P{Ascii}"#);
    test_parse_fails(r#"\p{Ascii}"#);
    test_parse_fails(r#"\P{General_Category = Uppercase_Letter}"#);
    test_parse_fails(r#"\p{General_Category = Uppercase_Letter}"#);
    test_parse_fails(r#"\P{_-_lOwEr_C-A_S-E_-_}"#);
    test_parse_fails(r#"\p{_-_lOwEr_C-A_S-E_-_}"#);
    test_parse_fails(r#"\P{any}"#);
    test_parse_fails(r#"\p{any}"#);
    test_parse_fails(r#"\P{ascii}"#);
    test_parse_fails(r#"\p{ascii}"#);
    test_parse_fails(r#"\P{assigned}"#);
    test_parse_fails(r#"\p{assigned}"#);
    test_parse_fails(r#"\P{gC=uppercase_letter}"#);
    test_parse_fails(r#"\p{gC=uppercase_letter}"#);
    test_parse_fails(r#"\P{gc=uppercaseletter}"#);
    test_parse_fails(r#"\p{gc=uppercaseletter}"#);
    test_parse_fails(r#"\P{lowercase}"#);
    test_parse_fails(r#"\p{lowercase}"#);
    test_parse_fails(r#"\P{lowercase}"#);
    test_parse_fails(r#"\p{lowercase}"#);
    test_parse_fails(r#"\P{General_Category=}"#);
    test_parse_fails(r#"\p{General_Category=}"#);
    test_parse_fails(r#"\P{General_Category}"#);
    test_parse_fails(r#"\p{General_Category}"#);
    test_parse_fails(r#"\P{Script_Extensions=}"#);
    test_parse_fails(r#"\p{Script_Extensions=}"#);
    test_parse_fails(r#"\P{Script_Extensions}"#);
    test_parse_fails(r#"\p{Script_Extensions}"#);
    test_parse_fails(r#"\P{Script=}"#);
    test_parse_fails(r#"\p{Script=}"#);
    test_parse_fails(r#"\P{Script}"#);
    test_parse_fails(r#"\p{Script}"#);
    test_parse_fails(r#"\P{UnknownBinaryProperty}"#);
    test_parse_fails(r#"\p{UnknownBinaryProperty}"#);
    test_parse_fails(r#"\P{Line_Breakz=WAT}"#);
    test_parse_fails(r#"\p{Line_Breakz=WAT}"#);
    test_parse_fails(r#"\P{Line_Breakz=Alphabetic}"#);
    test_parse_fails(r#"\p{Line_Breakz=Alphabetic}"#);
    test_parse_fails(r#"\\P{General_Category=WAT}"#);
    test_parse_fails(r#"\\p{General_Category=WAT}"#);
    test_parse_fails(r#"\\P{Script_Extensions=H_e_h}"#);
    test_parse_fails(r#"\\p{Script_Extensions=H_e_h}"#);
    test_parse_fails(r#"\\P{Script=FooBarBazInvalid}"#);
    test_parse_fails(r#"\\p{Script=FooBarBazInvalid}"#);
    test_parse_fails(r#"\P{Composition_Exclusion}"#);
    test_parse_fails(r#"\p{Composition_Exclusion}"#);
    test_parse_fails(r#"\P{Expands_On_NFC}"#);
    test_parse_fails(r#"\p{Expands_On_NFC}"#);
    test_parse_fails(r#"\P{Expands_On_NFD}"#);
    test_parse_fails(r#"\p{Expands_On_NFD}"#);
    test_parse_fails(r#"\P{Expands_On_NFKC}"#);
    test_parse_fails(r#"\p{Expands_On_NFKC}"#);
    test_parse_fails(r#"\P{Expands_On_NFKD}"#);
    test_parse_fails(r#"\p{Expands_On_NFKD}"#);
    test_parse_fails(r#"\P{FC_NFKC_Closure}"#);
    test_parse_fails(r#"\p{FC_NFKC_Closure}"#);
    test_parse_fails(r#"\P{Full_Composition_Exclusion}"#);
    test_parse_fails(r#"\p{Full_Composition_Exclusion}"#);
    test_parse_fails(r#"\P{Grapheme_Link}"#);
    test_parse_fails(r#"\p{Grapheme_Link}"#);
    test_parse_fails(r#"\P{Hyphen}"#);
    test_parse_fails(r#"\p{Hyphen}"#);
    test_parse_fails(r#"\P{Other_Alphabetic}"#);
    test_parse_fails(r#"\p{Other_Alphabetic}"#);
    test_parse_fails(r#"\P{Other_Default_Ignorable_Code_Point}"#);
    test_parse_fails(r#"\p{Other_Default_Ignorable_Code_Point}"#);
    test_parse_fails(r#"\P{Other_Grapheme_Extend}"#);
    test_parse_fails(r#"\p{Other_Grapheme_Extend}"#);
    test_parse_fails(r#"\P{Other_ID_Continue}"#);
    test_parse_fails(r#"\p{Other_ID_Continue}"#);
    test_parse_fails(r#"\P{Other_ID_Start}"#);
    test_parse_fails(r#"\p{Other_ID_Start}"#);
    test_parse_fails(r#"\P{Other_Lowercase}"#);
    test_parse_fails(r#"\p{Other_Lowercase}"#);
    test_parse_fails(r#"\P{Other_Math}"#);
    test_parse_fails(r#"\p{Other_Math}"#);
    test_parse_fails(r#"\P{Other_Uppercase}"#);
    test_parse_fails(r#"\p{Other_Uppercase}"#);
    test_parse_fails(r#"\P{Prepended_Concatenation_Mark}"#);
    test_parse_fails(r#"\p{Prepended_Concatenation_Mark}"#);
    test_parse_fails(r#"\P{Block=Adlam}"#);
    test_parse_fails(r#"\p{Block=Adlam}"#);
    test_parse_fails(r#"\P{FC_NFKC_Closure}"#);
    test_parse_fails(r#"\p{FC_NFKC_Closure}"#);
    test_parse_fails(r#"\P{Line_Break=Alphabetic}"#);
    test_parse_fails(r#"\P{Line_Break=Alphabetic}"#);
    test_parse_fails(r#"\p{Line_Break=Alphabetic}"#);
    test_parse_fails(r#"\p{Line_Break}"#);
}

fn build_test_string(lone_code_points: Vec<u32>, ranges: Vec<(u32, u32)>) -> String {
    let mut result = String::new();

    for code_point in lone_code_points {
        result.push(char::from_u32(code_point).unwrap_or(char::REPLACEMENT_CHARACTER));
    }

    for (start, end) in ranges.iter() {
        for code_point in *start..=*end {
            result.push(char::from_u32(code_point).unwrap_or(char::REPLACEMENT_CHARACTER));
        }
    }

    result
}

fn run_property_escape_test(tc: TestConfig, regexes: Vec<&str>, s: String) {
    for regex in regexes {
        tc.compile(regex).test_succeeds(&s);
    }
}

#[test]
fn unicode_escape_property_script_buhid() {
    test_with_configs(unicode_escape_property_script_buhid_tc)
}

fn unicode_escape_property_script_buhid_tc(tc: TestConfig) {
    let lone_code_points = vec![];
    let ranges = vec![(0x001740, 0x001753)];
    let regexes = vec![
        r#"^\p{Script=Buhid}+$"#,
        r#"^\p{Script=Buhd}+$"#,
        r#"^\p{sc=Buhid}+$"#,
        r#"^\p{sc=Buhd}+$"#,
    ];

    run_property_escape_test(tc, regexes, build_test_string(lone_code_points, ranges));

    let lone_code_points = vec![];
    let ranges = vec![
        (0x00DC00, 0x00DFFF),
        (0x000000, 0x00173F),
        (0x001754, 0x00DBFF),
        (0x00E000, 0x10FFFF),
    ];
    let regexes = vec![
        r#"^\P{Script=Buhid}+$"#,
        r#"^\P{Script=Buhd}+$"#,
        r#"^\P{sc=Buhid}+$"#,
        r#"^\P{sc=Buhd}+$"#,
    ];

    run_property_escape_test(tc, regexes, build_test_string(lone_code_points, ranges));
}
