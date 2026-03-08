use crate::{DeltaBlock, FoldPair, GenUnicode};
use ucd_parse::CaseStatus;

const ASCII_WORD_CHARS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_";

impl GenUnicode {
    pub(crate) fn generate_nonascii_folds_to_ascii_word_char(&mut self) {
        let mut arms = Vec::new();
        for case_fold in &self.case_folds {
            if !matches!(case_fold.status, CaseStatus::Common | CaseStatus::Simple) {
                continue;
            }
            let orig = case_fold.codepoint.value();
            let folded: u32 = case_fold.mapping[0].value();
            // non-ASCII (>= 0x80) that folds to ASCII word char
            if orig >= 0x80 && ASCII_WORD_CHARS.contains(char::from_u32(folded).unwrap_or('\0')) {
                arms.push(format!(
                    "0x{:04X} => true, // folds to {:?}",
                    orig,
                    char::from_u32(folded).unwrap()
                ));
            }
        }

        let comment = r"// \return whether a non-ASCII character folds to an ASCII word char.";
        self.scope.raw(format!(
            "{}\n#[inline]\npub(crate) fn nonascii_folds_to_ascii_word_char(cp: u32) -> bool {{\n    match cp {{\n        {}\n        _ => false,\n    }}\n}}",
            comment, arms.join("\n        ")
        ));
    }

    pub(crate) fn generate_case_folds(&mut self) {
        let mut fold_pairs = Vec::new();

        for case_fold in &self.case_folds {
            if case_fold.status != CaseStatus::Common && case_fold.status != CaseStatus::Simple {
                continue;
            }
            fold_pairs.push(FoldPair {
                orig: case_fold.codepoint.value(),
                folded: case_fold.mapping[0].value(),
            });
        }

        let mut delta_blocks: Vec<DeltaBlock> = Vec::new();
        for fp in &fold_pairs {
            match delta_blocks.last_mut() {
                Some(ref mut db) if db.can_append(*fp) => db.append(*fp),
                _ => delta_blocks.push(DeltaBlock::create(*fp)),
            }
        }

        let mut lines = Vec::new();
        for db in &delta_blocks {
            lines.push(format!(
                "FoldRange::from({start:#04X}, {length}, {delta}, {modulo}),",
                start = db.first().orig,
                length = db.length(),
                delta = db.delta(),
                modulo = db.stride().unwrap_or(1),
            ));
        }

        self.scope.raw(format!(
            "pub(crate) const FOLDS: [FoldRange; {}] = [\n    {}\n];",
            delta_blocks.len(),
            lines.join("\n    ")
        ));
    }
}
