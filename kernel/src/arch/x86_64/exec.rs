#[cfg(target_os = "none")]
global_asm!(include_str!("exec.S"));
