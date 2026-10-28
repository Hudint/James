// Worker bootstrap: installs the guard, routes console output to the James log and loads
// the plugin (native module or SinusBot script).

import { parentPort } from 'node:worker_threads';
import { pathToFileURL } from 'node:url';

import type { ToWorker } from '../protocol.ts';
import { installGuard } from './guard.ts';
import { Runtime } from './runtime.ts';

let runtime: Runtime | null = null;
const queue: ToWorker[] = [];
let busy = false;

/** Messages are handled strictly in order (a context must exist before its events). */
async function drain(): Promise<void> {
  if (busy) return;
  busy = true;
  while (queue.length) {
    const message = queue.shift()!;
    try {
      if (message.type === 'init') {
        try {
          await init(message);
        } catch (error: any) {
          parentPort!.postMessage({ type: 'loadFailed', error: error?.stack ?? String(error) });
        }
      } else await runtime?.handle(message);
    } catch (error: any) {
      runtime?.log('error', null, [`internal error: ${error?.stack ?? error}`]);
    }
  }
  busy = false;
}

parentPort!.on('message', (message: ToWorker) => {
  // Replies must not wait behind the handler that awaits them.
  // Requests (commands, HTTP) run concurrently so a slow handler does not stall events.
  if ((message.type === 'reply' || message.type === 'request') && runtime) {
    void runtime.handle(message);
    return;
  }
  queue.push(message);
  void drain();
});

async function init(message: Extract<ToWorker, { type: 'init' }>): Promise<void> {
  runtime = new Runtime(message);
  const rt = runtime;
  const log = (level: 'debug' | 'info' | 'warn' | 'error') => (...args: unknown[]) => rt.log(level, null, args);
  Object.assign(console, { log: log('info'), info: log('info'), debug: log('debug'), warn: log('warn'), error: log('error'), trace: log('debug') });
  process.on('uncaughtException', (error) => rt.log('error', null, [`uncaught exception: ${error?.stack ?? error}`]));
  process.on('unhandledRejection', (error: any) => rt.log('error', null, [`unhandled rejection: ${error?.stack ?? error}`]));
  installGuard(rt.permissions);

  if (message.manifest.kind === 'sinusbot') {
    const { setupSinusbot } = await import('../compat/sinusbot.ts');
    setupSinusbot(rt, message.entry);
  } else {
    const { createBot } = await import('./api.ts');
    const module = await import(pathToFileURL(message.entry).href);
    const definition = module.default;
    if (!definition || typeof definition.setup !== 'function') {
      throw new Error('the plugin must export default definePlugin({ setup(bot) { … } })');
    }
    rt.onContextStart = async (context) => {
      const bot = createBot(context);
      (bot as any).version = message.version;
      const cleanup = await definition.setup(bot);
      if (typeof cleanup === 'function') context.onStop(cleanup);
    };
  }
  rt.post({ type: 'ready' });
}
