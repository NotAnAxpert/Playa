use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio::time::{interval, Duration, Instant};

use crate::events::{Event, LineInfo, LyricLine};

pub struct PlaybackState {
    pub is_playing: bool,
    pub anchor_time: Instant,
    pub anchor_position_ms: u64,
}

impl PlaybackState {
    pub fn new() -> Self {
        Self {
            is_playing: false,
            anchor_time: Instant::now(),
            anchor_position_ms: 0,
        }
    }

    pub fn current_position_ms(&self) -> u64 {
        if !self.is_playing {
            return self.anchor_position_ms;
        }
        let elapsed = self.anchor_time.elapsed().as_millis() as u64;
        self.anchor_position_ms + elapsed
    }

    pub fn update_position(&mut self, position_ms: u64) {
        self.anchor_time = Instant::now();
        self.anchor_position_ms = position_ms;
    }

    pub fn set_playing(&mut self, playing: bool, position_ms: u64) {
        self.is_playing = playing;
        self.update_position(position_ms);
    }
}

pub struct SyncEngine {
    pub playback: Arc<Mutex<PlaybackState>>,
    pub lyrics: Arc<Mutex<Option<Vec<LyricLine>>>>,
}

impl SyncEngine {
    pub fn new() -> Self {
        Self {
            playback: Arc::new(Mutex::new(PlaybackState::new())),
            lyrics: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_lyrics(&self, lines: Option<Vec<LyricLine>>) {
        *self.lyrics.lock().unwrap() = lines;
    }

    pub async fn run(&self, tx: broadcast::Sender<Event>) {
        let mut tick = interval(Duration::from_millis(50));
        let mut last_line_index: Option<usize> = None;

        loop {
            tick.tick().await;

            let pos;
            let is_playing;
            {
                let state = self.playback.lock().unwrap();
                is_playing = state.is_playing;
                pos = state.current_position_ms();
            }

            if !is_playing {
                continue;
            }

            let lyrics_guard = self.lyrics.lock().unwrap();
            let lines = match lyrics_guard.as_ref() {
                Some(l) => l,
                None => continue,
            };

            let current = find_current_line(lines, pos);

            if current != last_line_index {
                last_line_index = current;
                let event = match current {
                    Some(idx) => {
                        let next = if idx + 1 < lines.len() {
                            Some(LineInfo {
                                text: lines[idx + 1].text.clone(),
                                start_ms: lines[idx + 1].start_ms,
                            })
                        } else {
                            None
                        };
                        Event::Line {
                            current: LineInfo {
                                text: lines[idx].text.clone(),
                                start_ms: lines[idx].start_ms,
                            },
                            next,
                            position_ms: pos,
                        }
                    }
                    None => Event::Line {
                        current: LineInfo {
                            text: String::new(),
                            start_ms: 0,
                        },
                        next: None,
                        position_ms: pos,
                    },
                };
                let _ = tx.send(event);
            }
        }
    }
}

fn find_current_line(lines: &[LyricLine], position_ms: u64) -> Option<usize> {
    match lines.binary_search_by(|line| line.start_ms.cmp(&position_ms)) {
        Ok(i) => Some(i),
        Err(0) => None,
        Err(i) => {
            let idx = i - 1;
            if position_ms <= lines[idx].end_ms {
                Some(idx)
            } else {
                None
            }
        }
    }
}
