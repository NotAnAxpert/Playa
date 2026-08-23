// --- DOM elements ---
const loginScreen = document.getElementById('login-screen');
const loginBtn = document.getElementById('login-btn');
const loginError = document.getElementById('login-error');
const lyricsView = document.getElementById('lyrics-view');
const currentEl = document.getElementById('current');
const nextEl = document.getElementById('next');
const trackEl = document.getElementById('track');
const gearBtn = document.getElementById('gear');
const settingsClose = document.getElementById('settings-close');
const disconnectBtn = document.getElementById('disconnect-btn');

// --- State ---
let currentTrackId = '';
let pollInterval = null;

// --- Token helpers ---

async function getToken() {
  const resp = await chrome.runtime.sendMessage({ action: 'getToken' });
  if (resp.error) return null;
  return resp.token;
}

async function login() {
  loginBtn.disabled = true;
  loginError.style.display = 'none';
  const resp = await chrome.runtime.sendMessage({ action: 'login' });
  loginBtn.disabled = false;
  if (resp.error) {
    loginError.textContent = 'Login failed. Please try again.';
    loginError.style.display = 'block';
    return;
  }
  showLyricsView();
  startPolling();
}

async function logout() {
  stopPolling();
  await chrome.runtime.sendMessage({ action: 'logout' });
  currentTrackId = '';
  syncEngine.setLyrics(null);
  showLoginScreen();
  document.body.classList.remove('settings-open');
}

// --- Screen switching ---

function showLoginScreen() {
  loginScreen.classList.add('active');
  lyricsView.classList.remove('active');
}

function showLyricsView() {
  loginScreen.classList.remove('active');
  lyricsView.classList.add('active');
}

// --- LRC parsing (ported from lyrics.rs) ---

function parseTimestamp(ts) {
  const parts = ts.split(':');
  if (parts.length !== 2) return null;
  const minutes = parseInt(parts[0], 10);
  if (isNaN(minutes)) return null;

  const secParts = parts[1].split('.');
  const seconds = parseInt(secParts[0], 10);
  if (isNaN(seconds)) return null;

  let ms = 0;
  if (secParts.length > 1) {
    const frac = secParts[1];
    const val = parseInt(frac, 10);
    if (isNaN(val)) return null;
    if (frac.length === 1) ms = val * 100;
    else if (frac.length === 2) ms = val * 10;
    else if (frac.length === 3) ms = val;
    else ms = parseInt(frac.substring(0, 3), 10);
  }

  return minutes * 60000 + seconds * 1000 + ms;
}

function parseLrc(lrcText) {
  const lines = [];

  for (const raw of lrcText.split('\n')) {
    const line = raw.trim();
    if (!line.startsWith('[')) continue;
    const close = line.indexOf(']');
    if (close === -1) continue;

    const timestamp = line.substring(1, close);
    const text = line.substring(close + 1).trim();
    if (!text) continue;

    const startMs = parseTimestamp(timestamp);
    if (startMs === null) continue;

    lines.push({ text, startMs, endMs: 0 });
  }

  for (let i = 0; i < lines.length; i++) {
    lines[i].endMs = i + 1 < lines.length
      ? lines[i + 1].startMs
      : lines[i].startMs + 5000;
  }

  return lines.length > 0 ? lines : null;
}

// --- Lyrics fetching (ported from lyrics.rs) ---

async function fetchLyrics(title, artist, durationSecs) {
  const url = new URL('https://lrclib.net/api/get');
  url.searchParams.set('track_name', title);
  url.searchParams.set('artist_name', artist);
  url.searchParams.set('duration', String(durationSecs));

  try {
    const resp = await fetch(url, {
      headers: { 'User-Agent': 'Playa/1.0.0' },
    });
    if (!resp.ok) return null;
    const data = await resp.json();
    if (!data.syncedLyrics) return null;
    return parseLrc(data.syncedLyrics);
  } catch {
    return null;
  }
}

// --- Sync engine (ported from sync.rs) ---

