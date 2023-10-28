use icu_properties::{sets, GeneralCategory, GeneralCategoryGroup, Script};

#[derive(Debug, Clone, Copy)]
pub(crate) enum UnicodePropertyBinary {
    Alphabetic,
    CaseIgnorable,
    Cased,
    ChangesWhenCasefolded,
    ChangesWhenCasemapped,
    ChangesWhenLowercased,
    ChangesWhenTitlecased,
    ChangesWhenUppercased,
    DefaultIgnorableCodePoint,
    GraphemeBase,
    GraphemeExtend,
    IDContinue,
    IDStart,
    Math,
    XIDContinue,
    XIDStart,
    ASCIIHexDigit,
    BidiControl,
    Dash,
    Deprecated,
    Diacritic,
    Extender,
    HexDigit,
    IDSBinaryOperator,
    IDSTrinaryOperator,
    Ideographic,
    JoinControl,
    LogicalOrderException,
    Lowercase,
    NoncharacterCodePoint,
    PatternSyntax,
    PatternWhiteSpace,
    QuotationMark,
    Radical,
    RegionalIndicator,
    SentenceTerminal,
    SoftDotted,
    TerminalPunctuation,
    UnifiedIdeograph,
    Uppercase,
    VariationSelector,
    WhiteSpace,
    Emoji,
    EmojiComponent,
    EmojiModifier,
    EmojiModifierBase,
    EmojiPresentation,
    ExtendedPictographic,
    ChangesWhenNFKCCasefolded,
    BidiMirrored,
    Ascii,
    Any,
    Assigned,
}

