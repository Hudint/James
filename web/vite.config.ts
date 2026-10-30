import { execSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

/** The commit being built: from the Docker build (no .git there), else from git. */
function revision(): string {
  if (process.env.JAMES_REVISION) return process.env.JAMES_REVISION;
  try {
    const git = (args: string) => execSync(`git ${args}`, { stdio: ['ignore', 'pipe', 'ignore'] }).toString().trim();
    // Uncommitted changes are marked: the build is not exactly that commit.
    return git('rev-parse HEAD') + (git('status --porcelain') ? '-dirty' : '');
  } catch {
    return '';
  }
}

const build = {
  version: JSON.parse(readFileSync(new URL('./package.json', import.meta.url), 'utf8')).version as string,
  revision: revision(),
  time: new Date().toISOString(),
};

// During development the API runs on the James binary (default 127.0.0.1:8087).
const api = process.env.JAMES_API ?? 'http://127.0.0.1:8087';

export default defineConfig({
  plugins: [sveltekit()],
  define: {
    __JAMES_BUILD__: JSON.stringify(build),
  },
  build: {
    // Fonts stay files: the CSP allows fonts only from 'self' (no data: URIs).
    assetsInlineLimit: (file) => (file.endsWith('.woff2') ? false : undefined),
  },
  server: {
    proxy: {
      '/api': { target: api, changeOrigin: true, ws: true },
    },
  },
});
