//! What does one "pending start offset" thread actually cost in bytes,
//! versus the 1-bit intuition ("it's just a bitset of live start offsets")?

use regress::automata::nfa::StateHandle;
use regress::automata::tdfa::{InputMark, TaggedNfaState, TdfaState};
use smallvec::SmallVec;
use std::mem::{align_of, size_of};
use std::rc::Rc;

fn main() {
    println!("size_of::<InputMark>()        = {}", size_of::<InputMark>());
    println!("size_of::<Option<InputMark>>() = {}", size_of::<Option<InputMark>>());
    println!("size_of::<TaggedNfaState>()    = {}", size_of::<TaggedNfaState>());
    println!("size_of::<TdfaState>()         = {} (inline capacity: 4 threads)", size_of::<TdfaState>());

    println!("\n--- component breakdown ---");
    println!(
        "size_of::<StateHandle>() (=u32) = {}, align = {}",
        size_of::<StateHandle>(),
        align_of::<StateHandle>()
    );
    type TagMap = SmallVec<[Option<InputMark>; 4]>;
    println!(
        "size_of::<Rc<TagMap>>()         = {}, align = {}",
        size_of::<Rc<TagMap>>(),
        align_of::<Rc<TagMap>>()
    );
    println!(
        "naive sum (4 + 8)               = {} vs actual struct size = {}",
        size_of::<StateHandle>() + size_of::<Rc<TagMap>>(),
        size_of::<TaggedNfaState>()
    );
    println!("align_of::<TaggedNfaState>()    = {}", align_of::<TaggedNfaState>());
    // A HashMap<TdfaState, TdfaStateId> entry's marginal cost once a
    // TdfaState's thread SmallVec has spilled to the heap (> 4 threads):
    // heap buffer of `threads * size_of::<TaggedNfaState>()`, retained for
    // the lifetime of the whole build (it's a live HashMap key).
}
