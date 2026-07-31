import { defineConfig } from 'vitest/config';

// Standalone config so the vector generator never joins the app's `npm test`
// run: the project config uses the default `**/*.{test,spec}.*` include, and
// `generate-vectors.ts` deliberately does not match it.
//
//   npx vitest run --config hcr-backend/tools/vectors.config.ts
export default defineConfig({
  test: {
    root: process.cwd(),
    include: ['hcr-backend/tools/generate-vectors.ts'],
    environment: 'node',
    globals: true,
    // No setupFiles: the app's setup pulls in jest-dom, which needs jsdom.
    testTimeout: 120_000,
  },
});
