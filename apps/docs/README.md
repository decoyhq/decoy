# Decoy browser manual

From the repository root, run `pnpm install --frozen-lockfile` and `pnpm --filter @decoyhq/docs dev`. Open the local URL printed by Rspress. Restart the dev server after editing an article so the importer regenerates the pages.

## Edit a page

The English landing page, manual index, quick start and glossary live in `content/`. Add task guides and reference pages under `content/guide/`, starting each file with a single H1. For example, `content/guide/playwright/responses.md` appears at `/docs/playwright/responses/`. Link to a sibling guide by its relative Markdown filename. Add its slug and sidebar label to `groups` in `rspress.config.ts`.

The site also renders example READMEs, the plugin package README, and the worker protocol directly from their source files. Edit those originals. The generated directory is disposable.

## Translate a page

English is the source language. Russian pages use the same route under `/ru/`. To translate an article, put its finished Markdown or MDX at the matching path under `content/ru/`, for example `content/ru/docs/routes/index.md`. Match the generated page's extension and use relative links to neighboring pages. Until a translation exists, the importer adds a notice above the English article.

For another language, register it in `themeConfig.locales` with its translated sidebar and nav, then extend the locale output in `scripts/import-guides.mjs` and supply its home page and fallback notice.

## Build and publish

Run `pnpm --filter @decoyhq/docs build` and serve `apps/docs/doc_build/` from a static host. For a project site under a path, set the base when building, for example `DOCS_BASE=/decoy/ pnpm --filter @decoyhq/docs build`. The output includes HTML, search data, an index at `llms.txt`, and an `index.md` for each article. Deploy that directory independently of a CLI release.

The Documentation site workflow builds a downloadable Pages artifact on docs pull requests. With GitHub Pages configured to use GitHub Actions, running the workflow manually on main publishes the manual under `/decoy/`.
