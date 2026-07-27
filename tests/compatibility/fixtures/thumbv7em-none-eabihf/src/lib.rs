#![no_std]

#[cfg(feature = "variant")]
const OFFSET: u32 = 1;
#[cfg(not(feature = "variant"))]
const OFFSET: u32 = 0;

pub fn checksum(bytes: &[u8]) -> u32 {
  bytes
    .iter()
    .fold(OFFSET, |sum, byte| sum.wrapping_add(u32::from(*byte)))
}
