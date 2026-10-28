registerPlugin({
  name: 'Legacy',
  version: '1.0',
  description: 'A SinusBot script',
  author: 'Test <test@example.com>',
  backends: ['ts3'],
  requiredModules: ['http'],
  vars: [{ name: 'prefix', title: 'Prefix', type: 'string', default: 'echo' }]
}, function (_, config, meta) {
  var event = require('event');
  var engine = require('engine');
  var backend = require('backend');
  var media = require('media');
  var store = require('store');
  var helpers = require('helpers');

  event.on('chat', function (ev) {
    if (ev.client.isSelf()) return;
    if (ev.text === 'playlists') {
      ev.client.chat('playlists: ' + media.getPlaylists().map(function (p) { return p.name(); }).join(','));
      return;
    }
    store.set('last', ev.text);
    ev.client.chat(config.prefix + ' ' + ev.text + ' ' + helpers.MD5Sum('x') + ' mode=' + ev.mode);
  });
  event.on('clientMove', function (ev) {
    if (!ev.fromChannel) engine.log('joined ' + ev.client.name() + ' into ' + ev.toChannel.name());
  });
  event.on('load', function () {
    engine.log('loaded on ' + engine.getInstanceID() + ' backend=' + engine.getBackend() + ' clients=' + backend.getClients().length);
  });
  setTimeout(function () { engine.log('timer fired'); }, 10);
});
