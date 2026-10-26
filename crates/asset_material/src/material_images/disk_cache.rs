use super::decode::{DecodedMips, MipStorage, decode_iwi_mips};
#[cfg(target_os = "horizon")]
use super::{is_fine_ui_name, is_font_name, is_large_ui_name, is_ui_name};
use std::sync::atomic::{AtomicU64, Ordering};

#[allow(dead_code)]
const MIP_CACHE_FORMAT: u32 = 2;
#[allow(dead_code)]
const MIP_MAGIC: &[u8; 8] = b"IWL1MIPS";
static MIP_HIT: AtomicU64 = AtomicU64::new(0);
static MIP_MISS: AtomicU64 = AtomicU64::new(0);
static MIP_IO_NS: AtomicU64 = AtomicU64::new(0);

pub fn mip_cache_cost() -> (u64, u64, f64) {
    (
        MIP_HIT.load(Ordering::Relaxed),
        MIP_MISS.load(Ordering::Relaxed),
        MIP_IO_NS.load(Ordering::Relaxed) as f64 / 1.0e6,
    )
}

#[allow(dead_code)]
fn encode_cache(mips: &DecodedMips) -> Vec<u8> {
    let mut out = Vec::with_capacity(28 + 4 * mips.level_sizes().len() + mips.payload().len());
    out.extend_from_slice(MIP_MAGIC);
    out.extend_from_slice(&MIP_CACHE_FORMAT.to_le_bytes());
    out.extend_from_slice(&mips.layout().width.to_le_bytes());
    out.extend_from_slice(&mips.layout().height.to_le_bytes());
    out.push(match mips.layout().storage {
        MipStorage::R32Float => 6,
        MipStorage::Rgba8 => 0,
        MipStorage::Bc1 => 1,
        MipStorage::Bc2 => 2,
        MipStorage::Bc3 => 3,
        MipStorage::Bc5 => 5,
    });
    out.extend_from_slice(&[0, 0, 0]);
    out.extend_from_slice(&(mips.level_sizes().len() as u32).to_le_bytes());
    let mut at = 0usize;
    for &size in mips.level_sizes() {
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&mips.payload()[at..at + size as usize]);
        at += size as usize;
    }
    out
}

#[allow(dead_code)]
fn decode_cache(bytes: &[u8]) -> Option<DecodedMips> {
    if bytes.len() < 28 || &bytes[..8] != MIP_MAGIC {
        return None;
    }
    let word = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    if word(8)? != MIP_CACHE_FORMAT {
        return None;
    }
    let width = word(12)?;
    let height = word(16)?;
    let storage = match bytes[20] {
        6 => MipStorage::R32Float,
        0 => MipStorage::Rgba8,
        1 => MipStorage::Bc1,
        2 => MipStorage::Bc2,
        3 => MipStorage::Bc3,
        5 => MipStorage::Bc5,
        _ => return None,
    };
    let count = word(24)? as usize;
    if count == 0 || count > bytes.len().saturating_sub(28) / 4 {
        return None;
    }
    let mut at = 28usize;
    let mut packed = Vec::with_capacity(bytes.len().saturating_sub(28));
    let mut sizes = Vec::with_capacity(count);
    for _ in 0..count {
        let size = word(at)?;
        at = at.checked_add(4)?;
        let end = at.checked_add(size as usize)?;
        packed.extend_from_slice(bytes.get(at..end)?);
        at = end;
        sizes.push(size);
    }
    if at != bytes.len() {
        return None;
    }
    DecodedMips::from_packed(width, height, storage, packed, sizes)
}

#[allow(dead_code)]
fn read_cached_mips(key: &str, io_at: std::time::Instant) -> Option<DecodedMips> {
    let hit = crate::cache_get("mips", key)?;
    MIP_IO_NS.fetch_add(io_at.elapsed().as_nanos() as u64, Ordering::Relaxed);
    let mips = decode_cache(&hit)?;
    MIP_HIT.fetch_add(1, Ordering::Relaxed);
    Some(mips.picmip(PICMIP_MAX))
}

