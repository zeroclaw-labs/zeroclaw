use std::{
    convert::TryFrom,
    marker::PhantomData,
    path::{Path, PathBuf},
};

use byte_slice_cast::ToByteSlice;

use crate::{index::Idx, Error};

use super::{InputCapabilities, InputPath};

/// Reads a graph that has been written via
/// [`crate::graph_ops::SerializeGraphOp`].
pub struct BinaryInput<NI: Idx + ToByteSlice> {
    _idx: PhantomData<NI>,
}

impl<NI: Idx + ToByteSlice> Default for BinaryInput<NI> {
    fn default() -> Self {
        Self { _idx: PhantomData }
    }
}

impl<NI: Idx + ToByteSlice> InputCapabilities<NI> for BinaryInput<NI> {
    type GraphInput = PathBuf;
}

impl<P> TryFrom<InputPath<P>> for PathBuf
where
    P: AsRef<Path>,
{
    type Error = Error;

    fn try_from(path: InputPath<P>) -> Result<Self, Self::Error> {
        Ok(PathBuf::from(path.0.as_ref()))
    }
}
