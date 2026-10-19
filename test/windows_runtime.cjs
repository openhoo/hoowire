'use strict';
// Opt-in CI probe of the real Windows app using an empty, isolated profile.
const { setTimeout: delay } = require('node:timers/promises');

async function main() {
  const port = Number(process.argv[2]);
  let target;
  for (let attempt = 0; attempt < 120; attempt++) {
    try {
      const pages = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
      target = pages.find(page => page.type === 'page' && page.url.includes('/build/index.html'));
      if (target) break;
    } catch { /* Wait for the isolated app to create its renderer. */ }
    await delay(500);
  }
  if (!target) throw new Error('Teamwire did not expose its main renderer.');
  const socket = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { socket.addEventListener('open', resolve, { once: true }); socket.addEventListener('error', reject, { once: true }); });
  let nextId = 0;
  const pending = new Map();
  socket.addEventListener('message', event => {
    const data = JSON.parse(event.data);
    const request = pending.get(data.id);
    if (!request) return;
    pending.delete(data.id);
    clearTimeout(request.timer);
    if (data.error) request.reject(new Error(data.error.message));
    else request.resolve(data.result);
  });
  function command(method, params) {
    return new Promise((resolve, reject) => {
      const id = ++nextId;
      const timer = setTimeout(() => { pending.delete(id); reject(new Error(`Timed out: ${method}`)); }, 45_000);
      pending.set(id, { resolve, reject, timer });
      socket.send(JSON.stringify({ id, method, params }));
    });
  }
  async function evaluate(expression) {
    const result = await command('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text);
    return result.result.value;
  }
  try {
    let bridge;
    for (let attempt = 0; attempt < 60; attempt++) {
      bridge = await evaluate('typeof window.teamwireGifAPI?.search === "function" && typeof window.teamwireGifPicker?.open === "function"');
      if (bridge) break;
      await delay(500);
    }
    if (!bridge) throw new Error('The Windows preload bridge or GIF picker did not load.');
    await evaluate('window.teamwireGifAPI.cancel()'); // Real Windows IPC sender validation.
    const label = await evaluate('window.teamwireGifPicker.label("en")');
    if (label !== 'Search GIFs') throw new Error('Unexpected picker label.');
    await evaluate(`(() => {
      const anchor = document.createElement('button');
      anchor.textContent = 'Hoowire CI probe'; document.body.append(anchor);
      window.teamwireGifPicker.open({ anchor, contextKey: 'isolated-ci', language: 'en',
        onSelectedFiles: async files => {
          const bytes = new Uint8Array(await files[0].arrayBuffer());
          window.__hoowireSelected = { name: files[0].name, type: files[0].type, size: files[0].size,
            signature: String.fromCharCode(...bytes.slice(0, 6)), isFile: files[0] instanceof File };
        } });
    })()`);
    const panel = await evaluate('!!document.querySelector("[data-teamwire-gif-picker]")?.shadowRoot.querySelector("[role=dialog]")');
    if (!panel) throw new Error('The GIF dialog did not open in the native Windows app.');
    const result = { bridge: true, senderValidation: true, pickerDialog: true, giphy: 'unavailable' };
    for (let attempt = 0; attempt < 50; attempt++) {
      const ready = await evaluate('!!document.querySelector("[data-teamwire-gif-picker]")?.shadowRoot.querySelector("button.gif")');
      if (ready) {
        await evaluate('document.querySelector("[data-teamwire-gif-picker]").shadowRoot.querySelector("button.gif").click()');
        for (let selection = 0; selection < 40; selection++) {
          const file = await evaluate('window.__hoowireSelected || null');
          if (file) {
            if (!file.isFile || file.type !== 'image/gif' || !['GIF87a', 'GIF89a'].includes(file.signature) || file.size <= 6) {
              throw new Error('Windows selection did not produce a valid GIF File.');
            }
            result.giphy = 'search-and-selection-passed';
            result.file = file;
            break;
          }
          await delay(500);
        }
        break;
      }
      const failed = await evaluate('!!document.querySelector("[data-teamwire-gif-picker]")?.shadowRoot.querySelector(".retry")');
      if (failed) break;
      await delay(500);
    }
    // GIPHY is an external service and can deny datacenter IPs. Its result is
    // reported separately from the platform/IPC/UI assertions above.
    await evaluate('window.teamwireGifPicker.closeForContext("isolated-ci")');
    process.stdout.write(JSON.stringify(result) + '\n');
  } finally {
    socket.close();
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
