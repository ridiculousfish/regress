//! Regex compiler back-end: transforms IR into a CompiledRegex

use crate::bytesearch::{AsciiBitmap, ByteArraySet};
use crate::insn::{CompiledRegex, Insn, LoopFields, MAX_BYTE_SEQ_LENGTH, MAX_CHAR_SET_LENGTH};
use crate::ir;
use crate::ir::Node;
use crate::startpredicate;
use crate::types::{BracketContents, CaptureGroupID, LoopID};
use crate::unicode;
use core::convert::TryInto;
#[cfg(not(feature = "std"))]
use {alloc::vec::Vec, hashbrown::HashMap};

/// \return an anchor instruction for a given IR anchor.
fn make_anchor(anchor_type: ir::AnchorType) -> Insn {
    match anchor_type {
        ir::AnchorType::StartOfLine => Insn::StartOfLine,
        ir::AnchorType::EndOfLine => Insn::EndOfLine,
    }
}

/// Weirdly placed optimization.
/// If the given bracket can be represented as ASCII contents, return the
/// bitmap. Otherwise nothing.
fn bracket_as_ascii(bc: &BracketContents) -> Option<AsciiBitmap> {
    let mut result = AsciiBitmap::default();
    // We just assume that inverted brackets contain non-ASCII characters.
    if bc.invert {
        return None;
    }
    for r in bc.cps.intervals() {
        debug_assert!(r.first <= r.last);
        if r.last >= 128 {
            return None;
        }
        for bit in r.first..=r.last {
            result.set(bit as u8)
        }
    }
    Some(result)
}

/// Type which wraps up the context needed to emit a CompiledRegex.
struct Emitter {
    result: CompiledRegex,

    // Number of loops seen so far.
    next_loop_id: LoopID,

    // List of group names, in order, with empties for unnamed groups.
    group_names: Vec<Box<str>>,
}

impl Emitter {
    /// Emit a ByteSet instruction.
    /// We awkwardly optimize it like so.
    fn make_byte_set_insn(&self, bytes: &[u8]) -> Insn {
        match bytes.len() {
            0 => Insn::JustFail,
            1 => Insn::ByteSeq1(bytes.try_into().unwrap()),
            2 => Insn::ByteSet2(ByteArraySet(bytes.try_into().unwrap())),
            3 => Insn::ByteSet3(ByteArraySet(bytes.try_into().unwrap())),
            4 => Insn::ByteSet4(ByteArraySet(bytes.try_into().unwrap())),
            _ => panic!("Byte set is too long"),
        }
    }

    /// Emit an instruction.
    /// Return the "instruction" as an index.
    fn emit_insn(&mut self, insn: Insn) {
        self.result.insns.push(insn);
    }

    /// Get an instruction at a given index.
    fn get_insn(&mut self, idx: u32) -> &mut Insn {
        &mut self.result.insns[idx as usize]
    }

    /// \return the offset of the next instruction emitted.
    fn next_offset(&self) -> u32 {
        self.result.insns.len() as u32
    }

    fn emit_insn_offset(&mut self, insn: Insn) -> u32 {
        let ret = self.next_offset();
        self.emit_insn(insn);
        ret
    }

