# Decoy browser manual

From the repository root, run `pnpm install --frozen-lockfile` and `pnpm --filter @decoyhq/docs dev`. Open the URL printed by Rspress.

## Edit a page

Edit the landing page at `content/index.mdx` and the manual in `content/docs/`. Use an `index.md` or `index.mdx` in a kebab-case directory so links end in `/`. For example, `content/docs/playwright/responses/index.md` appears at `/docs/playwright/responses/`. Link to other manual pages by their relative source paths. Add the new slug and label to the sidebar in `rspress.config.ts`.

Keep manual prose here. Link to runnable examples on GitHub rather than copying their READMEs.

## Build and publish

Run `pnpm --filter @decoyhq/docs build` and serve `apps/docs/doc-build/` from a static host. To build for a subpath, set `DOCS_BASE=/decoy/`. The output includes HTML, search data, `llms.txt`, and a Markdown version of each article.

The Documentation site workflow builds a downloadable Pages artifact on docs pull requests. With GitHub Pages configured to use GitHub Actions, running the workflow manually on main publishes the manual under `/decoy/`.
