// Plugin discovery: native plugins are directories with a package.json containing a
// "james" section, or single .ts files exporting definePlugin({...}); SinusBot scripts are
// single .js files calling registerPlugin(). Single files are read by running their top level
// in a sandbox, each in its own process (probe.ts).

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { types } from 'node:util';

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

export async function discover(pluginsDir: string, version: string): Promise<{ plugins: Discovered[]; errors: { id: string; error: string }[] }> {
  const plugins: Discovered[] = [];
  const errors: { id: string; error: string }[] = [];
  let entries: fs.Dirent[];
  try {
    entries = fs.readdirSync(pluginsDir, { withFileTypes: true });
  } catch (error: any) {
    if (error.code === 'ENOENT') return { plugins, errors };
    throw error;
  }
  // Files are read by probe processes, a few at a time, within an overall time budget.
  const deadline = Date.now() + SCAN_BUDGET_MS;
  const found: (Discovered | null)[] = [];
  const jobs = entries
    .sort((a, b) => a.name.localeCompare(b.name))
    .map((entry, index) => async () => {
      const full = path.join(pluginsDir, entry.name);
      try {
        if (entry.isDirectory() && fs.existsSync(path.join(full, 'package.json'))) {
          found[index] = native(full, entry.name, version);
        } else if (entry.isFile() && entry.name.endsWith('.js')) {
          found[index] = await sinusbot(pluginsDir, entry.name, deadline);
        } else if (entry.isFile() && entry.name.endsWith('.ts') && !entry.name.endsWith('.d.ts')) {
          found[index] = await singleFile(pluginsDir, entry.name, version, deadline);
        }
      } catch (error: any) {
        errors.push({ id: entry.name.replace(/\.(js|ts)$/, ''), error: describe(error) });
      }
    });
  let next = 0;
  await Promise.all(
    Array.from({ length: PROBES_AT_ONCE }, async () => {
      while (next < jobs.length) await jobs[next++]!();
    }),
  );
  for (const p of found) if (p) plugins.push(p);
  errors.sort((a, b) => a.id.localeCompare(b.id));
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
  checkShape(james);
  checkId(id);
  const main = typeof pkg.main === 'string' ? pkg.main : ['index.ts', 'index.js', 'index.mjs'].find((f) => fs.existsSync(path.join(dir, f)));
  if (!main) throw new Error('no entry point (set "main" in package.json)');
  const entry = path.resolve(dir, main);
  if (!entry.startsWith(dir + path.sep) || !fs.existsSync(entry)) throw new Error(`entry point '${main}' not found`);
  const webDir = james.web ? path.resolve(dir, typeof james.web === 'string' ? james.web : 'web') : null;
  if (webDir && (webDir === dir || !webDir.startsWith(dir + path.sep))) throw new Error("'james.web' must be a subdirectory of the plugin");
  const author = typeof pkg.author === 'string' ? pkg.author : pkg.author?.name;
  const manifest = nativeManifest(id, { ...james, name: james.name ?? pkg.name, version: pkg.version, description: pkg.description, author }, version, 'james');
  manifest.web = webDir !== null && fs.existsSync(webDir);
  return { manifest, dir, entry, webDir: manifest.web ? webDir : null, readable: [dir] };
}

/** A plugin in one `<id>.ts` file: its manifest is the argument of definePlugin(). */
async function singleFile(pluginsDir: string, file: string, version: string, deadline: number): Promise<Discovered> {
  const id = checkId(file.slice(0, -3));
  const entry = path.join(pluginsDir, file);
  const definition = await probe('james', entry, deadline);
  const manifest = nativeManifest(id, definition, version, '');
  const modules = path.join(pluginsDir, 'node_modules');
  return { manifest, dir: pluginsDir, entry, webDir: null, readable: [entry, modules].filter((p) => fs.existsSync(p)) };
}

/** Most config fields (also nested) and nesting of a manifest. */
const MAX_FIELDS = 300;
const MAX_DEPTH = 4;
/** Most entries in a list setting's default. */
const MAX_ITEMS = 100;

/** The manifest of a native plugin from its definition (package.json `james` or definePlugin()). */
function nativeManifest(id: string, d: Record<string, any>, version: string, prefix: string): Manifest {
  const at = (field: string) => (prefix ? `${prefix}.${field}` : field);
  const engine = typeof d.engine === 'string' ? d.engine : null;
  if (engine && !satisfies(version, engine)) throw new Error(`requires James ${engine} (this is ${version})`);
  if (d.config !== undefined && !Array.isArray(d.config)) throw new Error(`'${at('config')}' must be a list`);
  checkFields(d.config ?? [], 1, { count: 0 });
  return {
    id,
    kind: 'james',
    name: String(d.name ?? id).slice(0, 200),
    version: String(d.version ?? '').slice(0, 50),
    description: String(d.description ?? '').slice(0, 1000),
    author: String(d.author ?? '').slice(0, 200),
    engine,
    permissions: strings(d.permissions, at('permissions')),
    optionalPermissions: strings(d.optionalPermissions, at('optionalPermissions')),
    config: d.config ?? [],
    platforms: strings(d.platforms, at('platforms')),
    autorun: d.autorun === true,
    hidden: d.hidden === true,
    web: false,
  };
}

