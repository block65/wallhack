import { glob } from "astro/loaders";
import { defineCollection, z } from "astro:content";

const docs = defineCollection({
	loader: glob({ pattern: "**/*.{md,mdx,mdoc}", base: "./src/content/docs" }),
	schema: z.object({
		title: z.string(),
		description: z.string(),
		order: z.number().optional(),
		badge: z.string().optional(),
		badgeVariant: z.enum(["outline", "secondary"]).optional(),
	}),
});

export const collections = { docs };
