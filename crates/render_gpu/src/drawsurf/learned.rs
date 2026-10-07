//! Switch: pipeline keys play discovered, kept in the artifact cache so a
//! later boot or load builds them before play. A GL compile holds the context
//! lock and every frame stalls behind it, so a pipeline first built in play is
//! a stutter (countdown end, first raise or fire of a weapon, a new HUD state).
// ponytail: lists only grow; prune by last use if prebuilding outgrows the heap.

use std::sync::{Mutex, OnceLock};

use bevy::platform::collections::HashSet;

pub(super) struct Learned<const N: usize> {
    name: &'static str,
    state: OnceLock<Mutex<State<N>>>,
    write: Mutex<()>,
}

struct State<const N: usize> {
    bytes: Vec<u8>,
    seen: HashSet<[u8; N]>,
}

const KIND: &str = "pipelines";

impl<const N: usize> Learned<N> {
    pub(super) const fn new(name: &'static str) -> Self {
        Self {
            name,
            state: OnceLock::new(),
            write: Mutex::new(()),
        }
    }

    fn state(&self) -> &Mutex<State<N>> {
        self.state.get_or_init(|| {
            let mut state = State {
                bytes: Vec::new(),
                seen: HashSet::default(),
            };
            let bytes = assets::cache_get(KIND, self.name).unwrap_or_default();
            for record in bytes.chunks_exact(N) {
                state.absorb(record.try_into().expect("chunk is N bytes"));
            }
            Mutex::new(state)
        })
    }

    /// True when the record is new; call [`Self::save`] after a batch.
    pub(super) fn remember(&self, record: [u8; N]) -> bool {
        cfg!(target_os = "horizon") && self.state().lock().is_ok_and(|mut s| s.absorb(record))
    }

    /// Off the render thread: a file write costs tens of ms on the SD card.
    pub(super) fn save(&'static self) {
        assets::load_pool()
            .spawn(async move {
                let _write = self.write.lock();
                // Union with disk: a boot read that failed left the state
                // empty, and writing it alone would drop every learned key.
                let disk = assets::cache_get(KIND, self.name).unwrap_or_default();
                let bytes = self
                    .state()
                    .lock()
                    .map(|mut s| {
                        for record in disk.chunks_exact(N) {
                            s.absorb(record.try_into().expect("chunk is N bytes"));
                        }
                        s.bytes.clone()
                    })
                    .unwrap_or_default();
                if let Err(error) = assets::cache_put(KIND, self.name, &bytes) {
                    diag::warn!(World, "learned pipelines {}: {error}", self.name);
                }
            })
            .detach();
    }

    pub(super) fn records(&self) -> Vec<[u8; N]> {
        if !cfg!(target_os = "horizon") {
            return Vec::new();
        }
        self.state().lock().map_or_else(
            |_| Vec::new(),
            |s| {
                s.bytes
                    .chunks_exact(N)
                    .map(|r| r.try_into().expect("chunk is N bytes"))
                    .collect()
            },
        )
    }
}

impl<const N: usize> State<N> {
    fn absorb(&mut self, record: [u8; N]) -> bool {
        let new = self.seen.insert(record);
        if new {
            self.bytes.extend_from_slice(&record);
        }
        new
    }
}

/// Bytes of a pass state in a record: both words, cull, alpha test.
pub(super) const STATE_LEN: usize = 11;

/// Every input `compile_packed_state` takes, so `decode_state` rebuilds the
/// state exactly. None for an alpha func with no D3D raw value.
pub(super) fn encode_state(state: render_material::CompiledPassState) -> Option<[u8; STATE_LEN]> {
    let [word0, word1] = state.authored_words();
    let alpha_test = match state.alpha_test() {
        Some(test) => [u8::try_from(test.func.to_raw()?).ok()?, test.reference],
        None => [0, 0],
    };
    let mut out = [0; STATE_LEN];
    out[0..4].copy_from_slice(&word0.to_le_bytes());
    out[4..8].copy_from_slice(&word1.to_le_bytes());
    out[8..11].copy_from_slice(&[state.cull(), alpha_test[0], alpha_test[1]]);
    Some(out)
}

pub(super) fn decode_state(bytes: &[u8; STATE_LEN]) -> render_material::CompiledPassState {
    let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let alpha_test = (bytes[9] != 0)
        .then(|| d3d9_state::AlphaTest::from_raw(u32::from(bytes[9]), bytes[10]));
    render_material::compile_packed_state([word(0), word(4)], alpha_test, bytes[8])
}
