import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'

export default defineConfig({
  base: '/admin/',
  plugins: [react()],
  server: {
    host: '127.0.0.1',
    proxy: { '/admin/v1': 'http://127.0.0.1:8080' },
  },
  test: {
    environment: 'jsdom',
    setupFiles: './src/test-setup.ts',
    include: ['src/**/*.test.ts'],
  },
})
