//! Model arrays that can wait in a file on the SD card until first read.
//!
//! On Switch the heap also backs GPU memory, and common_mp holds ~330 MiB of
//! XModel geometry of which a match draws a fraction. A spilled array reads
//! its file back the first time anything dereferences it, then stays.
//! Files are named by content, so later boots find them and write nothing.
// ponytail: no eviction after reload and no pruning of old files; add both if
// matches churn through many weapons or game data changes often.

use std::io::{Read, Write};
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, OnceLock};

/// Any byte pattern is a valid value and there is no padding.
///
/// # Safety
/// Implementors must be `Copy` types with no padding bytes and no invalid bit
/// patterns.
pub unsafe trait Plain: Copy + Send + Sync + 'static {}

// SAFETY: integers and byte arrays have no padding or invalid patterns.
unsafe impl Plain for u32 {}
// SAFETY: as above.
unsafe impl<const N: usize> Plain for [u8; N] {}

pub struct Spill<T: Plain>(Arc<Slot<T>>);

struct Slot<T> {
    data: OnceLock<Arc<[T]>>,
    len: usize,
    file: Option<(PathBuf, String)>,
}

static SPILLED: AtomicU64 = AtomicU64::new(0);
static WRITTEN: AtomicU64 = AtomicU64::new(0);
static RELOADED: AtomicU64 = AtomicU64::new(0);

/// Bytes moved out of the heap, of those written this boot, and read back.
pub fn spill_stats() -> (u64, u64, u64) {
    (
        SPILLED.load(Relaxed),
        WRITTEN.load(Relaxed),
        RELOADED.load(Relaxed),
    )
}

impl<T: Plain> Spill<T> {
    pub fn is_resident(&self) -> bool {
        self.0.data.get().is_some()
    }

    /// The array as a shared slice, read back first if spilled.
    pub fn arc(&self) -> Arc<[T]> {
        self.resident().clone()
    }

    fn resident(&self) -> &Arc<[T]> {
        self.0.data.get_or_init(|| {
            let (path, label) = self
                .0
                .file
                .as_ref()
                .expect("a spill without data has a file");
            let data = read_plain::<T>(path, self.0.len).unwrap_or_else(|error| {
                panic!(
                    "model spill {label} at {} unreadable: {error}",
                    path.display()
                )
            });
            let bytes = byte_len::<T>(self.0.len);
            let total = RELOADED.fetch_add(bytes, Relaxed) + bytes;
            diag::info!(
                Zone,
                "xmodel spill: read back {label} {}KB ({}MiB in all)",
                bytes >> 10,
                total >> 20
            );
            data
        })
    }

    /// Moves the array to a file named `<label>-<content hash>` and drops it
    /// from the heap. Keeps it resident if the file cannot be written.
    pub fn spill(&mut self, label: &str) -> Result<u64, String> {
        let Some(data) = self.0.data.get() else {
            return Ok(0);
        };
        if data.is_empty() {
            return Ok(0);
        }
        let bytes = as_bytes(data);
        let key = format!(
            "{:016x}-{:x}",
            word_hash(word_hash(0xcbf2_9ce4_8422_2325, label.as_bytes()), bytes),
            bytes.len()
        );
        let path = spill_dir()?.join(&key[..2]).join(key);
        if !std::fs::metadata(&path).is_ok_and(|meta| meta.len() == bytes.len() as u64) {
            write_atomic(&path, bytes)?;
            WRITTEN.fetch_add(bytes.len() as u64, Relaxed);
        }
        let (len, size) = (data.len(), bytes.len() as u64);
        SPILLED.fetch_add(size, Relaxed);
        self.0 = Arc::new(Slot {
            data: OnceLock::new(),
            len,
            file: Some((path, label.to_owned())),
        });
        Ok(size)
    }
}

impl<T: Plain> Deref for Spill<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        self.resident()
    }
}

impl<T: Plain> Clone for Spill<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: Plain> Default for Spill<T> {
    fn default() -> Self {
        Self::from(Vec::new())
    }
}

impl<T: Plain> From<Arc<[T]>> for Spill<T> {
    fn from(data: Arc<[T]>) -> Self {
        Self(Arc::new(Slot {
            len: data.len(),
            data: OnceLock::from(data),
            file: None,
        }))
    }
}

impl<T: Plain> From<Vec<T>> for Spill<T> {
    fn from(data: Vec<T>) -> Self {
        Self::from(Arc::<[T]>::from(data))
    }
}

impl<T: Plain> std::fmt::Debug for Spill<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = if self.is_resident() {
            "resident"
        } else {
            "spilled"
        };
        write!(
            f,
            "Spill({} x {}, {state})",
            self.0.len,
            std::any::type_name::<T>()
        )
    }
}

fn byte_len<T>(len: usize) -> u64 {
    (len * size_of::<T>()) as u64
}

fn as_bytes<T: Plain>(data: &[T]) -> &[u8] {
    // SAFETY: `Plain` types have no padding, so every byte is initialised.
    unsafe { std::slice::from_raw_parts(data.as_ptr().cast(), size_of_val(data)) }
}

fn read_plain<T: Plain>(path: &std::path::Path, len: usize) -> Result<Arc<[T]>, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let out = Arc::<[T]>::new_zeroed_slice(len);
    // SAFETY: zeroed memory is a valid `Plain` value; the bytes are then
    // overwritten in place, so the file is never held twice.
    let mut out = unsafe { out.assume_init() };
    let slice = Arc::get_mut(&mut out).expect("fresh Arc is unique");
    // SAFETY: as in `as_bytes`, and any byte pattern is a valid `T`.
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(slice.as_mut_ptr().cast::<u8>(), size_of_val(slice))
    };
    file.read_exact(bytes).map_err(|e| e.to_string())?;
    if file.read(&mut [0]).map_err(|e| e.to_string())? != 0 {
        return Err("longer than recorded".into());
    }
    Ok(out)
}

fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().expect("spill path has a directory");
    std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let written = std::fs::File::create(&tmp).and_then(|mut file| file.write_all(bytes));
    if let Err(error) = written.and_then(|()| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("write {}: {error}", path.display()));
    }
    Ok(())
}

fn spill_dir() -> Result<PathBuf, String> {
    static DIR: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    DIR.get_or_init(|| {
        Ok(asset_transport::ensure_artifacts_dir()?
            .join("spill")
            .join("xmodel"))
    })
    .clone()
}

/// FNV-style over 8-byte words: a byte-wise hash of ~330 MiB costs over a
/// second on the Switch CPU.
fn word_hash(mut hash: u64, bytes: &[u8]) -> u64 {
    let mut words = bytes.chunks_exact(8);
    for word in &mut words {
        hash = (hash ^ u64::from_le_bytes(word.try_into().unwrap())).wrapping_mul(0x100_0000_01b3);
    }
    for &byte in words.remainder() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spilled_array_reads_back_the_same() {
        let data: Vec<u32> = (0..10_000).collect();
        let mut spill = Spill::from(data.clone());
        let label = format!("test-{}", std::process::id());
        assert!(spill.spill(&label).unwrap() > 0);
        assert!(!spill.is_resident());
        let shared = spill.clone();
        assert_eq!(&*shared, &data[..]);
        assert!(spill.is_resident(), "clones share one read");
        // Same content and label: the file is reused, not rewritten.
        let (_, written, _) = spill_stats();
        let mut again = Spill::from(data);
        again.spill(&label).unwrap();
        assert_eq!(spill_stats().1, written);
        let _ = std::fs::remove_file(&again.0.file.as_ref().unwrap().0);
    }
}
