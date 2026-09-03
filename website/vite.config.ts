import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import path from 'node:path';

// The doc sources live one level up, in the workspace's docs/ directory.
// Vite by default only allows files within the project root (website/), so we
// explicitly allow the repo root to be served and imported as raw text.
const repoRoot = path.resolve(__dirname, '..');

export default defineConfig({
  plugins: [react()],
  server: {
    fs: {
      allow: [repoRoot],
    },
  },
  // Relative ../docs globs are resolved from the importing file, so no alias is
  // strictly needed, but an explicit alias keeps the docs.ts index readable.
  resolve: {
    alias: {
      '@docs': path.resolve(repoRoot, 'docs'),
    },
  },
});
