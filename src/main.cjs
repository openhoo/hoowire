'use strict';

const { app, ipcMain, net } = require('electron');
const { pathToFileURL } = require('node:url');
const path = require('node:path');
const { createProvider } = require('./provider.cjs');

const provider = createProvider((...args) => net.fetch(...args));
const watched = new Set();

function checkSender(event) {
  const allowed = pathToFileURL(path.join(app.getAppPath(), 'build', 'index.html')).href;
  const frame = event.senderFrame;
  if (!frame || frame !== event.sender.mainFrame || frame.url.split(/[?#]/)[0] !== allowed) {
    throw new Error('GIF search is only available in the Teamwire window.');
  }
  if (!watched.has(event.sender.id)) {
    watched.add(event.sender.id);
    const id = event.sender.id;
    event.sender.once('destroyed', () => { provider.cancel(id); watched.delete(id); });
  }
  return event.sender.id;
}

ipcMain.handle('teamwire-gif:search', (event, query) => provider.search(checkSender(event), query));
ipcMain.handle('teamwire-gif:download', (event, id) => {
  if (typeof id !== 'string' || !/^[a-zA-Z0-9]{1,80}$/.test(id)) throw new Error('Invalid GIF.');
  return provider.download(checkSender(event), id);
});
ipcMain.handle('teamwire-gif:cancel', event => provider.cancel(checkSender(event)));
