#![allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals)]

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

pub mod same_name {
  pub fn nested() {}
}

pub fn same_name() {
  same_name::nested();
}

pub mod scoped {
  pub(in crate::scoped) mod twin {
    pub fn nested() {}
  }

  pub(in crate::scoped) fn twin() {
    twin::nested();
  }
}

pub mod namespace_coexistence {
  pub trait Shared {}
  pub const Shared: usize = 1;
  macro_rules! Shared {
    () => {
      2usize
    };
  }

  pub fn use_all<T: Shared>() -> usize {
    Shared + Shared!()
  }
}

pub trait AssociatedNames {
  type Shared;
  const Shared: usize;
}

pub trait type_origin {}
pub fn value_origin() {}

pub mod same_name_reexports {
  pub use crate::type_origin as Shared;
  pub use crate::value_origin as Shared;

  pub fn use_all<T: Shared>() {
    Shared();
  }
}

macro_rules! anonymous_definition {
  () => {
    const _: usize = 0;
  };
}

anonymous_definition!();
anonymous_definition!();

pub fn dependency_versions() {
  same_dependency_v1::marker();
  same_dependency_v2::marker();
}
