import markdoc from "@astrojs/markdoc";
import sitemap from "@astrojs/sitemap";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "astro/config";
import icon from "astro-icon";
import pagefind from "astro-pagefind";
import { syncer } from "./vite-plugin-syncer.ts";

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
		plugins: [
			tailwindcss(),
			syncer([
				{
					src: "../AI_DISCLOSURE.md",
					dest: "src/content/docs/ai-disclosure.mdoc",
					frontmatter: { order: 100 },
				},
			]),
		],
	},
});
