'use strict';

const MAX_HTML_BYTES = 5 * 1024 * 1024;
const MAX_GIF_BYTES = 20 * 1024 * 1024;
const TIMEOUT_MS = 15_000;

function mediaUrl(value) {
  try {
    const url = new URL(value);
    if (url.protocol !== 'https:' || !/^media\d*\.giphy\.com$/.test(url.hostname) ||
        !url.pathname.startsWith('/media/') || url.username || url.password || url.port) return null;
    return url.href;
  } catch {
    return null;
  }
}

// Read the public page's data, never execute any downloaded JavaScript.
function readJsonArray(text, marker) {
  const markerIndex = text.indexOf(marker);
  if (markerIndex < 0) return null;
  let start = markerIndex + marker.length;
  while (/\s/.test(text[start] || '') && start < text.length) start++;
  if (text[start] !== '[') return null;
  let depth = 0;
  let quoted = false;
  let escaped = false;
  for (let index = start; index < text.length; index++) {
    const character = text[index];
    if (quoted) {
      if (escaped) escaped = false;
      else if (character === '\\') escaped = true;
      else if (character === '"') quoted = false;
    } else if (character === '"') quoted = true;
    else if (character === '[') depth++;
    else if (character === ']' && --depth === 0) return JSON.parse(text.slice(start, index + 1));
  }
  return null;
}

function extractResults(html) {
  const chunks = [];
  for (const match of html.matchAll(/self\.__next_f\.push\(([\s\S]*?)\)<\/script>/g)) {
    try {
      const frame = JSON.parse(match[1]);
      if (Array.isArray(frame) && typeof frame[1] === 'string') chunks.push(frame[1]);
    } catch { /* A non-data script cannot contribute any results. */ }
  }
  const data = chunks.join('');
  const gifs = readJsonArray(data, '"initialGifs":');
  if (!gifs) throw new Error('GIPHY search is temporarily unavailable. Please try again.');
  const seen = new Set();
  const results = [];
  for (const gif of gifs) {
    if (gif.type !== 'gif' || !/^[a-zA-Z0-9]+$/.test(gif.id || '') || seen.has(gif.id)) continue;
    const images = gif.images || {};
    const rendition = [images.downsized, images.original, images.fixed_height]
      .find(item => item && mediaUrl(item.url) && /\.gif$/i.test(new URL(item.url).pathname) &&
        (!Number(item.size) || Number(item.size) <= MAX_GIF_BYTES));
    if (!rendition) continue;
    const preview = images.fixed_width || images.fixed_height || rendition;
    const previewUrl = mediaUrl(preview.webp) || mediaUrl(preview.url) || mediaUrl(rendition.url);
    seen.add(gif.id);
    results.push({
      id: gif.id,
      title: String(gif.title || 'GIF').slice(0, 180),
      creator: String(gif.user?.display_name || gif.username || '').slice(0, 100),
      url: mediaUrl(rendition.url),
      previewUrl,
      width: Math.max(1, Math.min(4096, Number(preview.width) || 200)),
      height: Math.max(1, Math.min(4096, Number(preview.height) || 200)),
    });
    if (results.length >= 50) break;
  }
  return results;
}

async function boundedBody(response, maximum, signal) {
  if (!response.ok) throw new Error(response.status === 429
    ? 'GIPHY is busy. Please try again in a moment.'
    : 'Could not reach GIPHY. Please try again.');
  if (Number(response.headers.get('content-length')) > maximum) throw new Error('This GIF is too large. Choose another one.');
  const reader = response.body.getReader();
  const chunks = [];
  let size = 0;
  try {
    while (true) {
      if (signal.aborted) throw new DOMException('Cancelled', 'AbortError');
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > maximum) throw new Error('This GIF is too large. Choose another one.');
      chunks.push(Buffer.from(value));
    }
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
  return Buffer.concat(chunks, size);
}

function createProvider(fetchRequest) {
  const clients = new Map();

  function stateFor(clientId) {
    if (!clients.has(clientId)) clients.set(clientId, { allowed: new Map(), search: null, download: null });
    return clients.get(clientId);
  }

  async function request(state, kind, url, limit, accept) {
    state[kind]?.abort();
    const controller = new AbortController();
    state[kind] = controller;
    const timer = setTimeout(() => controller.abort(), TIMEOUT_MS);
    try {
      const response = await fetchRequest(url, {
        method: 'GET', credentials: 'omit', redirect: 'error', cache: 'no-store',
        referrerPolicy: 'no-referrer', headers: { Accept: accept }, signal: controller.signal,
      });
      return await boundedBody(response, limit, controller.signal);
    } finally {
      clearTimeout(timer);
      if (state[kind] === controller) state[kind] = null;
    }
  }

  return {
    async search(clientId, input) {
      if (typeof input !== 'string' || input.length > 100) throw new Error('Please use a shorter search.');
      const query = input.trim() || 'reactions';
      const state = stateFor(clientId);
      const bytes = await request(state, 'search', `https://giphy.com/search/${encodeURIComponent(query)}`,
        MAX_HTML_BYTES, 'text/html');
      const results = extractResults(bytes.toString('utf8'));
      state.allowed = new Map(results.map(result => [result.id, { result, expires: Date.now() + 10 * 60_000 }]));
      return { results, query };
    },

    async download(clientId, id) {
      const state = stateFor(clientId);
      const allowed = state.allowed.get(id);
      if (!allowed || allowed.expires < Date.now()) throw new Error('Search again to select this GIF.');
      const bytes = await request(state, 'download', allowed.result.url, MAX_GIF_BYTES, 'image/gif');
      if (!['GIF87a', 'GIF89a'].includes(bytes.subarray(0, 6).toString('ascii'))) {
        throw new Error('This image could not be added as a GIF. Choose another one.');
      }
      return { bytes: new Uint8Array(bytes), name: `giphy-${id}.gif`, type: 'image/gif' };
    },

    cancel(clientId) {
      const state = clients.get(clientId);
      state?.search?.abort();
      state?.download?.abort();
      clients.delete(clientId);
    },
  };
}

module.exports = { createProvider, extractResults, mediaUrl, readJsonArray, MAX_GIF_BYTES };
