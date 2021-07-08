use std::fs::File;
use std::io::{self, BufRead};

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
        } else if fp.orig - self.first().orig >= 256 {
            // Length would be too big.
            false
        } else if self.delta() != fp.delta() {
            // Different deltas in this block.
            false
        } else if let Some(stride) = self.stride() {
            // Strides must match.
            stride == self.last().stride_to(fp)
        } else {
            // No stride yet.
            true
        }
    }

    fn append(&mut self, fp: FoldPair) {
        std::debug_assert!(self.can_append(fp));
        self.folds.push(fp)
    }
}

fn create_delta_blocks(fps: &[FoldPair]) -> Vec<DeltaBlock> {
    let mut blocks: Vec<DeltaBlock> = Vec::new();
    for &fp in fps {
        match blocks.last_mut() {
            Some(ref mut db) if db.can_append(fp) => db.append(fp),
            _ => blocks.push(DeltaBlock::create(fp)),
        }
    }
    blocks
}

fn format_delta_blocks(dbs: &[DeltaBlock]) -> String {
    let mut lines = Vec::new();
    for db in dbs {
        lines.push(format!(
            "FoldRange::from({start:#04X}, {length}, {delta}, {modulo}),",
            start = db.first().orig,
            length = db.length(),
            delta = db.delta(),
            modulo = db.stride().unwrap_or(1),
        ));
    }

    format!(
        r#"

pub(crate) const FOLDS: [FoldRange; {count}] = [
    {lines}
];
"#,
        count = dbs.len(),
        lines = lines.join("\n    ")
    )
}

/// Parse a CaseFolding line if it is of Common type.
/// Example line: "0051; C; 0071; # LATIN CAPITAL LETTER Q"
fn process_simple_fold(s: &str) -> Option<FoldPair> {
    // Trim trailing #s which are comments.
    if let Some(s) = s.trim().split('#').next() {
        let fields: Vec<&str> = s.split(';').map(str::trim).collect();
        if fields.len() != 4 {
            return None;
        }
        let status = fields[1];
        if status != "C" && status != "S" {
            return None;
        }
        let from_hex = |s: &str| u32::from_str_radix(s, 16).unwrap();
        let (orig, folded) = (from_hex(fields[0]), from_hex(fields[2]));
        return Some(FoldPair { orig, folded });
    }
    None
}

pub(crate) fn generate_folds() -> String {
    let file = File::open("CaseFolding.txt").expect("could not open CaseFolding.txt");
    let lines = io::BufReader::new(file).lines();

    let mut foldpairs = Vec::new();
    for line in lines {
        if let Some(s) = line.unwrap().as_str().trim().split('#').next() {
            if let Some(fp) = process_simple_fold(s) {
                foldpairs.push(fp);
            }
        }
    }
    let dblocks = create_delta_blocks(&foldpairs);

    format_delta_blocks(&dblocks).trim().to_string()
}
