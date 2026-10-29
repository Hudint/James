// Reads the manifest of one plugin file by running its top level in a sandbox: a SinusBot
// script (`sinusbot`, calls registerPlugin()) or a single-file James plugin (`james`, a module
// whose default export is definePlugin({...})). The source comes on stdin, the manifest goes
// to stdout as JSON ({ manifest } or { error }).
//
// Runs in its own short-lived process (started by discovery with the Node permission model,
// a small heap, no environment and a hard time limit), so a plugin that hangs, exhausts memory
// or escapes the sandbox only affects this process. Inside, the sandbox has no host objects:
// imports are inert stubs, dynamic import() fails, timers do nothing, and code cannot be
// generated from strings. `setup` is never called.

import { stripTypeScriptTypes } from 'node:module';
import { types } from 'node:util';
import vm from 'node:vm';

/** Time the plugin's top level may run (the process is also killed from outside). */
const TIMEOUT_MS = 1000;
/** Longest manifest (JSON) that is returned. */
const MAX_MANIFEST = 256 * 1024;

function context(): vm.Context {
  // A fresh global without links to this realm (a contextified host object would expose the
  // host's Function constructor through its prototype chain).
  const ctx = vm.createContext(vm.constants.DONT_CONTEXTIFY, {
    microtaskMode: 'afterEvaluate',
    codeGeneration: { strings: false, wasm: false },
  });
  // Callbacks that would run after evaluation, outside the time limit.
  vm.runInContext(
    `delete globalThis.FinalizationRegistry; delete globalThis.WeakRef;
     if (globalThis.Atomics) delete globalThis.Atomics.waitAsync;
     // Buffers live outside the limited heap (one call could allocate gigabytes).
     ['ArrayBuffer', 'SharedArrayBuffer', 'DataView', 'Atomics', 'Int8Array', 'Uint8Array', 'Uint8ClampedArray',
      'Int16Array', 'Uint16Array', 'Int32Array', 'Uint32Array', 'Float16Array', 'Float32Array', 'Float64Array',
      'BigInt64Array', 'BigUint64Array', 'WebAssembly'].forEach(function (name) { delete globalThis[name]; });
     var __stub = (function () {
       var s = new Proxy(function () {}, { get: function () { return s; }, apply: function () { return s; }, construct: function () { return s; } });
       return s;
     })();
     var console = { log() {}, info() {}, warn() {}, error() {}, debug() {} };
     var setTimeout = function () { return 0; }, setInterval = setTimeout, clearTimeout = function () {}, clearInterval = clearTimeout;
     var __definition = null;
     /** The manifest without functions (setup), serialised inside the sandbox. */
     function __manifest(value) {
       if (!value || typeof value !== 'object') throw new Error('the plugin does not export definePlugin({...})');
       var copy = {};
       for (var key in value) if (typeof value[key] !== 'function') copy[key] = value[key];
       return JSON.stringify(copy);
     }`,
    ctx,
    { timeout: TIMEOUT_MS, displayErrors: false },
  );
  return ctx;
}

/** dynamic import() is refused with an error of the sandbox (a host error would leak its realm). */
function refuseImport(ctx: vm.Context) {
  return () => {
    throw vm.runInContext('new Error("import() is not available while the plugin is read")', ctx);
  };
}

function probeSinusbot(code: string): string {
  const ctx = context();
  vm.runInContext(
    `function registerPlugin(m) {
       if (__definition) throw new Error('registerPlugin() called twice');
       if (!m || typeof m !== 'object') throw new Error('registerPlugin() needs a manifest object');
       __definition = JSON.stringify(m);
     }
     var require = function () { return __stub; };
     var module = { exports: {} }, exports = module.exports;`,
    ctx,
    { timeout: TIMEOUT_MS, displayErrors: false },
  );
  // displayErrors would read the thrown value's properties outside the time limit.
  const script = new vm.Script(code, { filename: 'plugin.js', importModuleDynamically: refuseImport(ctx) as never });
  script.runInContext(ctx, { timeout: TIMEOUT_MS, displayErrors: false });
  const manifest = vm.runInContext('__definition', ctx, { timeout: TIMEOUT_MS, displayErrors: false });
  if (typeof manifest !== 'string') throw new Error('the script does not call registerPlugin()');
  return manifest;
}

