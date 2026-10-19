'use strict';

const { contextBridge, ipcRenderer } = require('electron');
contextBridge.exposeInMainWorld('teamwireGifAPI', {
  search: query => ipcRenderer.invoke('teamwire-gif:search', query),
  download: id => ipcRenderer.invoke('teamwire-gif:download', id),
  cancel: () => ipcRenderer.invoke('teamwire-gif:cancel'),
});
