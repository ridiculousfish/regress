use crate::{chars_to_code_point_ranges, parse_line};
use std::fs::File;
use std::io::{self, BufRead};

use codegen::{Block, Enum, Function, Scope};

pub(crate) fn generate(scope: &mut Scope) {
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

    for (alias, orig_name, name, ucd_file_name) in BINARY_PROPERTIES {
        let file = File::open(ucd_file_name).unwrap();
        let lines = io::BufReader::new(file).lines();
        let mut chars = Vec::new();

        for line in lines {
            parse_line(&line.unwrap(), &mut chars, orig_name);
        }

        // Some properties cannot be packed into a CodePointRange.
        if ["Noncharacter_Code_Point"].contains(orig_name) {
            scope.raw(&format!(
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
            scope.raw(&format!(
                "pub(crate) const {}: [CodePointRange; {}] = [\n    {}\n];",
                orig_name.to_uppercase(),
                ranges.len(),
                ranges.join("\n    ")
            ));
        }

        scope
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

        property_enum.new_variant(name);

        is_property_fn_match_block.line(format!("{} => is_{}(c),", name, orig_name.to_lowercase()));

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

    scope.raw(&format!(
        "pub(crate) const ASCII: [CodePointRange; 1] = [\n    {}\n];",
        ascii_ranges.join("\n    ")
    ));

    scope
        .new_fn("is_ascii")
        .vis("pub(crate)")
        .arg("c", "char")
        .ret("bool")
        .line("ASCII.binary_search_by(|&cpr| cpr.compare(c as u32)).is_ok()")
        .doc("Return whether c has the 'ASCII' Unicode property.");

    scope.raw("pub(crate) const ANY: [CodePointRangeUnpacked; 1] = [\n    CodePointRangeUnpacked::from(0, 1114111)\n];");

    scope
        .new_fn("is_any")
        .vis("pub(crate)")
        .arg("c", "char")
        .ret("bool")
        .line("ANY.binary_search_by(|&cpr| cpr.compare(c as u32)).is_ok()")
        .doc("Return whether c has the 'Any' Unicode property.");

    scope
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

    scope
        .push_fn(is_property_fn)
        .push_enum(property_enum)
        .push_fn(property_from_str_fn);
}

// Structure: (Alias, Name, CamelCaseName, UCDFileName)
const BINARY_PROPERTIES: &[(&str, &str, &str, &str); 50] = &[
    (
        "Alpha",
        "Alphabetic",
        "Alphabetic",
        "DerivedCoreProperties.txt",
    ),
    (
        "CI",
        "Case_Ignorable",
        "CaseIgnorable",
        "DerivedCoreProperties.txt",
    ),
    ("", "Cased", "Cased", "DerivedCoreProperties.txt"),
    (
        "CWCF",
        "Changes_When_Casefolded",
        "ChangesWhenCasefolded",
        "DerivedCoreProperties.txt",
    ),
    (
        "CWCM",
        "Changes_When_Casemapped",
        "ChangesWhenCasemapped",
        "DerivedCoreProperties.txt",
    ),
    (
        "CWL",
        "Changes_When_Lowercased",
        "ChangesWhenLowercased",
        "DerivedCoreProperties.txt",
    ),
    (
        "CWT",
        "Changes_When_Titlecased",
        "ChangesWhenTitlecased",
        "DerivedCoreProperties.txt",
    ),
    (
        "CWU",
        "Changes_When_Uppercased",
        "ChangesWhenUppercased",
        "DerivedCoreProperties.txt",
    ),
    (
        "DI",
        "Default_Ignorable_Code_Point",
        "DefaultIgnorableCodePoint",
        "DerivedCoreProperties.txt",
    ),
    (
        "Gr_Base",
        "Grapheme_Base",
        "GraphemeBase",
        "DerivedCoreProperties.txt",
    ),
    (
        "Gr_Ext",
        "Grapheme_Extend",
        "GraphemeExtend",
        "DerivedCoreProperties.txt",
    ),
    (
        "IDC",
        "ID_Continue",
        "IDContinue",
        "DerivedCoreProperties.txt",
    ),
    ("IDS", "ID_Start", "IDStart", "DerivedCoreProperties.txt"),
    ("", "Math", "Math", "DerivedCoreProperties.txt"),
    (
        "XIDC",
        "XID_Continue",
        "XIDContinue",
        "DerivedCoreProperties.txt",
    ),
    ("XIDS", "XID_Start", "XIDStart", "DerivedCoreProperties.txt"),
    ("AHex", "ASCII_Hex_Digit", "ASCIIHexDigit", "PropList.txt"),
    ("Bidi_C", "Bidi_Control", "BidiControl", "PropList.txt"),
    ("", "Dash", "Dash", "PropList.txt"),
    ("Dep", "Deprecated", "Deprecated", "PropList.txt"),
    ("Dia", "Diacritic", "Diacritic", "PropList.txt"),
    ("Ext", "Extender", "Extender", "PropList.txt"),
    ("Hex", "Hex_Digit", "HexDigit", "PropList.txt"),
    (
        "IDSB",
        "IDS_Binary_Operator",
        "IDSBinaryOperator",
        "PropList.txt",
    ),
    (
        "IDST",
        "IDS_Trinary_Operator",
        "IDSTrinaryOperator",
        "PropList.txt",
    ),
    ("Ideo", "Ideographic", "Ideographic", "PropList.txt"),
    ("Join_C", "Join_Control", "JoinControl", "PropList.txt"),
    (
        "LOE",
        "Logical_Order_Exception",
        "LogicalOrderException",
        "PropList.txt",
    ),
    ("Lower", "Lowercase", "Lowercase", "PropList.txt"),
    (
        "NChar",
        "Noncharacter_Code_Point",
        "NoncharacterCodePoint",
        "PropList.txt",
    ),
    ("Pat_Syn", "Pattern_Syntax", "PatternSyntax", "PropList.txt"),
    (
        "Pat_WS",
        "Pattern_White_Space",
        "PatternWhiteSpace",
        "PropList.txt",
    ),
    ("QMark", "Quotation_Mark", "QuotationMark", "PropList.txt"),
    ("", "Radical", "Radical", "PropList.txt"),
    (
        "RI",
        "Regional_Indicator",
        "RegionalIndicator",
        "PropList.txt",
    ),
    (
        "STerm",
        "Sentence_Terminal",
        "SentenceTerminal",
        "PropList.txt",
    ),
    ("SD", "Soft_Dotted", "SoftDotted", "PropList.txt"),
    (
        "Term",
        "Terminal_Punctuation",
        "TerminalPunctuation",
        "PropList.txt",
    ),
    (
        "UIdeo",
        "Unified_Ideograph",
        "UnifiedIdeograph",
        "PropList.txt",
    ),
    ("Upper", "Uppercase", "Uppercase", "PropList.txt"),
    (
        "VS",
        "Variation_Selector",
        "VariationSelector",
        "PropList.txt",
    ),
    ("space", "White_Space", "WhiteSpace", "PropList.txt"),
    ("", "Emoji", "Emoji", "emoji-data.txt"),
    (
        "EComp",
        "Emoji_Component",
        "EmojiComponent",
        "emoji-data.txt",
    ),
    ("EMod", "Emoji_Modifier", "EmojiModifier", "emoji-data.txt"),
    (
        "EBase",
        "Emoji_Modifier_Base",
        "EmojiModifierBase",
        "emoji-data.txt",
    ),
    (
        "EPres",
        "Emoji_Presentation",
        "EmojiPresentation",
        "emoji-data.txt",
    ),
    (
        "ExtPict",
        "Extended_Pictographic",
        "ExtendedPictographic",
        "emoji-data.txt",
    ),
    (
        "CWKCF",
        "Changes_When_NFKC_Casefolded",
        "ChangesWhenNFKCCasefolded",
        "DerivedNormalizationProps.txt",
    ),
    (
        "Bidi_M",
        "Bidi_Mirrored",
        "BidiMirrored",
        "DerivedBinaryProperties.txt",
    ),
];
