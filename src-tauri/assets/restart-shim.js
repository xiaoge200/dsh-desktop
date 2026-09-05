(function () {
  if (window.__dshRestartShim) return;
  window.__dshRestartShim = true;
  if (!window.fetch || !window.__TAURI__ || !window.__TAURI__.core) return;
  var open = window.fetch.bind(window);
  var restartPaths = ['/dsh-market/restart', '/dsh-market/api/v1/restart'];
  function isRestartPost(input, init) {
    var url = typeof input === 'string'
      ? input
      : (input instanceof URL ? input.href : (input && input.url) || '');
    if (!url) return false;
    var method = ((init && init.method) || (input && input.method) || 'GET').toUpperCase();
    if (method !== 'POST') return false;
    var u;
    try { u = new URL(url, location.href); } catch (e) { return false; }
    if (u.origin !== location.origin) return false;
    return restartPaths.indexOf(u.pathname) !== -1;
  }
  window.fetch = function (input, init) {
    if (!isRestartPost(input, init)) return open(input, init);
    return window.__TAURI__.core.invoke('restart_service').then(
      function () {
        return new Response(JSON.stringify({ ok: true }), {
          status: 202,
          headers: { 'content-type': 'application/json' }
        });
      },
      function (err) {
        return new Response(
          JSON.stringify({ error: String(err && err.message ? err.message : err) }),
          { status: 500, headers: { 'content-type': 'application/json' } }
        );
      }
    );
  };
})();
