import { component, defineMarkdocConfig, nodes } from "@astrojs/markdoc/config";
import Shiki from "@astrojs/markdoc/shiki";

export default defineMarkdocConfig({
	extends: [
		Shiki({
			themes: {
				light: "github-light",
				dark: "github-dark",
			},
		}),
	],
	tags: {
		codeblock: {
			render: component("./src/components/CodeBlock.astro"),
			attributes: {
				code: { type: String, required: true },
				lang: { type: String, default: "bash" },
				title: { type: String },
			},
		},
		callout: {
			render: component("./src/components/Callout.astro"),
			attributes: {
				type: {
					type: String,
					default: "info",
					matches: ["info", "warning", "note", "tip"],
				},
				title: { type: String },
			},
		},
		card: {
			render: component("./src/components/Card.astro"),
			attributes: {
				title: { type: String, required: true },
				icon: { type: String },
				description: { type: String },
			},
		},
		badge: {
			render: component("./src/components/Badge.astro"),
			attributes: {
				variant: {
					type: String,
					default: "outline",
					matches: ["outline", "secondary", "default"],
				},
				text: { type: String, required: true },
			},
		},
		steps: {
			render: component("./src/components/Steps.astro"),
		},
		step: {
			render: component("./src/components/Step.astro"),
			attributes: {
				title: { type: String, required: true },
				number: { type: Number },
			},
		},
		grid: {
			render: component("./src/components/Grid.astro"),
			attributes: {
				cols: { type: Number, default: 2 },
			},
		},
		trafficflow: {
			render: component("./src/components/TrafficFlow.astro"),
			attributes: {
				flow: { type: String, required: true },
				label: { type: String },
			},
		},
		feature: {
			render: component("./src/components/Feature.astro"),
			attributes: {
				title: { type: String, required: true },
				icon: { type: String },
			},
		},
		featurelist: {
			render: component("./src/components/FeatureList.astro"),
			attributes: {
				title: { type: String },
				items: { type: Array, required: true },
				icon: {
					type: String,
					default: "border",
					matches: ["check", "border", "bullet"],
				},
			},
		},
		separator: {
			render: component("./src/components/Separator.astro"),
		},
	},
	nodes: {
		fence: {
			render: component("./src/components/CodeBlock.astro"),
			attributes: {
				language: { type: String },
				content: { type: String },
			},
		},
		heading: {
			...nodes.heading,
			render: component("./src/components/Heading.astro"),
		},
	},
});
