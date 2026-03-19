use std::fmt::Debug;
use std::iter::Sum;
use std::ops::{Range, RangeInclusive};
use std::sync::atomic::Ordering;

use atoi::FromRadix10;
use atomic::Atomic;

pub trait Idx:
    Copy
    + std::ops::Add<Output = Self>
    + std::ops::AddAssign
    + std::ops::Sub<Output = Self>
    + std::ops::Div<Output = Self>
    + std::ops::Mul<Output = Self>
    + Ord
    + Debug
    + Send
    + Sum
    + Sync
    + Sized
    + 'static
{
    fn new(idx: usize) -> Self;

    fn zero() -> Self;

    fn index(self) -> usize;

    type RangeIter: Iterator<Item = Self>;

    fn range(self, end: Self) -> Self::RangeIter;

    type RangeInclusiveIter: Iterator<Item = Self>;

    fn range_inclusive(self, end: Self) -> Self::RangeInclusiveIter;

    fn parse(bytes: &[u8]) -> (Self, usize);

    fn get_and_increment(this: &Atomic<Self>, order: Ordering) -> Self {
        Self::fetch_add(this, Self::new(1), order)
    }

    fn fetch_add(this: &Atomic<Self>, val: Self, order: Ordering) -> Self;
}

macro_rules! impl_idx {
    ($TYPE:ty) => {
        impl Idx for $TYPE {
            #[inline]
            fn new(idx: usize) -> Self {
                assert!(idx <= <$TYPE>::MAX as usize);
                idx as $TYPE
            }

            #[inline]
            fn zero() -> Self {
                0
            }

            #[inline]
            fn index(self) -> usize {
                self as usize
            }

            type RangeIter = Range<Self>;

            #[inline]
            fn range(self, end: Self) -> Self::RangeIter {
                self..end
            }

            type RangeInclusiveIter = RangeInclusive<Self>;

            #[inline]
            fn range_inclusive(self, end: Self) -> Self::RangeInclusiveIter {
                self..=end
            }

            #[inline]
            fn parse(bytes: &[u8]) -> (Self, usize) {
                FromRadix10::from_radix_10(bytes)
            }

            #[inline]
            fn fetch_add(this: &Atomic<$TYPE>, val: $TYPE, order: Ordering) -> $TYPE {
                this.fetch_add(val, order)
            }
        }
    };
}

impl_idx!(u8);
impl_idx!(u16);
impl_idx!(u32);
impl_idx!(u64);
impl_idx!(usize);

impl_idx!(i8);
impl_idx!(i16);
impl_idx!(i32);
impl_idx!(i64);
impl_idx!(isize);
