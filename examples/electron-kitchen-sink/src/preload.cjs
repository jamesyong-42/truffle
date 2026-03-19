const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('truffle', {
  // MeshNode
  meshCreate: (config) => ipcRenderer.invoke('mesh:create', config),
  meshStart: () => ipcRenderer.invoke('mesh:start'),
  meshStop: () => ipcRenderer.invoke('mesh:stop'),
  meshIsRunning: () => ipcRenderer.invoke('mesh:isRunning'),
  meshLocalDevice: () => ipcRenderer.invoke('mesh:localDevice'),
  meshDeviceId: () => ipcRenderer.invoke('mesh:deviceId'),
  meshDevices: () => ipcRenderer.invoke('mesh:devices'),
  meshDeviceById: (id) => ipcRenderer.invoke('mesh:deviceById', id),
  meshIsPrimary: () => ipcRenderer.invoke('mesh:isPrimary'),
  meshPrimaryId: () => ipcRenderer.invoke('mesh:primaryId'),
  meshRole: () => ipcRenderer.invoke('mesh:role'),
  meshSendEnvelope: (deviceId, ns, type, payload) =>
    ipcRenderer.invoke('mesh:sendEnvelope', deviceId, ns, type, payload),
  meshBroadcastEnvelope: (ns, type, payload) =>
    ipcRenderer.invoke('mesh:broadcastEnvelope', ns, type, payload),
  meshSubscribedNamespaces: () => ipcRenderer.invoke('mesh:subscribedNamespaces'),
  meshAuthStatus: () => ipcRenderer.invoke('mesh:authStatus'),
  meshAuthUrl: () => ipcRenderer.invoke('mesh:authUrl'),
  meshProtocolVersion: () => ipcRenderer.invoke('mesh:protocolVersion'),

  // Store Sync
  syncCreate: (config) => ipcRenderer.invoke('sync:create', config),
  syncStart: () => ipcRenderer.invoke('sync:start'),
  syncStop: () => ipcRenderer.invoke('sync:stop'),
  syncDispose: () => ipcRenderer.invoke('sync:dispose'),
  syncHandleLocalChanged: (storeId, slice) =>
    ipcRenderer.invoke('sync:handleLocalChanged', storeId, slice),
  syncHandleSyncMessage: (from, msgType, payload) =>
    ipcRenderer.invoke('sync:handleSyncMessage', from, msgType, payload),
  syncHandleDeviceDiscovered: (deviceId) =>
    ipcRenderer.invoke('sync:handleDeviceDiscovered', deviceId),
  syncHandleDeviceOffline: (deviceId) =>
    ipcRenderer.invoke('sync:handleDeviceOffline', deviceId),

  // File Transfer
  ftCreate: (config) => ipcRenderer.invoke('ft:create', config),
  ftSendFile: (targetDeviceId, filePath) =>
    ipcRenderer.invoke('ft:sendFile', targetDeviceId, filePath),
  ftAcceptTransfer: (offer, savePath) =>
    ipcRenderer.invoke('ft:acceptTransfer', offer, savePath),
  ftRejectTransfer: (offer, reason) =>
    ipcRenderer.invoke('ft:rejectTransfer', offer, reason),
  ftCancelTransfer: (transferId) =>
    ipcRenderer.invoke('ft:cancelTransfer', transferId),
  ftGetTransfers: () => ipcRenderer.invoke('ft:getTransfers'),
  ftHandleBusMessage: (msgType, payload) =>
    ipcRenderer.invoke('ft:handleBusMessage', msgType, payload),
  ftPickFile: () => ipcRenderer.invoke('ft:pickFile'),

  // Sidecar
  sidecarResolve: () => ipcRenderer.invoke('sidecar:resolve'),

  // Event listeners
  onMeshEvent: (cb) => {
    const handler = (_, event) => cb(event);
    ipcRenderer.on('mesh:event', handler);
    return () => ipcRenderer.removeListener('mesh:event', handler);
  },
  onSyncOutgoing: (cb) => {
    const handler = (_, msg) => cb(msg);
    ipcRenderer.on('sync:outgoing', handler);
    return () => ipcRenderer.removeListener('sync:outgoing', handler);
  },
  onFtEvent: (cb) => {
    const handler = (_, event) => cb(event);
    ipcRenderer.on('ft:event', handler);
    return () => ipcRenderer.removeListener('ft:event', handler);
  },
  onFtBusMessage: (cb) => {
    const handler = (_, msg) => cb(msg);
    ipcRenderer.on('ft:busMessage', handler);
    return () => ipcRenderer.removeListener('ft:busMessage', handler);
  },
});
