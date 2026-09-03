(function () {
  if (window.__dshCtxInstalled) return;
  window.__dshCtxInstalled = true;
  document.addEventListener('contextmenu', function (e) {
    e.preventDefault();
    try {
      if (window.__TAURI__ && window.__TAURI__.core) {
        window.__TAURI__.core.invoke('open_context_menu');
      }
    } catch (err) {
      console.error('context menu failed', err);
    }
  }, true);
})();
