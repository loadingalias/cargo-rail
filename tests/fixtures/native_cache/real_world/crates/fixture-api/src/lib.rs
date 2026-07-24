use fixture_macros::fixture_component;
use fixture_types::Record;

#[fixture_component]
pub fn response() -> Record {
  let mut record = fixture_storage::record();
  record.id += fixture_build::GENERATED_VALUE;
  record
}
