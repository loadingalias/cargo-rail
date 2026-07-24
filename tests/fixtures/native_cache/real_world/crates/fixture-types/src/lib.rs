#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
  pub id: u64,
  pub value: String,
}

impl Record {
  pub fn new(id: u64, value: impl Into<String>) -> Self {
    Self { id, value: value.into() }
  }
}
