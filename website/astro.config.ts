import { defineConfig } from 'astro/config';
import markdoc from '@astrojs/markdoc';
import tailwindcss from '@tailwindcss/vite';
import icon from 'astro-icon';

export default defineConfig({
  integrations: [markdoc(), icon()],
  vite: {
    plugins: [tailwindcss()],
  },
});
