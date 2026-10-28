import { defineConfig } from 'vite';
import { resolve } from 'path';

export default defineConfig({
  base: '/assets/',
  build: {
    outDir: 'web/dist',
    emptyOutDir: true,
    assetsInlineLimit: 0,
    assetsDir: '',
    rollupOptions: {
      input: {
        phonon: resolve(import.meta.dirname, 'web/src/phonon.js'),
      },
      output: {
        entryFileNames: '[name].js',
        chunkFileNames: '[name]-[hash].js',
        assetFileNames: '[name]-[hash].[ext]',
        format: 'es',
      },
    },
  },
});
