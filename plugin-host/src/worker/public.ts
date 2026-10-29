// Runtime of the `@james/api` module as seen by plugins (types: packages/api/index.d.ts).

export function definePlugin<T extends { setup: (bot: any) => unknown }>(definition: T): T {
  if (!definition || typeof definition.setup !== 'function') {
    throw new TypeError('definePlugin() needs an object with a setup(bot) function');
  }
  return definition;
}

export function defineConfig<T>(fields: T): T {
  return fields;
}