async function probeJames(source: string): Promise<string> {
  // Only erasable TypeScript (as when the plugin runs): enums and namespaces are an error.
  const code = stripTypeScriptTypes(source, { mode: 'strip' });
  const ctx = context();
  const module = new vm.SourceTextModule(code, {
    context: ctx,
    identifier: 'plugin.ts',
    importModuleDynamically: refuseImport(ctx) as never,
    initializeImportMeta() {},
  });
  const hasTopLevelAwait = (module as unknown as { hasTopLevelAwait?: () => boolean }).hasTopLevelAwait;
  if (hasTopLevelAwait?.call(module)) throw new Error('top-level await is not supported');
  // Every import is the inert stub, except definePlugin, which only keeps its argument.
  const api = vm.runInContext(
    `({ definePlugin: function (d) { __definition = d; return d; }, defineConfig: function (c) { return c; } })`,
    ctx,
  );
  const imported = importedNames(code);
  await module.link((specifier) => {
    const names = [
      ...new Set(['default', ...(specifier === '@james/api' ? ['definePlugin', 'defineConfig'] : []), ...(imported.get(specifier) ?? [])]),
    ];
    return new vm.SyntheticModule(
      names,
      function () {
        for (const name of names) {
          const own = specifier === '@james/api' && (name === 'definePlugin' || name === 'defineConfig');
          this.setExport(name, own ? api[name] : vm.runInContext('__stub', ctx));
        }
      },
      { context: ctx, identifier: specifier },
    );
  });
  await module.evaluate({ timeout: TIMEOUT_MS });
  if (module.status !== 'evaluated') throw new Error('the plugin did not finish loading');
  // Copied inside the sandbox, under the time limit (getters run there, not here).
  const json = vm.runInContext('__manifest(__definition)', ctx, { timeout: TIMEOUT_MS, displayErrors: false });
  if (typeof json !== 'string') throw new Error('the plugin does not call definePlugin({...})');
  return json;
}

/** Names imported (or re-exported) by name per module, so that each stub provides them. */
function importedNames(code: string): Map<string, string[]> {
  const names = new Map<string, string[]>();
  for (const match of code.matchAll(/(?:import|export)\s*(?:[\w$]+\s*,\s*)?\{([^}]*)\}\s*from\s*['"]([^'"]+)['"]/g)) {
    const list = names.get(match[2]!) ?? [];
    // Comments inside the braces are not names.
    const inner = match[1]!.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/[^\n]*/g, '');
    for (const part of inner.split(',')) {
      const name = part.trim().split(/\s+as\s+/)[0]?.trim();
      if (name && /^[A-Za-z_$][\w$]*$/.test(name)) list.push(name);
    }
    names.set(match[2]!, list);
  }
  return names;
}

/** Message of an error without running code of the error object. */
function describe(error: unknown): string {
  if (!types.isNativeError(error)) return 'the plugin failed while being read';
  const message = Object.getOwnPropertyDescriptor(error, 'message');
  return message && 'value' in message && typeof message.value === 'string' ? message.value.slice(0, 500) : 'the plugin failed while being read';
}

/** Ends this process even if discovery (which also kills it) is gone. */
const LIFETIME_MS = 5000;

async function main() {
  setTimeout(() => process.exit(1), LIFETIME_MS).unref();
  const kind = process.argv[2];
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) chunks.push(chunk as Buffer);
  const source = Buffer.concat(chunks).toString('utf8');
  let out: string;
  try {
    const manifest = kind === 'sinusbot' ? probeSinusbot(source) : await probeJames(source);
    if (manifest.length > MAX_MANIFEST) throw new Error('the manifest is too large');
    // One JSON object, whatever the sandbox returned.
    const parsed: unknown = JSON.parse(manifest);
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('the manifest is not an object');
    out = JSON.stringify({ manifest: parsed });
  } catch (error) {
    out = JSON.stringify({ error: describe(error) });
  }
  process.stdout.write(out, () => process.exit(0));
}

void main();
