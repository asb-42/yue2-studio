import path from 'path';
import { defineConfig, loadEnv } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import { readFileSync, existsSync } from 'fs';

// The studio version: the app's own package in a Linux/web fork without the
// Tauri shell, else the desktop shell's manifest as before.
function appVersion(): string {
  const fromApp = JSON.parse(readFileSync(path.resolve(import.meta.dirname, 'package.json'), 'utf8')).version;
  const tauriManifest = path.resolve(import.meta.dirname, '../desktop/src-tauri/tauri.conf.json');
  if (existsSync(tauriManifest)) {
    try {
      return JSON.parse(readFileSync(tauriManifest, 'utf8')).version ?? fromApp;
    } catch {
      return fromApp;
    }
  }
  return fromApp;
}

const appVersionString: string = appVersion();

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, '.', '');
  return {
    server: {
      port: 3791,
      host: '0.0.0.0',
      proxy: {
        '/v1': {
          target: 'http://127.0.0.1:8791',
          changeOrigin: true,
        },
        '/setup': {
          target: 'http://127.0.0.1:8791',
          changeOrigin: true,
        },
        '/engine': {
          target: 'http://127.0.0.1:8791',
          changeOrigin: true,
        },
        '/health': {
          target: 'http://127.0.0.1:8791',
          changeOrigin: true,
        },
        '/mcp': {
          target: 'http://127.0.0.1:8791',
          changeOrigin: true,
        },
        // There is deliberately no proxy for the retired ACE Node service:
        // the studio talks to the native Rust server only, so a stray legacy
        // request fails loudly in development instead of silently 500-ing.
      },
    },
    build: {
      // Tailwind's palette is oklch(); WebView2 before 111 (the last one for
      // Windows 7 and 8.1 is 109) drops those colours and every surface turns
      // transparent. Lightning CSS writes a plain colour first for it.
      cssTarget: 'chrome100',
      cssMinify: 'lightningcss',
      rollupOptions: {
        // the visualiser's own window is a second page
        input: {
          main: path.resolve(import.meta.dirname, 'index.html'),
          visualizer: path.resolve(import.meta.dirname, 'visualizer.html'),
        },
      },
    },
    optimizeDeps: {
      exclude: ['@ffmpeg/ffmpeg', '@ffmpeg/util'],
    },
    define: {
      __APP_VERSION__: JSON.stringify(appVersionString),
    },
    plugins: [react(), tailwindcss()],
    resolve: {
      alias: {
        '@': path.resolve(import.meta.dirname, '.'),
      }
    }
  };
});
