import fs from 'node:fs/promises';
import path from 'node:path';
import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Dev server on a fixed port (Tauri devUrl). In browser-dev mode we also
// serve the local media files (library folders + data\thumbs) through a small
// middleware so the same UI code path works outside Tauri (Chromium in both).
const MEDIA_PREFIX = '/localfile/';

const MIME = {
  '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.png': 'image/png',
  '.gif': 'image/gif', '.webp': 'image/webp', '.bmp': 'image/bmp',
  '.tif': 'image/tiff', '.tiff': 'image/tiff', '.heic': 'image/heic',
  '.mp4': 'video/mp4', '.mov': 'video/quicktime', '.m4v': 'video/x-m4v',
  '.webm': 'video/webm', '.mkv': 'video/x-matroska', '.avi': 'video/x-msvideo',
};

function mediaMiddleware() {
  return {
    name: 'photoapp-localfile',
    configureServer(server) {
      server.middlewares.use(async (req, res, next) => {
        const u = new URL(req.url || '/', 'http://localhost');
        if (!u.pathname.startsWith(MEDIA_PREFIX)) return next();
        let abs;
        try {
          abs = decodeURIComponent(u.pathname.slice(MEDIA_PREFIX.length));
        } catch {
          return next();
        }
        // Accept C:\..., /C:/..., C:/...
        abs = abs.replace(/^\/?([a-zA-Z]):[\/\\]/, (m, d) => `${d}:\\`).replace(/\//g, '\\');
        if (!/^[a-zA-Z]:\\/i.test(abs)) {
          res.statusCode = 403;
          return res.end('forbidden path');
        }
        try {
          const st = await fs.stat(abs);
          if (!st.isFile()) {
            res.statusCode = 404;
            return res.end('not a file');
          }
          const mime = MIME[path.extname(abs).toLowerCase()] || 'application/octet-stream';
          const range = req.headers.range;
          if (range) {
            const m = /bytes=(\d*)-(\d*)/.exec(range);
            const start = m && m[1] ? parseInt(m[1], 10) : 0;
            const end = m && m[2] ? parseInt(m[2], 10) : st.size - 1;
            const len = Math.max(0, end - start + 1);
            res.statusCode = 206;
            res.setHeader('Content-Range', `bytes ${start}-${end}/${st.size}`);
            res.setHeader('Accept-Ranges', 'bytes');
            res.setHeader('Content-Length', len);
            res.setHeader('Content-Type', mime);
            const handle = await fs.open(abs, 'r');
            try {
              const buf = Buffer.alloc(len);
              await handle.read(buf, 0, len, start);
              res.end(buf);
            } finally {
              await handle.close();
            }
          } else {
            res.setHeader('Content-Type', mime);
            res.setHeader('Content-Length', st.size);
            const buf = await fs.readFile(abs);
            res.end(buf);
          }
        } catch {
          res.statusCode = 404;
          res.end('not found');
        }
      });
    },
  };
}

export default defineConfig({
  plugins: [svelte(), mediaMiddleware()],
  server: {
    port: 1420,
    strictPort: true,
    // Browser-dev mode: forward /api/* to the Rust JSON server (default 8787).
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:8787',
        changeOrigin: true,
      },
    },
    watch: {
      // Do not watch the media roots or the data dir.
      ignored: ['**/data/**', '**/target/**'],
    },
  },
  build: {
    target: 'es2022',
    outDir: 'dist',
    emptyOutDir: true,
  },
});
