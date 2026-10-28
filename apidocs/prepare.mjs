// Collects the sources of the API reference in .build/: the API declarations (the file name
// becomes the module name) and the plugin guides with page titles.
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';

const out = new URL('.build/', import.meta.url);
rmSync(out, { recursive: true, force: true });
mkdirSync(out);
copyFileSync(new URL('../packages/api/index.d.ts', import.meta.url), new URL('Plugin API.d.ts', out));
const pages = [
  ['README.md', 'Plugin guide'],
  ['sinusbot.md', 'SinusBot compatibility'],
];
for (const [file, title] of pages) {
  const source = readFileSync(new URL(`../docs/plugins/${file}`, import.meta.url), 'utf8');
  // Links leaving docs/plugins must point to files that exist (they become GitHub URLs).
  for (const [, target] of source.matchAll(/\]\((\.\.\/[^)#\s]+)/g)) {
    if (!existsSync(new URL(`../docs/plugins/${target}`, import.meta.url))) {
      throw new Error(`${file}: broken link ${target}`);
    }
  }
  const text = source
    // Links to other files of the repository point to GitHub.
    .replaceAll('](../../', '](https://github.com/Hudint/James/blob/main/')
    .replaceAll('](../', '](https://github.com/Hudint/James/blob/main/docs/');
  writeFileSync(new URL(file, out), `---\ntitle: ${title}\n---\n\n${text}`);
}
