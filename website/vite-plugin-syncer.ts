import { readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import type { Plugin } from "vite";

export interface SyncDocEntry {
	/** Path to the source markdown file (relative to plugin file). */
	src: string;
	/** Path to the destination .mdoc file (relative to plugin file). */
	dst: string;
	/** Extra frontmatter keys merged after extracted title/description. */
	frontmatter?: Record<string, string | number>;
}

export function syncer(files: SyncDocEntry[]): Plugin {
	const resolved = files.map(({ src, dst, frontmatter }) => ({
		src: fileURLToPath(new URL(src, import.meta.url)),
		dst: fileURLToPath(new URL(dst, import.meta.url)),
		frontmatter,
	}));

	const sync = async () => {
		await Promise.all(
			resolved.map(async ({ src, dst, frontmatter }) => {
				const raw = await readFile(src, "utf-8");
				const lines = raw.split("\n");

				const title = lines
					.find((l) => l.startsWith("# "))
					?.replace(/^#\s+/, "");

				const description = lines.find(
					(l) => l.length > 0 && !l.startsWith("#") && !l.startsWith("!["),
				);

				const fm = { title, description, ...frontmatter };
				const header = [
					"---",
					...Object.entries(fm)
						.filter(([_, v]) => v !== undefined)
						.map(([k, v]) => `${k}: ${v}`),
					"---",
					"",
				].join("\n");

				await writeFile(dst, header + raw, "utf-8");
			}),
		);
	};

	return {
		name: "syncer",
		async buildStart() {
			await sync();
		},
		configureServer(server) {
			const srcPaths = resolved.map(({ src }) => src);
			for (const src of srcPaths) {
				server.watcher.add(src);
			}
			server.watcher.on("change", async (changed) => {
				if (srcPaths.includes(changed)) {
					await sync();
				}
			});
		},
	};
}
