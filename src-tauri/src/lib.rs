mod events;
mod lyrics;
mod sync;

use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use log::{info, warn};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::broadcast;
use tokio::time::{interval, Duration};

use crate::events::Event;
use crate::sync::SyncEngine;

const SPOTIFY_CLIENT_ID: &str = "a141922c57214d0b8dc977df36d1c494";

// --- macOS cursor position via CoreGraphics FFI ---

#[cfg(target_os = "macos")]
mod cursor {
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGPoint {
        pub x: f64,
        pub y: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventCreate(source: *const std::ffi::c_void) -> *mut std::ffi::c_void;
        fn CGEventGetLocation(event: *const std::ffi::c_void) -> CGPoint;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: *const std::ffi::c_void);
    }

    pub fn position() -> (f64, f64) {
        unsafe {
            let event = CGEventCreate(std::ptr::null());
            let point = CGEventGetLocation(event);
            CFRelease(event);
            (point.x, point.y)
        }
    }
}

// --- Shared interaction state ---

struct Interaction {
    active: AtomicBool,
    locked: AtomicBool,
}

// JS calls this on mouseleave to return to click-through
#[tauri::command]
fn deactivate(app: AppHandle, state: tauri::State<'_, Arc<Interaction>>) {
    state.active.store(false, Ordering::Relaxed);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_ignore_cursor_events(true);
    }
    let _ = app.emit("interactive", false);
}

// JS calls this on mousedown to start native window drag
#[tauri::command]
fn start_drag(window: tauri::WebviewWindow) {
    let _ = window.start_dragging();
}

// --- Settings commands ---

#[tauri::command]
fn get_settings(state: tauri::State<'_, AppSettings>) -> Settings {
    state.0.lock().unwrap().clone()
}

#[tauri::command]
fn update_setting(
    app: AppHandle,
    state: tauri::State<'_, AppSettings>,
    key: String,
    value: String,
) {
    let mut settings = state.0.lock().unwrap();
    match key.as_str() {
        "theme" => settings.theme = value,
        "font_size" => settings.font_size = value,
        _ => return,
    }
    save_settings(&settings);
    let _ = app.emit("settings-changed", settings.clone());
}

#[tauri::command]
fn toggle_lock(
    app: AppHandle,
    settings_state: tauri::State<'_, AppSettings>,
    inter_state: tauri::State<'_, Arc<Interaction>>,
) {
    let mut settings = settings_state.0.lock().unwrap();
    settings.locked = !settings.locked;
    save_settings(&settings);

    inter_state.locked.store(settings.locked, Ordering::Relaxed);

    if settings.locked {
        inter_state.active.store(false, Ordering::Relaxed);
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.set_ignore_cursor_events(true);
        }
        let _ = app.emit("interactive", false);
    }

    let _ = app.emit("settings-changed", settings.clone());
}

#[tauri::command]
fn open_settings(app: AppHandle) {
    if let Some(window) = app.get_webview_window("settings") {
        let _ = window.set_focus();
        return;
    }

    let _ = tauri::WebviewWindowBuilder::new(
        &app,
        "settings",
        tauri::WebviewUrl::App("settings.html".into()),
    )
    .title("Playa Settings")
    .inner_size(340.0, 320.0)
    .decorations(false)
    .transparent(true)
    .resizable(false)
    .center()
    .always_on_top(true)
    .build();
}

#[tauri::command]
fn close_window(window: tauri::WebviewWindow) {
    let _ = window.close();
}

// --- Token management ---

fn data_dir() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".playa")
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedToken {
    access_token: String,
    refresh_token: String,
}

fn save_token(token: &SavedToken) {
    let dir = data_dir();
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(
        dir.join("token.json"),
        serde_json::to_string(token).unwrap(),
    );
}

