# Website Writing Guide

This guide ensures a consistent, professional, and high-signal voice across the wallhack documentation.

## Voice & Tone

- **Technical but Accessible:** Write for someone with a solid networking background. Don't over-explain IP routing or SOCKS, but don't assume deep expertise in QUIC internals.
- **Technically precise:** Don't use analogies or metaphors that are technically wrong, even if they sound intuitive. The audience will notice.
    - ❌ "Tools behave as if they're on the target network." (you have routed *access*, not presence)
    - ✅ "Traffic routes to the target network transparently."
- **Avoid "wallhack" as a Subject:** Do not use the product name as the subject of a sentence. Make the feature or component the subject instead. Imperative ("Establish a TUN interface...") is only appropriate for user instructions — not for describing what the system does.
    - ❌ "wallhack creates a TUN interface..."
    - ❌ "Establish a TUN interface..." (imperative used to describe system behaviour)
    - ✅ "A TUN interface is established on the entry node..." (describes what happens)
    - ✅ "The TUN interface routes all traffic..." (component as subject)
- **No Filler:** Avoid marketing-speak and "filler" phrases.
    - ❌ "Multiple distribution methods to fit your deployment workflow"
    - ✅ "Choose the distribution method that fits your environment"
- **Direct & Opinionated:** Clearly state tradeoffs. If a mode is "faster but less reliable," say it exactly like that.
- **Write for humans, not systems:** Before publishing any sentence, ask whether a real person would say it out loud. Jargon like "isolated Linux network namespaces connected via virtual ethernet pairs" means nothing to a reader. Translate it to plain English: "both nodes on the same machine with no real network between them."
- **Context before data:** Never drop numbers or tables on the reader without first explaining what is being measured, why it was measured, and what it means. A page full of tables with no introduction is not documentation.
- **Subject Clarity:** Ensure "it" or "this" always has a clear antecedent. When describing system behavior, make the feature or component the subject. Never use "wallhack" or "it" (referring to the project) as the subject.
- **Establish before describing:** Don't introduce a term and immediately describe its behavior. The reader needs to know what something *is* before you tell them what it *does*.
    - ❌ "WebSockets wraps traffic as HTTPS for firewall traversal."
    - ✅ "Two transports are available. WebSockets wraps traffic as HTTPS..."
- **Use line breaks:** A wall of sentences is hard to read. Break introductory paragraphs into separate thoughts. One idea per paragraph.
- **Undefined terms:** If you introduce a term, tool, or concept that a reader might not recognise, explain it or link to it. Don't assume they know what netem, yamux, or veth pairs are.

## Document Structure

### Guide vs. Reference — merge or separate intentionally

A topic should either be one page or two clearly distinct pages. If you end up with a "guide" page and a "reference" page for the same topic that are linked to each other, ask whether they should be merged. Two pages for one topic with a link between them is usually a sign they should be one page. Separate guide and reference only makes sense when the reference is long enough that it would bury the guide content.

### Frontmatter

Every page requires frontmatter for SEO and navigation:

- `title`: Short and concise (e.g., "Installation", "Single Hop"). Used in the sidebar and `<title>` tag.
- `description`: A 1-2 sentence summary for SEO. **Do not repeat this description as the first sentence of your document.**
- `order`: Integer for sidebar sorting.

### Headings

- **H1 Start:** Every document must start with a single H1 (`#`) that matches the `title`.
- **Hierarchy:** Use H2 (`##`) and H3 (`###`) for sub-sections. Avoid going deeper than H3 unless absolutely necessary.
- **Descriptive headings:** A heading should tell the reader what they'll find in the section — not be a teaser or clever phrase. If the heading could mean anything, it means nothing.
    - ❌ "Beyond the basics", "Beyond SOCKS", "Transports"
    - ✅ "Performance and Reliability", "Transport Modes", "Download"
