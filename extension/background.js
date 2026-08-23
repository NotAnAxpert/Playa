const SPOTIFY_CLIENT_ID = 'a141922c57214d0b8dc977df36d1c494';
const SCOPES = 'user-read-currently-playing user-read-playback-state';

let popupWindowId = null;

// --- Window lifecycle ---

chrome.action.onClicked.addListener(async () => {
  if (popupWindowId !== null) {
    try {
      await chrome.windows.update(popupWindowId, { focused: true });
      return;
    } catch {
      popupWindowId = null;
    }
  }

  const stored = await chrome.storage.local.get(['windowWidth', 'windowHeight']);
  const width = stored.windowWidth || 400;
  const height = stored.windowHeight || 500;

  const win = await chrome.windows.create({
    url: 'popup.html',
    type: 'popup',
    width,
    height,
  });

  popupWindowId = win.id;
});

chrome.windows.onRemoved.addListener((windowId) => {
  if (windowId === popupWindowId) {
    popupWindowId = null;
  }
});

chrome.windows.onBoundsChanged.addListener((win) => {
  if (win.id === popupWindowId) {
    chrome.storage.local.set({
      windowWidth: win.width,
      windowHeight: win.height,
    });
  }
});

// --- OAuth PKCE helpers ---

function generateRandomString(length) {
  const array = new Uint8Array(length);
  crypto.getRandomValues(array);
  return btoa(String.fromCharCode(...array))
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/, '');
}

async function sha256(plain) {
  const encoder = new TextEncoder();
  const data = encoder.encode(plain);
  return crypto.subtle.digest('SHA-256', data);
}

function base64UrlEncode(buffer) {
  const bytes = new Uint8Array(buffer);
  return btoa(String.fromCharCode(...bytes))
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/, '');
}

async function doLogin() {
  const codeVerifier = generateRandomString(64);
  const hashed = await sha256(codeVerifier);
  const codeChallenge = base64UrlEncode(hashed);

  const redirectUri = chrome.identity.getRedirectURL('callback');

  const authUrl = new URL('https://accounts.spotify.com/authorize');
  authUrl.searchParams.set('client_id', SPOTIFY_CLIENT_ID);
  authUrl.searchParams.set('response_type', 'code');
  authUrl.searchParams.set('redirect_uri', redirectUri);
  authUrl.searchParams.set('scope', SCOPES);
  authUrl.searchParams.set('code_challenge_method', 'S256');
  authUrl.searchParams.set('code_challenge', codeChallenge);

  const responseUrl = await chrome.identity.launchWebAuthFlow({
    url: authUrl.toString(),
    interactive: true,
  });

  const url = new URL(responseUrl);
  const code = url.searchParams.get('code');
  if (!code) {
    throw new Error('No authorization code received');
  }

  const tokenResp = await fetch('https://accounts.spotify.com/api/token', {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({
      grant_type: 'authorization_code',
      code,
      redirect_uri: redirectUri,
      client_id: SPOTIFY_CLIENT_ID,
      code_verifier: codeVerifier,
    }),
  });

  if (!tokenResp.ok) {
    throw new Error(`Token exchange failed: ${tokenResp.status}`);
  }

  const data = await tokenResp.json();
  await chrome.storage.local.set({
    access_token: data.access_token,
    refresh_token: data.refresh_token,
    token_expiry: Date.now() + data.expires_in * 1000,
  });

  return data.access_token;
}

async function refreshToken() {
  const stored = await chrome.storage.local.get(['refresh_token']);
  if (!stored.refresh_token) {
    throw new Error('not_authenticated');
  }

  const resp = await fetch('https://accounts.spotify.com/api/token', {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({
      grant_type: 'refresh_token',
      refresh_token: stored.refresh_token,
      client_id: SPOTIFY_CLIENT_ID,
    }),
  });

  if (!resp.ok) {
    throw new Error(`Token refresh failed: ${resp.status}`);
  }

  const data = await resp.json();
  const updates = {
    access_token: data.access_token,
    token_expiry: Date.now() + data.expires_in * 1000,
  };
  if (data.refresh_token) {
    updates.refresh_token = data.refresh_token;
  }
  await chrome.storage.local.set(updates);

  return data.access_token;
}

async function getValidToken() {
  const stored = await chrome.storage.local.get(['access_token', 'refresh_token', 'token_expiry']);
  if (!stored.refresh_token) {
    throw new Error('not_authenticated');
  }

  if (stored.access_token && stored.token_expiry && Date.now() < stored.token_expiry - 60000) {
    return stored.access_token;
  }

  return refreshToken();
}

// --- Message handler ---

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (message.action === 'login') {
    doLogin()
      .then((token) => sendResponse({ success: true, token }))
      .catch((err) => sendResponse({ error: err.message }));
    return true;
  }

  if (message.action === 'getToken') {
    getValidToken()
      .then((token) => sendResponse({ token }))
      .catch((err) => sendResponse({ error: err.message }));
    return true;
  }

  if (message.action === 'logout') {
    chrome.storage.local.remove(['access_token', 'refresh_token', 'token_expiry'])
      .then(() => sendResponse({ success: true }));
    return true;
  }
});
