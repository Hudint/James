// Type-level tests of the plugin API (checked with tsc in CI, never run).
import { definePlugin, type Bot, type PluginDefinition } from '@james/api';

// Untyped: settings are unknown.
const untyped = definePlugin({
  setup(bot) {
    const value: unknown = bot.config.anything;
    bot.on('speechRecognized', ({ text, language, client }) => {
      const heard: string = text;
      const lang: string | null = language;
      void [heard, lang, client.name];
    });
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

// Single-file plugin: manifest fields, settings built with code and typed from them.
const events = [
  { name: 'clientMute', title: 'Microphone muted' },
  { name: 'clientAway', title: 'Away' },
] as const;
const inferred = definePlugin({
  name: 'AFK',
  version: '1.0.0',
  permissions: ['moderation'],
  config: [
    { name: 'ignores', title: 'Ignore', type: 'multiline' },
    { name: 'interval', title: 'Interval', type: 'number', default: 5 },
    ...events.flatMap(({ name, title }) => [
      { name: `${name}Active`, title: `${title}: active`, type: 'checkbox' },
      { name: `${name}Channel`, title: `${title}: channel`, type: 'channel' },
    ] as const),
    { name: 'rules', title: 'Rules', type: 'array', vars: [{ name: 'group', title: 'Group', type: 'string' }, { name: 'on', title: 'On', type: 'checkbox' }] },
  ],
  setup(bot) {
    const interval: number = bot.config.interval;
    const active: boolean | undefined = bot.config.clientMuteActive;
    const channel: string | undefined = bot.config.clientAwayChannel;
    const group: string | undefined = bot.config.rules?.[0]?.group;
    // @ts-expect-error a number, not a string
    const wrong: string = bot.config.interval;
    // @ts-expect-error not a setting
    void bot.config.clientDeafActive;
    void interval;
    void active;
    void channel;
    void group;
    void wrong;
  },
});
void inferred;

// @ts-expect-error unknown permission
definePlugin({ permissions: ['root'], setup() {} });

// Fields defined apart keep their types.
import { defineConfig } from '@james/api';
const fields = defineConfig([{ name: 'greeting', title: 'Greeting', type: 'string', default: 'Hi' }]);
definePlugin({
  config: fields,
  setup(bot) {
    const greeting: string = bot.config.greeting;
    void greeting;
  },
});
