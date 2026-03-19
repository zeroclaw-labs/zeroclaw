pub mod binary;
#[cfg(feature = "dotgraph")]
#[cfg_attr(all(feature = "dotgraph", has_doc_cfg), doc(cfg(feature = "dotgraph")))]
pub mod dotgraph;
pub mod edgelist;
#[cfg(feature = "gdl")]
#[cfg_attr(all(feature = "gdl", has_doc_cfg), doc(cfg(feature = "gdl")))]
pub mod gdl;
pub mod graph500;

pub use binary::BinaryInput;
#[cfg(feature = "dotgraph")]
pub use dotgraph::DotGraph;
#[cfg(feature = "dotgraph")]
pub use dotgraph::DotGraphInput;
pub use edgelist::EdgeList;
pub use edgelist::EdgeListInput;
pub use edgelist::Edges;
pub use graph500::Graph500;
pub use graph500::Graph500Input;

use crate::index::Idx;

pub struct InputPath<P>(pub(crate) P);

pub trait InputCapabilities<NI: Idx> {
    type GraphInput;
}

#[derive(Debug, Clone, Copy)]
pub enum Direction {
    Outgoing,
    Incoming,
    Undirected,
}

/// Used by input formats to read node or edge values from bytes.
pub trait ParseValue: Default + Sized {
    /// Parses a value from a slice.
    ///
    /// # Example
    ///
    /// ```
    /// use graph_builder::input::ParseValue;
    ///
    /// let bytes = "13.37".as_bytes();
    ///
    /// let (number, len) = f32::parse(bytes);
    ///
    /// assert_eq!(number, 13.37);
    /// assert_eq!(len, 5);
    /// ```
    ///
    /// # Return
    ///
    /// Returns a tuple containing two entries. The first is the parsed value,
    /// the second is the index of the byte right after the parsed value.
    fn parse(bytes: &[u8]) -> (Self, usize);
}

impl ParseValue for () {
    fn parse(_bytes: &[u8]) -> (Self, usize) {
        ((), 0)
    }
}

macro_rules! impl_parse_value {
    ($atoi:path, $($ty:ty),+ $(,)?) => {
        $(
            impl $crate::input::ParseValue for $ty {
                fn parse(bytes: &[u8]) -> (Self, usize) {
                    if bytes.len() == 0 {
                        (<$ty as ::std::default::Default>::default(), 0)
                    } else {
                        $atoi(bytes)
                    }
                }
            }
        )+
    };
}

impl_parse_value!(
    ::atoi::FromRadix10::from_radix_10,
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
);

impl_parse_value!(
    ::atoi::FromRadix10Signed::from_radix_10_signed,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
);

impl_parse_value!(parse_float, f32, f64);

fn parse_float<T: fast_float2::FastFloat>(bytes: &[u8]) -> (T, usize) {
    fast_float2::parse_partial(bytes).unwrap()
}
