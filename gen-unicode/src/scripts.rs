use crate::{chars_to_code_point_ranges, pack_adjacent_chars, GenUnicode};
use codegen::{Block, Enum, Function};
use ucd_parse::Codepoints;

struct Script {
    long: String,
    names: Vec<String>,
    enum_name: String,
    codepoints_sc_name: String,
    codepoints_scx_name: String,
    codepoints_sc: Vec<(u32, u32)>,
    codepoints_scx: Vec<(u32, u32)>,
}

// TODO: Wait for https://github.com/tc39/ecma262/issues/3190 to be resolved.
const EXCLUDED_SCRIPTS: [&str; 2] = ["Unknown", "Katakana_Or_Hiragana"];

impl GenUnicode {
    pub(crate) fn generate_scripts(&mut self) {
        let mut scripts = Vec::new();

        for alias in &self.property_value_aliases {
            if alias.property == "sc" && !EXCLUDED_SCRIPTS.contains(&alias.long.as_str()) {
                let mut names = Vec::new();
                names.push(alias.long.clone());
                if alias.long != alias.abbreviation {
                    names.push(alias.abbreviation.clone());
                }
                names.extend(alias.aliases.clone());

                let mut script_ranges: Vec<_> = self
                    .scripts
                    .iter()
                    .filter_map(|sc| {
                        if sc.script == alias.long {
                            Some(codepoints_to_range(&sc.codepoints))
                        } else {
                            None
                        }
                    })
                    .collect();
                script_ranges.sort();
                pack_adjacent_chars(&mut script_ranges);

                let mut script_extension_ranges: Vec<_> = self
                    .script_extensions
                    .iter()
                    .filter_map(|scx| {
                        if scx.scripts.contains(&alias.abbreviation) {
                            Some(codepoints_to_range(&scx.codepoints))
                        } else {
                            None
                        }
                    })
                    .collect();
                script_extension_ranges.sort();
                pack_adjacent_chars(&mut script_extension_ranges);

                scripts.push(Script {
                    long: alias.long.clone(),
                    names,
                    enum_name: alias.long.replace('_', ""),
                    codepoints_sc_name: alias.long.to_uppercase(),
                    codepoints_scx_name: format!("{}_EXTENSIONS", alias.long.to_uppercase()),
                    codepoints_sc: script_ranges,
                    codepoints_scx: script_extension_ranges,
                });
            }
        }

        let mut property_enum = Enum::new("UnicodePropertyValueScript");
        property_enum
            .vis("pub")
            .derive("Debug")
            .derive("Clone")
            .derive("Copy");

        let mut is_property_fn_sc = Function::new("is_property_value_script");
        is_property_fn_sc
            .vis("pub(crate)")
            .arg("c", "u32")
            .arg("value", "&UnicodePropertyValueScript")
            .ret("bool")
            .line("use UnicodePropertyValueScript::*;");
        let mut is_property_fn_match_block_sc = Block::new("match value");

        let mut is_property_fn_scx = Function::new("is_property_value_script_extensions");
        is_property_fn_scx
            .vis("pub(crate)")
            .arg("c", "u32")
            .arg("value", "&UnicodePropertyValueScript")
            .ret("bool")
            .line("use UnicodePropertyValueScript::*;");
        let mut is_property_fn_match_block_scx = Block::new("match value");

        let mut property_from_str_fn = Function::new("unicode_property_value_script_from_str");
        property_from_str_fn
            .arg("s", "&str")
            .ret("Option<UnicodePropertyValueScript>")
            .vis("pub(crate)")
            .line("use UnicodePropertyValueScript::*;");
        let mut property_from_str_fn_match_block = Block::new("match s");

        for script in &scripts {
            property_enum.new_variant(&script.enum_name);

            property_from_str_fn_match_block.line(format!(
                "{} => Some({}),",
                script
                    .names
                    .iter()
                    .map(|s| format!("\"{}\"", s))
                    .collect::<Vec<_>>()
                    .join("| "),
                script.enum_name,
            ));

            let ranges = chars_to_code_point_ranges(&script.codepoints_sc);

            self.scope.raw(&format!(
                "const {}: [CodePointRange; {}] = [\n    {}\n];",
                script.codepoints_sc_name,
                ranges.len(),
                ranges.join("\n    ")
            ));

            is_property_fn_match_block_sc.line(format!(
                "{} => {}.binary_search_by(|&cpr| cpr.compare(c)).is_ok(),",
                script.enum_name, script.codepoints_sc_name,
            ));

            if script.codepoints_scx.is_empty() {
                is_property_fn_match_block_scx.line(format!(
                    "{} => {}.binary_search_by(|&cpr| cpr.compare(c)).is_ok(),",
                    script.enum_name, script.codepoints_sc_name,
                ));

                continue;
            }

            is_property_fn_match_block_scx.line(&format!(
                "{} => {}.binary_search_by(|&cpr| cpr.compare(c)).is_ok() || {}.binary_search_by(|&cpr| cpr.compare(c)).is_ok(),",
                script.enum_name,
                script.codepoints_scx_name,
                script.codepoints_sc_name,
            ));

            let ranges = chars_to_code_point_ranges(&script.codepoints_scx);

            self.scope.raw(&format!(
                "const {}: [CodePointRange; {}] = [\n    {}\n];",
                script.codepoints_scx_name,
                ranges.len(),
                ranges.join("\n    ")
            ));
        }

        property_from_str_fn_match_block.line("_ => None,");
        property_from_str_fn.push_block(property_from_str_fn_match_block);

        is_property_fn_sc.push_block(is_property_fn_match_block_sc);
        is_property_fn_scx.push_block(is_property_fn_match_block_scx);

        self.scope.push_enum(property_enum);
        self.scope.push_fn(property_from_str_fn);
        self.scope.push_fn(is_property_fn_sc);
        self.scope.push_fn(is_property_fn_scx);

        self.generate_scripts_tests(&scripts);
    }

