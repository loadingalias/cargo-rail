#![no_std]

#[cfg(feature = "variant")]
#[used]
static PAYLOAD: [u8; 4] = *b"rail";
#[cfg(not(feature = "variant"))]
#[used]
static PAYLOAD: [u8; 4] = cargo_rail_compat_wasm_support::PAYLOAD;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
  loop {
    core::hint::spin_loop();
  }
}
