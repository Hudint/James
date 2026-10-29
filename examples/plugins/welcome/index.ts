// Greets clients when they connect and counts their visits per instance.
import { definePlugin } from '@james/api';

/** The settings (package.json `config`; all have defaults). */
type Config = {
  message: string;
  poke: boolean;
  ignoreGroups: string[];
};

export default definePlugin<Config>({
  setup(bot) {
    // Typed, but saved by hand: not necessarily a list.
    const ignored = new Set(Array.isArray(bot.config.ignoreGroups) ? bot.config.ignoreGroups : []);

    bot.on('clientJoin', async ({ client }) => {
      if (client.query || client.serverGroups.some((g) => ignored.has(g))) return;
      const key = `visits:${client.uid}`;
      const visits = (bot.store.instance.get<number>(key) ?? 0) + 1;
      bot.store.instance.set(key, visits);
      const text = bot.config.message.replaceAll('{name}', client.name).replaceAll('{visits}', String(visits));
      await (bot.config.poke ? client.poke(text) : client.chat(text));
    });

    bot.commands.register({
      name: 'visits',
      description: 'Shows how often you have been here',
      run: ({ invoker }) => `You have been here ${bot.store.instance.get<number>(`visits:${invoker.uid}`) ?? 0} times.`,
    });
  },
});
