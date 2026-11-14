// SPDX-License-Identifier: GPL-3.0-only
import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { svelteTesting } from '@testing-library/svelte/vite';
import { defineConfig } from 'vitest/config';

export default defineConfig({
  plugins: [
    // SvelteKit 3: configuration lives here (no svelte.config.js).
    // SPA inside Tauri: no SSR, everything served from the bundled `build/` folder.
    sveltekit({ adapter: adapter({ fallback: 'index.html' }) }),
    svelteTesting()
  ],
  clearScreen: false,
  server: { port: 5175, strictPort: true },
  test: {
    environment: 'jsdom',
    include: ['src/**/*.test.ts'],
    setupFiles: ['src/lib/test/setup.ts'],
    coverage: {
      provider: 'v8',
      include: ['src/**/*.{ts,svelte}'],
      exclude: ['src/**/*.test.ts', 'src/lib/test/**'],
      reporter: ['text-summary', 'html', 'lcov'],
      reportsDirectory: '../../target/coverage/manager'
    }
  }
});