/// Largest IWD texture side kept. On Switch the ~3 GB heap also backs GPU
/// memory, and every common_mp texture at full size does not fit beside the map.
// 256, not 512: at 512 GPU textures still filled the heap past class select.
// ponytail: fixed 256 on Horizon; a setting if docked play wants sharper textures.
#[cfg(target_os = "horizon")]
const PICMIP_MAX: u32 = 256;
#[cfg(not(target_os = "horizon"))]
const PICMIP_MAX: u32 = u32::MAX;

fn mip_cache_key(crc: u32, size: u64, entry: &str) -> String {
    format!(
        "{MIP_CACHE_FORMAT:08x}-{crc:08x}-{size:x}-{:016x}",
        crate::fnv1a64(entry.as_bytes())
    )
}

pub(super) fn load_or_decode_mips(
    candidate: &asset_transport::IwdFile,
    name: &str,
) -> Result<DecodedMips, String> {
    #[cfg(target_os = "horizon")]
    {
        let bytes = candidate.read()?;
        let mips = decode_iwi_mips(&bytes)?;
        let entry = candidate.entry();
        if is_font_name(entry) || is_font_name(name) {
            // Fonts: NEVER clamp or picmip! Keep 100% native resolution for crystal clear text.
            Ok(mips)
        } else if is_fine_ui_name(entry) || is_fine_ui_name(name) {
            // Reticles, scopes, compass ticks: up to 1024 to keep fine 1px lines crisp.
            Ok(mips.picmip(1024))
        } else if is_large_ui_name(entry) || is_large_ui_name(name) {
            // Large loadscreen & overhead minimap images: clamp to 512 to save 12-15 MB RAM each.
            Ok(mips.picmip(512))
        } else if is_ui_name(entry) || is_ui_name(name) {
            // General UI icons, badges, indicators up to 512.
            Ok(mips.picmip(512))
        } else {
            // 3D world geometry, props, models: clamped to 256 for Switch memory budget.
            Ok(mips.picmip(PICMIP_MAX))
        }
    }
    #[cfg(not(target_os = "horizon"))]
    {
        let _ = name;
        let key = mip_cache_key(candidate.crc32(), candidate.size(), candidate.entry());
        let io_at = std::time::Instant::now();
        if let Some(mips) = read_cached_mips(&key, io_at) {
            return Ok(mips);
        }

        let _flight = crate::cache_flight("mips", &key);
        if let Some(mips) = read_cached_mips(&key, std::time::Instant::now()) {
            return Ok(mips);
        }
        let bytes = candidate.read()?;
        let mips = decode_iwi_mips(&bytes)?.picmip(PICMIP_MAX);
        let blob = encode_cache(&mips);
        let store_at = std::time::Instant::now();
        if let Err(error) = crate::cache_put("mips", &key, &blob) {
            diag::warn!(Zone, "mip cache store {key}: {error}");
        }
        MIP_IO_NS.fetch_add(store_at.elapsed().as_nanos() as u64, Ordering::Relaxed);
        MIP_MISS.fetch_add(1, Ordering::Relaxed);
        Ok(mips)
    }
}

pub(super) fn load_or_decode_mips_native(
    candidate: &asset_transport::IwdFile,
    name: &str,
) -> Result<DecodedMips, String> {
    let bytes = candidate.read()?;
    #[cfg(target_os = "horizon")]
    {
        let mips = decode_iwi_mips(&bytes)?;
        let entry = candidate.entry();
        if is_font_name(entry) || is_font_name(name) {
            // Fonts: NEVER clamp or picmip! Keep 100% native resolution for crystal clear text.
            Ok(mips)
        } else if is_fine_ui_name(entry) || is_fine_ui_name(name) {
            Ok(mips.picmip(1024))
        } else if is_large_ui_name(entry) || is_large_ui_name(name) {
            // Large loadscreen & overhead minimap images: clamp to 512 to save 12-15 MB RAM each.
            Ok(mips.picmip(512))
        } else {
            // General UI: 512
            Ok(mips.picmip(512))
        }
    }
    #[cfg(not(target_os = "horizon"))]
    {
        let _ = name;
        decode_iwi_mips(&bytes)
    }
}
