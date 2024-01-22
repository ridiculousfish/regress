use crate::{DeltaBlock, FoldPair, GenUnicode};

impl GenUnicode {
    pub(crate) fn generate_uppercase(&mut self) {
        let mut fold_pairs = Vec::new();

        for data in &self.data {
            let Some(uppercase) = data.simple_uppercase_mapping else {
                continue;
            };
            fold_pairs.push(FoldPair {
                orig: data.codepoint.value(),
                folded: uppercase.value(),
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

        self.scope.raw(&format!(
            "pub(crate) const TO_UPPERCASE: [FoldRange; {}] = [\n    {}\n];",
            delta_blocks.len(),
            lines.join("\n    ")
        ));
    }
}
