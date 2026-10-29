// Type-level tests of the plugin API (checked with tsc in CI, never run).
import { definePlugin, type Bot, type PluginDefinition } from '@james/api';

// Untyped: settings are unknown.
const untyped = definePlugin({
  setup(bot) {
    const value: unknown = bot.config.anything;
    // @ts-expect-error unknown is not a string
    const text: string = bot.config.anything;
    void value;
    void text;
  },
});

// Typed settings.
type Config = { message: string; channel?: string; mode?: number };

function helper(bot: Bot<Config>): string {
  return bot.config.message;
}

const typed = definePlugin<Config>({
  setup(bot) {
    const message: string = bot.config.message;
    const channel: string | undefined = bot.config.channel;
    // @ts-expect-error not a setting
    void bot.config.other;
    // @ts-expect-error read-only
    bot.config.message = 'x';
    void bot.saveConfig({ message: 'hi' });
    // @ts-expect-error message is required
    void bot.saveConfig({ channel: '1' });
    helper(bot);
    void message;
    void channel;
  },
});

// Typed and untyped plugins together.
const all: PluginDefinition<any>[] = [untyped, typed];
void all;