const syncEngine = {
  isPlaying: false,
  anchorTime: performance.now(),
  anchorPositionMs: 0,
  lyrics: null,
  lastLineIndex: null,
  tickId: null,

  currentPositionMs() {
    if (!this.isPlaying) return this.anchorPositionMs;
    return this.anchorPositionMs + (performance.now() - this.anchorTime);
  },

  setPlayback(playing, positionMs) {
    this.isPlaying = playing;
    this.anchorTime = performance.now();
    this.anchorPositionMs = positionMs;
  },

  setLyrics(lines) {
    this.lyrics = lines;
    this.lastLineIndex = null;
  },

  findCurrentLine(positionMs) {
    const lines = this.lyrics;
    if (!lines || lines.length === 0) return null;

    let lo = 0, hi = lines.length;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (lines[mid].startMs <= positionMs) lo = mid + 1;
      else hi = mid;
    }

    if (lo === 0) return null;
    const idx = lo - 1;
    return positionMs <= lines[idx].endMs ? idx : null;
  },

  tick() {
    if (!this.isPlaying || !this.lyrics) return;

    const pos = this.currentPositionMs();
    const current = this.findCurrentLine(pos);

    if (current !== this.lastLineIndex) {
      this.lastLineIndex = current;

      if (current !== null) {
        currentEl.textContent = this.lyrics[current].text;
        currentEl.className = '';
        nextEl.textContent = current + 1 < this.lyrics.length
          ? this.lyrics[current + 1].text
          : '';
      } else {
        currentEl.textContent = '';
        currentEl.className = '';
        nextEl.textContent = '';
      }
    }
  },

  start() {
    if (this.tickId) return;
    this.tickId = setInterval(() => this.tick(), 50);
  },

  stop() {
    if (this.tickId) {
      clearInterval(this.tickId);
      this.tickId = null;
    }
  },
};

// --- Spotify polling (ported from lib.rs run_backend) ---

async function pollSpotify() {
  const token = await getToken();
  if (!token) {
    stopPolling();
    showLoginScreen();
    return;
  }

  let resp;
  try {
    resp = await fetch('https://api.spotify.com/v1/me/player/currently-playing', {
      headers: { Authorization: 'Bearer ' + token },
    });
  } catch {
    return;
  }

  if (resp.status === 401) return;
  if (resp.status === 204) return;
  if (!resp.ok) return;

  let body;
  try {
    body = await resp.json();
  } catch {
    return;
  }

  const positionMs = body.progress_ms || 0;
  syncEngine.setPlayback(body.is_playing, positionMs);

  if (!body.item) return;

  const trackId = body.item.id || '';
  if (trackId && trackId !== currentTrackId) {
    currentTrackId = trackId;

    const artist = body.item.artists?.[0]?.name || '';
    trackEl.textContent = artist + ' — ' + body.item.name;

    const durationSecs = Math.floor(body.item.duration_ms / 1000);
    const lines = await fetchLyrics(body.item.name, artist, durationSecs);

    if (lines) {
      syncEngine.setLyrics(lines);
      currentEl.textContent = '';
      currentEl.className = '';
      nextEl.textContent = '';
    } else {
      syncEngine.setLyrics(null);
      currentEl.textContent = 'No lyrics available';
      currentEl.className = 'no-lyrics';
      nextEl.textContent = '';
    }
  }
}

function startPolling() {
  syncEngine.start();
  pollSpotify();
  pollInterval = setInterval(pollSpotify, 2000);
}

function stopPolling() {
  syncEngine.stop();
  if (pollInterval) {
    clearInterval(pollInterval);
    pollInterval = null;
  }
}

// --- Settings ---

function applySettings(settings) {
  document.body.dataset.theme = settings.theme || 'default';
  document.body.dataset.fontSize = settings.fontSize || 'medium';

  document.querySelectorAll('.theme-card').forEach(c => {
    c.classList.toggle('active', c.dataset.theme === settings.theme);
  });
  document.querySelectorAll('#sizes button').forEach(b => {
    b.classList.toggle('active', b.dataset.size === settings.fontSize);
  });
}

async function loadSettings() {
  const stored = await chrome.storage.local.get(['theme', 'fontSize']);
  applySettings({
    theme: stored.theme || 'default',
    fontSize: stored.fontSize || 'medium',
  });
}

async function updateSetting(key, value) {
  await chrome.storage.local.set({ [key]: value });
  const stored = await chrome.storage.local.get(['theme', 'fontSize']);
  applySettings({
    theme: stored.theme || 'default',
    fontSize: stored.fontSize || 'medium',
  });
}

// --- Event listeners ---

loginBtn.addEventListener('click', login);
disconnectBtn.addEventListener('click', logout);

gearBtn.addEventListener('click', () => {
  document.body.classList.toggle('settings-open');
});

settingsClose.addEventListener('click', () => {
  document.body.classList.remove('settings-open');
});

document.querySelectorAll('.theme-card').forEach(card => {
  card.addEventListener('click', () => {
    updateSetting('theme', card.dataset.theme);
  });
});

document.querySelectorAll('#sizes button').forEach(btn => {
  btn.addEventListener('click', () => {
    updateSetting('fontSize', btn.dataset.size);
  });
});

// --- Init ---

(async () => {
  await loadSettings();
  const token = await getToken();
  if (token) {
    showLyricsView();
    startPolling();
  } else {
    showLoginScreen();
  }
})();
