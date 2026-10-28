// Helper for James plugin pages: <script src="/plugins/_/james.js"></script>
// Plugin pages run sandboxed; the web interface opens them with the instance and a token in
// the URL fragment (#instance=…&token=…), which this helper uses for the plugin's endpoints.
(function () {
  'use strict';
  var params = new URLSearchParams(location.hash.slice(1));
  var instance = params.get('instance');
  var token = params.get('token');
  // Keep the token out of the browser history (it stays in memory only).
  if (token) {
    try {
      history.replaceState(null, '', location.pathname + location.search);
    } catch (e) {
      /* sandboxed documents may not change the history */
    }
  }
  var plugin = decodeURIComponent(location.pathname.split('/')[2] || '');
  var base = location.origin.replace(/^null$/, location.protocol + '//' + location.host);
  var api = base + '/api/v1/instances/' + encodeURIComponent(instance || '') + '/plugins/' + encodeURIComponent(plugin);

  window.James = {
    instance: instance,
    plugin: plugin,
    /** True when the page was opened from the web interface (with a token). */
    ready: !!(instance && token),
    /** Calls an endpoint registered with bot.web.handle(name, …); resolves to its result. */
    call: function (name, body) {
      return fetch(api + '/events/' + encodeURIComponent(name), {
        method: 'POST',
        headers: { authorization: 'Bearer ' + token, 'content-type': 'application/json' },
        body: JSON.stringify(body === undefined ? null : body),
      }).then(function (response) {
        return response.json().catch(function () { return null; }).then(function (data) {
          if (!response.ok) throw new Error((data && data.error) || response.statusText);
          return data;
        });
      });
    },
    /** Opens the plugin's WebSocket; `onMessage` receives each text message. */
    connect: function (onMessage) {
      var socket = new WebSocket(api.replace(/^http/, 'ws') + '/ws', ['james-plugin', token]);
      socket.onmessage = function (event) { onMessage(event.data); };
      return socket;
    },
  };
})();
