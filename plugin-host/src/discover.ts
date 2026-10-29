// Plugin discovery: native plugins are directories with a package.json containing a
// "james" section; SinusBot scripts are single .js files calling registerPlugin().

import fs from 'node:fs';
import path from 'node:path';
import { types } from 'node:util';
import vm from 'node:vm';

export interface Manifest {
  id: string;
  kind: 'james' | 'sinusbot';
  name: string;
  version: string;
  description: string;
  author: string;
  engine: string | null;
  permissions: string[];
  /** Usable when an administrator grants them; the plugin runs without. */
  optionalPermissions: string[];
  config: unknown[];
  platforms: string[];
  autorun: boolean;
  hidden: boolean;
  web: boolean;
}

export interface Discovered {
  manifest: Manifest;
  /** Plugin directory (SinusBot: the scripts directory). */
  dir: string;
  /** Module to load. */
  entry: string;
  /** Static web files, if any. */
  webDir: string | null;
  /** Files and directories the plugin's process may read (Node permission model). */
  readable: string[];
}

const ID = /^[a-z0-9][a-z0-9_-]{0,63}$/;

/** SinusBot `requiredModules` → James permissions (graphics has no equivalent). */
const SINUSBOT_MODULES: Record<string, string | null> = {
  http: 'http',
  net: 'net',
  ws: 'web',
  db: 'db',
  fs: 'fs',
  graphics: null,
  'discord-dangerous': null,
};

const SINUSBOT_BACKENDS: Record<string, string> = { ts3: 'teamspeak', discord: 'discord' };

/** What SinusBot lets every script do without declaring it: offered, granted by an admin. */
const SINUSBOT_OPTIONAL = ['moderation', 'channels', 'playlists'];

export function discover(pluginsDir: string, version: string): { plugins: Discovered[]; errors: { id: string; error: string }[] } {
  const plugins: Discovered[] = [];
  const errors: { id: string; error: string }[] = [];
  let entries: fs.Dirent[];
  try {
    entries = fs.readdirSync(pluginsDir, { withFileTypes: true });
  } catch (error: any) {
    if (error.code === 'ENOENT') return { plugins, errors };
    throw error;
  }
  for (const entry of entries.sort((a, b) => a.name.localeCompare(b.name))) {
    const full = path.join(pluginsDir, entry.name);
    try {
      if (entry.isDirectory() && fs.existsSync(path.join(full, 'package.json'))) {
        const found = native(full, entry.name, version);
        if (found) plugins.push(found);
      } else if (entry.isFile() && entry.name.endsWith('.js')) {
        plugins.push(sinusbot(pluginsDir, entry.name));
      }
    } catch (error: any) {
      errors.push({ id: entry.name.replace(/\.js$/, ''), error: describe(error) });
    }
  }
  // Two plugins with one id (e.g. `foo/` and `Foo.js`): neither is loaded.
  const counts = new Map<string, number>();
  for (const p of plugins) counts.set(p.manifest.id, (counts.get(p.manifest.id) ?? 0) + 1);
  for (const [id, count] of counts) {
    if (count > 1) errors.push({ id, error: 'two plugins use this id' });
  }
  return { plugins: plugins.filter((p) => counts.get(p.manifest.id) === 1), errors };
}

function checkId(id: string): string {
  if (!ID.test(id)) throw new Error(`invalid plugin id '${id}' (lowercase letters, digits, '-', '_')`);
  return id;
}

function strings(value: unknown, field: string): string[] {
  if (value === undefined) return [];
  if (!Array.isArray(value) || !value.every((v) => typeof v === 'string')) throw new Error(`'${field}' must be a list of strings`);
  return value;
}