function checkFields(fields: unknown[], depth: number, seen: { count: number }) {
  if (depth > MAX_DEPTH) throw new Error('the settings are nested too deeply');
  for (const field of fields) {
    if (++seen.count > MAX_FIELDS) throw new Error(`more than ${MAX_FIELDS} settings`);
    if (!field || typeof field !== 'object' || typeof (field as any).name !== 'string') throw new Error('every setting needs a name');
    const vars = (field as any).vars;
    if (vars !== undefined && !Array.isArray(vars)) throw new Error(`'vars' of setting '${(field as any).name}' must be a list`);
    if (Array.isArray(vars)) checkFields(vars, depth + 1, seen);
    const fallback = (field as any).default;
    if (Array.isArray(fallback) && fallback.length > MAX_ITEMS) throw new Error(`the default of '${(field as any).name}' has too many entries`);
  }
}

/** Deepest nesting and most values of a manifest (James parses it with a nesting limit). */
const MAX_NESTING = 16;
const MAX_VALUES = 20_000;

function checkShape(value: unknown) {
  let values = 0;
  const walk = (v: unknown, depth: number) => {
    if (depth > MAX_NESTING) throw new Error('the manifest is nested too deeply');
    if (++values > MAX_VALUES) throw new Error('the manifest is too large');
    if (v && typeof v === 'object') for (const item of Object.values(v)) walk(item, depth + 1);
  };
  walk(value, 0);
}

/** Largest plugin file that is read. */
const MAX_SOURCE = 1024 * 1024;
const PROBE = fileURLToPath(new URL('./probe.ts', import.meta.url));
const HOST_DIR = path.dirname(path.dirname(PROBE));

/** Probe processes at the same time, and the time all of them together may take. */
const PROBES_AT_ONCE = 4;
const SCAN_BUDGET_MS = 40_000;
/** Time one probe may take (it is killed after it). */
const PROBE_MS = 3000;
/** Largest probe output (the manifest as JSON). */
const MAX_OUTPUT = 300 * 1024;

/**
 * Runs `file`'s top level in the sandbox of probe.ts, in a process of its own: the Node
 * permission model (reads only the host's files; the source comes on stdin), a small heap, no
 * environment, no inherited descriptors besides stdio (the IPC channel to James is fd 3), and
 * killed after a few seconds.
 */
function probe(kind: 'james' | 'sinusbot', file: string, deadline: number): Promise<Record<string, any>> {
  if (fs.statSync(file).size > MAX_SOURCE) throw new Error('the file is too large');
  const source = fs.readFileSync(file);
  if (source.length > MAX_SOURCE) throw new Error('the file is too large');
  const time = Math.min(PROBE_MS, deadline - Date.now());
  if (time <= 0) throw new Error('not read: reading the plugins took too long');
  return new Promise((resolve, reject) => {
    const child = spawn(
      process.execPath,
      [
        '--permission',
        `--allow-fs-read=${HOST_DIR}`,
        '--experimental-vm-modules',
        '--disable-warning=ExperimentalWarning',
        '--max-old-space-size=64',
        PROBE,
        kind,
      ],
      // fd 3 is replaced (by /dev/null) instead of inherited.
      { env: {}, stdio: ['pipe', 'pipe', 'ignore', 'ignore'] },
    );
    const chunks: Buffer[] = [];
    let size = 0;
    let failure: string | null = null;
    const stop = (why: string) => {
      failure ??= why;
      child.kill('SIGKILL');
    };
    const timer = setTimeout(() => stop('reading the plugin took too long'), time);
    child.stdout!.on('data', (chunk: Buffer) => {
      size += chunk.length;
      if (size > MAX_OUTPUT) stop('the manifest is too large');
      else chunks.push(chunk);
    });
    child.stdin!.on('error', () => {});
    child.stdin!.end(source);
    child.on('error', (error) => stop(error.message));
    child.on('close', (code) => {
      clearTimeout(timer);
      if (failure) return reject(new Error(failure));
      let parsed: { manifest?: unknown; error?: unknown } | null = null;
      try {
        parsed = code === 0 ? JSON.parse(Buffer.concat(chunks).toString('utf8')) : null;
      } catch {
        parsed = null;
      }
      if (!parsed) return reject(new Error('the plugin could not be read (it may use too much memory)'));
      if (typeof parsed.error === 'string') return reject(new Error(parsed.error));
      if (!parsed.manifest || typeof parsed.manifest !== 'object' || Array.isArray(parsed.manifest)) {
        return reject(new Error('the plugin could not be read'));
      }
      try {
        checkShape(parsed.manifest);
      } catch (error) {
        return reject(error);
      }
      resolve(parsed.manifest as Record<string, any>);
    });
  });
}

/** Reads a SinusBot script's manifest by running its top level with stubbed globals. */
async function sinusbot(scriptsDir: string, file: string, deadline: number): Promise<Discovered> {
  const id = checkId(file.slice(0, -3).toLowerCase());
  const entry = path.join(scriptsDir, file);
  const manifest = await probe('sinusbot', entry, deadline);
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
  if (Array.isArray(manifest.vars)) checkFields(manifest.vars, 1, { count: 0 });
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
