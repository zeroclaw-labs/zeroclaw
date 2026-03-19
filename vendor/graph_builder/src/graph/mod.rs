pub mod adj_list;
pub mod csr;

/// Represents the target of an edge and its associated value.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Target<NI, EV> {
    pub target: NI,
    pub value: EV,
}

impl<T: Ord, V> Ord for Target<T, V> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.target.cmp(&other.target)
    }
}

impl<T: PartialOrd, V> PartialOrd for Target<T, V> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.target.partial_cmp(&other.target)
    }
}

impl<T: PartialEq, V> PartialEq for Target<T, V> {
    fn eq(&self, other: &Self) -> bool {
        self.target.eq(&other.target)
    }
}

impl<T: Eq, V> Eq for Target<T, V> {}

impl<T, EV> Target<T, EV> {
    pub fn new(target: T, value: EV) -> Self {
        Self { target, value }
    }
}
