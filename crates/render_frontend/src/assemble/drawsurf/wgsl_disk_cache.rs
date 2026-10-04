use std::sync::atomic::{AtomicU64, Ordering};

use d3d9_sm3::{PassLoweringAbi, PassWgsl};

use super::material_runtime::RuntimeShaderPair;

// Bump when emitted WGSL changes as well as when the binary format changes.
pub const WGSL_CACHE_FORMAT: u32 = 5;

const MAGIC: &[u8; 8] = b"IWLWGSL\n";

static HIT: AtomicU64 = AtomicU64::new(0);
static MISS: AtomicU64 = AtomicU64::new(0);
static IO_NS: AtomicU64 = AtomicU64::new(0);

pub fn stats() -> (u64, u64, f64) {
    (
        HIT.load(Ordering::Relaxed),
        MISS.load(Ordering::Relaxed),
        IO_NS.load(Ordering::Relaxed) as f64 / 1.0e6,
    )
}

pub fn load(pair: RuntimeShaderPair, abi: &PassLoweringAbi) -> Option<PassWgsl> {
    let key = cache_key(pair, abi);
    let started = std::time::Instant::now();
    let mut pack = pack().lock().ok()?;
    let pack = pack.as_mut()?;
    let bytes = pack.read(key).or_else(|| {
        // Entries from the file-per-entry cache move into the pack once.
        let bytes = assets::cache_get("wgsl", &format!("{key:016x}"))?;
        let _ = pack.append(key, &bytes);
        Some(bytes)
    });
    IO_NS.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
    let module = decode(&bytes?)?;
    HIT.fetch_add(1, Ordering::Relaxed);
    Some(module)
}

pub fn store(pair: RuntimeShaderPair, abi: &PassLoweringAbi, module: &PassWgsl) {
    MISS.fetch_add(1, Ordering::Relaxed);
    let key = cache_key(pair, abi);
    let bytes = encode(module);
    let started = std::time::Instant::now();
    if let Ok(mut pack) = pack().lock()
        && let Some(pack) = pack.as_mut()
        && let Err(error) = pack.append(key, &bytes)
    {
        diag::warn!(World, "wgsl cache store {key:016x}: {error}");
    }
    IO_NS.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
}

fn cache_key(pair: RuntimeShaderPair, abi: &PassLoweringAbi) -> u64 {
    let mut hash = assets::fnv1a64(&WGSL_CACHE_FORMAT.to_le_bytes());
    hash = assets::fnv1a64_more(hash, &pair.vertex.program_hash.to_le_bytes());
    hash = assets::fnv1a64_more(hash, &pair.pixel.program_hash.to_le_bytes());
    hash = assets::fnv1a64_more(hash, format!("{abi:?}").as_bytes());
    hash
}

/// All entries in one append-only file: a file per entry cost ~38 ms of SD
/// card I/O each on Switch, minutes over a map's few thousand passes. Records
/// are `key u64 | len u32 | bytes`; the index holds offsets, not sources.
struct Pack {
    file: std::fs::File,
    index: std::collections::HashMap<u64, (u64, u32)>,
    end: u64,
}

impl Pack {
    fn open(path: &std::path::Path) -> std::io::Result<Self> {
        use std::io::Read;
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let mut index = std::collections::HashMap::new();
        let mut at = 0usize;
        while let Some(head) = bytes.get(at..at + 12) {
            let key = u64::from_le_bytes(head[..8].try_into().expect("8 bytes"));
            let len = u32::from_le_bytes(head[8..].try_into().expect("4 bytes"));
            let body = at + 12;
            if body + len as usize > bytes.len() {
                break;
            }
            index.insert(key, (body as u64, len));
            at = body + len as usize;
        }
        // A torn last record (crash mid-append) is cut so appends stay aligned.
        file.set_len(at as u64)?;
        Ok(Self { file, index, end: at as u64 })
    }

    fn read(&mut self, key: u64) -> Option<Vec<u8>> {
        use std::io::{Read, Seek};
        let &(offset, len) = self.index.get(&key)?;
        let mut bytes = vec![0; len as usize];
        self.file.seek(std::io::SeekFrom::Start(offset)).ok()?;
        self.file.read_exact(&mut bytes).ok()?;
        Some(bytes)
    }

