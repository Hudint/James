// Pushes the current track to a web page over a WebSocket and announces new tracks.
import { definePlugin, type QueueItem } from '@james/api';

export default definePlugin({
  setup(bot) {
    const describe = (item: QueueItem | null) =>
      item ? { title: item.track.title, artist: item.track.artist ?? null, thumbnail: item.track.thumbnail ?? null, requestedBy: item.requestedBy?.name ?? null } : null;

    // Answers POST /api/v1/instances/<instance>/plugins/now-playing/events/current
    bot.web.handle('current', () => ({ instance: bot.instance.name, current: describe(bot.player.current) }));

    bot.on('ws.connect', ({ connection }) => bot.web.send({ current: describe(bot.player.current) }, connection));
    bot.on('trackStart', async ({ item }) => {
      bot.web.send({ current: describe(item) });
      if (bot.config.announce) {
        await bot.chatChannel(`Now playing: [b]${item.track.title}[/b]${item.track.artist ? ` by ${item.track.artist}` : ''}`);
      }
    });
    bot.on('trackEnd', () => {
      if (!bot.player.current) bot.web.send({ current: null });
    });
  },
});