function native(dir: string, id: string, version: string): Discovered | null {
  const pkg = JSON.parse(fs.readFileSync(path.join(dir, 'package.json'), 'utf8'));
  const james = pkg.james;
  if (!james || typeof james !== 'object') return null; // an ordinary npm package
  checkId(id);
  const engine = typeof james.engine === 'string' ? james.engine : null;
  if (engine && !satisfies(version, engine)) throw new Error(`requires James ${engine} (this is ${version})`);
  const main = typeof pkg.main === 'string' ? pkg.main : ['index.ts', 'index.js', 'index.mjs'].find((f) => fs.existsSync(path.join(dir, f)));
  if (!main) throw new Error('no entry point (set "main" in package.json)');
  const entry = path.resolve(dir, main);
  if (!entry.startsWith(dir + path.sep) || !fs.existsSync(entry)) throw new Error(`entry point '${main}' not found`);
  if (james.config !== undefined && !Array.isArray(james.config)) throw new Error("'james.config' must be a list");
  const webDir = james.web ? path.resolve(dir, typeof james.web === 'string' ? james.web : 'web') : null;
  if (webDir && (webDir === dir || !webDir.startsWith(dir + path.sep))) throw new Error("'james.web' must be a subdirectory of the plugin");
  return {
    manifest: {
      id,
      kind: 'james',
      name: String(james.name ?? pkg.name ?? id),
      version: String(pkg.version ?? ''),
      description: String(pkg.description ?? ''),
      author: typeof pkg.author === 'string' ? pkg.author : String(pkg.author?.name ?? ''),
      engine,
      permissions: strings(james.permissions, 'james.permissions'),
      optionalPermissions: strings(james.optionalPermissions, 'james.optionalPermissions'),
      config: james.config ?? [],
      platforms: strings(james.platforms, 'james.platforms'),
      autorun: james.autorun === true,
      hidden: james.hidden === true,
      web: webDir !== null && fs.existsSync(webDir),
    },
    dir,
    entry,
    webDir: webDir && fs.existsSync(webDir) ? webDir : null,
    readable: [dir],
  };
}

/** Reads a SinusBot script's manifest by running its top level with stubbed globals. */
function sinusbot(scriptsDir: string, file: string): Discovered {
  const id = checkId(file.slice(0, -3).toLowerCase());
  const entry = path.join(scriptsDir, file);
  const manifest = probeSinusbot(fs.readFileSync(entry, 'utf8'), file);
  const webDir = path.join(scriptsDir, file.slice(0, -3), 'html');
  const hasWeb = manifest.enableWeb === true || manifest.enableweb === true;
  const permissions = new Set<string>();
  for (const module of strings(manifest.requiredModules, 'requiredModules')) {
    if (!(module in SINUSBOT_MODULES)) throw new Error(`unknown required module '${module}'`);
    const permission = SINUSBOT_MODULES[module];
    if (permission) permissions.add(permission);
  }
  if (hasWeb) permissions.add('web');
  // James extension (SinusBot ignores it): permissions a script needs, e.g. ['moderation'].
  for (const permission of strings(manifest.permissions, 'permissions')) permissions.add(permission);
  const backends = manifest.backends === undefined ? ['ts3'] : strings(manifest.backends, 'backends');
  return {
    manifest: {
      id,
      kind: 'sinusbot',
      name: String(manifest.name ?? id),
      version: String(manifest.version ?? ''),
      description: String(manifest.description ?? ''),
      author: String(manifest.author ?? ''),
      // SinusBot engine versions are not James versions.
      engine: null,
      permissions: [...permissions],
      optionalPermissions: SINUSBOT_OPTIONAL.filter((p) => !permissions.has(p)),
      config: Array.isArray(manifest.vars) ? manifest.vars : [],
      platforms: backends.map((b) => SINUSBOT_BACKENDS[b]).filter((b): b is string => !!b),
      autorun: manifest.autorun === true,
      hidden: manifest.hidden === true,
      web: hasWeb && fs.existsSync(webDir),
    },
    dir: scriptsDir,
    entry,
    webDir: hasWeb && fs.existsSync(webDir) ? webDir : null,
    // The script, its own folder and npm packages next to the scripts.
    readable: [entry, path.join(scriptsDir, file.slice(0, -3)), path.join(scriptsDir, 'node_modules')].filter((p) => fs.existsSync(p)),
  };
}

