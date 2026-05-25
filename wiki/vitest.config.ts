import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'
import path from 'path'

export default defineConfig({
  plugins: [react()],
  test: {
    // jsdom gives us a browser-like DOM environment for React components
    environment: 'jsdom',
    // Run the setup file before every test suite
    setupFiles: ['./src/__tests__/setup.ts'],
    // Keep the global timeout generous — markdown+katex processing can be slow
    testTimeout: 10_000,
    // Frontend tests live under src/__tests__/; the Node-only script tests
    // under scripts/__tests__/ run via Jest (see `test:scripts`).
    include: ['src/__tests__/**/*.test.{ts,tsx}'],
    exclude: ['node_modules', 'scripts/**', '.next', 'out'],
    // ── Coverage (run with `npm run test:coverage:frontend`) ──────────────
    // The v8 provider uses Node's built-in coverage instrumentation — no
    // babel transform needed.  HTML + JSON summary are written under
    // `coverage/frontend/`; `text` keeps the in-terminal table.
    coverage: {
      provider: 'v8',
      reporter: ['text', 'html', 'json-summary'],
      reportsDirectory: './coverage/frontend',
      // App Router page shells are thin data-fetch wrappers — coverage for
      // those lives in integration/e2e tests.  Unit coverage targets the
      // logic-bearing modules that Vitest can exercise in jsdom.
      include: [
        'src/components/**/*.{ts,tsx}',
        'src/lib/**/*.{ts,tsx}',
        'src/app/onboarding/**/*.{ts,tsx}',
      ],
      exclude: [
        'src/**/*.test.{ts,tsx}',
        'src/__tests__/**',
        'src/**/*.d.ts',
      ],
      all: true,
    },
  },
  resolve: {
    alias: {
      // Mirror the "@/*" path alias from tsconfig.json
      '@': path.resolve(__dirname, './src'),
    },
  },
})
