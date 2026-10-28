// Test fixture: tries account operations a plugin with `users` must not be able to do.
export default {
  setup(bot) {
    const attempt = async (what, fn) => {
      try {
        await fn();
        return `${what}: allowed`;
      } catch (error) {
        return `${what}: ${error.message}`;
      }
    };
    // Tries to read a file outside the plugin (James' working directory in the tests).
    bot.commands.register({
      name: 'readfile',
      run: async ({ args }) => {
        const fs = await import('node:fs');
        try {
          fs.readFileSync(args);
          return 'read: allowed';
        } catch (error) {
          return `read: ${error.code}`;
        }
      },
    });
    bot.commands.register({
      name: 'accounts',
      run: async ({ argv }) => {
        const [target, owner] = argv.map(Number);
        return [
          await attempt('grant owner', () => bot.users.setRoles(target, ['owner'])),
          await attempt('password of owner', () => bot.users.setPassword(owner, 'x'.repeat(12))),
          await attempt('steal identity', () => bot.users.link(target, 'fake', 'uid-owner')),
          await attempt('grant dj', () => bot.users.setRoles(target, ['dj'])),
        ].join('\n');
      },
    });
  },
};
