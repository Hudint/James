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
    if (ev.text.indexOf('say ') === 0) {
      require('audio').setTTSDefaultLocale('en');
      require('audio').say(ev.text.slice(4));
      return;
    }
    if (ev.text.indexOf('track ') === 0) {
      var track = media.getTrackByID(ev.text.slice(6));
      if (!track) { ev.client.chat('no track'); return; }
      track.play();
      ev.client.chat('track ' + track.id() + ' ' + track.type());
      return;
    }
    if (ev.text.indexOf('id ') === 0) {
      ev.client.chat('id ' + media.getTrackByID(ev.text.slice(3)).id() + ' playing=' + require('audio').isPlaying());
      return;
    }
    if (ev.text === 'ip') {
      ev.client.chat('ip=' + ev.client.getIPAddress() + ' again=' + ev.client.getIPAddress());
      return;
    }
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
