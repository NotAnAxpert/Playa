use log::{info, warn};

use crate::events::LyricLine;

const LRCLIB_API: &str = "https://lrclib.net/api/get";

#[derive(serde::Deserialize, Debug)]
struct LrclibResponse {
    #[serde(rename = "syncedLyrics")]
    synced_lyrics: Option<String>,
}

pub async fn fetch_lyrics(
    client: &reqwest::Client,
    title: &str,
    artist: &str,
    duration_secs: u64,
) -> Option<Vec<LyricLine>> {
    let resp = client
        .get(LRCLIB_API)
        .query(&[
            ("track_name", title),
            ("artist_name", artist),
            ("duration", &duration_secs.to_string()),
        ])
        .header("User-Agent", "Playa/0.1.0")
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        warn!("LRCLIB returned {}", resp.status());
        return None;
    }

    let body: LrclibResponse = resp.json().await.ok()?;
    let synced = body.synced_lyrics?;

    let lines = parse_lrc(&synced);

    if lines.is_empty() {
        None
    } else {
        info!("Loaded {} synced lyric lines from LRCLIB", lines.len());
        Some(lines)
    }
}

fn parse_lrc(lrc: &str) -> Vec<LyricLine> {
    let mut lines = Vec::new();

    for line in lrc.lines() {
        let line = line.trim();
        if !line.starts_with('[') {
            continue;
        }

        let close = match line.find(']') {
            Some(i) => i,
            None => continue,
        };

        let timestamp = &line[1..close];
        let text = line[close + 1..].trim().to_string();

        if text.is_empty() {
            continue;
        }

        if let Some(ms) = parse_timestamp(timestamp) {
            lines.push(LyricLine {
                text,
                start_ms: ms,
                end_ms: 0,
                words: vec![],
            });
        }
    }

    for i in 0..lines.len() {
        if i + 1 < lines.len() {
            lines[i].end_ms = lines[i + 1].start_ms;
        } else {
            lines[i].end_ms = lines[i].start_ms + 5000;
        }
    }

    lines
}

fn parse_timestamp(ts: &str) -> Option<u64> {
    let parts: Vec<&str> = ts.split(':').collect();
    if parts.len() != 2 {
        return None;
    }
    let minutes: u64 = parts[0].parse().ok()?;
    let sec_parts: Vec<&str> = parts[1].split('.').collect();
    let seconds: u64 = sec_parts[0].parse().ok()?;
    let centiseconds: u64 = if sec_parts.len() > 1 {
        let frac = sec_parts[1];
        match frac.len() {
            1 => frac.parse::<u64>().ok()? * 100,
            2 => frac.parse::<u64>().ok()? * 10,
            3 => frac.parse::<u64>().ok()?,
            _ => frac[..3].parse::<u64>().ok()?,
        }
    } else {
        0
    };

    Some(minutes * 60_000 + seconds * 1_000 + centiseconds)
}
