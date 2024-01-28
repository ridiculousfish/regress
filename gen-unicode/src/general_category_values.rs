use crate::{codepoints_to_range, codepoints_to_ranges, pack_adjacent_codepoints, GenUnicode};
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

        let mut is_property_fn = Function::new("is_property_value_general_category");
        is_property_fn
            .vis("pub(crate)")
            .arg("cp", "u32")
            .arg("value", "&UnicodePropertyValueGeneralCategory")
            .ret("bool")
            .line("use UnicodePropertyValueGeneralCategory::*;");
        let mut is_property_fn_match_block = Block::new("match value");

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

            pack_adjacent_codepoints(&mut codepoints);

            // Some properties cannot be packed into a CodePointRange.
            if ["Unassigned", "Private_Use"].contains(orig_name) {
                self.scope.raw(&format!(
                    "pub(crate) const {}: [CodePointRangeUnpacked; {}] = [\n    {}\n];",
                    orig_name.to_uppercase(),
                    codepoints.len(),
                    codepoints
                        .iter()
                        .map(|cs| format!("CodePointRangeUnpacked::from({}, {}),", cs.0, cs.1))
                        .collect::<Vec<String>>()
                        .join("\n    ")
                ));
            } else {
                let ranges = codepoints_to_ranges(&codepoints);
                self.scope.raw(&format!(
                    "pub(crate) const {}: [CodePointRange; {}] = [\n    {}\n];",
                    orig_name.to_uppercase(),
                    ranges.len(),
                    ranges.join("\n    ")
                ));
            }

            self.scope
                .new_fn(&format!("is_{}", orig_name.to_lowercase()))
                .vis("pub(crate)")
                .arg("cp", "u32")
                .ret("bool")
                .line(&format!(
                    "{}.binary_search_by(|&cpr| cpr.compare(cp)).is_ok()",
                    orig_name.to_uppercase()
                ))
                .doc(&format!(
                    "Return whether cp has the '{}' Unicode property.",
                    orig_name
                ));

            property_enum.new_variant(*name);

            is_property_fn_match_block.line(format!(
                "{} => is_{}(cp),",
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

        for (alias0, alias1, orig_name, name, value_names_str) in GENERAL_CATEGORY_VALUES_DERIVED {
            let value_name_ifs: Vec<String> = value_names_str
                .split(',')
                .map(|name| format!("is_{}(cp)", name.to_lowercase()))
                .collect();

            self.scope
                .new_fn(&format!("is_{}", orig_name.to_lowercase()))
                .vis("pub(crate)")
                .arg("cp", "u32")
                .ret("bool")
                .line(value_name_ifs.join(" || "))
                .doc(&format!(
                    "Return whether cp has the '{}' Unicode property.",
                    orig_name
                ));

            property_enum.new_variant(*name);

            is_property_fn_match_block.line(format!(
                "{} => is_{}(cp),",
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

        is_property_fn.push_block(is_property_fn_match_block);

        property_from_str_fn_match_block.line("_ => None,");
        property_from_str_fn.push_block(property_from_str_fn_match_block);

        self.scope
            .push_fn(is_property_fn)
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

        for (alias0, alias1, orig_name, name, value_names_str) in GENERAL_CATEGORY_VALUES_DERIVED {
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
const GENERAL_CATEGORY_VALUES_DERIVED: &[(&str, &str,&str, &str, &str); 8] = &[
    ("", "LC", "Cased_Letter", "CasedLetter", "Lowercase_Letter,Titlecase_Letter,Uppercase_Letter"),
    ("", "C", "Other", "Other", "Control,Format,Surrogate,Unassigned,Private_Use"),
    ("", "L", "Letter", "Letter", "Lowercase_Letter,Modifier_Letter,Other_Letter,Titlecase_Letter,Uppercase_Letter"),
    ("Combining_Mark", "M", "Mark", "Mark", "Spacing_Mark,Enclosing_Mark,Nonspacing_Mark"),
    ("", "N", "Number", "Number","Decimal_Number,Letter_Number,Other_Number"),
    ("punct", "P", "Punctuation", "Punctuation", "Connector_Punctuation,Dash_Punctuation,Close_Punctuation,Final_Punctuation,Initial_Punctuation,Other_Punctuation,Open_Punctuation"),
    ("", "S", "Symbol", "Symbol", "Currency_Symbol,Modifier_Symbol,Math_Symbol,Other_Symbol"),
    ("", "Z", "Separator", "Separator", "Line_Separator,Paragraph_Separator,Space_Separator"),
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
