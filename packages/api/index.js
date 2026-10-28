// Inside James the plugin host provides this module; this copy lets tools and tests import it.
export function definePlugin(definition) {
  if (!definition || typeof definition.setup !== 'function') {
    throw new TypeError('definePlugin() needs an object with a setup(bot) function');
  }
  return definition;
}
