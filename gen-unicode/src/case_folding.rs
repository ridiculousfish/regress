use crate::{GenUnicode, MAX_LENGTH};
use ucd_parse::CaseStatus;

type CodePoint = u32;

#[derive(Debug, Copy, Clone)]
struct FoldPair {
    orig: CodePoint,
    folded: CodePoint,
}

impl FoldPair {
    fn delta(self) -> i32 {
        (self.folded as i32) - (self.orig as i32)
    }

    fn stride_to(self, rhs: FoldPair) -> u32 {
        rhs.orig - self.orig
    }
}

struct DeltaBlock {
    /// Folds original -> folded.
    folds: Vec<FoldPair>,
}

impl DeltaBlock {
    fn create(fp: FoldPair) -> DeltaBlock {
        DeltaBlock { folds: vec![fp] }
    }

    fn stride(&self) -> Option<u32> {
        if self.folds.len() >= 2 {
            Some(self.folds[0].stride_to(self.folds[1]))
        } else {
            None
        }
    }

    fn first(&self) -> FoldPair {
        *self.folds.first().unwrap()
    }

    fn last(&self) -> FoldPair {
        *self.folds.last().unwrap()
    }

    fn length(&self) -> usize {
        (self.last().orig as usize) - (self.first().orig as usize) + 1
    }

    fn delta(&self) -> i32 {
        self.first().delta()
    }

    #[allow(clippy::if_same_then_else)]
    fn can_append(&self, fp: FoldPair) -> bool {
        if self.folds.is_empty() {
            // New block.
            true
        } else if fp.orig - self.first().orig > MAX_LENGTH {
            // Length would be too big.
            false
        } else if self.delta() != fp.delta() {
            // Different deltas in this block.
            false
        } else if let Some(stride) = self.stride() {
            // Strides must match and be power of 2.
            stride == self.last().stride_to(fp)
        } else {
            // No stride yet.
            // Stride must be power of 2.
            self.last().stride_to(fp).is_power_of_two()
        }
    }

    fn append(&mut self, fp: FoldPair) {
        std::debug_assert!(self.can_append(fp));
        self.folds.push(fp)
    }
}

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

        self.scope.raw(&format!(
            "pub(crate) const FOLDS: [FoldRange; {}] = [\n    {}\n];",
            delta_blocks.len(),
            lines.join("\n    ")
        ));
    }
}
