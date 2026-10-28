Reference of the plugin API of [James](https://github.com/Hudint/James), the voice bot for
TeamSpeak 3 and 6. Plugins are JavaScript or TypeScript modules; each runs in its own Node.js
process and talks to the bot through the API documented here.

```ts
import { definePlugin } from '@james/api';

export default definePlugin({
  setup(bot) {
    bot.on('clientJoin', ({ client }) => client.chat(`Welcome, ${client.name}!`));
    bot.commands.register({
      name: 'hello',
      description: 'Says hello',
      run: ({ invoker }) => `Hello ${invoker.name}!`,
    });
  },
});
```

## Where to start

- **New to James plugins?** Read the [plugin guide](.build/README.md): directory layout,
  manifest, lifecycle and examples.
- **Coming from SinusBot?** Your scripts run unchanged; see
  [SinusBot compatibility](.build/sinusbot.md) for what is covered.
- **The API** starts at {@link "Plugin API"!Bot | Bot}, the object `setup` receives:
  - events: {@link "Plugin API"!Bot.on | Bot.on} with all names and payloads in {@link "Plugin API"!Events | Events}
  - server state: {@link "Plugin API"!Client | Client}, {@link "Plugin API"!Channel | Channel}, {@link "Plugin API"!ServerInfo | ServerInfo}
  - chat commands: {@link "Plugin API"!Commands | Commands}
  - music: {@link "Plugin API"!Player | Player}, {@link "Plugin API"!Playlists | Playlists}, {@link "Plugin API"!Library | Library}
  - text to speech: {@link "Plugin API"!Bot.say | Bot.say}
  - storage: {@link "Plugin API"!Store | Store}
  - web pages and HTTP endpoints: {@link "Plugin API"!Web | Web}
  - accounts and other bots: {@link "Plugin API"!Users | Users}, {@link "Plugin API"!Instances | Instances}
- **The manifest** (the `"james"` section of `package.json`) is described by {@link "Plugin API"!Manifest | Manifest};
  the permissions a plugin can ask for are {@link "Plugin API"!Permission | Permission}.

## Editor support

Install the types as a dev dependency to get completion and type checking:

```sh
npm install --save-dev <path to James>/packages/api
```

The package is MIT licensed, so plugins may use any license.
