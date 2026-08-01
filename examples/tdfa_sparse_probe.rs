//! Given tag_map slots run ~50% populated (measured via REGRESS_TDFA_MEM_TRACE
//! on real patterns), does a sparse (tag, mark) association list beat the
//! current dense `SmallVec<[Option<InputMark>; 4]>` for TaggedNfaState?
//! Empirical sizes using the real smallvec crate, several TagIdx widths.

use smallvec::SmallVec;
use std::mem::size_of;

#[derive(Copy, Clone)]
struct Mark(u32);

// Current shape: dense, indexed by tag, num_tags logical slots (fits inline
// up to 4 tags before spilling to heap).
struct TaggedDense {
    state: u32,
    tag_map: SmallVec<[Option<Mark>; 4]>,
}

// Sparse alternative: only populated (tag, mark) pairs, various TagIdx widths
// and inline capacities.
struct TaggedSparseU32K2 {
    state: u32,
    tag_map: SmallVec<[(u32, Mark); 2]>,
}
struct TaggedSparseU16K2 {
    state: u32,
    tag_map: SmallVec<[(u16, Mark); 2]>,
}
struct TaggedSparseU8K2 {
    state: u32,
    tag_map: SmallVec<[(u8, Mark); 2]>,
}
struct TaggedSparseU8K4 {
    state: u32,
    tag_map: SmallVec<[(u8, Mark); 4]>,
}

fn main() {
    println!("current: TaggedDense (real shape)      = {} bytes", size_of::<TaggedDense>());
    println!(
        "sparse:  TaggedSparseU32K2 ((u32,Mark) x2) = {} bytes",
        size_of::<TaggedSparseU32K2>()
    );
    println!(
        "sparse:  TaggedSparseU16K2 ((u16,Mark) x2) = {} bytes",
        size_of::<TaggedSparseU16K2>()
    );
    println!(
        "sparse:  TaggedSparseU8K2  ((u8,Mark)  x2) = {} bytes",
        size_of::<TaggedSparseU8K2>()
    );
    println!(
        "sparse:  TaggedSparseU8K4  ((u8,Mark)  x4) = {} bytes",
        size_of::<TaggedSparseU8K4>()
    );
    println!("size_of::<(u32,Mark)>() = {}", size_of::<(u32, Mark)>());
    println!("size_of::<(u16,Mark)>() = {}", size_of::<(u16, Mark)>());
    println!("size_of::<(u8,Mark)>()  = {}", size_of::<(u8, Mark)>());
}
