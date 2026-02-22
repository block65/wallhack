set working-directory := "website"

# Install website dependencies (pnpm, frozen lockfile)
deps:
    pnpm install --frozen-lockfile --silent

# Website lint (biome)
lint: deps
    pnpm check

# Website build (astro)
build: deps
    pnpm build
