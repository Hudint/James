// Module guard: Node.js modules behind a permission cannot be loaded without it, and
// `@james/api` resolves to the host's implementation. Plugins are trusted code; this is a
// safety net against accidents, not a sandbox.

import { registerHooks } from 'node:module';

const GUARDED: Record<string, string> = {
  http: 'http',
  https: 'http',
  http2: 'http',
  net: 'net',
  tls: 'net',
  dgram: 'net',
  child_process: 'process',
  cluster: 'process',
  sqlite: 'db',
};

const API_URL = new URL('./public.ts', import.meta.url).href;

export function installGuard(permissions: Set<string>): void {
  registerHooks({
    resolve(specifier, context, next) {
      if (specifier === '@james/api') return { url: API_URL, shortCircuit: true };
      const name = specifier.replace(/^node:/, '').split('/')[0];
      const permission = GUARDED[name];
      // Only built-ins: a package that happens to be called "net" is unaffected.
      if (permission && !permissions.has(permission) && (specifier.startsWith('node:') || !context.parentURL?.includes('/node_modules/'))) {
        throw new Error(`the plugin lacks the permission '${permission}' needed for '${specifier}'`);
      }
      return next(specifier, context);
    },
  });
  if (!permissions.has('http')) {
    const denied = () => {
      throw new Error("the plugin lacks the permission 'http' needed for network requests");
    };
    globalThis.fetch = (() => Promise.reject(new Error("the plugin lacks the permission 'http' needed for fetch()"))) as typeof fetch;
    for (const name of ['WebSocket', 'EventSource'] as const) {
      Object.defineProperty(globalThis, name, { get: denied, configurable: true });
    }
  }
}
