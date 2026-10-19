'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { createProvider, extractResults, mediaUrl, MAX_GIF_BYTES } = require('../src/provider.cjs');

const media = 'https://media1.giphy.com/media/test/giphy.gif?cid=preserved';
function gif(id = 'testGIF', overrides = {}) {
  return { type: 'gif', id, title: 'Quoted "]" and \\ title', username: 'Creator',
    images: { downsized: { url: media, size: '100', width: '200', height: '100' } }, ...overrides };
}
function page(gifs) {
  const frame = JSON.stringify([1, `1:${JSON.stringify({ initialGifs: gifs })}`]).replace(/</g, '\\u003c');
  return `<script>self.__next_f.push(${frame})</script>`;
}

test('reads public result data without running page scripts and preserves media URLs', () => {
  const results = extractResults(page([gif(), gif(), gif('two', { title: '<script>alert(1)</script>' })]));
  assert.equal(results.length, 2);
  assert.equal(results[0].title, 'Quoted "]" and \\ title');
  assert.equal(results[0].url, media);
  assert.equal(results[1].title, '<script>alert(1)</script>');
});

test('accepts empty searches and reports an unexpected page structure', () => {
  assert.deepEqual(extractResults(page([])), []);
  assert.throws(() => extractResults('<script>not a result</script>'), /unavailable/);
});

test('rejects arbitrary hosts, protocols, credentials, ports and oversized renditions', () => {
  for (const url of ['file:///etc/passwd', 'http://media1.giphy.com/media/a.gif',
    'https://media1.giphy.com.evil.test/media/a.gif', 'https://localhost/media/a.gif',
    'https://user:password@media1.giphy.com/media/a.gif', 'https://media1.giphy.com:444/media/a.gif']) {
    assert.equal(mediaUrl(url), null);
  }
  assert.equal(extractResults(page([gif('huge', { images: { original: { url: media, size: MAX_GIF_BYTES + 1 } } })])).length, 0);
});

test('only downloads results returned to the same client and validates GIF content', async () => {
  const calls = [];
  const provider = createProvider(async (url, options) => {
    calls.push({ url, options });
    return new Response(url.startsWith('https://giphy.com/search/') ? page([gif()]) : Buffer.from('GIF89aTEST'));
  });
  await provider.search(1, 'happy /?');
  assert.equal(calls[0].url, 'https://giphy.com/search/happy%20%2F%3F');
  assert.equal(calls[0].options.credentials, 'omit');
  await assert.rejects(provider.download(2, 'testGIF'), /Search again/);
  await assert.rejects(provider.download(1, 'unknown'), /Search again/);
  const file = await provider.download(1, 'testGIF');
  assert.equal(file.type, 'image/gif');
  assert.equal(Buffer.from(file.bytes).subarray(0, 6).toString(), 'GIF89a');
  assert.equal(calls.at(-1).url, media);
  provider.cancel(1);
  await assert.rejects(provider.download(1, 'testGIF'), /Search again/);
});

test('rejects HTML downloads and advertised oversized responses', async () => {
  let big = false;
  const provider = createProvider(async url => new Response(url.startsWith('https://giphy.com/search/')
    ? page([gif()]) : '<html>blocked</html>', { headers: big ? { 'content-length': String(MAX_GIF_BYTES + 1) } : {} }));
  await provider.search(1, 'happy');
  await assert.rejects(provider.download(1, 'testGIF'), /could not be added/);
  big = true;
  await assert.rejects(provider.download(1, 'testGIF'), /too large/);
});

test('cancels in-flight searches when the picker closes', async () => {
  const provider = createProvider((url, options) => new Promise((resolve, reject) => {
    options.signal.addEventListener('abort', () => reject(new DOMException('Cancelled', 'AbortError')));
  }));
  const pending = provider.search(1, 'happy');
  provider.cancel(1);
  await assert.rejects(pending, { name: 'AbortError' });
});
