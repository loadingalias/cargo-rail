#![no_std]

extern crate alloc;

use alloc::vec::Vec;

#[cfg(feature = "variant")]
const REPEAT: usize = 3;
#[cfg(not(feature = "variant"))]
const REPEAT: usize = 2;

pub fn repeat(bytes: &[u8]) -> Vec<u8> {
  let mut output = Vec::with_capacity(bytes.len().saturating_mul(REPEAT));
  for _ in 0..REPEAT {
    output.extend_from_slice(bytes);
  }
  output
}
