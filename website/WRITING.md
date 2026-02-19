# Website Writing Style

Follow the rules in `../standards/writing/technical.md`

## 1. Frontmatter & SEO
Every page requires metadata for navigation and search:
- `title`: Short/concise. Used in sidebar and `<title>`.
- `description`: 1-2 sentence summary. **Do not repeat this as the first sentence of prose.**
- `order`: Integer for sidebar sorting.

### Heading SEO
- **H1 Start:** Every document must start with a single H1 (`#`) matching the `title`.
- **Contextual H1s:** Include descriptive suffixes (e.g., "# [Feature] Reference") to distinguish page content from sidebar labels.

## 2. Custom Components (Markdoc/MDX)
Use these built-in tags to add structure:
- **Callouts:** `{% callout type="..." title="..." %}`. Types: `info`, `note`, `tip`, `warning`.
- **Steps:** `{% steps %}` and `{% step %}` for ALL sequential processes.
- **Grids/Cards:** `{% grid %}` and `{% card %}` for parallel comparisons only.
- **Traffic Flows:** `{% trafficflow flow="..." label="..." /%}` for topology diagrams.

## 3. Code & CLI Conventions
- **Binary invocation:** Use the binary name directly (e.g., `[binary]`) — never `./[binary]`.
- **IP Realism:** Use private IPs (`10.0.0.5`, `192.168.1.1`) and standard ports (`443`, `8080`).
- **Formatting:** Use backticks for CLI flags (`--fast`), file paths (`/etc/config`), and env vars (`$HOME`).

## 4. Build-Time Rules (Technical)
- **Prop Consistency:** Component props for code tags must use `content` and `language` to match standard Markdown highlighters.
- **Tailwind v4 Scoped Styles:** When using `@apply` in `<style>` blocks, include `@reference "../styles/global.css";` to avoid build errors.