    fn append(&mut self, key: u64, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::{Seek, Write};
        if self.index.contains_key(&key) {
            return Ok(());
        }
        let len = u32::try_from(bytes.len()).map_err(std::io::Error::other)?;
        let mut record = Vec::with_capacity(12 + bytes.len());
        record.extend_from_slice(&key.to_le_bytes());
        record.extend_from_slice(&len.to_le_bytes());
        record.extend_from_slice(bytes);
        self.file.seek(std::io::SeekFrom::Start(self.end))?;
        self.file.write_all(&record)?;
        self.index.insert(key, (self.end + 12, len));
        self.end += record.len() as u64;
        Ok(())
    }
}

fn pack() -> &'static std::sync::Mutex<Option<Pack>> {
    static PACK: std::sync::OnceLock<std::sync::Mutex<Option<Pack>>> = std::sync::OnceLock::new();
    PACK.get_or_init(|| {
        let opened = asset_transport::ensure_artifacts_dir().and_then(|root| {
            let dir = root.join("cache").join("wgsl");
            std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
            let path = dir.join(format!("pack-v{WGSL_CACHE_FORMAT}.bin"));
            Pack::open(&path).map_err(|error| format!("{}: {error}", path.display()))
        });
        std::sync::Mutex::new(match opened {
            Ok(pack) => Some(pack),
            Err(error) => {
                diag::warn!(World, "wgsl cache disabled: {error}");
                None
            }
        })
    })
}

fn encode(module: &PassWgsl) -> Vec<u8> {
    let source = module.source.as_bytes();
    let mut out = Vec::with_capacity(8 + 24 + source.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&WGSL_CACHE_FORMAT.to_le_bytes());
    out.extend_from_slice(&(module.attribute_count as u32).to_le_bytes());
    out.extend_from_slice(&(module.varying_count as u32).to_le_bytes());
    out.extend_from_slice(&(module.vertex_constant_len as u32).to_le_bytes());
    out.extend_from_slice(&(module.pixel_constant_len as u32).to_le_bytes());
    out.extend_from_slice(&(module.sampler_count as u32).to_le_bytes());
    out.extend_from_slice(&(source.len() as u32).to_le_bytes());
    out.extend_from_slice(source);
    out
}

fn decode(bytes: &[u8]) -> Option<PassWgsl> {
    if bytes.len() < 8 + 24 {
        return None;
    }
    if &bytes[..8] != MAGIC {
        return None;
    }
    let format = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    if format != WGSL_CACHE_FORMAT {
        return None;
    }
    let attribute_count = u32::from_le_bytes(bytes[12..16].try_into().ok()?) as usize;
    let varying_count = u32::from_le_bytes(bytes[16..20].try_into().ok()?) as usize;
    let vertex_constant_len = u32::from_le_bytes(bytes[20..24].try_into().ok()?) as usize;
    let pixel_constant_len = u32::from_le_bytes(bytes[24..28].try_into().ok()?) as usize;
    let sampler_count = u32::from_le_bytes(bytes[28..32].try_into().ok()?) as usize;
    let source_len = u32::from_le_bytes(bytes[32..36].try_into().ok()?) as usize;
    let source = bytes.get(36..36 + source_len)?;
    let source = std::str::from_utf8(source).ok()?.to_owned();
    Some(PassWgsl {
        source,
        attribute_count,
        varying_count,
        vertex_constant_len,
        pixel_constant_len,
        sampler_count,
    })
}

#[cfg(test)]
mod tests {
    use super::Pack;

    #[test]
    fn pack_round_trips_and_cuts_a_torn_record() {
        let path = std::env::temp_dir().join(format!("iw4l-wgsl-pack-{}.bin", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut pack = Pack::open(&path).unwrap();
        pack.append(1, b"one").unwrap();
        pack.append(2, b"second").unwrap();
        assert_eq!(pack.read(2).as_deref(), Some(&b"second"[..]));
        drop(pack);

        // Half a record at the end, as a crash mid-append leaves it.
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(&3u64.to_le_bytes());
        bytes.extend_from_slice(&99u32.to_le_bytes());
        bytes.extend_from_slice(b"torn");
        std::fs::write(&path, bytes).unwrap();

        let mut pack = Pack::open(&path).unwrap();
        assert_eq!(pack.read(1).as_deref(), Some(&b"one"[..]));
        assert_eq!(pack.read(3), None);
        pack.append(3, b"third").unwrap();
        drop(pack);
        let mut pack = Pack::open(&path).unwrap();
        assert_eq!(pack.read(3).as_deref(), Some(&b"third"[..]));
        assert_eq!(pack.read(2).as_deref(), Some(&b"second"[..]));
        let _ = std::fs::remove_file(&path);
    }
}
