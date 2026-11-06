use serde::{Deserialize, Serialize};
/// OMEMO device identifier.
///
/// Valid range per XEP-0384: 1 ≤ id ≤ 2^31−1.  Use `DeviceId::from(v)` to
/// wrap a `u32`; use `.get()` to recover the inner value.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeviceId(u32);

impl DeviceId {
    pub fn get(self) -> u32 {
        self.0
    }
    pub fn is_valid_xep384(self) -> bool {
        self.0 > 0 && self.0 <= i32::MAX as u32
    }
}

impl From<u32> for DeviceId {
    fn from(v: u32) -> Self {
        DeviceId(v)
    }
}
impl From<DeviceId> for u32 {
    fn from(d: DeviceId) -> Self {
        d.0
    }
}

impl std::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DeviceId({})", self.0)
    }
}

impl std::str::FromStr for DeviceId {
    type Err = std::num::ParseIntError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<u32>().map(DeviceId)
    }
}
