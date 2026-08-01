//! Would a `NonZeroU32`-backed `InputMark` (niche-optimizing `Option<InputMark>`
//! from 8 -> 4 bytes) actually shrink `TaggedNfaState`, once `SmallVec`'s own
//! layout/padding is accounted for? Reconstructs the real struct shapes using
//! the same `smallvec` crate regress depends on, rather than guessing.

use smallvec::SmallVec;
use std::mem::size_of;
use std::num::NonZeroU32;

#[derive(Copy, Clone)]
struct MarkPlain(u32);
#[derive(Copy, Clone)]
struct MarkNiche(NonZeroU32);

struct TaggedPlain {
    state: u32,
    tag_map: SmallVec<[Option<MarkPlain>; 4]>,
}
struct TaggedNiche {
    state: u32,
    tag_map: SmallVec<[Option<MarkNiche>; 4]>,
}

fn main() {
    println!("size_of::<Option<MarkPlain>>() = {}", size_of::<Option<MarkPlain>>());
    println!("size_of::<Option<MarkNiche>>() = {}", size_of::<Option<MarkNiche>>());
    println!(
        "size_of::<SmallVec<[Option<MarkPlain>;4]>>() = {}",
        size_of::<SmallVec<[Option<MarkPlain>; 4]>>()
    );
    println!(
        "size_of::<SmallVec<[Option<MarkNiche>;4]>>() = {}",
        size_of::<SmallVec<[Option<MarkNiche>; 4]>>()
    );
    println!("size_of::<TaggedPlain>() (current shape) = {}", size_of::<TaggedPlain>());
    println!("size_of::<TaggedNiche>() (NonZeroU32 shape) = {}", size_of::<TaggedNiche>());

    // Real regress type, for a direct before/after comparison against the
    // library's actual current (non-niche) TaggedNfaState.
    println!(
        "\nreal regress::automata::tdfa::TaggedNfaState = {}",
        size_of::<regress::automata::tdfa::TaggedNfaState>()
    );
}