    /// Emit instructions corresponding to a given node.
    fn emit_node(&mut self, node: &Node) {
        enum Emitter<'a> {
            Node(&'a Node),
            NodeLoopFinish {
                loop_instruction_index: u32,
            },
            NodeLookaroundAssertionFinish {
                lookaround_instruction_index: u32,
            },
            NodeAltMiddle {
                alt_instruction_index: u32,
                right_node: &'a Node,
            },
            NodeAltFinish {
                alt_instruction_index: u32,
                jump_instruction_index: u32,
                right_branch_index: u32,
            },
            EndCaptureGroup {
                group: CaptureGroupID,
            },
        }

        let mut stack = vec![Emitter::Node(node)];

        while let Some(inst) = stack.pop() {
            match inst {
                Emitter::NodeLoopFinish {
                    loop_instruction_index,
                } => {
                    self.emit_insn(Insn::LoopAgain {
                        begin: loop_instruction_index,
                    });
                    // Fix up our loop exit.
                    let exit = self.next_offset();
                    match self.get_insn(loop_instruction_index) {
                        Insn::EnterLoop(fields) => fields.exit = exit,
                        _ => panic!("Should be an EnterLoop instruction"),
                    }
                }
                Emitter::NodeLookaroundAssertionFinish {
                    lookaround_instruction_index,
                } => {
                    self.emit_insn(Insn::Goal);
                    // Fix up the continuation.
                    let next_insn = self.next_offset();
                    match self.get_insn(lookaround_instruction_index) {
                        Insn::Lookbehind {
                            ref mut continuation,
                            ..
                        } => *continuation = next_insn,
                        Insn::Lookahead {
                            ref mut continuation,
                            ..
                        } => *continuation = next_insn,
                        _ => panic!("Should be a Lookaround instruction"),
                    }
                }
                Emitter::NodeAltMiddle {
                    alt_instruction_index,
                    right_node,
                } => {
                    let jump_insn = self.emit_insn_offset(Insn::Jump { target: 0 });
                    let right_branch = self.next_offset();
                    stack.push(Emitter::NodeAltFinish {
                        alt_instruction_index,
                        jump_instruction_index: jump_insn,
                        right_branch_index: right_branch,
                    });
                    stack.push(Emitter::Node(right_node));
                }
                Emitter::NodeAltFinish {
                    alt_instruction_index,
                    jump_instruction_index,
                    right_branch_index,
                } => {
                    let exit = self.next_offset();
                    // Fix up our jump targets.
                    match self.get_insn(alt_instruction_index) {
                        Insn::Alt { secondary } => *secondary = right_branch_index,
                        _ => panic!("Should be an Alt instruction"),
                    }
                    match self.get_insn(jump_instruction_index) {
                        Insn::Jump { target } => *target = exit,
                        _ => panic!("Should be a Jump instruction"),
                    }
                }
                Emitter::EndCaptureGroup { group } => self.emit_insn(Insn::EndCaptureGroup(group)),
                Emitter::Node(node) => match node {
                    Node::Empty => {}
                    Node::Goal => self.emit_insn(Insn::Goal),
                    Node::Char { c, icase } => {
                        let c = *c;
                        if !*icase {
                            self.emit_insn(Insn::Char(c))
                        } else {
                            core::debug_assert!(
                                unicode::fold_code_point(c, self.result.flags.unicode) == c,
                                "Char {:x} should be folded",
                                c
                            );
                            self.emit_insn(Insn::CharICase(c))
                        }
                    }
                    Node::Cat(children) => {
                        for nn in children.iter().rev() {
                            stack.push(Emitter::Node(nn));
                        }
                    }
                    Node::Alt(left, right) => {
                        // Alternation is followed by the primary branch and has a jump to secondary
                        // branch. After primary branch, jump to the continuation.
                        let alt_insn = self.emit_insn_offset(Insn::Alt { secondary: 0 });
                        stack.push(Emitter::NodeAltMiddle {
                            alt_instruction_index: alt_insn,
                            right_node: right,
                        });
                        stack.push(Emitter::Node(left));
                    }
                    Node::Bracket(contents) => {
                        if let Some(ascii_contents) = bracket_as_ascii(contents) {
                            self.emit_insn(Insn::AsciiBracket(ascii_contents))
                        } else {
                            let idx = self.result.brackets.len();
                            self.result.brackets.push(contents.clone());
                            self.emit_insn(Insn::Bracket(idx))
                        }
                    }
                    Node::MatchAny => self.emit_insn(Insn::MatchAny),
                    Node::MatchAnyExceptLineTerminator => {
                        self.emit_insn(Insn::MatchAnyExceptLineTerminator)
                    }
                    Node::Anchor(anchor_type) => self.emit_insn(make_anchor(*anchor_type)),
                    Node::Loop {
                        loopee,
                        quant,
                        enclosed_groups,
                    } => {
                        let loop_id = self.next_loop_id;
                        self.next_loop_id += 1;
                        let loop_insn = self.emit_insn_offset(Insn::EnterLoop(LoopFields {
                            loop_id,
                            min_iters: quant.min,
                            max_iters: quant.max,
                            greedy: quant.greedy,
                            exit: 0,
                        }));
                        self.result.loops += 1;
                        // Emit a sequence of ResetCaptureGroup for any contained groups.
                        for gid in enclosed_groups.start..enclosed_groups.end {
                            self.emit_insn(Insn::ResetCaptureGroup(gid))
                        }
                        stack.push(Emitter::NodeLoopFinish {
                            loop_instruction_index: loop_insn,
                        });
                        stack.push(Emitter::Node(loopee));
                    }
                    Node::Loop1CharBody { loopee, quant } => {
                        self.emit_insn(Insn::Loop1CharBody {
                            min_iters: quant.min,
                            max_iters: quant.max,
                            greedy: quant.greedy,
                        });
                        stack.push(Emitter::Node(loopee));
                    }
                    Node::CaptureGroup(contents, group) => {
                        let group = *group as CaptureGroupID;
                        self.result.groups += 1;
                        self.group_names.push("".into());
                        self.emit_insn(Insn::BeginCaptureGroup(group));
                        stack.push(Emitter::EndCaptureGroup { group });
                        stack.push(Emitter::Node(contents));
                    }
                    Node::NamedCaptureGroup(contents, group, name) => {
                        let group = *group as CaptureGroupID;
                        self.result.groups += 1;
                        self.group_names.push(name.as_str().into());
                        self.emit_insn(Insn::BeginCaptureGroup(group));
                        stack.push(Emitter::EndCaptureGroup { group });
                        stack.push(Emitter::Node(contents));
                    }
                    Node::LookaroundAssertion {
                        negate,
                        backwards,
                        start_group,
                        end_group,
                        contents,
                    } => {
                        let lookaround = if *backwards {
                            self.emit_insn_offset(Insn::Lookbehind {
                                negate: *negate,
                                start_group: *start_group,
                                end_group: *end_group,
                                continuation: 0,
                            })
                        } else {
                            self.emit_insn_offset(Insn::Lookahead {
                                negate: *negate,
                                start_group: *start_group,
                                end_group: *end_group,
                                continuation: 0,
                            })
                        };
                        stack.push(Emitter::NodeLookaroundAssertionFinish {
                            lookaround_instruction_index: lookaround,
                        });
                        stack.push(Emitter::Node(contents));
                    }
                    Node::WordBoundary { invert } => {
                        self.emit_insn(Insn::WordBoundary { invert: *invert })
                    }
                    &Node::BackRef(group) => {
                        debug_assert!(group >= 1, "Group should not be zero");
                        // -1 because \1 matches the first capture group, which has index 0.
                        self.emit_insn(Insn::BackRef(group - 1))
                    }

                    Node::ByteSet(bytes) => self.emit_insn(self.make_byte_set_insn(bytes)),

                    Node::CharSet(chars) => {
                        debug_assert!(chars.len() <= MAX_CHAR_SET_LENGTH);
                        if chars.is_empty() {
                            self.emit_insn(Insn::JustFail);
                        } else {
                            let mut arr = [chars[0]; MAX_CHAR_SET_LENGTH];
                            arr[..chars.len()].copy_from_slice(chars.as_slice());
                            self.emit_insn(Insn::CharSet(arr))
                        }
                    }

                    #[allow(clippy::assertions_on_constants)]
                    Node::ByteSequence(bytes) => {
                        assert!(
                            MAX_BYTE_SEQ_LENGTH == 16,
                            "Need to update our emitting logic"
                        );
                        for chunk in bytes.as_slice().chunks(MAX_BYTE_SEQ_LENGTH) {
                            let insn = match chunk.len() {
                                1 => Insn::ByteSeq1(chunk.try_into().unwrap()),
                                2 => Insn::ByteSeq2(chunk.try_into().unwrap()),
                                3 => Insn::ByteSeq3(chunk.try_into().unwrap()),
                                4 => Insn::ByteSeq4(chunk.try_into().unwrap()),
                                5 => Insn::ByteSeq5(chunk.try_into().unwrap()),
                                6 => Insn::ByteSeq6(chunk.try_into().unwrap()),
                                7 => Insn::ByteSeq7(chunk.try_into().unwrap()),
                                8 => Insn::ByteSeq8(chunk.try_into().unwrap()),
                                9 => Insn::ByteSeq9(chunk.try_into().unwrap()),
                                10 => Insn::ByteSeq10(chunk.try_into().unwrap()),
                                11 => Insn::ByteSeq11(chunk.try_into().unwrap()),
                                12 => Insn::ByteSeq12(chunk.try_into().unwrap()),
                                13 => Insn::ByteSeq13(chunk.try_into().unwrap()),
                                14 => Insn::ByteSeq14(chunk.try_into().unwrap()),
                                15 => Insn::ByteSeq15(chunk.try_into().unwrap()),
                                16 => Insn::ByteSeq16(chunk.try_into().unwrap()),
                                _ => panic!("Unexpected chunk size"),
                            };
                            self.emit_insn(insn)
                        }
                    }
                },
            }
        }
    }
}

/// Compile the given IR to a CompiledRegex.
pub fn emit(n: &ir::Regex) -> CompiledRegex {
    let mut emitter = Emitter {
        next_loop_id: 0,
        group_names: Vec::new(),
        result: CompiledRegex {
            insns: Vec::new(),
            brackets: Vec::new(),
            loops: 0,
            groups: 0,
            group_names: Box::new([]),
            flags: n.flags,
            start_pred: startpredicate::predicate_for_re(n),
        },
    };
    emitter.emit_node(&n.node);
    let mut result = emitter.result;
    // Populate group names, unless all are empty.
    debug_assert!(
        result.group_names.is_empty(),
        "Group names should not be set"
    );
    if emitter.group_names.iter().any(|s| !s.is_empty()) {
        result.group_names = emitter.group_names.into_boxed_slice();
    }
    debug_assert!(
        result.group_names.is_empty() || result.group_names.len() == result.groups as usize
    );
    result
}
