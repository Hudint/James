// An unmodified SinusBot script: greets clients joining the bot's channel.
registerPlugin({
  name: 'Channel greeter',
  version: '1.0',
  description: 'Greets clients that join the bot\'s channel',
  author: 'James examples',
  vars: [
    { name: 'message', title: 'Message (%n = nickname)', type: 'string', default: 'Hi %n, enjoy the music!' },
    { name: 'mode', title: 'Send as', type: 'select', options: ['private message', 'poke'], default: '0' }
  ]
}, function (_, config) {
  var event = require('event');
  var backend = require('backend');

  event.on('clientMove', function (ev) {
    var own = backend.getCurrentChannel();
    if (!ev.toChannel || !own || !ev.toChannel.equals(own) || ev.client.isSelf()) return;
    var text = config.message.replace('%n', ev.client.name());
    if (config.mode == 1) ev.client.poke(text); else ev.client.chat(text);
  });
});
