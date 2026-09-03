(function () {
  if (window.__dshNotifyShim) return;
  window.__dshNotifyShim = true;
  var api = { permission: 'granted', maxActions: 2 };
  api.requestPermission = function (cb) {
    if (typeof cb === 'function') {
      try { cb('granted'); } catch (e) {}
    }
    return Promise.resolve('granted');
  };
  function Shim(title, options) {
    this.title = title;
    this.options = options || {};
    try {
      if (window.__TAURI__ && window.__TAURI__.event) {
        window.__TAURI__.event.emit('dsh-notify-request', {
          title: String(title),
          body: String(this.options.body || '')
        });
      }
    } catch (e) {}
  }
  Shim.prototype.close = function () {};
  Shim.permission = api.permission;
  Shim.maxActions = api.maxActions;
  Shim.requestPermission = api.requestPermission;
  try {
    Object.defineProperty(window, 'Notification', {
      value: Shim,
      configurable: true,
      writable: true
    });
  } catch (e) {}
})();
