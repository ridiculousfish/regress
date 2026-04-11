use crate::{
    codepoints_to_range, format_interval_table, merge_sorted_ranges, pack_adjacent_codepoints,
    remove_codepoints, GenUnicode,
};
use codegen::{Block, Enum, Function};

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
const EXCLUDED_SCRIPTS: [&str; 1] = ["Katakana_Or_Hiragana"];

impl GenUnicode {
    pub(crate) fn generate_scripts(&mut self) {
        let mut scripts = Vec::new();
        let mut scx_ranges = Vec::new();

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
                merge_sorted_ranges(&mut script_ranges);

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
                merge_sorted_ranges(&mut script_extension_ranges);
                scx_ranges.extend(script_extension_ranges.clone());

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

        scx_ranges.sort();
        merge_sorted_ranges(&mut scx_ranges);

        // Compute Script=Unknown: the complement of all assigned-script codepoints in [0, 0x10FFFF].
        // Scripts.txt never lists Zzzz entries; Unknown is defined by absence.
        {
            // Build the union of all codepoints assigned to any known script.
            let mut assigned: Vec<(u32, u32)> = scripts
                .iter()
                .flat_map(|s| s.codepoints_sc.iter().copied())
                .collect();
            assigned.sort();
            merge_sorted_ranges(&mut assigned);

            // Complement within [0x0000, 0x10FFFF].
            let mut unknown: Vec<(u32, u32)> = Vec::new();
            let mut cursor: u32 = 0;
            for (start, end) in &assigned {
                if cursor < *start {
                    unknown.push((cursor, *start - 1));
                }
                cursor = *end + 1;
            }
            if cursor <= 0x10FFFF {
                unknown.push((cursor, 0x10FFFF));
            }

            // Assign to the Unknown entry.
            if let Some(unknown_script) = scripts.iter_mut().find(|s| s.long == "Unknown") {
                unknown_script.codepoints_sc = unknown.clone();
                unknown_script.codepoints_scx = unknown;
            }
        }

        // Delete script extensions ranges from the "Common" and "Inherited" script extension ranges.
        for script in &mut scripts {
            if script
                .names
                .iter()
                .any(|name| ["Common", "Inherited"].contains(&name.as_str()))
            {
                script.codepoints_scx = script.codepoints_sc.clone();

                for range in &scx_ranges {
                    remove_codepoints(&mut script.codepoints_scx, *range);
                }

                script.codepoints_scx.sort();
                merge_sorted_ranges(&mut script.codepoints_scx);
            }
        }

        let mut property_enum = Enum::new("UnicodePropertyValueScript");
        property_enum
            .vis("pub")
            .derive("Debug")
            .derive("Clone")
            .derive("Copy");

        let mut as_ranges_fn_sc = Function::new("script_value_ranges");
        as_ranges_fn_sc
            .vis("pub(crate)")
            .arg("value", "&UnicodePropertyValueScript")
            .ret("&'static [Interval]")
            .line("use UnicodePropertyValueScript::*;");
        let mut as_ranges_fn_sc_match_block = Block::new("match value");

        let mut as_ranges_fn_scx = Function::new("script_extensions_value_ranges");
        as_ranges_fn_scx
            .vis("pub(crate)")
            .arg("value", "&UnicodePropertyValueScript")
            .ret("&'static [Interval]")
            .line("use UnicodePropertyValueScript::*;");
        let mut as_ranges_fn_scx_match_block = Block::new("match value");

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

            self.scope.raw(format_interval_table(
                &script.codepoints_sc_name,
                &script.codepoints_sc,
            ));

            as_ranges_fn_sc_match_block.line(format!(
                "{} => &{},",
                script.enum_name, script.codepoints_sc_name
            ));

            if script.codepoints_scx.is_empty() {
                as_ranges_fn_scx_match_block.line(format!(
                    "{} => &{},",
                    script.enum_name, script.codepoints_sc_name
                ));

                continue;
            }

            if script
                .names
                .iter()
                .any(|name| ["Common", "Inherited"].contains(&name.as_str()))
            {
                self.scope.raw(format_interval_table(
                    &script.codepoints_scx_name,
                    &script.codepoints_scx,
                ));

                as_ranges_fn_scx_match_block.line(format!(
                    "{} => &{},",
                    script.enum_name, script.codepoints_scx_name
                ));
            } else {
                let mut codepoints = script.codepoints_sc.clone();
                codepoints.extend(&script.codepoints_scx);
                codepoints.sort();
                merge_sorted_ranges(&mut codepoints);

                self.scope.raw(format_interval_table(
                    &script.codepoints_scx_name,
                    &codepoints,
                ));

                as_ranges_fn_scx_match_block.line(format!(
                    "{} => &{},",
                    script.enum_name, script.codepoints_scx_name
                ));
            }
        }

        property_from_str_fn_match_block.line("_ => None,");
        property_from_str_fn.push_block(property_from_str_fn_match_block);

        as_ranges_fn_sc.push_block(as_ranges_fn_sc_match_block);
        as_ranges_fn_scx.push_block(as_ranges_fn_scx_match_block);

        self.scope.push_enum(property_enum);
        self.scope.push_fn(property_from_str_fn);
        self.scope.push_fn(as_ranges_fn_sc);
        self.scope.push_fn(as_ranges_fn_scx);

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

            // Exclude surrogate code points (U+D800..U+DFFF) — they are not valid
            // Rust `char` values and char::from_u32() returns None for them.
            let test_codepoints: Vec<(u32, u32)> = script
                .codepoints_sc
                .iter()
                .flat_map(|&(start, end)| {
                    const SUR_START: u32 = 0xD800;
                    const SUR_END: u32 = 0xDFFF;
                    let mut parts = Vec::new();
                    if start < SUR_START && end >= SUR_START {
                        parts.push((start, SUR_START - 1));
                    }
                    if end > SUR_END && start <= SUR_END {
                        parts.push((SUR_END + 1, end));
                    }
                    if end < SUR_START || start > SUR_END {
                        parts.push((start, end));
                    }
                    parts
                })
                .collect();

            // If the total number of codepoints is very large, only sample the
            // first and last codepoint of each range to keep test runtime bounded.
            const SAMPLE_THRESHOLD: u32 = 10_000;
            let total: u32 = test_codepoints.iter().map(|(s, e)| e - s + 1).sum();
            let sample_only = total > SAMPLE_THRESHOLD;

            if sample_only {
                // Emit a flat array of individual sampled codepoints.
                let samples: Vec<u32> = test_codepoints
                    .iter()
                    .flat_map(|&(s, e)| if s == e { vec![s] } else { vec![s, e] })
                    .collect();
                f.line(format!(
                    "static CODE_POINTS: [u32; {}] = [{}];",
                    samples.len(),
                    samples
                        .iter()
                        .map(|cp| cp.to_string())
                        .collect::<Vec<_>>()
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
                f.line(r#"for regex in REGEXES { let regex = tc.compilef(regex, "u"); for &cp in &CODE_POINTS { regex.test_succeeds(&char::from_u32(cp).unwrap().to_string()); } }"#);
            } else {
                f.line(format!(
                    "static CODE_POINTS: [std::ops::RangeInclusive<u32>; {}] = [\n    {}\n];",
                    test_codepoints.len(),
                    test_codepoints
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
                f.line(r#"for regex in REGEXES { let regex = tc.compilef(regex, "u"); for range in &CODE_POINTS { for cp in range.clone() { regex.test_succeeds(&char::from_u32(cp).unwrap().to_string()); } } }"#);
            }
        }
    }
}
