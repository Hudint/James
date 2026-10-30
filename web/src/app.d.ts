// See https://svelte.dev/docs/kit/types#app.d.ts
declare global {
  namespace App {}
  /** Which web interface this is (set when it is built, see vite.config.ts). */
  const __JAMES_BUILD__: { version: string; revision: string; time: string };
}

export {};
