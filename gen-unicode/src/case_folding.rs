use crate::{DeltaBlock, FoldPair, GenUnicode};
use ucd_parse::CaseStatus;

impl GenUnicode {
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
