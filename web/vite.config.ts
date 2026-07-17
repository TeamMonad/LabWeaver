import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import { resolve } from 'path'

export default defineConfig(({ command, mode }) => {
  const isFixture = process.env.VITE_DATA_MODE === 'fixture'

  if (command === 'build' && isFixture && mode === 'production') {
    throw new Error('生产构建（production mode）禁止 VITE_DATA_MODE=fixture')
  }

  const bannerVisible =
    command === 'build'
      ? isFixture
      : 'import.meta.env.DEV && import.meta.env.VITE_DATA_MODE === "fixture"'

  return {
    plugins: [vue()],
    define: {
      __FIXTURE_BANNER__: bannerVisible,
      // Compile-time fixture data-mode flag. In production builds this is the
      // literal `false`, so Rollup can eliminate the fixture adapter import
      // branch and no fixture chunk is emitted (production bundle gate).
      __IS_FIXTURE__: isFixture,
    },
    resolve: {
      alias: {
        '@': resolve(__dirname, 'src'),
      },
    },
    server: {
      port: 5173,
      host: true,
      proxy: {
        '/api': {
          target: process.env.VITE_API_PROXY_TARGET || 'http://localhost:8080',
          changeOrigin: true,
        },
        '/auth': {
          target: process.env.VITE_API_PROXY_TARGET || 'http://localhost:8080',
          changeOrigin: true,
        },
      },
    },
    test: {
      environment: 'jsdom',
      globals: true,
      setupFiles: ['./tests/setup.ts'],
      exclude: ['node_modules', 'dist', 'e2e'],
    },
  }
})
