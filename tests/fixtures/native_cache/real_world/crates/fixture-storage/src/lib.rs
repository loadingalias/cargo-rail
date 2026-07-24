use fixture_types::Record;

pub fn record() -> Record {
  Record::new(fixture_git::git_value(), "stored")
}

#[cfg(feature = "json")]
pub fn encoded() -> String {
  serde_json::to_string(&record()).expect("fixture record is serializable")
}
