const { contextBridge, ipcRenderer } = require('electron');
contextBridge.exposeInMainWorld('loreviaUpdates', {
  check: () => ipcRenderer.invoke('desktop:check-update'),
  install: version => ipcRenderer.invoke('desktop:install-update', version),
});
