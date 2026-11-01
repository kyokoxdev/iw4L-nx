use std::ffi::{CString, c_char, c_int};
use std::sync::Mutex;

/// Values match uam's `pipeline_stage`.
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub enum Stage {
    Vertex = 0,
    TessCtrl = 1,
    TessEval = 2,
    Geometry = 3,
    Fragment = 4,
    Compute = 5,
}

unsafe extern "C" {
    fn uam_compile(
        glsl: *const c_char,
        stage: c_int,
        out: *mut *mut u8,
        out_len: *mut usize,
    ) -> c_int;
    fn uam_free(p: *mut u8);
}

/// Mesa keeps a global GL context: one compile at a time.
static LOCK: Mutex<()> = Mutex::new(());

/// Err is uam's return code; uam prints the log to stderr.
pub fn compile(glsl: &str, stage: Stage) -> Result<Vec<u8>, i32> {
    let glsl = CString::new(glsl).map_err(|_| -1)?;
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut out, mut len) = (std::ptr::null_mut(), 0usize);
    // SAFETY: glsl is NUL-terminated; on success out holds len bytes from malloc.
    unsafe {
        let rc = uam_compile(glsl.as_ptr(), stage as c_int, &mut out, &mut len);
        if rc != 0 {
            return Err(rc);
        }
        let dksh = std::slice::from_raw_parts(out, len).to_vec();
        uam_free(out);
        Ok(dksh)
    }
}
