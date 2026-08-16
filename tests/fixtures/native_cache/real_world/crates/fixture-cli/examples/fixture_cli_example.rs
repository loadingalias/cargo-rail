use anyhow::Result;

fn main() -> Result<()> {
  let total = fixture_service_a::service_value() + fixture_service_b::service_value();
  assert_eq!(total, 119);
  Ok(())
}