- **No Self-Links:** Don't start a section by repeating its title.
    - ❌ `## Scan Mode` followed by "Scan mode is..."
    - ✅ `## Scan Mode` followed by "Use this mode to identify..."

## Components (Markdoc Tags)

Use the built-in components to add structure and visual interest.

### Callouts
Use `{% callout type="..." title="..." %}` for information that needs to stand out.
- `info` (default): General useful information.
- `note`: Important context or subtle "gotchas."
- `tip`: Helpful shortcuts or best practices.
- `warning`: Critical safety or stability warnings.

### Steps
Use `{% steps %}` and `{% step %}` for any sequential process (installation, configuration, deployment). Do not use lists or plain headers for sequences.

### Grids & Cards
Use `{% grid %}` and `{% card %}` only for **genuinely parallel comparisons** (e.g., comparing QUIC vs WebSockets). 
- **Rule of Thumb:** If the content reads better as sequential H2 sections, don't use a grid.
- Do not put prose or long descriptions inside card fields. Use them for high-level summaries.

### Technical Implementation Rules
- **Prop Consistency:** Ensure component props for code-related tags align with standard Markdown attributes (`content` and `language`) to prevent build-time failures in the highlighter.
- **Tailwind v4 Scoped Styles:** When using `@apply` inside a component's `<style>` block, you must include `@reference "../styles/global.css";` to avoid unknown utility errors during build.

### Traffic Flows
Use `{% trafficflow flow="..." label="..." /%}` to visualize tunnel topology. This is essential for complex usage examples.

## Terminology & Formatting

- **CLI Flags:** Use canonical flag names in backticks (`--fast`).
- **Context:** When introducing a flag, show it in a command example immediately or shortly after.
- **WebSockets, not WebSocket:** Use the plural form — it's the colloquially accepted term.
- **Consistency:** Use established terms. Don't switch between "relay," "hop," and "proxy" if they refer to the same component. Canonical section name for CLI/REST API: **Interfaces** (not "Management"). This applies site-wide — if a term is introduced on one page, use the same term on all pages.
    - Canonical terms for network conditions: **packet loss** (not "lossy networks", not "degraded"). "All networks are lossy" — be specific.
    - Canonical terms for transport choice: **reliable** (low/no packet loss), **high packet loss** (>~1%).
- **Interface terminology over implementation terminology:** Use the names users encounter at the interface level, not internal implementation names. If the protocol uses `exit_id` internally but the REST API exposes it as `peer_id`, the docs use `peer_id`. Never leak internal identifiers into user-facing documentation.
- **Audience-aware brevity:** Don't explain what the audience already knows. The target audience knows ligolo, knows SOCKS proxies, knows why Layer 3 matters. State differentiators directly without re-teaching the problem they already understand.
- **Avoid Ambiguous Directional Terms:** Do not use terms like "upstream," "downstream," "inbound," or "outbound" without explicit context. In a pivot, "inbound" could mean "towards the attacker" or "towards the target."
    - ❌ "Connect upstream to the entry node."
    - ✅ "Connect back to the entry node."
    - ✅ "Listen for connections from the next hop."
- **Paths & Files:** Use backticks for file paths, environment variables, and binary names (`/etc/wallhack.conf`, `$HOME`, `wallhack-slim`).

## Code Examples

- **Realism:** Use realistic values (private IPs like `10.0.0.5`, standard ports like `443` or `8080`). Introduce any IPs or networks in the scenario description before they appear in a command.
- **Binary invocation:** Always use `wallhack` — never `./wallhack`. The binary is assumed to be in the path.
- **Focus:** Keep examples focused on the feature being discussed. Don't include unrelated flags.
- **Verification:** Always verify flags against the actual CLI source. Never document a flag that hasn't been implemented yet.
- **Feature flags:** Don't note that a feature requires `--features X` if that feature is included in the default binary. Only call out feature flags when users genuinely need to pass them to unlock functionality.
- **Output:** If a command produces unique or important output, show it in a separate code block or a comment to manage expectations.
