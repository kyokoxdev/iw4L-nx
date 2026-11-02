//! B5: the `.dkpk` pack. Header `DKPK | format u32 | uam_version u32 | count
//! u32`, then entries sorted by key: `key u64 | glsl_len u32 | stage u8 |
//! dksh_len u32 | pad to 0x100 | dksh`. All little-endian.

use std::collections::BTreeMap;
use std::path::Path;

pub const FORMAT: u32 = 1;
/// uam 1.1.0, as `major << 16 | minor << 8 | patch`.
pub const UAM_VERSION: u32 = 0x01_01_00;
const ALIGN: usize = 0x100;

pub struct Entry {
    pub glsl_len: u32,
    pub stage: u8,
    pub dksh: Vec<u8>,
}

pub fn encode(entries: &BTreeMap<u64, Entry>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"DKPK");
    for word in [FORMAT, UAM_VERSION, entries.len() as u32] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    for (key, entry) in entries {
        out.extend_from_slice(&key.to_le_bytes());
        out.extend_from_slice(&entry.glsl_len.to_le_bytes());
        out.push(entry.stage);
        out.extend_from_slice(&(entry.dksh.len() as u32).to_le_bytes());
        out.resize(out.len().next_multiple_of(ALIGN), 0);
        out.extend_from_slice(&entry.dksh);
    }
    out
}

/// Inverse of `encode`, for the round-trip check (the device reader lives in
/// the wgpu fork, K2).
#[cfg(test)]
pub fn decode(bytes: &[u8]) -> Option<BTreeMap<u64, Entry>> {
    let u32_at = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    if bytes.get(..4)? != b"DKPK" || u32_at(4)? != FORMAT {
        return None;
    }
    let mut entries = BTreeMap::new();
    let mut at = 16;
    for _ in 0..u32_at(12)? {
        let key = u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?);
        let glsl_len = u32_at(at + 8)?;
        let stage = *bytes.get(at + 12)?;
        let dksh_len = u32_at(at + 13)? as usize;
        at = (at + 17).next_multiple_of(ALIGN);
        let dksh = bytes.get(at..at + dksh_len)?.to_vec();
        at += dksh_len;
        entries.insert(key, Entry { glsl_len, stage, dksh });
    }
    Some(entries)
}

/// `.tmp` then rename, so a killed bake never leaves a torn pack.
pub fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("dkpk.tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut entries = BTreeMap::new();
        entries.insert(7, Entry { glsl_len: 10, stage: 4, dksh: vec![1, 2, 3] });
        entries.insert(3, Entry { glsl_len: 20, stage: 0, dksh: vec![9; 300] });
        let bytes = encode(&entries);
        let back = decode(&bytes).unwrap();
        assert_eq!(back.len(), 2);
        for (key, entry) in &entries {
            let got = &back[key];
            assert_eq!((got.glsl_len, got.stage, &got.dksh), (entry.glsl_len, entry.stage, &entry.dksh));
        }
        assert_eq!(encode(&back), bytes);
    }
}
