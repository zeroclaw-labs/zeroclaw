use std::mem::MaybeUninit;

pub(crate) trait MaybeUninitWriteSliceExt<T> {
    fn write_slice_compat<'a>(this: &'a mut [MaybeUninit<T>], src: &[T]) -> &'a mut [T]
    where
        T: Copy;
}

#[cfg(not(has_maybe_uninit_write_slice))]
impl<T> MaybeUninitWriteSliceExt<T> for MaybeUninit<T> {
    fn write_slice_compat<'a>(this: &'a mut [MaybeUninit<T>], src: &[T]) -> &'a mut [T]
    where
        T: Copy,
    {
        // SAFETY: &[T] and &[MaybeUninit<T>] have the same layout
        let uninit_src: &[MaybeUninit<T>] = unsafe { std::mem::transmute(src) };

        this.copy_from_slice(uninit_src);

        // SAFETY: Valid elements have just been copied into `this` so it is initialized
        // SAFETY: similar to safety notes for `slice_get_ref`, but we have a
        // mutable reference which is also guaranteed to be valid for writes.
        unsafe { &mut *(this as *mut [MaybeUninit<T>] as *mut [T]) }
    }
}

#[cfg(has_maybe_uninit_write_slice)]
impl<T> MaybeUninitWriteSliceExt<T> for MaybeUninit<T> {
    fn write_slice_compat<'a>(this: &'a mut [MaybeUninit<T>], src: &[T]) -> &'a mut [T]
    where
        T: Copy,
    {
        MaybeUninit::write_slice(this, src)
    }
}

pub(crate) trait NewUninitExt<T> {
    fn new_uninit_slice_compat(len: usize) -> Box<[MaybeUninit<T>]>;

    unsafe fn assume_init_compat(self) -> Box<[T]>;
}

#[cfg(not(has_new_uninit))]
impl<T> NewUninitExt<T> for Box<[MaybeUninit<T>]> {
    fn new_uninit_slice_compat(len: usize) -> Box<[MaybeUninit<T>]> {
        use std::mem::ManuallyDrop;
        use std::slice::from_raw_parts_mut;

        let vec = Vec::<T>::with_capacity(len);
        let mut vec = ManuallyDrop::new(vec);

        unsafe {
            let slice = from_raw_parts_mut(vec.as_mut_ptr() as *mut MaybeUninit<T>, len);
            Box::from_raw(slice)
        }
    }

    unsafe fn assume_init_compat(self) -> Box<[T]> {
        unsafe { Box::from_raw(Box::into_raw(self) as *mut [T]) }
    }
}

#[cfg(has_new_uninit)]
impl<T> NewUninitExt<T> for Box<[MaybeUninit<T>]> {
    fn new_uninit_slice_compat(len: usize) -> Box<[MaybeUninit<T>]> {
        Box::<[T]>::new_uninit_slice(len)
    }

    unsafe fn assume_init_compat(self) -> Box<[T]> {
        unsafe { self.assume_init() }
    }
}

pub(crate) trait SlicePartitionDedupExt<T: PartialEq> {
    fn partition_dedup_compat(&mut self) -> (&mut [T], &mut [T]);
}

#[cfg(not(has_slice_partition_dedup))]
impl<T: PartialEq> SlicePartitionDedupExt<T> for [T] {
    fn partition_dedup_compat(&mut self) -> (&mut [T], &mut [T]) {
        let len = self.len();
        if len <= 1 {
            return (self, &mut []);
        }

        let ptr = self.as_mut_ptr();
        let mut next_read: usize = 1;
        let mut next_write: usize = 1;

        unsafe {
            while next_read < len {
                let ptr_read = ptr.add(next_read);
                let prev_ptr_write = ptr.add(next_write - 1);
                // Changed from if `!same_bucket(&mut *ptr_read, &mut *prev_ptr_write)`
                // as the original implementation is copied from `partition_dedup_by`.
                if *ptr_read != *prev_ptr_write {
                    if next_read != next_write {
                        let ptr_write = prev_ptr_write.offset(1);
                        core::ptr::swap(ptr_read, ptr_write);
                    }
                    next_write += 1;
                }
                next_read += 1;
            }
        }

        self.split_at_mut(next_write)
    }
}

#[cfg(has_slice_partition_dedup)]
impl<T: PartialEq> SlicePartitionDedupExt<T> for [T] {
    fn partition_dedup_compat(&mut self) -> (&mut [T], &mut [T]) {
        self.partition_dedup()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_partition_dedup_compat() {
        let mut slice = [1, 2, 2, 3, 3, 2, 1, 1];
        let (dedup, duplicates) = slice.partition_dedup_compat();

        assert_eq!(dedup, [1, 2, 3, 2, 1]);
        assert_eq!(duplicates, [2, 3, 1]);
    }

    #[test]
    fn test_partition_dedup_compat_drop_type() {
        use std::sync::atomic::AtomicUsize;

        #[derive(Debug)]
        struct Foo<'a> {
            i: usize,
            c: &'a AtomicUsize,
        }

        impl<'a> Foo<'a> {
            fn new(i: usize, c: &'a AtomicUsize) -> Self {
                Self { i, c }
            }
        }

        impl PartialEq for Foo<'_> {
            fn eq(&self, other: &Self) -> bool {
                self.i.eq(&other.i)
            }
        }

        impl Drop for Foo<'_> {
            fn drop(&mut self) {
                self.c.fetch_add(1, atomic::Ordering::SeqCst);
            }
        }

        let c = AtomicUsize::new(0);
        let mut slice = [1, 2, 2, 3, 3, 2, 1, 1]
            .into_iter()
            .map(|n| Foo::new(n, &c))
            .collect::<Vec<_>>();
        let (dedup, duplicates) = slice.partition_dedup_compat();

        assert_eq!(
            dedup,
            [
                Foo::new(1, &c),
                Foo::new(2, &c),
                Foo::new(3, &c),
                Foo::new(2, &c),
                Foo::new(1, &c)
            ]
        );
        assert_eq!(
            duplicates,
            [Foo::new(2, &c), Foo::new(3, &c), Foo::new(1, &c)]
        );
        // drop impl is alled for the 8 instances created in assert_eq!
        assert_eq!(8, c.load(atomic::Ordering::SeqCst));
        // drop the input data
        drop(slice);
        assert_eq!(16, c.load(atomic::Ordering::SeqCst));
    }
}