impl UnicodePropertyBinary {
    fn from_str(s: &str) -> Option<Self> {
        use UnicodePropertyBinary::*;
        match s {
            "Alpha" | "Alphabetic" => Some(Alphabetic),
            "CI" | "Case_Ignorable" => Some(CaseIgnorable),
            "Cased" => Some(Cased),
            "CWCF" | "Changes_When_Casefolded" => Some(ChangesWhenCasefolded),
            "CWCM" | "Changes_When_Casemapped" => Some(ChangesWhenCasemapped),
            "CWL" | "Changes_When_Lowercased" => Some(ChangesWhenLowercased),
            "CWT" | "Changes_When_Titlecased" => Some(ChangesWhenTitlecased),
            "CWU" | "Changes_When_Uppercased" => Some(ChangesWhenUppercased),
            "DI" | "Default_Ignorable_Code_Point" => Some(DefaultIgnorableCodePoint),
            "Gr_Base" | "Grapheme_Base" => Some(GraphemeBase),
            "Gr_Ext" | "Grapheme_Extend" => Some(GraphemeExtend),
            "IDC" | "ID_Continue" => Some(IDContinue),
            "IDS" | "ID_Start" => Some(IDStart),
            "Math" => Some(Math),
            "XIDC" | "XID_Continue" => Some(XIDContinue),
            "XIDS" | "XID_Start" => Some(XIDStart),
            "AHex" | "ASCII_Hex_Digit" => Some(ASCIIHexDigit),
            "Bidi_C" | "Bidi_Control" => Some(BidiControl),
            "Dash" => Some(Dash),
            "Dep" | "Deprecated" => Some(Deprecated),
            "Dia" | "Diacritic" => Some(Diacritic),
            "Ext" | "Extender" => Some(Extender),
            "Hex" | "Hex_Digit" => Some(HexDigit),
            "IDSB" | "IDS_Binary_Operator" => Some(IDSBinaryOperator),
            "IDST" | "IDS_Trinary_Operator" => Some(IDSTrinaryOperator),
            "Ideo" | "Ideographic" => Some(Ideographic),
            "Join_C" | "Join_Control" => Some(JoinControl),
            "LOE" | "Logical_Order_Exception" => Some(LogicalOrderException),
            "Lower" | "Lowercase" => Some(Lowercase),
            "NChar" | "Noncharacter_Code_Point" => Some(NoncharacterCodePoint),
            "Pat_Syn" | "Pattern_Syntax" => Some(PatternSyntax),
            "Pat_WS" | "Pattern_White_Space" => Some(PatternWhiteSpace),
            "QMark" | "Quotation_Mark" => Some(QuotationMark),
            "Radical" => Some(Radical),
            "RI" | "Regional_Indicator" => Some(RegionalIndicator),
            "STerm" | "Sentence_Terminal" => Some(SentenceTerminal),
            "SD" | "Soft_Dotted" => Some(SoftDotted),
            "Term" | "Terminal_Punctuation" => Some(TerminalPunctuation),
            "UIdeo" | "Unified_Ideograph" => Some(UnifiedIdeograph),
            "Upper" | "Uppercase" => Some(Uppercase),
            "VS" | "Variation_Selector" => Some(VariationSelector),
            "space" | "White_Space" => Some(WhiteSpace),
            "Emoji" => Some(Emoji),
            "EComp" | "Emoji_Component" => Some(EmojiComponent),
            "EMod" | "Emoji_Modifier" => Some(EmojiModifier),
            "EBase" | "Emoji_Modifier_Base" => Some(EmojiModifierBase),
            "EPres" | "Emoji_Presentation" => Some(EmojiPresentation),
            "ExtPict" | "Extended_Pictographic" => Some(ExtendedPictographic),
            "CWKCF" | "Changes_When_NFKC_Casefolded" => Some(ChangesWhenNFKCCasefolded),
            "Bidi_M" | "Bidi_Mirrored" => Some(BidiMirrored),
            "ASCII" => Some(Ascii),
            "Any" => Some(Any),
            "Assigned" => Some(Assigned),
            _ => None,
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub(crate) enum UnicodePropertyName {
    GeneralCategory,
    Script,
    ScriptExtensions,
}

impl UnicodePropertyName {
    pub(crate) fn from_str(s: &str) -> Option<Self> {
        use UnicodePropertyName::*;
        match s {
            "General_Category" | "gc" => Some(GeneralCategory),
            "Script" | "sc" => Some(Script),
            "Script_Extensions" | "scx" => Some(ScriptExtensions),
            _ => None,
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub(crate) enum UnicodePropertyValue {
    Binary(UnicodePropertyBinary),
    GeneralCategory(GeneralCategoryGroup),
    Script(Script),
}

impl UnicodePropertyValue {
    pub(crate) fn from_str(s: &str, name: Option<UnicodePropertyName>) -> Option<Self> {
        match name {
            Some(UnicodePropertyName::GeneralCategory) => {
                GeneralCategoryGroup::name_to_enum_mapper()
                    .get_strict(s)
                    .map(Self::GeneralCategory)
            }
            Some(UnicodePropertyName::Script | UnicodePropertyName::ScriptExtensions) => {
                Script::name_to_enum_mapper()
                    .get_strict(s)
                    .map(Self::Script)
            }
            None => {
                if let Some(binary) = UnicodePropertyBinary::from_str(s) {
                    Some(Self::Binary(binary))
                } else if let Some(general_category) =
                    GeneralCategoryGroup::name_to_enum_mapper().get_strict(s)
                {
                    Some(Self::GeneralCategory(general_category))
                } else {
                    Script::name_to_enum_mapper()
                        .get_strict(s)
                        .map(Self::Script)
                }
            }
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub struct PropertyEscape {
    name: Option<UnicodePropertyName>,
    value: UnicodePropertyValue,
}

impl PropertyEscape {
    pub(crate) fn new(name: Option<UnicodePropertyName>, value: UnicodePropertyValue) -> Self {
        Self { name, value }
    }

    pub(crate) fn contains(&self, c: u32) -> bool {
        use UnicodePropertyBinary::*;

        match (self.name, &self.value) {
            (
                Some(UnicodePropertyName::GeneralCategory) | None,
                UnicodePropertyValue::GeneralCategory(t),
            ) => t.contains(icu_properties::maps::general_category().get32(c)),
            (Some(UnicodePropertyName::Script), UnicodePropertyValue::Script(t)) => {
                icu_properties::maps::script().get32(c) == *t
            }
            (Some(UnicodePropertyName::ScriptExtensions), UnicodePropertyValue::Script(t)) => {
                icu_properties::script::script_with_extensions().has_script(c, *t)
            }
            (None, UnicodePropertyValue::Binary(t)) => match t {
                Alphabetic => sets::alphabetic().contains32(c),
                CaseIgnorable => sets::case_ignorable().contains32(c),
                Cased => sets::cased().contains32(c),
                ChangesWhenCasefolded => sets::changes_when_casefolded().contains32(c),
                ChangesWhenCasemapped => sets::changes_when_casemapped().contains32(c),
                ChangesWhenLowercased => sets::changes_when_lowercased().contains32(c),
                ChangesWhenTitlecased => sets::changes_when_titlecased().contains32(c),
                ChangesWhenUppercased => sets::changes_when_uppercased().contains32(c),
                DefaultIgnorableCodePoint => sets::default_ignorable_code_point().contains32(c),
                GraphemeBase => sets::grapheme_base().contains32(c),
                GraphemeExtend => sets::grapheme_extend().contains32(c),
                IDContinue => sets::id_continue().contains32(c),
                IDStart => sets::id_start().contains32(c),
                Math => sets::math().contains32(c),
                XIDContinue => sets::xid_continue().contains32(c),
                XIDStart => sets::xid_start().contains32(c),
                ASCIIHexDigit => sets::ascii_hex_digit().contains32(c),
                BidiControl => sets::bidi_control().contains32(c),
                Dash => sets::dash().contains32(c),
                Deprecated => sets::deprecated().contains32(c),
                Diacritic => sets::diacritic().contains32(c),
                Extender => sets::extender().contains32(c),
                HexDigit => sets::hex_digit().contains32(c),
                IDSBinaryOperator => sets::ids_binary_operator().contains32(c),
                IDSTrinaryOperator => sets::ids_trinary_operator().contains32(c),
                Ideographic => sets::ideographic().contains32(c),
                JoinControl => sets::join_control().contains32(c),
                LogicalOrderException => sets::logical_order_exception().contains32(c),
                Lowercase => sets::lowercase().contains32(c),
                NoncharacterCodePoint => sets::noncharacter_code_point().contains32(c),
                PatternSyntax => sets::pattern_syntax().contains32(c),
                PatternWhiteSpace => sets::pattern_white_space().contains32(c),
                QuotationMark => sets::quotation_mark().contains32(c),
                Radical => sets::radical().contains32(c),
                RegionalIndicator => sets::regional_indicator().contains32(c),
                SentenceTerminal => sets::sentence_terminal().contains32(c),
                SoftDotted => sets::soft_dotted().contains32(c),
                TerminalPunctuation => sets::terminal_punctuation().contains32(c),
                UnifiedIdeograph => sets::unified_ideograph().contains32(c),
                Uppercase => sets::uppercase().contains32(c),
                VariationSelector => sets::variation_selector().contains32(c),
                WhiteSpace => sets::white_space().contains32(c),
                Emoji => sets::emoji().contains32(c),
                EmojiComponent => sets::emoji_component().contains32(c),
                EmojiModifier => sets::emoji_modifier().contains32(c),
                EmojiModifierBase => sets::emoji_modifier_base().contains32(c),
                EmojiPresentation => sets::emoji_presentation().contains32(c),
                ExtendedPictographic => sets::extended_pictographic().contains32(c),
                ChangesWhenNFKCCasefolded => sets::changes_when_nfkc_casefolded().contains32(c),
                BidiMirrored => sets::bidi_mirrored().contains32(c),
                Ascii => c <= 127,
                Any => c <= 0x10FFFF,
                Assigned => {
                    icu_properties::maps::general_category().get32(c) != GeneralCategory::Unassigned
                }
            },
            _ => false,
        }
    }
}
