# Decoy browser manual

From the repository root, run `pnpm install --frozen-lockfile` and `pnpm --filter @decoyhq/docs dev`. Open the URL printed by Rspress.

## Edit a page

Edit the English manual in `content/en/docs/` and its Russian counterpart in `content/ru/docs/`. Keep both locales in sync and use the same kebab-case directory and `index.md` or `index.mdx` name in each. For example, `content/en/docs/playwright/responses/index.md` appears at `/docs/playwright/responses/`, with its Russian counterpart at `/ru/docs/playwright/responses/`. Link to other manual pages by their relative source paths. Add the slug and English label to the sidebar in `rspress.config.ts`, and add a Russian label when it differs. Keep Decoy's core terms in English in the Russian guides.

The two `content/{en,ru}/index.mdx` landing pages share `theme/landing.tsx`.

Keep manual prose here. Link to runnable examples on GitHub rather than copying their READMEs.

## Build and publish

Run `pnpm --filter @decoyhq/docs build` and serve `apps/docs/doc-build/` from a static host. To build for a subpath, set `DOCS_BASE=/decoy/`. The output includes HTML, search data, `llms.txt`, and a Markdown version of each article.

The Documentation site workflow builds a downloadable Pages artifact on docs pull requests. With GitHub Pages configured to use GitHub Actions, running the workflow manually on main publishes the manual under `/decoy/`. If main advances before deployment, the workflow skips the stale build; run it again to publish the current revision.
