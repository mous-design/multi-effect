import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig(({mode}) => ({
  plugins: [react()],
  css: { preprocessorOptions: { scss: { api: 'modern-compiler' } } },
  build: {
    sourcemap: mode === 'debug'
  },
  server: {
    proxy: {
      '/api': 'http://localhost:8080',
      '/ws': { target: 'ws://localhost:8080', ws: true },
    },
  },
}))
