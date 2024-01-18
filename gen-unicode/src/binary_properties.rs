use crate::{chars_to_code_point_ranges, codepoints_to_range, pack_adjacent_chars, GenUnicode};
use codegen::{Block, Enum, Function};

impl GenUnicode {
    pub(crate) fn generate_binary_properties(&mut self) {
        let mut property_enum = Enum::new("UnicodePropertyBinary");
        property_enum
            .vis("pub")
            .derive("Debug")
            .derive("Clone")
            .derive("Copy");

        let mut is_property_fn = Function::new("is_property_binary");
        is_property_fn
            .vis("pub(crate)")
            .arg("c", "char")
            .arg("value", "&UnicodePropertyBinary")
            .ret("bool")
            .line("use UnicodePropertyBinary::*;");
        let mut is_property_fn_match_block = Block::new("match value");

        let mut property_from_str_fn = Function::new("unicode_property_binary_from_str");
        property_from_str_fn
            .arg("s", "&str")
            .ret("Option<UnicodePropertyBinary>")
            .vis("pub")
            .line("use UnicodePropertyBinary::*;");
        let mut property_from_str_fn_match_block = Block::new("match s");

        for (alias, orig_name, name, ucd_file) in BINARY_PROPERTIES {
            let mut chars = ucd_file.chars(orig_name, self);

            pack_adjacent_chars(&mut chars);

            // Some properties cannot be packed into a CodePointRange.
            if ["Noncharacter_Code_Point"].contains(orig_name) {
                self.scope.raw(&format!(
                    "pub(crate) const {}: [CodePointRangeUnpacked; {}] = [\n    {}\n];",
                    orig_name.to_uppercase(),
                    chars.len(),
                    chars
                        .iter()
                        .map(|cs| format!("CodePointRangeUnpacked::from({}, {}),", cs.0, cs.1))
                        .collect::<Vec<String>>()
                        .join("\n    ")
                ));
            } else {
                let ranges = chars_to_code_point_ranges(&chars);
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
                .arg("c", "char")
                .ret("bool")
                .line(&format!(
                    "{}.binary_search_by(|&cpr| cpr.compare(c as u32)).is_ok()",
                    orig_name.to_uppercase()
                ))
                .doc(&format!(
                    "Return whether c has the '{}' Unicode property.",
                    orig_name
                ));

            property_enum.new_variant(*name);

            is_property_fn_match_block.line(format!(
                "{} => is_{}(c),",
                name,
                orig_name.to_lowercase()
            ));

            property_from_str_fn_match_block.line(if alias.is_empty() {
                format!("\"{}\" => Some({}),", orig_name, name)
            } else {
                format!("\"{}\" | \"{}\" => Some({}),", alias, orig_name, name)
            });
        }

        // These are special ranges that are not in the UCD files
        property_enum.new_variant("Ascii");
        property_enum.new_variant("Any");
        property_enum.new_variant("Assigned");

        let ascii_ranges = chars_to_code_point_ranges(&[(0, 127)]);

        self.scope.raw(&format!(
            "pub(crate) const ASCII: [CodePointRange; 1] = [\n    {}\n];",
            ascii_ranges.join("\n    ")
        ));

        self.scope
            .new_fn("is_ascii")
            .vis("pub(crate)")
            .arg("c", "char")
            .ret("bool")
            .line("ASCII.binary_search_by(|&cpr| cpr.compare(c as u32)).is_ok()")
            .doc("Return whether c has the 'ASCII' Unicode property.");

        self.scope.raw("pub(crate) const ANY: [CodePointRangeUnpacked; 1] = [\n    CodePointRangeUnpacked::from(0, 1114111)\n];");

        self.scope
            .new_fn("is_any")
            .vis("pub(crate)")
            .arg("c", "char")
            .ret("bool")
            .line("ANY.binary_search_by(|&cpr| cpr.compare(c as u32)).is_ok()")
            .doc("Return whether c has the 'Any' Unicode property.");

        self.scope
            .new_fn("is_assigned")
            .vis("pub(crate)")
            .arg("c", "char")
            .ret("bool")
            .line("UNASSIGNED.binary_search_by(|&cpr| cpr.compare(c as u32)).is_err()")
            .doc("Return whether c has the 'Any' Unicode property.");

        is_property_fn_match_block.line("Ascii => is_ascii(c),");
        is_property_fn_match_block.line("Any => is_any(c),");
        is_property_fn_match_block.line("Assigned => is_assigned(c),");

        property_from_str_fn_match_block.line("\"ASCII\" => Some(Ascii),");
        property_from_str_fn_match_block.line("\"Any\" => Some(Any),");
        property_from_str_fn_match_block.line("\"Assigned\" => Some(Assigned),");

        is_property_fn.push_block(is_property_fn_match_block);

        property_from_str_fn_match_block.line("_ => None,");
        property_from_str_fn.push_block(property_from_str_fn_match_block);

        self.scope
            .push_fn(is_property_fn)
            .push_enum(property_enum)
            .push_fn(property_from_str_fn);
    }

    pub(crate) fn generate_binary_properties_tests(&mut self) {
        for (alias, orig_name, name, ucd_file) in BINARY_PROPERTIES {
            let chars = ucd_file.chars(orig_name, self);

            self.scope_tests
                .new_fn(&format!(
                    "unicode_escape_property_binary_{}",
                    name.to_lowercase()
                ))
                .attr("test")
                .line(format!(
                    "test_with_configs(unicode_escape_property_binary_{}_tc)",
                    name.to_lowercase()
                ));

            let f = self.scope_tests.new_fn(&format!(
                "unicode_escape_property_binary_{}_tc",
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

            let mut regexes = vec![format!(r#""^\\p{{{}}}+$""#, orig_name)];

            if !alias.is_empty() {
                regexes.push(format!(r#""^\\p{{{}}}+$""#, alias));
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

enum UCDFile {
    CoreProperty,
    Property,
    EmojiProperty,
    DerivedBinaryProperties,
    DerivedNormalizationProperty,
}

impl UCDFile {
    fn chars(&self, property: &str, gen_unicode: &GenUnicode) -> Vec<(u32, u32)> {
        let mut chars = Vec::new();

        match self {
            Self::CoreProperty => {
                for row in &gen_unicode.core_property {
                    if row.property == *property {
                        chars.push(codepoints_to_range(&row.codepoints));
                    }
                }
            }
            Self::Property => {
                for row in &gen_unicode.properties {
                    if row.property == *property {
                        chars.push(codepoints_to_range(&row.codepoints));
                    }
                }
            }
            Self::EmojiProperty => {
                for row in &gen_unicode.emoji_properties {
                    if row.property == *property {
                        chars.push(codepoints_to_range(&row.codepoints));
                    }
                }
            }
            Self::DerivedBinaryProperties => {
                for row in &gen_unicode.derived_binary_properties {
                    if row.property == *property {
                        chars.push(codepoints_to_range(&row.codepoints));
                    }
                }
            }
            Self::DerivedNormalizationProperty => {
                for row in &gen_unicode.derived_normalization_properties {
                    if row.property == *property {
                        chars.push(codepoints_to_range(&row.codepoints));
                    }
                }
            }
        }

        chars
    }
}

// Structure: (Alias, Name, CamelCaseName, UCDFileName)
const BINARY_PROPERTIES: &[(&str, &str, &str, UCDFile); 50] = &[
    ("Alpha", "Alphabetic", "Alphabetic", UCDFile::CoreProperty),
    (
        "CI",
        "Case_Ignorable",
        "CaseIgnorable",
        UCDFile::CoreProperty,
    ),
    ("", "Cased", "Cased", UCDFile::CoreProperty),
    (
        "CWCF",
        "Changes_When_Casefolded",
        "ChangesWhenCasefolded",
        UCDFile::CoreProperty,
    ),
    (
        "CWCM",
        "Changes_When_Casemapped",
        "ChangesWhenCasemapped",
        UCDFile::CoreProperty,
    ),
    (
        "CWL",
        "Changes_When_Lowercased",
        "ChangesWhenLowercased",
        UCDFile::CoreProperty,
    ),
    (
        "CWT",
        "Changes_When_Titlecased",
        "ChangesWhenTitlecased",
        UCDFile::CoreProperty,
    ),
    (
        "CWU",
        "Changes_When_Uppercased",
        "ChangesWhenUppercased",
        UCDFile::CoreProperty,
    ),
    (
        "DI",
        "Default_Ignorable_Code_Point",
        "DefaultIgnorableCodePoint",
        UCDFile::CoreProperty,
    ),
    (
        "Gr_Base",
        "Grapheme_Base",
        "GraphemeBase",
        UCDFile::CoreProperty,
    ),
    (
        "Gr_Ext",
        "Grapheme_Extend",
        "GraphemeExtend",
        UCDFile::CoreProperty,
    ),
    ("IDC", "ID_Continue", "IDContinue", UCDFile::CoreProperty),
    ("IDS", "ID_Start", "IDStart", UCDFile::CoreProperty),
    ("", "Math", "Math", UCDFile::CoreProperty),
    ("XIDC", "XID_Continue", "XIDContinue", UCDFile::CoreProperty),
    ("XIDS", "XID_Start", "XIDStart", UCDFile::CoreProperty),
    (
        "AHex",
        "ASCII_Hex_Digit",
        "ASCIIHexDigit",
        UCDFile::Property,
    ),
    ("Bidi_C", "Bidi_Control", "BidiControl", UCDFile::Property),
    ("", "Dash", "Dash", UCDFile::Property),
    ("Dep", "Deprecated", "Deprecated", UCDFile::Property),
    ("Dia", "Diacritic", "Diacritic", UCDFile::Property),
    ("Ext", "Extender", "Extender", UCDFile::Property),
    ("Hex", "Hex_Digit", "HexDigit", UCDFile::Property),
    (
        "IDSB",
        "IDS_Binary_Operator",
        "IDSBinaryOperator",
        UCDFile::Property,
    ),
    (
        "IDST",
        "IDS_Trinary_Operator",
        "IDSTrinaryOperator",
        UCDFile::Property,
    ),
    ("Ideo", "Ideographic", "Ideographic", UCDFile::Property),
    ("Join_C", "Join_Control", "JoinControl", UCDFile::Property),
    (
        "LOE",
        "Logical_Order_Exception",
        "LogicalOrderException",
        UCDFile::Property,
    ),
    ("Lower", "Lowercase", "Lowercase", UCDFile::CoreProperty),
    (
        "NChar",
        "Noncharacter_Code_Point",
        "NoncharacterCodePoint",
        UCDFile::Property,
    ),
    (
        "Pat_Syn",
        "Pattern_Syntax",
        "PatternSyntax",
        UCDFile::Property,
    ),
    (
        "Pat_WS",
        "Pattern_White_Space",
        "PatternWhiteSpace",
        UCDFile::Property,
    ),
    (
        "QMark",
        "Quotation_Mark",
        "QuotationMark",
        UCDFile::Property,
    ),
    ("", "Radical", "Radical", UCDFile::Property),
    (
        "RI",
        "Regional_Indicator",
        "RegionalIndicator",
        UCDFile::Property,
    ),
    (
        "STerm",
        "Sentence_Terminal",
        "SentenceTerminal",
        UCDFile::Property,
    ),
    ("SD", "Soft_Dotted", "SoftDotted", UCDFile::Property),
    (
        "Term",
        "Terminal_Punctuation",
        "TerminalPunctuation",
        UCDFile::Property,
    ),
    (
        "UIdeo",
        "Unified_Ideograph",
        "UnifiedIdeograph",
        UCDFile::Property,
    ),
    ("Upper", "Uppercase", "Uppercase", UCDFile::CoreProperty),
    (
        "VS",
        "Variation_Selector",
        "VariationSelector",
        UCDFile::Property,
    ),
    ("space", "White_Space", "WhiteSpace", UCDFile::Property),
    ("", "Emoji", "Emoji", UCDFile::EmojiProperty),
    (
        "EComp",
        "Emoji_Component",
        "EmojiComponent",
        UCDFile::EmojiProperty,
    ),
    (
        "EMod",
        "Emoji_Modifier",
        "EmojiModifier",
        UCDFile::EmojiProperty,
    ),
    (
        "EBase",
        "Emoji_Modifier_Base",
        "EmojiModifierBase",
        UCDFile::EmojiProperty,
    ),
    (
        "EPres",
        "Emoji_Presentation",
        "EmojiPresentation",
        UCDFile::EmojiProperty,
    ),
    (
        "ExtPict",
        "Extended_Pictographic",
        "ExtendedPictographic",
        UCDFile::EmojiProperty,
    ),
    (
        "CWKCF",
        "Changes_When_NFKC_Casefolded",
        "ChangesWhenNFKCCasefolded",
        UCDFile::DerivedNormalizationProperty,
    ),
    (
        "Bidi_M",
        "Bidi_Mirrored",
        "BidiMirrored",
        UCDFile::DerivedBinaryProperties,
    ),
];
