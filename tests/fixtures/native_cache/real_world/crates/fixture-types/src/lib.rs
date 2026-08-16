#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
  pub id: u64,
  pub value: String,
}

impl Record {
  /// Creates a record.
  ///
  /// ```
  /// let record = fixture_types::Record::new(7, "retained doctest");
  /// assert_eq!(record.id, 7);
  /// ```
  pub fn new(id: u64, value: impl Into<String>) -> Self {
    Self { id, value: value.into() }
  }
}
