// Moves clients that are away (immediately) or idle (checked every minute) into an AFK channel.
import { definePlugin, type Client } from '@james/api';

export default definePlugin({
  setup(bot) {
    const target = String(bot.config.channel ?? '');
    if (!target) {
      bot.log.warn('no AFK channel configured');
      return;
    }
    const move = async (client: Client, why: string) => {
      if (client.isSelf || client.query || client.channel === target) return;
      try {
        await client.moveTo(target);
        bot.log.info(`moved ${client.name} (${why})`);
      } catch (error) {
        bot.log.warn(`cannot move ${client.name}:`, error);
      }
    };

    if (bot.config.away) bot.on('clientAway', ({ client }) => move(client, 'away'));

    const idleMs = Number(bot.config.idleMinutes ?? 0) * 60_000;
    if (idleMs > 0) {
      // TeamSpeak reports idle times on request; each answer arrives as a clientInfo event.
      bot.on('clientInfo', ({ client }) => {
        if ((client.connection?.idleMs ?? 0) >= idleMs) void move(client, 'idle');
      });
      bot.setInterval(() => {
        for (const client of bot.clients()) {
          if (!client.isSelf && client.channel !== target) client.requestInfo().catch(() => {});
        }
      }, 60_000);
    }
  },
});
