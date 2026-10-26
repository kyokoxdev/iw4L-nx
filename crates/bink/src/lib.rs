//! Bink 1 (`BIKf`..`BIKi`) demuxer and decoder: video to YUV 4:2:0 planes or
//! RGBA, first audio track to interleaved f32. One frame per call, file read
//! per frame — nothing is held beyond two reference frames.

mod audio;
mod bits;
mod tables;
mod video;

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver};

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Unsupported(String),
    Format(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "bink: {e}"),
            Self::Unsupported(what) => write!(f, "bink: unsupported {what}"),
            Self::Format(what) => write!(f, "bink: corrupt stream ({what})"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AudioTrack {
    pub rate: u32,
    pub channels: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Header {
    pub revision: u8,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub frames: u32,
    /// First audio track; others are skipped.
    pub audio: Option<AudioTrack>,
}

const FLAG_ALPHA: u32 = 0x0010_0000;
const AUD_STEREO: u16 = 0x2000;
const AUD_DCT: u16 = 0x1000;
#[cfg(target_os = "horizon")]
const PREFETCH_CAPACITY: usize = 90;
#[cfg(not(target_os = "horizon"))]
const PREFETCH_CAPACITY: usize = 180;

struct Prefetch {
    rx: Receiver<Result<Vec<u8>, std::io::Error>>,
    stop: Arc<AtomicBool>,
}

pub struct Bink {
    file: Option<File>,
    header: Header,
    /// `(offset, size)` per frame.
    index: Vec<(u64, u32)>,
    next: usize,
    video: video::Video,
    audio: Option<audio::Audio>,
    tracks: usize,
    packet: Vec<u8>,
    prefetch: Option<Prefetch>,
}

impl Drop for Bink {
    fn drop(&mut self) {
        if let Some(prefetch) = &self.prefetch {
            prefetch.stop.store(true, Ordering::Relaxed);
        }
    }
}

impl Bink {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let mut file = File::open(path)?;
        let mut h = [0u8; 44];
        file.read_exact(&mut h)?;
        let word = |i: usize| u32::from_le_bytes(h[i * 4..i * 4 + 4].try_into().unwrap());
        let revision = h[3];
        if &h[..3] != b"BIK" || !(b'f'..=b'i').contains(&revision) {
            return Err(Error::Unsupported(format!(
                "container {:?}",
                String::from_utf8_lossy(&h[..4])
            )));
        }
        let file_size = u64::from(word(1)) + 8;
        let frames = word(2);
        let (width, height) = (word(5), word(6));
        let (fps_num, fps_den) = (word(7), word(8));
        let flags = word(9);
        let tracks = word(10) as usize;
        if frames == 0 || frames > 1_000_000 || fps_num == 0 || fps_den == 0 || tracks > 256 {
            return Err(Error::Format("header"));
        }
        if !(1..=7680).contains(&width) || !(1..=4800).contains(&height) {
            return Err(Error::Format("dimensions"));
        }

        let mut audio_track = None;
        if tracks > 0 {
            let mut t = vec![0u8; tracks * 12];
            file.read_exact(&mut t)?;
            let rate = u16::from_le_bytes([t[tracks * 4], t[tracks * 4 + 1]]);
            let aflags = u16::from_le_bytes([t[tracks * 4 + 2], t[tracks * 4 + 3]]);
            if rate > 0 {
                let channels = if aflags & AUD_STEREO != 0 { 2 } else { 1 };
                audio_track = Some((
                    AudioTrack {
                        rate: u32::from(rate),
                        channels,
                    },
                    aflags & AUD_DCT != 0,
                ));
            }
        }

        let mut raw = vec![0u8; frames as usize * 4];
        file.read_exact(&mut raw)?;
        let offsets: Vec<u64> = raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&c| u64::from(u32::from_le_bytes(c) & !1))
            .chain(std::iter::once(file_size))
            .collect();
        let mut index = Vec::with_capacity(frames as usize);
        for w in offsets.windows(2) {
            if w[1] <= w[0] {
                return Err(Error::Format("frame index"));
            }
            index.push((w[0], (w[1] - w[0]) as u32));
        }

        let header = Header {
            revision,
            width,
            height,
            fps_num,
            fps_den,
            frames,
            audio: audio_track.map(|(a, _)| a),
        };
        Ok(Self {
            file: Some(file),
            video: video::Video::new(
                width as usize,
                height as usize,
                revision,
                flags & FLAG_ALPHA != 0,
            ),
            audio: audio_track.map(|(a, dct)| audio::Audio::new(a.rate, a.channels, dct)),
            header,
            index,
            next: 0,
            tracks,
            packet: Vec::new(),
            prefetch: None,
        })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    fn ensure_prefetch(&mut self) -> Result<(), Error> {
        if self.prefetch.is_some() || self.next >= self.index.len() {
            return Ok(());
        }
        let Some(mut file) = self.file.take() else {
            return Ok(());
        };
        let stop = Arc::new(AtomicBool::new(false));
        let stop_reader = stop.clone();
        let (tx, rx) = sync_channel::<Result<Vec<u8>, std::io::Error>>(PREFETCH_CAPACITY);
        let remaining_index = self.index[self.next..].to_vec();

        std::thread::Builder::new()
            .name("iw4l-bink-io".into())
            .spawn(move || {
                #[cfg(target_os = "horizon")]
                {
                    platform_nx::pin_current_thread(1, 0b110, "iw4l-bink-io");
                    platform_nx::raise_current_thread("iw4l-bink-io");
                }
                for (offset, size) in remaining_index {
                    if stop_reader.load(Ordering::Relaxed) {
                        break;
                    }
                    let mut buf = vec![0u8; size as usize];
                    let res = file
                        .seek(SeekFrom::Start(offset))
                        .and_then(|_| file.read_exact(&mut buf));
                    match res {
                        Ok(()) => {
                            if tx.send(Ok(buf)).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Err(e));
                            break;
                        }
                    }
                }
            })
            .map_err(Error::Io)?;

        self.prefetch = Some(Prefetch { rx, stop });
        Ok(())
    }

    /// Decodes the next frame; its audio (first track, interleaved, at the
    /// track's own rate) is appended to `audio`. `false` once past the end.
    pub fn next_frame(&mut self, audio: &mut Vec<f32>) -> Result<bool, Error> {
        if self.next >= self.index.len() {
            return Ok(false);
        }
        self.ensure_prefetch()?;
        let packet = match &self.prefetch {
            Some(prefetch) => match prefetch.rx.recv() {
                Ok(Ok(buf)) => buf,
                Ok(Err(e)) => return Err(Error::Io(e)),
                Err(_) => return Ok(false),
            },
            None => return Ok(false),
        };
        self.next += 1;
        self.packet = packet;

        let mut at = 0usize;
        for track in 0..self.tracks {
            let len = self
                .packet
                .get(at..at + 4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
                .ok_or(Error::Format("audio size"))?;
            let data = self
                .packet
                .get(at + 4..at + 4 + len)
                .ok_or(Error::Format("audio packet"))?;
            if track == 0
                && len >= 4
                && let Some(dec) = &mut self.audio
            {
                dec.decode(data, audio)?;
            }
            at += 4 + len;
        }
        self.video.decode(&self.packet[at..])?;
        Ok(true)
    }

    /// Current frame as RGBA8, `width * height * 4` bytes.
    pub fn rgba(&self, out: &mut [u8]) {
        let (w, h) = (self.header.width as usize, self.header.height as usize);
        video::to_rgba(&self.video.cur, w, h, out);
    }
}
