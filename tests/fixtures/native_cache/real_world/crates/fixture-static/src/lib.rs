use fixture_types::Record;

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(".text", ".p2align 2", ".Lcargo_rail_integrated_assembly:", "nop");

#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(".text", ".p2align 2", ".Lcargo_rail_integrated_assembly:", "nop");

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
core::arch::global_asm!(include_str!("integrated_assembly.s"));

#[cfg(target_arch = "aarch64")]
#[unsafe(no_mangle)]
#[target_feature(enable = "neon")]
pub unsafe extern "C" fn fixture_integrated_asm(value: u64) -> u64 {
  let output;
  // SAFETY: this copies one general-purpose register without reading memory or changing the stack or flags.
  unsafe {
    core::arch::asm!(
      "mov {output}, {value}",
      output = lateout(reg) output,
      value = in(reg) value,
      options(nomem, nostack, preserves_flags)
    );
  }
  output
}

#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
#[target_feature(enable = "sse2")]
pub unsafe extern "C" fn fixture_integrated_asm(value: u64) -> u64 {
  let output;
  // SAFETY: this copies one general-purpose register without reading memory or changing the stack or flags.
  unsafe {
    core::arch::asm!(
      "mov {output}, {value}",
      output = lateout(reg) output,
      value = in(reg) value,
      options(nomem, nostack, preserves_flags)
    );
  }
  output
}

#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fixture_naked_asm_identity(_value: u64) -> u64 {
  core::arch::naked_asm!("ret");
}

#[cfg(all(target_arch = "x86_64", not(windows)))]
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fixture_naked_asm_identity(_value: u64) -> u64 {
  core::arch::naked_asm!("mov rax, rdi", "ret");
}

#[cfg(all(target_arch = "x86_64", windows))]
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fixture_naked_asm_identity(_value: u64) -> u64 {
  core::arch::naked_asm!("mov rax, rcx", "ret");
}

#[unsafe(no_mangle)]
pub extern "C" fn fixture_static_value() -> u64 {
  Record::new(37, "static archive").id
}
