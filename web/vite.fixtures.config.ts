import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

const root = fileURLToPath(new URL('.', import.meta.url))

export default defineConfig({
  plugins: [vue()],
  root,
  resolve: {
    alias: [
      { find: '@/composables/useAuth', replacement: fileURLToPath(new URL('./fixtures/auth.ts', import.meta.url)) },
      { find: '@/generated/contracts', replacement: fileURLToPath(new URL('./fixtures/contracts.ts', import.meta.url)) },
      { find: '@/api/client', replacement: fileURLToPath(new URL('./fixtures/api-client.ts', import.meta.url)) },
      { find: '@/config', replacement: fileURLToPath(new URL('./fixtures/config.ts', import.meta.url)) },
      { find: '@', replacement: fileURLToPath(new URL('./src', import.meta.url)) },
    ],
  },
  server: {
    host: '127.0.0.1',
    port: 4174,
  },
  build: {
    target: 'es2022',
    outDir: '../artifacts/fixture-build',
    emptyOutDir: true,
    rollupOptions: {
      input: fileURLToPath(new URL('./fixture-preview.html', import.meta.url)),
    },
  },
})
