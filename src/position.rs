use std::cmp::Eq;
use std::marker::PhantomData;
use std::ops;

/// A trait which references a position in an input string.
/// The intent is that this may be satisfied via indexes or pointers.
/// Positions must be subtractable, producing usize; they also obey other "pointer arithmetic" ideas.
pub trait PositionType: std::fmt::Debug + Copy + Clone + PartialEq + Eq + PartialOrd + Ord
where
    Self: ops::Add<usize, Output = Self>,
    Self: ops::Sub<usize, Output = Self>,
    Self: ops::Sub<Self, Output = usize>,
    Self: ops::AddAssign<usize>,
    Self: ops::SubAssign<usize>,
{
}

/// Choose the preferred position type with this alias.
#[cfg(feature = "index-positions")]
pub type DefPosition<'a> = IndexPosition<'a>;

#[cfg(not(feature = "index-positions"))]
pub type DefPosition<'a> = RefPosition<'a>;

/// A simple index-based position.
/// It remembers the lifetime of the slice it is tied to.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct IndexPosition<'a>(usize, PhantomData<&'a ()>);

#[allow(dead_code)]
impl<'a> IndexPosition<'a> {
    /// IndexPosition does not enforce its size.
    pub fn check_size() {}

    pub fn new(pos: usize) -> Self {
        Self(pos, PhantomData)
    }

    pub fn offset(self) -> usize {
        self.0
    }
}

impl ops::Add<usize> for IndexPosition<'_> {
    type Output = Self;
    fn add(self, rhs: usize) -> Self::Output {
        debug_assert!(self.0 + rhs >= self.0, "Overflow");
        IndexPosition(self.0 + rhs, PhantomData)
    }
}

impl ops::AddAssign<usize> for IndexPosition<'_> {
    fn add_assign(&mut self, rhs: usize) {
        *self = *self + rhs;
    }
}

impl ops::SubAssign<usize> for IndexPosition<'_> {
    fn sub_assign(&mut self, rhs: usize) {
        *self = *self - rhs;
    }
}

impl<'a> ops::Sub<IndexPosition<'a>> for IndexPosition<'a> {
    type Output = usize;
    fn sub(self, rhs: Self) -> Self::Output {
        debug_assert!(self.0 >= rhs.0, "Underflow");
        self.0 - rhs.0
    }
}

impl ops::Sub<usize> for IndexPosition<'_> {
    type Output = Self;
    fn sub(self, rhs: usize) -> Self::Output {
        debug_assert!(self.0 >= rhs, "Underflow");
        IndexPosition(self.0 - rhs, PhantomData)
    }
}

impl PositionType for IndexPosition<'_> {}

/// A reference position holds a reference to a byte and uses pointer arithmetic.
/// This must use raw pointers because it must be capable of representing the one-past-the-end value.
/// TODO: thread lifetimes through this.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RefPosition<'a>(std::ptr::NonNull<u8>, PhantomData<&'a ()>);

#[allow(dead_code)]
impl RefPosition<'_> {
    /// The big idea of RefPosition is that Option<RefPosition> becomes pointer-sized, by using nullptr as the None value.
    /// Good candidate for const-panics when stabilized.
    pub fn check_size() {
        if std::mem::size_of::<Option<Self>>() > std::mem::size_of::<*const u8>() {
            panic!("Option<RefPosition> should be pointer sized")
        }
    }

    /// Access the underlying pointer.
    #[inline(always)]
    pub fn ptr(self) -> *const u8 {
        self.0.as_ptr()
    }

    /// Construct from a pointer, which must never be null.
    #[inline(always)]
    pub fn new(ptr: *const u8) -> Self {
        debug_assert!(!ptr.is_null(), "Pointer cannot be null");
        // Annoyingly there's no *const NonNull.
        let mutp = ptr as *mut u8;
        let nonnullp = if cfg!(feature = "prohibit-unsafe") {
            std::ptr::NonNull::new(mutp).expect("Pointer was null")
        } else {
            unsafe { std::ptr::NonNull::new_unchecked(mutp) }
        };
        Self(nonnullp, PhantomData)
    }
}

impl PositionType for RefPosition<'_> {}

impl ops::Add<usize> for RefPosition<'_> {
    type Output = Self;
    fn add(self, rhs: usize) -> Self::Output {
        Self::new(unsafe { self.ptr().add(rhs) })
    }
}

impl<'a> ops::Sub<RefPosition<'a>> for RefPosition<'a> {
    type Output = usize;
    fn sub(self, rhs: Self) -> Self::Output {
        debug_assert!(self.0 >= rhs.0, "Underflow");
        // Note Rust has backwards naming here. The "origin" is self, not the param; the rhs is the offset value.
        unsafe { self.ptr().offset_from(rhs.ptr()) as usize }
    }
}

impl ops::Sub<usize> for RefPosition<'_> {
    type Output = Self;
    fn sub(self, rhs: usize) -> Self::Output {
        debug_assert!(self.ptr() as usize >= rhs, "Underflow");
        Self::new(unsafe { self.ptr().sub(rhs) })
    }
}

impl ops::AddAssign<usize> for RefPosition<'_> {
    fn add_assign(&mut self, rhs: usize) {
        *self = *self + rhs;
    }
}

impl ops::SubAssign<usize> for RefPosition<'_> {
    fn sub_assign(&mut self, rhs: usize) {
        *self = *self - rhs;
    }
}
