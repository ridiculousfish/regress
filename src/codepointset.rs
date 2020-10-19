use crate::util::SliceHelp;
use std::cmp::Ordering;
use std::iter::once;

pub type CodePoint = u32;

/// The maximum (inclusive) code point.
pub const CODE_POINT_MAX: CodePoint = 0x10FFFF;

/// An list of sorted, inclusive, non-empty ranges of code points.
/// This is more efficient than InclusiveRange because it does not need to carry
/// around the Option<bool>.
#[derive(Debug, Copy, Clone)]
pub struct Interval {
    pub first: CodePoint,
    pub last: CodePoint,
}

impl Interval {
    /// Return whether self is before rhs.
    fn is_before(self, other: Interval) -> bool {
        self.last < other.first
    }

    /// Return whether self is strictly before rhs.
    /// "Strictly" here means there is at least one value after the end of self,
    /// and before the start of rhs. Overlapping *or abutting* intervals are
    /// not considered strictly before.
    fn is_strictly_before(self, rhs: Interval) -> bool {
        self.last + 1 < rhs.first
    }

    /// Compare two intervals.
    /// Overlapping *or abutting* intervals are considered equal.
    fn mergecmp(self, rhs: Interval) -> Ordering {
        if self.is_strictly_before(rhs) {
            Ordering::Less
        } else if rhs.is_strictly_before(self) {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }

    /// Return whether self is mergeable with rhs.
    fn mergeable(self, rhs: Interval) -> bool {
        self.mergecmp(rhs) == Ordering::Equal
    }

    /// Return whether self contains a code point \p cp.
    pub fn contains(self, cp: CodePoint) -> bool {
        self.first <= cp && cp <= self.last
    }

    /// Return whether self overlaps 'other'.
    /// Overlaps means that we share at least one code point with 'other'.
    pub fn overlaps(self, other: Interval) -> bool {
        !self.is_before(other) && !other.is_before(self)
    }

    /// Return the interval of codepoints.
    pub fn codepoints(self) -> std::ops::Range<u32> {
        debug_assert!(self.last + 1 > self.last, "Overflow");
        self.first..(self.last + 1)
    }

    /// Return the number of contained code points.
    pub fn count_codepoints(self) -> usize {
        (self.last - self.first + 1) as usize
    }
}

/// Merge two intervals, which must be overlapping or abutting.
fn merge_intervals(x: Interval, y: &Interval) -> Interval {
    debug_assert!(x.mergeable(*y), "Ranges not mergeable");
    Interval {
        first: std::cmp::min(x.first, y.first),
        last: std::cmp::max(x.last, y.last),
    }
}

#[derive(Clone, Debug, Default)]
pub struct CodePointSet {
    ivs: Vec<Interval>,
}

/// A set of code points stored via as disjoint, non-abutting, sorted intervals.
impl CodePointSet {
    pub fn new() -> CodePointSet {
        CodePointSet { ivs: Vec::new() }
    }

    fn assert_is_well_formed(&self) {
        if cfg!(debug_assertions) {
            for iv in &self.ivs {
                debug_assert!(iv.last <= CODE_POINT_MAX);
                debug_assert!(iv.first <= iv.last);
            }
            for w in self.ivs.windows(2) {
                debug_assert!(w[0].is_strictly_before(w[1]));
            }
        }
    }

    /// Construct from sorted, disjoint intervals. Note these are not allowed to
    /// even abut.
    pub fn from_sorted_disjoint_intervals(ivs: Vec<Interval>) -> CodePointSet {
        let res = CodePointSet { ivs };
        res.assert_is_well_formed();
        res
    }

    /// Add an interval of code points to the set.
    pub fn add(&mut self, new_iv: Interval) {
        // Find the mergeable subarray, that is, the range of intervals that intersect
        // or abut new_iv.
        let mergeable = self.ivs.equal_range_by(|iv| iv.mergecmp(new_iv));

        // Check our work.
        if cfg!(debug_assertions) {
            for (idx, iv) in self.ivs.iter().enumerate() {
                if idx < mergeable.start {
                    debug_assert!(iv.is_strictly_before(new_iv));
                } else if idx >= mergeable.end {
                    debug_assert!(new_iv.is_strictly_before(*iv));
                } else {
                    debug_assert!(iv.mergeable(new_iv) && new_iv.mergeable(*iv));
                }
            }
        }

        // Merge all the overlapping intervals (possibly none), and then replace the
        // range.
        let merged_iv = self.ivs[mergeable.clone()]
            .iter()
            .fold(new_iv, merge_intervals);
        self.ivs.splice(mergeable, once(merged_iv));
        self.assert_is_well_formed();
    }

    /// Add a single code point to the set.
    pub fn add_one(&mut self, cp: CodePoint) {
        self.add(Interval {
            first: cp,
            last: cp,
        })
    }

    /// Add another code point set.
    pub fn add_set(&mut self, mut rhs: CodePointSet) {
        // Prefer to add to the set with more intervals.
        if self.ivs.len() < rhs.ivs.len() {
            std::mem::swap(self, &mut rhs);
        }
        for iv in rhs.intervals() {
            self.add(*iv)
        }
    }

    /// \return the intervals
    pub fn intervals(&self) -> &[Interval] {
        self.ivs.as_slice()
    }

    /// \return the number of intervals that would be produced by inverting.
    pub fn inverted_interval_count(&self) -> usize {
        let mut result = 0;
        let mut start: CodePoint = 0;
        for iv in &self.ivs {
            if start < iv.first {
                result += 1;
            }
            start = iv.last + 1;
        }
        if start <= CODE_POINT_MAX {
            result += 1;
        }
        result
    }

    /// \return an inverted set: a set containing every code point NOT in the
    /// receiver.
    pub fn inverted(&self) -> CodePointSet {
        // The intervals we collect.
        let mut inverted_ivs = Vec::new();

        // The first code point *not* in the previous interval.
        let mut start: CodePoint = 0;
        for iv in &self.ivs {
            if start < iv.first {
                inverted_ivs.push(Interval {
                    first: start,
                    last: iv.first - 1,
                })
            }
            start = iv.last + 1;
        }
        if start <= CODE_POINT_MAX {
            inverted_ivs.push(Interval {
                first: start,
                last: CODE_POINT_MAX,
            })
        }
        CodePointSet::from_sorted_disjoint_intervals(inverted_ivs)
    }
}
