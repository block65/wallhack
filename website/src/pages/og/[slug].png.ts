import { readFile } from "node:fs/promises";
import { getCollection } from "astro:content";
import type { APIContext, InferGetStaticPropsType } from "astro";
import { Resvg } from "@resvg/resvg-js";
import satori from "satori";

 function load(specifier: string) {
	if (!import.meta.resolve) {
		throw new Error("import.meta.resolve is not available")
	};
	return Promise.resolve(import.meta.resolve(specifier)).then((url) => readFile(new URL(url)));
}

const [interRegular, interBold, dmSerif] = await Promise.all([
	load("@fontsource/inter/files/inter-latin-400-normal.woff"),
	load("@fontsource/inter/files/inter-latin-700-normal.woff"),
	load("@fontsource/dm-serif-display/files/dm-serif-display-latin-400-normal.woff")		,
]);

export async function getStaticPaths() {
	const docs = await getCollection("docs");
	return [
		...docs.map((doc) => ({
			params: { slug: doc.slug },
			props: { title: doc.data.title, description: doc.data.description },
		})),
		{
			params: { slug: "rest-api" },
			props: {
				title: "REST API",
				description: "Programmatic HTTP interface for headless management of entry nodes.",
			},
		},
	];
}

type Props = InferGetStaticPropsType<typeof getStaticPaths>;

export async function GET({ props }: APIContext<Props>) {
	const { title, description } = props;

	const svg = await satori(
		{
			type: "div",
			props: {
				style: {
					display: "flex",
					width: "100%",
					height: "100%",
					background: "#0f1117",
				},
				children: [
					// Left accent bar
					{
						type: "div",
						props: {
							style: {
								width: "8px",
								height: "100%",
								background: "#7c8fff",
								flexShrink: 0,
							},
						},
					},
					// Main content
					{
						type: "div",
						props: {
							style: {
								display: "flex",
								flexDirection: "column",
								flex: 1,
								padding: "64px",
							},
							children: [
								// Brand
								{
									type: "div",
									props: {
										style: {
											fontFamily: "DM Serif Display",
											fontSize: 34,
											color: "#7c8fff",
										},
										children: "wallhack",
									},
								},
								// Spacer
								{
									type: "div",
									props: { style: { flex: 1 } },
								},
								// Title
								{
									type: "div",
									props: {
										style: {
											fontFamily: "Inter",
											fontWeight: 700,
											fontSize: 58,
											color: "#f0f0f5",
											lineHeight: 1.1,
										},
										children: title,
									},
								},
								// Description
								...(description
									? [
										{
											type: "div",
											props: {
												style: {
													fontFamily: "Inter",
													fontWeight: 400,
													fontSize: 24,
													color: "#8890a0",
													marginTop: 20,
													lineHeight: 1.4,
												},
												children: description,
											},
										},
									]
									: []),
								// Domain
								{
									type: "div",
									props: {
										style: {
											fontFamily: "Inter",
											fontWeight: 400,
											fontSize: 18,
											color: "#45495a",
											marginTop: 48,
										},
										children: "wallhack.net",
									},
								},
							],
						},
					},
				],
			},
		},
		{
			width: 1200,
			height: 630,
			fonts: [
				{ name: "Inter", data: interRegular, weight: 400, style: "normal" },
				{ name: "Inter", data: interBold, weight: 700, style: "normal" },
				{
					name: "DM Serif Display",
					data: dmSerif,
					weight: 400,
					style: "normal",
				},
			],
		}
	);

	const png = new Uint8Array(new Resvg(svg).render().asPng());

	return new Response(png, {
		headers: {
			"Content-Type": "image/png",
			"Cache-Control": "public, max-age=31536000, immutable",
		},
	});
}
