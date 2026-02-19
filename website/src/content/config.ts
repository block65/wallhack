import { defineCollection, z } from "astro:content";

const docs = defineCollection({
	type: "content",
	schema: z.object({
		title: z.string(),
		description: z.string(),
		order: z.number().optional(),
		badge: z.string().optional(),
		badgeVariant: z.enum(["outline", "secondary"]).optional(),
	}),
});

export const collections = { docs };
