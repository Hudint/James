import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

// During development the API runs on the James binary (default 127.0.0.1:8087).
const api = process.env.JAMES_API ?? 'http://127.0.0.1:8087';

export default defineConfig({
  plugins: [sveltekit()],
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
