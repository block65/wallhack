import markdoc from "@astrojs/markdoc";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "astro/config";
import icon from "astro-icon";

export default defineConfig({
	trailingSlash: "never",
	prefetch: {
		prefetchAll: true,
		defaultStrategy: "hover",
	},
	integrations: [markdoc(), icon()],
	vite: {
		plugins: [tailwindcss()],
	},
});
