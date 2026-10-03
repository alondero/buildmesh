import { defineConfig } from 'vite';

export default defineConfig({
  root: 'android/terminal',
  base: './',
  build: { outDir: '../app/build/generated/terminal', emptyOutDir: true },
});
