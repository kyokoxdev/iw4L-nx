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

impl Stage {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Vertex,
            1 => Self::TessCtrl,
            2 => Self::TessEval,
            3 => Self::Geometry,
            4 => Self::Fragment,
            5 => Self::Compute,
            _ => return None,
        })
    }
}

/// `iw4l-bake uam`: a compile server, since Mesa's context is global and
/// per-process. Each request on stdin is `stage u8 | len u32 | glsl`; each
/// reply on stdout is `rc i32 | len u32 | dksh`. Little-endian; exits on EOF.
pub fn worker() -> std::process::ExitCode {
    use std::io::{Read, Write};
    let (mut stdin, mut stdout) = (std::io::stdin().lock(), std::io::stdout().lock());
    let mut head = [0u8; 5];
    while stdin.read_exact(&mut head).is_ok() {
        let mut glsl = vec![0; u32::from_le_bytes(head[1..].try_into().unwrap()) as usize];
        if stdin.read_exact(&mut glsl).is_err() {
            return 2.into();
        }
        let result = match (Stage::from_u8(head[0]), String::from_utf8(glsl)) {
            (Some(stage), Ok(glsl)) => compile(&glsl, stage),
            _ => Err(2),
        };
        let (rc, dksh) = match result {
            Ok(dksh) => (0, dksh),
            Err(rc) => (rc, Vec::new()),
        };
        let reply = [&rc.to_le_bytes()[..], &(dksh.len() as u32).to_le_bytes(), &dksh].concat();
        if stdout.write_all(&reply).and_then(|()| stdout.flush()).is_err() {
            return 2.into();
        }
    }
    0.into()
}

/// Compiles every job across all cores, one `worker` process per core.
pub fn compile_many(jobs: &[(u64, Stage, &str)]) -> Vec<(u64, Result<Vec<u8>, i32>)> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let exe = std::env::current_exe().expect("current_exe");
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(jobs.len());
    let mut done: Vec<_> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| -> std::io::Result<Vec<_>> {
                    let mut child = Command::new(&exe)
                        .arg("uam")
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .spawn()?;
                    let (mut to, mut from) = (child.stdin.take().unwrap(), child.stdout.take().unwrap());
                    let mut out = Vec::new();
                    while let Some(&(key, stage, glsl)) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                        to.write_all(&[&[stage as u8][..], &(glsl.len() as u32).to_le_bytes(), glsl.as_bytes()].concat())?;
                        let mut head = [0u8; 8];
                        from.read_exact(&mut head)?;
                        let rc = i32::from_le_bytes(head[..4].try_into().unwrap());
                        let mut dksh = vec![0; u32::from_le_bytes(head[4..].try_into().unwrap()) as usize];
                        from.read_exact(&mut dksh)?;
                        out.push((key, if rc == 0 { Ok(dksh) } else { Err(rc) }));
                    }
                    drop(to);
                    child.wait()?;
                    Ok(out)
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap().expect("uam worker died"))
            .collect()
    });
    done.sort_by_key(|(key, _)| *key);
    done
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
