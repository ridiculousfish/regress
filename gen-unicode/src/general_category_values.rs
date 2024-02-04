use crate::{codepoints_to_range, format_interval_table, pack_adjacent_codepoints, GenUnicode};
use codegen::{Block, Enum, Function};
use std::collections::HashMap;

impl GenUnicode {
    pub(crate) fn generate_general_category(&mut self) {
        let mut property_enum = Enum::new("UnicodePropertyValueGeneralCategory");
        property_enum
            .vis("pub")
            .derive("Debug")
            .derive("Clone")
            .derive("Copy");

        let mut as_ranges_fn = Function::new("general_category_property_value_ranges");
        as_ranges_fn
            .vis("pub(crate)")
            .arg("value", "&UnicodePropertyValueGeneralCategory")
            .ret("&'static [Interval]")
            .line("use UnicodePropertyValueGeneralCategory::*;");
        let mut as_ranges_fn_match_block = Block::new("match value");

        let mut property_from_str_fn =
            Function::new("unicode_property_value_general_category_from_str");
        property_from_str_fn
            .arg("s", "&str")
            .ret("Option<UnicodePropertyValueGeneralCategory>")
            .vis("pub")
            .line("use UnicodePropertyValueGeneralCategory::*;");
        let mut property_from_str_fn_match_block = Block::new("match s");

        for (alias0, alias1, orig_name, name) in GENERAL_CATEGORY_VALUES {
            let mut codepoints = Vec::new();

            for row in &self.derived_general_category {
                if row.general_category == *alias1 {
                    codepoints.push(codepoints_to_range(&row.codepoints));
                }
            }

            codepoints.sort();
            pack_adjacent_codepoints(&mut codepoints);

            self.scope.raw(format_interval_table(
                &orig_name.to_uppercase(),
                &codepoints,
            ));

            self.scope
                .new_fn(&format!("{}_ranges", orig_name.to_lowercase()))
                .vis("pub(crate)")
                .ret("&'static [Interval]")
                .line(&format!("&{}", orig_name.to_uppercase()))
                .doc(&format!(
                    "Return the code point ranges of the '{}' Unicode property.",
                    orig_name
                ));

            property_enum.new_variant(*name);

            as_ranges_fn_match_block.line(format!(
                "{} => {}_ranges(),",
                name,
                orig_name.to_lowercase()
            ));

            property_from_str_fn_match_block.line(if alias0.is_empty() {
                format!("\"{}\" | \"{}\" => Some({}),", alias1, orig_name, name)
            } else {
                format!(
                    "\"{}\" | \"{}\" | \"{}\" => Some({}),",
                    alias0, alias1, orig_name, name
                )
            });
        }

        for (alias0, alias1, orig_name, name, _, alias1_names) in GENERAL_CATEGORY_VALUES_DERIVED {
            let alias1_strings: Vec<&str> = alias1_names.split(',').collect();

            let mut codepoints = Vec::new();

            for row in &self.derived_general_category {
                if alias1_strings.contains(&row.general_category.as_str()) {
                    codepoints.push(codepoints_to_range(&row.codepoints));
                }
            }

            codepoints.sort();
            pack_adjacent_codepoints(&mut codepoints);

            self.scope.raw(format_interval_table(
                &orig_name.to_uppercase(),
                &codepoints,
            ));

            self.scope
                .new_fn(&format!("{}_ranges", orig_name.to_lowercase()))
                .vis("pub(crate)")
                .ret("&'static [Interval]")
                .line(&format!("&{}", orig_name.to_uppercase()))
                .doc(&format!(
                    "Return the code point ranges of the '{}' Unicode property.",
                    orig_name
                ));

            property_enum.new_variant(*name);

            as_ranges_fn_match_block.line(format!(
                "{} => {}_ranges(),",
                name,
                orig_name.to_lowercase()
            ));

            property_from_str_fn_match_block.line(if alias0.is_empty() {
                format!("\"{}\" | \"{}\" => Some({}),", alias1, orig_name, name)
            } else {
                format!(
                    "\"{}\" | \"{}\" | \"{}\" => Some({}),",
                    alias0, alias1, orig_name, name
                )
            });
        }

        as_ranges_fn.push_block(as_ranges_fn_match_block);

        property_from_str_fn_match_block.line("_ => None,");
        property_from_str_fn.push_block(property_from_str_fn_match_block);

        self.scope
            .push_fn(as_ranges_fn)
            .push_enum(property_enum)
            .push_fn(property_from_str_fn);
    }

