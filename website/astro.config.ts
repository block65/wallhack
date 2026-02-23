import markdoc from "@astrojs/markdoc";
import sitemap from "@astrojs/sitemap";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "astro/config";
import icon from "astro-icon";
import pagefind from "astro-pagefind";

export default defineConfig({
	site: "https://wallhack.net",
	trailingSlash: "never",
	build: {
		format: "preserve",
	},
	prefetch: {
		prefetchAll: true,
		defaultStrategy: "hover",
	},
	integrations: [markdoc(), icon(), sitemap(), pagefind()],
	vite: {
		plugins: [tailwindcss()],
	},
});
