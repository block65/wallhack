import { readFile } from "node:fs/promises";
import { Resvg } from "@resvg/resvg-js";
import type { APIContext, InferGetStaticPropsType } from "astro";
import { getCollection } from "astro:content";
import satori from "satori";
import { SITE_NAME } from "../../consts.ts";

function load(specifier: string) {
	if (!import.meta.resolve) {
		throw new Error("import.meta.resolve is not available");
	}
	return Promise.resolve(import.meta.resolve(specifier)).then((url) =>
		readFile(new URL(url)),
	);
}

const [interRegular, interBold, dmSerif] = await Promise.all([
	load("@fontsource/inter/files/inter-latin-400-normal.woff"),
	load("@fontsource/inter/files/inter-latin-700-normal.woff"),
	load(
		"@fontsource/dm-serif-display/files/dm-serif-display-latin-400-normal.woff",
	),
]);

export async function getStaticPaths() {
	const docs = await getCollection("docs");
	return [
		...docs.map((doc) => ({
			params: { slug: doc.id },
			props: { title: doc.data.title, description: doc.data.description },
		})),
		{
			params: { slug: "rest-api" },
			props: {
				title: "REST API",
				description:
					"Programmatic HTTP interface for headless management of entry nodes.",
			},
		},
	];
}

type Props = InferGetStaticPropsType<typeof getStaticPaths>;

export async function GET({ props, params }: APIContext<Props>) {
	// Use the brand name as the OG title for the home page — "Overview" is
	// correct for SEO/meta but not meaningful as an image headline.
	const title = params.slug === "index" ? SITE_NAME : props.title;
	const { description } = props;

	// 1200x630 (1.91:1) is the universal OG image size.
	// WhatsApp crops to a centre square (~630x630), so keep all important
	// content centred — avoid placing anything important near the left/right edges.
	const svg = await satori(
		{
			type: "div",
			props: {
				style: {
					display: "flex",
					flexDirection: "column",
					justifyContent: "center",
					alignItems: "center",
					width: "100%",
					height: "100%",
					background: "#0f1117",
					padding: "80px",
				},
				children: [
					// Brand
					{
						type: "div",
						props: {
							style: {
								fontFamily: "DM Serif Display",
								fontSize: 36,
								color: "#7c8fff",
								marginBottom: 40,
							},
							children: "wallhack",
						},
					},
					// Title
					{
						type: "div",
						props: {
							style: {
								fontFamily: "Inter",
								fontWeight: 700,
								fontSize: 56,
								color: "#f0f0f5",
								lineHeight: 1.1,
								textAlign: "center",
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
											marginTop: 24,
											lineHeight: 1.4,
											textAlign: "center",
											maxWidth: "700px",
											textWrap: "balance",
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
		},
	);

	const png = new Uint8Array(new Resvg(svg).render().asPng());

	return new Response(png, {
		headers: {
			"Content-Type": "image/png",
			"Cache-Control": "public, max-age=31536000, immutable",
		},
	});
}