/**
 * Reads a SinusBot script's manifest by running its top level with stubbed globals. Runs in
 * the discovery process, which can read the plugin directory and nothing else; the sandbox
 * only gets functions created inside it, and microtasks are bounded by the timeout too.
 */
export function probeSinusbot(code: string, filename: string): Record<string, any> {
  // A fresh global without links to this realm (a contextified host object would expose the
  // host's Function constructor through its prototype chain).
  const context = vm.createContext(vm.constants.DONT_CONTEXTIFY, { microtaskMode: 'afterEvaluate', codeGeneration: { strings: false, wasm: false } });
  vm.runInContext(
    `var __manifest = null;
     var __stub = new Proxy(function () {}, { get: function () { return __stub; }, apply: function () { return __stub; } });
     function registerPlugin(m) {
       if (__manifest) throw new Error('registerPlugin() called twice');
       if (!m || typeof m !== 'object') throw new Error('registerPlugin() needs a manifest object');
       __manifest = JSON.stringify(m);
     }
     var require = function () { return __stub; };
     var module = { exports: {} }, exports = module.exports;
     var console = { log() {}, info() {}, warn() {}, error() {}, debug() {} };
     var setTimeout = function () { return 0; }, setInterval = setTimeout, clearTimeout = function () {}, clearInterval = clearTimeout;`,
    context,
  );
  // displayErrors would read the thrown value's properties outside the time limit.
  vm.runInContext(code, context, { filename, timeout: 1000, displayErrors: false });
  const manifest = vm.runInContext('__manifest', context, { timeout: 1000, displayErrors: false });
  if (typeof manifest !== 'string') throw new Error('the script does not call registerPlugin()');
  // A plain copy, parsed outside the sandbox.
  return JSON.parse(manifest);
}

/**
 * Message of an error without running code of the error object: a script may throw a Proxy
 * or an object with a looping `message` getter, outside the probe's time limit.
 */
export function describe(error: unknown): string {
  if (!types.isNativeError(error)) return 'the script failed while being read';
  const message = Object.getOwnPropertyDescriptor(error, 'message');
  return message && 'value' in message && typeof message.value === 'string' ? message.value.slice(0, 500) : 'the script failed while being read';
}

/** Minimal semver range check: space-separated comparators (>=, >, <=, <, =, ^, ~). */
export function satisfies(version: string, range: string): boolean {
  const parse = (v: string) => v.replace(/^v/, '').split(/[.+-]/).slice(0, 3).map((n) => Number.parseInt(n, 10) || 0);
  const cmp = (a: number[], b: number[]) => {
    for (let i = 0; i < 3; i++) if (a[i] !== b[i]) return a[i] - b[i];
    return 0;
  };
  const v = parse(version);
  return range
    .replace(/(>=|<=|>|<|=|\^|~)\s+/g, '$1')
    .trim()
    .split(/\s+/)
    .filter(Boolean)
    .every((part) => {
      const m = /^(>=|<=|>|<|=|\^|~)?\s*(.+)$/.exec(part);
      if (!m) return false;
      const [, op = '=', target] = m;
      const t = parse(target);
      const c = cmp(v, t);
      switch (op) {
        case '>=': return c >= 0;
        case '<=': return c <= 0;
        case '>': return c > 0;
        case '<': return c < 0;
        case '^': return c >= 0 && (t[0] > 0 ? v[0] === t[0] : t[1] > 0 ? v[0] === 0 && v[1] === t[1] : cmp(v, t) === 0);
        case '~': return c >= 0 && v[0] === t[0] && v[1] === t[1];
        default: return c === 0;
      }
    });
}
