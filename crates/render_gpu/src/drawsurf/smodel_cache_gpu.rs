use bevy::prelude::*;
use bevy::render::render_resource::{Buffer, BufferDescriptor, BufferUsages};
use bevy::render::renderer::{RenderDevice, RenderQueue};
use lighting_iw4::{
    SMC_BANK_VB_BYTES, SMC_INDEX_U16_N, SMC_VB_BYTES, SmcPatchLock, smc_stream_source_byte_offset,
};

use crate::drawsurf::backend::DYNAMIC_INDEX_BUFFER_CAPACITY;
use crate::drawsurf::gpu_resources::{padded_upload_len, write_buffer_padded};

#[derive(Resource, Default)]
pub struct SmodelCacheGpu {
    vb: Option<Buffer>,
    ib: Option<Buffer>,

    dynamic_ib: Option<Buffer>,
    uploaded_revision: Option<u64>,
}

impl SmodelCacheGpu {
    pub fn ensure(&mut self, device: &RenderDevice) {
        if self.vb.is_none() {
            self.vb = Some(device.create_buffer(&BufferDescriptor {
                label: Some("iw4_r_alloc_static_model_cache_vb"),
                size: u64::from(SMC_VB_BYTES),
                usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        if self.ib.is_none() {
            self.ib = Some(device.create_buffer(&BufferDescriptor {
                label: Some("iw4_r_cache_static_model_indices"),
                size: (SMC_INDEX_U16_N * 2) as u64,
                usage: BufferUsages::INDEX | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        if self.dynamic_ib.is_none() {
            self.dynamic_ib = Some(device.create_buffer(&BufferDescriptor {
                label: Some("iw4_r_set_index_data_smc"),
                size: u64::from(DYNAMIC_INDEX_BUFFER_CAPACITY) * 2,
                usage: BufferUsages::INDEX | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
    }

    pub fn vertex_buffer(&self) -> Option<&Buffer> {
        self.vb.as_ref()
    }

    pub fn index_buffer(&self) -> Option<&Buffer> {
        self.ib.as_ref()
    }

    pub fn dynamic_index_buffer(&self) -> Option<&Buffer> {
        self.dynamic_ib.as_ref()
    }

    pub fn write_dynamic_indices(
        &mut self,
        queue: &RenderQueue,
        indices: &[u16],
        layout_revision: u64,
    ) -> bool {
        if self.uploaded_revision == Some(layout_revision) && layout_revision != 0 {
            return true;
        }
        let Some(ib) = self.dynamic_ib.as_ref() else {
            return false;
        };
        if indices.is_empty() {
            self.uploaded_revision = Some(layout_revision);
            return false;
        }
        let bytes = bytemuck::cast_slice(indices);
        let cap = (DYNAMIC_INDEX_BUFFER_CAPACITY as usize).saturating_mul(2);
        if padded_upload_len(bytes.len()) > cap {
            return false;
        }
        write_buffer_padded(queue, ib, bytes);
        self.uploaded_revision = Some(layout_revision);
        false
    }

    pub fn bank_byte_range(cache_index: u16) -> Option<(u64, u64)> {
        let start = u64::from(smc_stream_source_byte_offset(cache_index)?);
        Some((start, start + u64::from(SMC_BANK_VB_BYTES)))
    }

    /// Writes this frame's cache patches, one `write_buffer` per run of
    /// byte-adjacent patches: wgpu's GL backend makes (and drops) a staging
    /// buffer per write, 0.1–0.5 ms each on Switch. Returns (patches, writes)
    /// for the vertex and index buffers.
    pub fn write_patches(
        &self,
        queue: &RenderQueue,
        vb_patches: &[(SmcPatchLock, Vec<u8>)],
        ib_patches: &[(u32, Vec<u8>)],
    ) -> [(usize, usize); 2] {
        let vb = self.vb.as_ref().map_or((0, 0), |vb| {
            let patches: Vec<_> = vb_patches
                .iter()
                .filter(|(lock, bytes)| lock.accepts_bytes(bytes.len()))
                .map(|(lock, bytes)| (u64::from(lock.byte_offset), bytes.as_slice()))
                .collect();
            (patches.len(), write_runs(queue, vb, &patches))
        });
        let ib = self.ib.as_ref().map_or((0, 0), |ib| {
            let patches: Vec<_> = ib_patches
                .iter()
                .filter(|(off, bytes)| {
                    (*off as usize).saturating_add(bytes.len()) <= SMC_INDEX_U16_N * 2
                })
                .map(|(off, bytes)| (u64::from(*off), bytes.as_slice()))
                .collect();
            (patches.len(), write_runs(queue, ib, &patches))
        });
        [vb, ib]
    }
}

/// Writes `patches`, merging runs that are byte-adjacent once sorted by
/// offset. Overlapping patches are written as given: sorting would reorder
/// writes to the same bytes. Returns the number of writes.
fn write_runs(queue: &RenderQueue, buffer: &Buffer, patches: &[(u64, &[u8])]) -> usize {
    let mut sorted = patches.to_vec();
    sorted.sort_by_key(|&(offset, _)| offset);
    if sorted
        .windows(2)
        .any(|w| w[1].0 < w[0].0 + w[0].1.len() as u64)
    {
        for &(offset, bytes) in patches {
            queue.write_buffer(buffer, offset, bytes);
        }
        return patches.len();
    }
    let runs = merge_runs(&sorted);
    for (offset, bytes) in &runs {
        queue.write_buffer(buffer, *offset, bytes);
    }
    runs.len()
}

/// Sorted, non-overlapping patches joined where one ends at the next's start.
fn merge_runs(sorted: &[(u64, &[u8])]) -> Vec<(u64, Vec<u8>)> {
    let mut runs: Vec<(u64, Vec<u8>)> = Vec::new();
    for &(offset, bytes) in sorted {
        match runs.last_mut() {
            Some((start, run)) if *start + run.len() as u64 == offset => {
                run.extend_from_slice(bytes)
            }
            _ => runs.push((offset, bytes.to_vec())),
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::merge_runs;

    #[test]
    fn adjacent_patches_merge() {
        let (a, b, c) = ([1u8; 4], [2u8; 8], [3u8; 4]);
        let runs = merge_runs(&[(0, &a), (4, &b), (16, &c)]);
        assert_eq!(runs, vec![(0, [&a[..], &b[..]].concat()), (16, c.to_vec())]);
    }
}
