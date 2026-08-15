#![allow(dead_code)]

pub struct PublicType {
  private_field: usize,
}

impl PublicType {
  pub fn public_method(&self) -> usize {
    self.private_field
  }
}

fn private_function(value: &PublicType) -> usize {
  value.public_method()
}
