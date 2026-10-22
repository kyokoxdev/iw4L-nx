/// A script cinematic (`CinematicInGame*`): which video, and when it ends.
/// The authority keeps it; the local client plays it. Lengths come from
/// `LevelData::videos`, read from the video files at install, so the sim
/// never opens a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptCinematic {
    /// Lowercase `video/<name>.bik` stem.
    pub name: String,
    pub looped: bool,
    pub fullscreen: bool,
    /// Level time it started; replaying the same video is a new cinematic.
    pub start_ms: i32,
    /// `None` while looping.
    pub end_ms: Option<i32>,
}

impl ScriptCinematic {
    pub fn playing(&self, now_ms: i32) -> bool {
        self.end_ms.is_none_or(|end| now_ms < end)
    }
}
