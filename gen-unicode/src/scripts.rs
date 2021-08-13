use crate::{chars_to_code_point_ranges, parse_line};
use std::fs::File;
use std::io::{self, BufRead};

use codegen::{Block, Enum, Function, Scope};

pub(crate) fn generate(scope: &mut Scope) {
    let mut property_enum = Enum::new("UnicodePropertyValueScript");
    property_enum
        .vis("pub")
        .derive("Debug")
        .derive("Clone")
        .derive("Copy");

    let mut is_property_fn = Function::new("is_property_value_script");
    is_property_fn
        .vis("pub(crate)")
        .arg("c", "char")
        .arg("value", "&UnicodePropertyValueScript")
        .ret("bool")
        .line("use UnicodePropertyValueScript::*;");
    let mut is_property_fn_match_block = Block::new("match value");

    let mut property_from_str_fn = Function::new("unicode_property_value_script_from_str");
    property_from_str_fn
        .arg("s", "&str")
        .ret("Option<UnicodePropertyValueScript>")
        .vis("pub")
        .line("use UnicodePropertyValueScript::*;");
    let mut property_from_str_fn_match_block = Block::new("match s");

    for (alias0, alias1, orig_name, name) in SCRIPTS {
        let file = File::open("Scripts.txt").expect("could not open Scripts.txt");
        let lines = io::BufReader::new(file).lines();
        let mut chars = Vec::new();

        for line in lines {
            parse_line(&line.unwrap(), &mut chars, orig_name);
        }

        let ranges = chars_to_code_point_ranges(&chars);

        scope.raw(&format!(
            "pub(crate) const {}: [CodePointRange; {}] = [\n    {}\n];",
            orig_name.to_uppercase(),
            ranges.len(),
            ranges.join("\n    ")
        ));

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

        property_from_str_fn_match_block.line(if alias0.is_empty() && alias1.is_empty() {
            format!("\"{}\" => Some({}),", orig_name, name)
        } else if alias0.is_empty() {
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

    scope
        .push_fn(is_property_fn)
        .push_enum(property_enum)
        .push_fn(property_from_str_fn);
}

pub(crate) fn generate_tests(scope: &mut Scope) {
    for (alias0, alias1, orig_name, name) in SCRIPTS {
        let file = File::open("Scripts.txt").expect("could not open Scripts.txt");
        let lines = io::BufReader::new(file).lines();
        let mut chars = Vec::new();

        for line in lines {
            parse_line(&line.unwrap(), &mut chars, orig_name);
        }

        scope
            .new_fn(&format!(
                "unicode_escape_property_script_{}",
                name.to_lowercase()
            ))
            .attr("test")
            .line(format!(
                "test_with_configs(unicode_escape_property_script_{}_tc)",
                name.to_lowercase()
            ));

        let f = scope.new_fn(&format!(
            "unicode_escape_property_script_{}_tc",
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
            format!(r#""^\\p{{Script={}}}+$""#, orig_name),
            format!(r#""^\\p{{sc={}}}+$""#, orig_name),
        ];

        if !alias0.is_empty() {
            regexes.push(format!(r#""^\\p{{Script={}}}+$""#, alias0));
            regexes.push(format!(r#""^\\p{{sc={}}}+$""#, alias0));
        }

        if !alias1.is_empty() {
            regexes.push(format!(r#""^\\p{{Script={}}}+$""#, alias1));
            regexes.push(format!(r#""^\\p{{sc={}}}+$""#, alias1));
        }

        f.line(format!(
            "const REGEXES: [&str; {}] = [\n    {},\n];",
            regexes.len(),
            regexes.join(",\n    ")
        ));

        let mut b = Block::new("for regex in REGEXES");
        b.line("let regex = tc.compile(regex);");

        let mut bb = Block::new("for code_point in CODE_POINTS");
        bb.line("regex.test_succeeds(code_point);");

        b.push_block(bb);

        f.push_block(b);
    }
}

// Structure: (Alias, Alias, Name, CamelCaseName)
const SCRIPTS: &[(&str, &str, &str, &str); 156] = &[
    ("", "Adlm", "Adlam", "Adlam"),
    ("", "", "Ahom", "Ahom"),
    ("", "Hluw", "Anatolian_Hieroglyphs", "AnatolianHieroglyphs"),
    ("", "Arab", "Arabic", "Arabic"),
    ("", "Armn", "Armenian", "Armenian"),
    ("", "Avst", "Avestan", "Avestan"),
    ("", "Bali", "Balinese", "Balinese"),
    ("", "Bamu", "Bamum", "Bamum"),
    ("", "Bass", "Bassa_Vah", "BassaVah"),
    ("", "Batk", "Batak", "Batak"),
    ("", "Beng", "Bengali", "Bengali"),
    ("", "Bhks", "Bhaiksuki", "Bhaiksuki"),
    ("", "Bopo", "Bopomofo", "Bopomofo"),
    ("", "Brah", "Brahmi", "Brahmi"),
    ("", "Brai", "Braille", "Braille"),
    ("", "Bugi", "Buginese", "Buginese"),
    ("", "Buhd", "Buhid", "Buhid"),
    ("", "Cans", "Canadian_Aboriginal", "CanadianAboriginal"),
    ("", "Cari", "Carian", "Carian"),
    ("", "Aghb", "Caucasian_Albanian", "CaucasianAlbanian"),
    ("", "Cakm", "Chakma", "Chakma"),
    ("", "", "Cham", "Cham"),
    ("", "Chrs", "Chorasmian", "Chorasmian"),
    ("", "Cher", "Cherokee", "Cherokee"),
    ("", "Zyyy", "Common", "Common"),
    ("Copt", "Qaac", "Coptic", "Coptic"),
    ("", "Xsux", "Cuneiform", "Cuneiform"),
    ("", "Cprt", "Cypriot", "Cypriot"),
    ("", "Cyrl", "Cyrillic", "Cyrillic"),
    ("", "Dsrt", "Deseret", "Deseret"),
    ("", "Deva", "Devanagari", "Devanagari"),
    ("", "Diak", "Dives_Akuru", "DivesAkuru"),
    ("", "Dogr", "Dogra", "Dogra"),
    ("", "Dupl", "Duployan", "Duployan"),
    ("", "Egyp", "Egyptian_Hieroglyphs", "EgyptianHieroglyphs"),
    ("", "Elba", "Elbasan", "Elbasan"),
    ("", "Elym", "Elymaic", "Elymaic"),
    ("", "Ethi", "Ethiopic", "Ethiopic"),
    ("", "Geor", "Georgian", "Georgian"),
    ("", "Glag", "Glagolitic", "Glagolitic"),
    ("", "Goth", "Gothic", "Gothic"),
    ("", "Gran", "Grantha", "Grantha"),
    ("", "Grek", "Greek", "Greek"),
    ("", "Gujr", "Gujarati", "Gujarati"),
    ("", "Gong", "Gunjala_Gondi", "GunjalaGondi"),
    ("", "Guru", "Gurmukhi", "Gurmukhi"),
    ("", "Hani", "Han", "Han"),
    ("", "Hang", "Hangul", "Hangul"),
    ("", "Rohg", "Hanifi_Rohingya", "HanifiRohingya"),
    ("", "Hano", "Hanunoo", "Hanunoo"),
    ("", "Hatr", "Hatran", "Hatran"),
    ("", "Hebr", "Hebrew", "Hebrew"),
    ("", "Hira", "Hiragana", "Hiragana"),
    ("", "Armi", "Imperial_Aramaic", "ImperialAramaic"),
    ("Zinh", "Qaai", "Inherited", "Inherited"),
    ("", "Phli", "Inscriptional_Pahlavi", "InscriptionalPahlavi"),
    (
        "",
        "Prti",
        "Inscriptional_Parthian",
        "InscriptionalParthian",
    ),
    ("", "Java", "Javanese", "Javanese"),
    ("", "Kthi", "Kaithi", "Kaithi"),
    ("", "Knda", "Kannada", "Kannada"),
    ("", "Kana", "Katakana", "Katakana"),
    ("", "Kali", "Kayah_Li", "KayahLi"),
    ("", "Khar", "Kharoshthi", "Kharoshthi"),
    ("", "Kits", "Khitan_Small_Script", "KhitanSmallScript"),
    ("", "Khmr", "Khmer", "Khmer"),
    ("", "Khoj", "Khojki", "Khojki"),
    ("", "Sind", "Khudawadi", "Khudawadi"),
    ("", "Laoo", "Lao", "Lao"),
    ("", "Latn", "Latin", "Latin"),
    ("", "Lepc", "Lepcha", "Lepcha"),
    ("", "Limb", "Limbu", "Limbu"),
    ("", "Lina", "Linear_A", "LinearA"),
    ("", "Linb", "Linear_B", "LinearB"),
    ("", "", "Lisu", "Lisu"),
    ("", "Lyci", "Lycian", "Lycian"),
    ("", "Lydi", "Lydian", "Lydian"),
    ("", "Mahj", "Mahajani", "Mahajani"),
    ("", "Maka", "Makasar", "Makasar"),
    ("", "Mlym", "Malayalam", "Malayalam"),
    ("", "Mand", "Mandaic", "Mandaic"),
    ("", "Mani", "Manichaean", "Manichaean"),
    ("", "Marc", "Marchen", "Marchen"),
    ("", "Medf", "Medefaidrin", "Medefaidrin"),
    ("", "Gonm", "Masaram_Gondi", "MasaramGondi"),
    ("", "Mtei", "Meetei_Mayek", "MeeteiMayek"),
    ("", "Mend", "Mende_Kikakui", "MendeKikakui"),
    ("", "Merc", "Meroitic_Cursive", "MeroiticCursive"),
    ("", "Mero", "Meroitic_Hieroglyphs", "MeroiticHieroglyphs"),
    ("", "Plrd", "Miao", "Miao"),
    ("", "", "Modi", "Modi"),
    ("", "Mong", "Mongolian", "Mongolian"),
    ("", "Mroo", "Mro", "Mro"),
    ("", "Mult", "Multani", "Multani"),
    ("", "Mymr", "Myanmar", "Myanmar"),
    ("", "Nbat", "Nabataean", "Nabataean"),
    ("", "Nand", "Nandinagari", "Nandinagari"),
    ("", "Talu", "New_Tai_Lue", "NewTaiLue"),
    ("", "", "Newa", "Newa"),
    ("", "Nkoo", "Nko", "Nko"),
    ("", "Nshu", "Nushu", "Nushu"),
    ("", "Hmnp", "Nyiakeng_Puachue_Hmong", "NyiakengPuachueHmong"),
    ("", "Ogam", "Ogham", "Ogham"),
    ("", "Olck", "Ol_Chiki", "OlChiki"),
    ("", "Hung", "Old_Hungarian", "OldHungarian"),
    ("", "Ital", "Old_Italic", "OldItalic"),
    ("", "Narb", "Old_North_Arabian", "OldNorthArabian"),
    ("", "Perm", "Old_Permic", "OldPermic"),
    ("", "Xpeo", "Old_Persian", "OldPersian"),
    ("", "Sogo", "Old_Sogdian", "OldSogdian"),
    ("", "Sarb", "Old_South_Arabian", "OldSouthArabian"),
    ("", "Orkh", "Old_Turkic", "OldTurkic"),
    ("", "Orya", "Oriya", "Oriya"),
    ("", "Osge", "Osage", "Osage"),
    ("", "Osma", "Osmanya", "Osmanya"),
    ("", "Hmng", "Pahawh_Hmong", "PahawhHmong"),
    ("", "Palm", "Palmyrene", "Palmyrene"),
    ("", "Pauc", "Pau_Cin_Hau", "PauCinHau"),
    ("", "Phag", "Phags_Pa", "PhagsPa"),
    ("", "Phnx", "Phoenician", "Phoenician"),
    ("", "Phlp", "Psalter_Pahlavi", "PsalterPahlavi"),
    ("", "Rjng", "Rejang", "Rejang"),
    ("", "Runr", "Runic", "Runic"),
    ("", "Samr", "Samaritan", "Samaritan"),
    ("", "Saur", "Saurashtra", "Saurashtra"),
    ("", "Shrd", "Sharada", "Sharada"),
    ("", "Shaw", "Shavian", "Shavian"),
    ("", "Sidd", "Siddham", "Siddham"),
    ("", "Sgnw", "SignWriting", "SignWriting"),
    ("", "Sinh", "Sinhala", "Sinhala"),
    ("", "Sogd", "Sogdian", "Sogdian"),
    ("", "Sora", "Sora_Sompeng", "SoraSompeng"),
    ("", "Soyo", "Soyombo", "Soyombo"),
    ("", "Sund", "Sundanese", "Sundanese"),
    ("", "Sylo", "Syloti_Nagri", "SylotiNagri"),
    ("", "Syrc", "Syriac", "Syriac"),
    ("", "Tglg", "Tagalog", "Tagalog"),
    ("", "Tagb", "Tagbanwa", "Tagbanwa"),
    ("", "Tale", "Tai_Le", "TaiLe"),
    ("", "Lana", "Tai_Tham", "TaiTham"),
    ("", "Tavt", "Tai_Viet", "TaiViet"),
    ("", "Takr", "Takri", "Takri"),
    ("", "Taml", "Tamil", "Tamil"),
    ("", "Tang", "Tangut", "Tangut"),
    ("", "Telu", "Telugu", "Telugu"),
    ("", "Thaa", "Thaana", "Thaana"),
    ("", "", "Thai", "Thai"),
    ("", "Tibt", "Tibetan", "Tibetan"),
    ("", "Tfng", "Tifinagh", "Tifinagh"),
    ("", "Tirh", "Tirhuta", "Tirhuta"),
    ("", "Ugar", "Ugaritic", "Ugaritic"),
    ("", "Vaii", "Vai", "Vai"),
    ("", "Wcho", "Wancho", "Wancho"),
    ("", "Wara", "Warang_Citi", "WarangCiti"),
    ("", "Yezi", "Yezidi", "Yezidi"),
    ("", "Yiii", "Yi", "Yi"),
    ("", "Zanb", "Zanabazar_Square", "ZanabazarSquare"),
];
