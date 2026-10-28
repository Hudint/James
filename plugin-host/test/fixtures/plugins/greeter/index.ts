import { definePlugin, type Bot } from '@james/api';

export default definePlugin({
  setup(bot: Bot) {
    bot.on('clientJoin', ({ client }) => client.chat(`${bot.config.greeting}, ${client.name}!`));
    bot.on('clientNick', ({ client, oldName }) => bot.chatServer(`${oldName} is now ${client.name}`));
    bot.commands.register({
      name: 'hello',
      aliases: ['hi'],
      run: ({ invoker, argv }) => {
        const count = (bot.store.instance.get<number>('count') ?? 0) + 1;
        bot.store.instance.set('count', count);
        return `Hello ${invoker.name} (${count}) ${argv.join('|')}`;
      },
    });
    bot.commands.register({
      name: 'kickme',
      run: async ({ invoker }) => {
        try {
          await invoker.kick('bye');
          return 'kicked';
        } catch (error) {
          return (error as Error).message;
        }
      },
    });
    bot.web.handle('ping', (request) => ({ pong: request.body, clients: bot.clients().length }));
    bot.on('custom:hello', (event) => bot.log.info('custom event', event.data));
    return () => bot.log.info('stopped');
  },
});
