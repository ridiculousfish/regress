use crate::{GenUnicode, UCD_PATH};
use codegen::{Block, Enum, Function};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead};
use std::str::FromStr;
use ucd_parse::{Codepoint, CodepointRange};

const UCD_PATH_EMOJI_SEQUENCES: &str = "/emoji-sequences.txt";
const UCD_PATH_EMOJI_ZWJ_SEQUENCES: &str = "/emoji-zwj-sequences.txt";

fn parse_file(path: &str, properties: &mut HashMap<String, Vec<Vec<Codepoint>>>) {
    let file = File::open(path).unwrap();
    for line in io::BufReader::new(file).lines() {
        let line = line.unwrap();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let mut fields = line.split(';');
        let codepoints = fields.next().unwrap().trim();
        let property = fields.next().unwrap().trim().to_string();

        let property_codepoints = match properties.entry(property) {
            Entry::Occupied(o) => o.into_mut(),
            Entry::Vacant(v) => v.insert(Vec::new()),
        };

        if codepoints.contains("..") {
            for cp in CodepointRange::from_str(codepoints).unwrap() {
                property_codepoints.push(vec![cp]);
            }
        } else if codepoints.contains(' ') {
            property_codepoints.push(
                codepoints
                    .split_whitespace()
                    .map(|s| Codepoint::from_str(s).unwrap())
                    .collect(),
            );
        } else {
            property_codepoints.push(vec![Codepoint::from_str(codepoints).unwrap()]);
        }
    }
}

impl GenUnicode {
    pub(crate) fn generate_string_properties(&mut self) {
        let mut properties: HashMap<String, Vec<Vec<Codepoint>>> = HashMap::new();

        parse_file(
            &(UCD_PATH.to_string() + UCD_PATH_EMOJI_SEQUENCES),
            &mut properties,
        );
        parse_file(
            &(UCD_PATH.to_string() + UCD_PATH_EMOJI_ZWJ_SEQUENCES),
            &mut properties,
        );

        let mut property_sets = Vec::new();
        for (name, sets) in properties {
            property_sets.push((name.clone(), name.replace('_', ""), sets));
        }
        property_sets.sort_by(|(name_a, _, _), (name_b, _, _)| name_a.cmp(name_b));

        let mut rgi_emoji_sets = Vec::new();
        for (name, _, sets) in &property_sets {
            if [
                "Basic_Emoji",
                "Emoji_Keycap_Sequence",
                "RGI_Emoji_Flag_Sequence",
                "RGI_Emoji_Tag_Sequence",
                "RGI_Emoji_Modifier_Sequence",
                "RGI_Emoji_ZWJ_Sequence",
            ]
            .contains(&name.as_str())
            {
                rgi_emoji_sets.extend(sets.clone());
            }
        }

        property_sets.push((
            "RGI_Emoji".to_string(),
            "RGIEmoji".to_string(),
            rgi_emoji_sets,
        ));

        let mut property_enum = Enum::new("UnicodeStringProperty");
        property_enum
            .vis("pub")
            .derive("Debug")
            .derive("Clone")
            .derive("Copy");

        let mut as_ranges_fn = Function::new("string_property_sets");
        as_ranges_fn
            .vis("pub(crate)")
            .arg("value", "&UnicodeStringProperty")
            .ret("&'static [&'static [u32]]")
            .line("use UnicodeStringProperty::*;");
        let mut as_ranges_fn_match_block = Block::new("match value");

        let mut property_from_str_fn = Function::new("unicode_string_property_from_str");
        property_from_str_fn
            .arg("s", "&str")
            .ret("Option<UnicodeStringProperty>")
            .vis("pub")
            .line("use UnicodeStringProperty::*;");
        let mut property_from_str_fn_match_block = Block::new("match s");

        for (name, name_enum, sets) in &property_sets {
            self.scope.raw(format!(
                "static {}: &[&[u32]; {}] = &[{}];",
                name.to_uppercase(),
                sets.len(),
                sets.iter()
                    .map(|codepoints| codepoints
                        .iter()
                        .map(|cp| cp.value().to_string())
                        .collect::<Vec<String>>()
                        .join(","))
                    .map(|s| format!("&[{}],", s.as_str()))
                    .collect::<Vec<String>>()
                    .join("")
            ));

            self.scope
                .new_fn(&format!("{}_sets", name.to_lowercase()))
                .vis("pub(crate)")
                .ret("&'static [&'static [u32]]")
                .line(&format!("{}.as_slice()", name.to_uppercase()))
                .doc(&format!(
                    "Return the code point ranges of the '{}' Unicode property.",
                    name
                ));

            property_enum.new_variant(name_enum);

            as_ranges_fn_match_block.line(format!(
                "{} => {}_sets(),",
                &name_enum,
                name.to_lowercase()
            ));

            property_from_str_fn_match_block.line(format!("\"{}\" => Some({}),", name, name_enum));
        }

        as_ranges_fn.push_block(as_ranges_fn_match_block);
        property_from_str_fn_match_block.line("_ => None,");
        property_from_str_fn.push_block(property_from_str_fn_match_block);

        self.scope.push_enum(property_enum);
        self.scope.push_fn(as_ranges_fn);
        self.scope.push_fn(property_from_str_fn);
    }
}
