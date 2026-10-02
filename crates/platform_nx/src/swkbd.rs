//! Blocking system software keyboard (libnx `swkbd`).

use core::ffi::{c_char, c_void};

// sizeof(SwkbdConfig) is 1264 on libnx (checked with devkitA64 gcc); the
// buffer is rounded up so a libnx growth does not corrupt the stack.
#[repr(C, align(8))]
struct SwkbdConfig([u8; 1536]);

/// `offsetof(SwkbdConfig, arg.arg.arg.stringLenMax)`; its setter is
/// `static inline` in libnx, so there is no symbol to call.
const STRING_LEN_MAX: usize = 940;

unsafe extern "C" {
    fn swkbdCreate(config: *mut SwkbdConfig, max_dictwords: i32) -> u32;
    fn swkbdClose(config: *mut SwkbdConfig);
    fn swkbdConfigMakePresetDefault(config: *mut SwkbdConfig);
    fn swkbdConfigMakePresetPassword(config: *mut SwkbdConfig);
    fn swkbdConfigSetInitialText(config: *mut SwkbdConfig, text: *const c_char);
    fn swkbdShow(config: *mut SwkbdConfig, out: *mut c_void, out_size: usize) -> u32;
}

/// Opens the keyboard over `initial` and returns the entered line, `None` if
/// cancelled. Blocks until the applet closes.
pub fn prompt(initial: &str, max_chars: usize, masked: bool) -> Option<String> {
    let initial = std::ffi::CString::new(initial).ok()?;
    let mut config = SwkbdConfig([0; 1536]);
    // UTF-8 out of at most 4096 UTF-16 units, NUL included.
    let mut out = vec![0u8; 4 * 4096 + 1];
    // SAFETY: `config` is large enough and 8-aligned; `out` is NUL-terminated
    // by libnx within `out.len()`; swkbdClose pairs with a successful create.
    unsafe {
        if swkbdCreate(&mut config, 0) != 0 {
            return None;
        }
        if masked {
            swkbdConfigMakePresetPassword(&mut config);
        } else {
            swkbdConfigMakePresetDefault(&mut config);
        }
        swkbdConfigSetInitialText(&mut config, initial.as_ptr());
        let len_max = u32::try_from(max_chars).unwrap_or(u32::MAX);
        config.0[STRING_LEN_MAX..STRING_LEN_MAX + 4].copy_from_slice(&len_max.to_ne_bytes());
        let rc = swkbdShow(&mut config, out.as_mut_ptr().cast(), out.len());
        swkbdClose(&mut config);
        if rc != 0 {
            return None;
        }
    }
    let len = out.iter().position(|&b| b == 0).unwrap_or(out.len());
    Some(String::from_utf8_lossy(&out[..len]).into_owned())
}
