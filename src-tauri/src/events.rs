use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct LyricLine {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub words: Vec<WordInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WordInfo {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LineInfo {
    pub text: String,
    pub start_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Track {
        title: String,
        artist: String,
        album: String,
        track_id: String,
        duration_ms: u32,
    },
    Lyrics {
        lines: Vec<LyricLine>,
        sync_type: String,
    },
    Line {
        current: LineInfo,
        next: Option<LineInfo>,
        position_ms: u64,
    },
    State {
        playing: bool,
        position_ms: u64,
    },
    NoLyrics,
}