    fn generate_scripts_tests(&mut self, scripts: &[Script]) {
        for script in scripts {
            if script.codepoints_sc.is_empty() {
                continue;
            }

            let test_name = script.long.to_lowercase();

            self.scope_tests
                .new_fn(&format!("unicode_escape_property_script_{test_name}"))
                .attr("test")
                .line(format!(
                    "test_with_configs(unicode_escape_property_script_{test_name}_tc)"
                ));

            let f = self
                .scope_tests
                .new_fn(&format!("unicode_escape_property_script_{test_name}_tc"))
                .arg("tc", "TestConfig");

            f.line(format!(
                "const CODE_POINTS: [std::ops::RangeInclusive<u32>; {}] = [\n    {}\n];",
                script.codepoints_sc.len(),
                script
                    .codepoints_sc
                    .iter()
                    .map(|(start, end)| format!("{}..={}", start, end))
                    .collect::<Vec<String>>()
                    .join(", ")
            ));

            let mut regexes = Vec::with_capacity(script.names.len());
            for alias in &script.names {
                regexes.push(format!(r#""^\\p{{Script={}}}+$""#, alias));
                regexes.push(format!(r#""^\\p{{sc={}}}+$""#, alias));
            }

            f.line(format!(
                "const REGEXES: [&str; {}] = [\n    {},\n];",
                regexes.len(),
                regexes.join(",\n    ")
            ));

            f.line(r#"for regex in REGEXES { let regex = tc.compile(regex); for range in CODE_POINTS { for cp in range { regex.test_succeeds(&char::from_u32(cp).unwrap().to_string()); } } }"#);
        }
    }
}

fn codepoints_to_range(cp: &Codepoints) -> (u32, u32) {
    match cp {
        Codepoints::Single(cp) => (cp.value(), cp.value()),
        Codepoints::Range(range) => (range.start.value(), range.end.value()),
    }
}