fn load_token() -> Option<SavedToken> {
    let path = data_dir().join("token.json");
    let data = fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

// --- Settings management ---

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
struct Settings {
    theme: String,
    font_size: String,
    locked: bool,
    #[serde(default = "default_width")]
    window_width: f64,
    #[serde(default = "default_height")]
    window_height: f64,
}

fn default_width() -> f64 { 800.0 }
fn default_height() -> f64 { 150.0 }

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: "default".into(),
            font_size: "medium".into(),
            locked: false,
            window_width: default_width(),
            window_height: default_height(),
        }
    }
}

struct AppSettings(std::sync::Mutex<Settings>);

fn save_settings(settings: &Settings) {
    let dir = data_dir();
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(
        dir.join("settings.json"),
        serde_json::to_string(settings).unwrap(),
    );
}

fn load_settings() -> Settings {
    let path = data_dir().join("settings.json");
    match fs::read_to_string(path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => Settings::default(),
    }
}

async fn refresh_access_token(
    client: &reqwest::Client,
    refresh_token: &str,
) -> Option<SavedToken> {
    let resp = client
        .post("https://accounts.spotify.com/api/token")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", SPOTIFY_CLIENT_ID),
        ])
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        warn!("Token refresh failed: {}", resp.status());
        return None;
    }

    #[derive(serde::Deserialize)]
    struct RefreshResponse {
        access_token: String,
        refresh_token: Option<String>,
    }

    let body: RefreshResponse = resp.json().await.ok()?;
    Some(SavedToken {
        access_token: body.access_token,
        refresh_token: body
            .refresh_token
            .unwrap_or_else(|| refresh_token.to_string()),
    })
}

fn do_oauth_login() -> SavedToken {
    info!("Opening browser for Spotify login...");
    let oauth_token = librespot_oauth::OAuthClientBuilder::new(
        SPOTIFY_CLIENT_ID,
        "http://127.0.0.1:8898/login",
        vec![
            "streaming",
            "user-read-currently-playing",
            "user-read-playback-state",
        ],
    )
    .open_in_browser()
    .build()
    .expect("Failed to build OAuth client")
    .get_access_token()
    .expect("Failed to get access token");

    let saved = SavedToken {
        access_token: oauth_token.access_token,
        refresh_token: oauth_token.refresh_token,
    };
    save_token(&saved);
    saved
}

// --- Spotify API types ---

#[derive(serde::Deserialize, Debug)]
struct SpotifyCurrentlyPlaying {
    is_playing: bool,
    progress_ms: Option<u64>,
    item: Option<SpotifyTrackItem>,
}

#[derive(serde::Deserialize, Debug)]
struct SpotifyTrackItem {
    id: Option<String>,
    name: String,
    duration_ms: u64,
    artists: Vec<SpotifyArtist>,
    album: Option<SpotifyAlbum>,
}

#[derive(serde::Deserialize, Debug)]
struct SpotifyArtist {
    name: String,
}

#[derive(serde::Deserialize, Debug)]
struct SpotifyAlbum {
    name: String,
}

// --- App entry ---

pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    info!("Starting Playa");

    let settings = load_settings();

    let interaction = Arc::new(Interaction {
        active: AtomicBool::new(false),
        locked: AtomicBool::new(settings.locked),
    });

    let saved_width = settings.window_width;
    let saved_height = settings.window_height;
    let app_settings = AppSettings(std::sync::Mutex::new(settings));

    let inter_shortcut = interaction.clone();

    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, _shortcut, event| {
                    if event.state == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        let is_active = inter_shortcut.active.load(Ordering::Relaxed);
                        let new_active = !is_active;
                        inter_shortcut.active.store(new_active, Ordering::Relaxed);

                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.set_ignore_cursor_events(!new_active);
                        }
                        let _ = app.emit("interactive", new_active);
                    }
                })
                .build(),
        )
        .manage(interaction.clone())
        .manage(app_settings)
        .invoke_handler(tauri::generate_handler![
            deactivate,
            start_drag,
            get_settings,
            update_setting,
            toggle_lock,
            open_settings,
            close_window,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();

            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_ignore_cursor_events(true);
                let _ = window.set_size(tauri::Size::Logical(tauri::LogicalSize::new(saved_width, saved_height)));

                let handle_resize = app.handle().clone();
                let window_clone = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::Resized(size) = event {
                        let scale = window_clone.scale_factor().unwrap_or(1.0);
                        let w = size.width as f64 / scale;
                        let h = size.height as f64 / scale;
                        let state = handle_resize.state::<AppSettings>();
                        let mut s = state.0.lock().unwrap();
                        s.window_width = w;
                        s.window_height = h;
                        save_settings(&s);
                    }
                });
            }

            use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut};
            let shortcut = Shortcut::new(Some(Modifiers::META | Modifiers::SHIFT), Code::KeyL);
            app.global_shortcut().register(shortcut)?;
            info!("Cmd+Shift+L to toggle interaction");

            // Hover-to-interact: polls cursor position, activates when cursor enters the overlay
            let inter_poll = interaction;
            let handle_poll = handle.clone();
            tauri::async_runtime::spawn(async move {
                hover_poll(handle_poll, inter_poll).await;
            });

            tauri::async_runtime::spawn(async move {
                run_backend(handle).await;
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Failed to run Playa");
}

// --- Hover-to-interact polling ---

#[cfg(target_os = "macos")]
async fn hover_poll(app: AppHandle, state: Arc<Interaction>) {
    let mut tick = interval(Duration::from_millis(100));
    let mut was_near = false;

    loop {
        tick.tick().await;

        let window = match app.get_webview_window("main") {
            Some(w) => w,
            None => continue,
        };

        let scale = window.scale_factor().unwrap_or(1.0);
        let pos = match window.outer_position() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let size = match window.outer_size() {
            Ok(s) => s,
            Err(_) => continue,
        };

        let win_x = pos.x as f64 / scale;
        let win_y = pos.y as f64 / scale;
        let win_w = size.width as f64 / scale;
        let win_h = size.height as f64 / scale;

        let (cx, cy) = cursor::position();

        let margin = 60.0;
        let near = cx >= win_x - margin && cx <= win_x + win_w + margin
                && cy >= win_y - margin && cy <= win_y + win_h + margin;
        if near != was_near {
            was_near = near;
            let _ = app.emit("proximity", near);
        }

        let is_locked = state.locked.load(Ordering::Relaxed);

        if state.active.load(Ordering::Relaxed) {
            let should_deactivate = if is_locked {
                let icon_size = 44.0;
                !(cx >= win_x && cx <= win_x + icon_size && cy >= win_y && cy <= win_y + icon_size)
            } else {
                !(cx >= win_x && cx <= win_x + win_w && cy >= win_y && cy <= win_y + win_h)
            };
            if should_deactivate {
                state.active.store(false, Ordering::Relaxed);
                let _ = window.set_ignore_cursor_events(true);
                let _ = app.emit("interactive", false);
            }
            continue;
        }

        let in_bounds = if is_locked {
            let icon_size = 44.0;
            cx >= win_x && cx <= win_x + icon_size && cy >= win_y && cy <= win_y + icon_size
        } else {
            cx >= win_x && cx <= win_x + win_w && cy >= win_y && cy <= win_y + win_h
        };

        if in_bounds {
            state.active.store(true, Ordering::Relaxed);
            let _ = window.set_ignore_cursor_events(false);
            let _ = app.emit("interactive", true);
        }
    }
}

#[cfg(not(target_os = "macos"))]
async fn hover_poll(_app: AppHandle, _state: Arc<Interaction>) {
    std::future::pending::<()>().await;
}

// --- Spotify backend ---

async fn run_backend(app: AppHandle) {
    let http_client = reqwest::Client::new();

    let mut token = if let Some(saved) = load_token() {
        info!("Refreshing saved token...");
        match refresh_access_token(&http_client, &saved.refresh_token).await {
            Some(t) => {
                info!("Token refreshed");
                save_token(&t);
                t
            }
            None => {
                warn!("Refresh failed, need fresh login");
                match tauri::async_runtime::spawn_blocking(do_oauth_login).await {
                    Ok(t) => t,
                    Err(e) => {
                        warn!("OAuth failed: {}", e);
                        return;
                    }
                }
            }
        }
    } else {
        match tauri::async_runtime::spawn_blocking(do_oauth_login).await {
            Ok(t) => t,
            Err(e) => {
                warn!("OAuth failed: {}", e);
                return;
            }
        }
    };

    let (tx, _rx) = broadcast::channel::<Event>(64);
    let sync_engine = Arc::new(SyncEngine::new());

    let mut event_rx = tx.subscribe();
    let app_for_events = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Ok(event) = event_rx.recv().await {
            let _ = app_for_events.emit("playa", &event);
        }
    });

    let sync_tx = tx.clone();
    let sync_ref = sync_engine.clone();
    tauri::async_runtime::spawn(async move {
        sync_ref.run(sync_tx).await;
    });

    info!("Playa is running — play music in Spotify to see lyrics");

    let mut poll_interval = interval(Duration::from_secs(2));
    let mut current_track_id = String::new();

    loop {
        poll_interval.tick().await;

        let resp = http_client
            .get("https://api.spotify.com/v1/me/player/currently-playing")
            .bearer_auth(&token.access_token)
            .send()
            .await;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                warn!("Spotify API request failed: {}", e);
                continue;
            }
        };

        if resp.status().as_u16() == 401 {
            info!("Token expired, refreshing...");
            if let Some(new_token) =
                refresh_access_token(&http_client, &token.refresh_token).await
            {
                save_token(&new_token);
                token = new_token;
            } else {
                warn!("Token refresh failed");
            }
            continue;
        }

        if resp.status().as_u16() == 204 {
            continue;
        }

        if !resp.status().is_success() {
            warn!("Spotify API returned {}", resp.status());
            continue;
        }

        let body = match resp.json::<SpotifyCurrentlyPlaying>().await {
            Ok(b) => b,
            Err(e) => {
                warn!("Failed to parse Spotify response: {}", e);
                continue;
            }
        };

        let position_ms = body.progress_ms.unwrap_or(0);

        sync_engine
            .playback
            .lock()
            .unwrap()
            .set_playing(body.is_playing, position_ms);

        let _ = tx.send(Event::State {
            playing: body.is_playing,
            position_ms,
        });

        if let Some(item) = &body.item {
            let track_id = item.id.clone().unwrap_or_default();

            if track_id != current_track_id && !track_id.is_empty() {
                current_track_id = track_id.clone();

                let artist = item
                    .artists
                    .first()
                    .map(|a| a.name.clone())
                    .unwrap_or_default();
                let album = item
                    .album
                    .as_ref()
                    .map(|a| a.name.clone())
                    .unwrap_or_default();

                info!("Now playing: {} - {}", artist, item.name);

                let _ = tx.send(Event::Track {
                    title: item.name.clone(),
                    artist: artist.clone(),
                    album,
                    track_id: track_id.clone(),
                    duration_ms: item.duration_ms as u32,
                });

                let duration_secs = item.duration_ms / 1000;
                match lyrics::fetch_lyrics(&http_client, &item.name, &artist, duration_secs).await
                {
                    Some(lines) => {
                        info!("Lyrics loaded ({} lines)", lines.len());
                        let _ = tx.send(Event::Lyrics {
                            lines: lines.clone(),
                            sync_type: "line".into(),
                        });
                        sync_engine.set_lyrics(Some(lines));
                    }
                    None => {
                        info!("No synced lyrics available");
                        let _ = tx.send(Event::NoLyrics);
                        sync_engine.set_lyrics(None);
                    }
                }
            }
        }
    }
}