    pub(crate) fn generate_general_category_tests(&mut self) {
        let mut char_map: HashMap<&str, Vec<(u32, u32)>> = HashMap::new();

        for (alias0, alias1, orig_name, name) in GENERAL_CATEGORY_VALUES {
            // We skip surrogates, as rust does not allow them as chars.
            if *name == "Surrogate" {
                continue;
            }

            let mut chars = Vec::new();

            for row in &self.derived_general_category {
                if row.general_category == *alias1 {
                    chars.push(codepoints_to_range(&row.codepoints));
                }
            }

            char_map.insert(orig_name, chars.clone());

            self.scope_tests
                .new_fn(&format!(
                    "unicode_escape_property_gc_{}",
                    name.to_lowercase()
                ))
                .attr("test")
                .line(format!(
                    "test_with_configs(unicode_escape_property_gc_{}_tc)",
                    name.to_lowercase()
                ));

            let f = self.scope_tests.new_fn(&format!(
                "unicode_escape_property_gc_{}_tc",
                name.to_lowercase()
            ));

            f.arg("tc", "TestConfig");

            let code_points: Vec<String> = chars
                .iter()
                .map(|c| format!("\"\\u{{{:x}}}\"", c.0))
                .collect();

            f.line(format!(
                "const CODE_POINTS: [&str; {}] = [\n    {},\n];",
                code_points.len(),
                code_points.join(",\n    ")
            ));

            let mut regexes = vec![
                format!(r#""^\\p{{General_Category={}}}+$""#, orig_name),
                format!(r#""^\\p{{gc={}}}+$""#, orig_name),
                format!(r#""^\\p{{{}}}+$""#, orig_name),
            ];

            if !alias0.is_empty() {
                regexes.push(format!(r#""^\\p{{General_Category={}}}+$""#, alias0));
                regexes.push(format!(r#""^\\p{{gc={}}}+$""#, alias0));
                regexes.push(format!(r#""^\\p{{{}}}+$""#, alias0));
            }

            if !alias1.is_empty() {
                regexes.push(format!(r#""^\\p{{General_Category={}}}+$""#, alias1));
                regexes.push(format!(r#""^\\p{{gc={}}}+$""#, alias1));
                regexes.push(format!(r#""^\\p{{{}}}+$""#, alias1));
            }

            f.line(format!(
                "const REGEXES: [&str; {}] = [\n    {},\n];",
                regexes.len(),
                regexes.join(",\n    ")
            ));

            let mut b = Block::new("for regex in REGEXES");
            b.line(r#"let regex = tc.compilef(regex, "u");"#);

            let mut bb = Block::new("for code_point in CODE_POINTS");
            bb.line("regex.test_succeeds(code_point);");

            b.push_block(bb);

            f.push_block(b);
        }

        for (alias0, alias1, orig_name, name, value_names_str, _) in GENERAL_CATEGORY_VALUES_DERIVED
        {
            let mut chars = Vec::new();

            for value_name in value_names_str.split(',') {
                if let Some(cs) = char_map.get(value_name) {
                    chars.append(&mut cs.clone());
                }
            }

            self.scope_tests
                .new_fn(&format!(
                    "unicode_escape_property_gc_{}",
                    name.to_lowercase()
                ))
                .attr("test")
                .line(format!(
                    "test_with_configs(unicode_escape_property_gc_{}_tc)",
                    name.to_lowercase()
                ));

            let f = self.scope_tests.new_fn(&format!(
                "unicode_escape_property_gc_{}_tc",
                name.to_lowercase()
            ));

            f.arg("tc", "TestConfig");

            let code_points: Vec<String> = chars
                .iter()
                .map(|c| format!("\"\\u{{{:x}}}\"", c.0))
                .collect();

            f.line(format!(
                "const CODE_POINTS: [&str; {}] = [\n    {},\n];",
                code_points.len(),
                code_points.join(",\n    ")
            ));

            let mut regexes = vec![
                format!(r#""^\\p{{General_Category={}}}+$""#, orig_name),
                format!(r#""^\\p{{gc={}}}+$""#, orig_name),
                format!(r#""^\\p{{{}}}+$""#, orig_name),
            ];

            if !alias0.is_empty() {
                regexes.push(format!(r#""^\\p{{General_Category={}}}+$""#, alias0));
                regexes.push(format!(r#""^\\p{{gc={}}}+$""#, alias0));
                regexes.push(format!(r#""^\\p{{{}}}+$""#, alias0));
            }

            if !alias1.is_empty() {
                regexes.push(format!(r#""^\\p{{General_Category={}}}+$""#, alias1));
                regexes.push(format!(r#""^\\p{{gc={}}}+$""#, alias1));
                regexes.push(format!(r#""^\\p{{{}}}+$""#, alias1));
            }

            f.line(format!(
                "const REGEXES: [&str; {}] = [\n    {},\n];",
                regexes.len(),
                regexes.join(",\n    ")
            ));

            let mut b = Block::new("for regex in REGEXES");
            b.line(r#"let regex = tc.compilef(regex, "u");"#);

            let mut bb = Block::new("for code_point in CODE_POINTS");
            bb.line("regex.test_succeeds(code_point);");

            b.push_block(bb);

            f.push_block(b);
        }
    }
}

// Structure: (Alias, Alias, Name, CamelCaseName, CommaSeparatedValueNames)
const GENERAL_CATEGORY_VALUES_DERIVED: &[(&str, &str, &str, &str, &str, &str); 8] = &[
    ("", "LC", "Cased_Letter", "CasedLetter", "Lowercase_Letter,Titlecase_Letter,Uppercase_Letter", "Ll,Lt,Lu"),
    ("", "C", "Other", "Other", "Control,Format,Surrogate,Unassigned,Private_Use", "Cc,Cf,Cs,Cn,Co"),
    ("", "L", "Letter", "Letter", "Lowercase_Letter,Modifier_Letter,Other_Letter,Titlecase_Letter,Uppercase_Letter", "Ll,Lm,Lo,Lt,Lu"),
    ("Combining_Mark", "M", "Mark", "Mark", "Spacing_Mark,Enclosing_Mark,Nonspacing_Mark", "Mc,Me,Mn"),
    ("", "N", "Number", "Number","Decimal_Number,Letter_Number,Other_Number", "Nd,Nl,No"),
    ("punct", "P", "Punctuation", "Punctuation", "Connector_Punctuation,Dash_Punctuation,Close_Punctuation,Final_Punctuation,Initial_Punctuation,Other_Punctuation,Open_Punctuation", "Pc,Pd,Pe,Pf,Pi,Po,Ps"),
    ("", "S", "Symbol", "Symbol", "Currency_Symbol,Modifier_Symbol,Math_Symbol,Other_Symbol", "Sc,Sk,Sm,So"),
    ("", "Z", "Separator", "Separator", "Line_Separator,Paragraph_Separator,Space_Separator", "Zl,Zp,Zs"),
];

// Structure: (Alias, Alias, Name, CamelCaseName)
const GENERAL_CATEGORY_VALUES: &[(&str, &str, &str, &str); 30] = &[
    ("", "Pe", "Close_Punctuation", "ClosePunctuation"),
    ("", "Pc", "Connector_Punctuation", "ConnectorPunctuation"),
    ("cntrl", "Cc", "Control", "Control"),
    ("", "Sc", "Currency_Symbol", "CurrencySymbol"),
    ("", "Pd", "Dash_Punctuation", "DashPunctuation"),
    ("digit", "Nd", "Decimal_Number", "DecimalNumber"),
    ("", "Me", "Enclosing_Mark", "EnclosingMark"),
    ("", "Pf", "Final_Punctuation", "FinalPunctuation"),
    ("", "Cf", "Format", "Format"),
    ("", "Pi", "Initial_Punctuation", "InitialPunctuation"),
    ("", "Nl", "Letter_Number", "LetterNumber"),
    ("", "Zl", "Line_Separator", "LineSeparator"),
    ("", "Ll", "Lowercase_Letter", "LowercaseLetter"),
    ("", "Sm", "Math_Symbol", "MathSymbol"),
    ("", "Lm", "Modifier_Letter", "ModifierLetter"),
    ("", "Sk", "Modifier_Symbol", "ModifierSymbol"),
    ("", "Mn", "Nonspacing_Mark", "NonspacingMark"),
    ("", "Ps", "Open_Punctuation", "OpenPunctuation"),
    ("", "Lo", "Other_Letter", "OtherLetter"),
    ("", "No", "Other_Number", "OtherNumber"),
    ("", "Po", "Other_Punctuation", "OtherPunctuation"),
    ("", "So", "Other_Symbol", "OtherSymbol"),
    ("", "Zp", "Paragraph_Separator", "ParagraphSeparator"),
    ("", "Co", "Private_Use", "PrivateUse"),
    ("", "Zs", "Space_Separator", "SpaceSeparator"),
    ("", "Mc", "Spacing_Mark", "SpacingMark"),
    ("", "Cs", "Surrogate", "Surrogate"),
    ("", "Lt", "Titlecase_Letter", "TitlecaseLetter"),
    ("", "Cn", "Unassigned", "Unassigned"),
    ("", "Lu", "Uppercase_Letter", "UppercaseLetter"),
];
