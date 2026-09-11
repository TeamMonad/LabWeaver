import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import { resolve } from 'path'

export default defineConfig(() => ({
    plugins: [vue()],
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
  }))
