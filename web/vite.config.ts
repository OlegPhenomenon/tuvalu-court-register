import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// The backend rejects non-GET requests whose Origin host differs from Host
// (CSRF hardening, ARCHITECTURE.md §2). changeOrigin fixes Host; the proxyReq
// hook rewrites Origin to the backend's own origin so POST/PATCH work in dev.
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:8088',
        changeOrigin: true,
        configure: (proxy) => {
          // http-proxy-3 typed emitter; typed structurally to stay @types/node-free.
          const emitter = proxy as unknown as {
            on(event: 'proxyReq', cb: (proxyReq: { setHeader(name: string, value: string): void }) => void): void;
          };
          emitter.on('proxyReq', (proxyReq) => {
            proxyReq.setHeader('Origin', 'http://127.0.0.1:8088');
          });
        },
      },
    },
  },
  build: {
    outDir: 'dist',
  },
});
