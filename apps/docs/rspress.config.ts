import { resolve } from 'node:path'
import { defineConfig } from '@rspress/core'

const groups = {
  'Get started': {
    '': 'Overview',
    install: 'Install',
    'quick-start': 'Quick start',
    examples: 'Runnable examples',
    terms: 'Glossary',
  },
  'Write mocks': {
    routes: 'Routes',
    collections: 'Collections',
    grpc: 'gRPC',
    websocket: 'WebSocket',
  },
  Transformers: {
    transformers: 'Overview',
    'transformer-pipelines': 'Configure a Pipeline',
    'transformer-sdk': 'TypeScript SDK',
    'transformer-worker-protocol': 'Worker protocol',
  },
  'Run Decoy': {
    config: 'Configuration',
    tls: 'TLS',
    tui: 'Terminal UI',
    troubleshooting: 'Troubleshooting',
  },
  Playwright: {
    playwright: 'Set up Playwright',
    'playwright/responses': 'Switch responses in a test',
    'playwright/websockets': 'Bridge browser WebSockets',
  },
  Testplane: { testplane: 'Set up Testplane', 'testplane/responses': 'Switch responses in a test' },
  Reference: {
    'sessions-and-control': 'Sessions and the Control API',
  },
}
const base = process.env.DOCS_BASE ?? '/'

export default defineConfig({
  root: 'content',
  outDir: 'doc-build',
  title: 'Decoy',
  lang: 'en',
  base,
  builderConfig: { performance: { buildCache: { cacheDigest: [base] } } },
  logo: '/brand/decoy.svg',
  logoText: 'Decoy',
  icon: '/brand/decoy.svg',
  route: { cleanUrls: true },
  llms: true,
  webMcp: false,
  globalStyles: resolve('theme/site.css'),
  themeConfig: {
    darkMode: 'dark',
    socialLinks: [
      { icon: 'github', mode: 'link', content: 'https://github.com/decoyhq/decoy' },
      { icon: 'npm', mode: 'link', content: 'https://www.npmjs.com/package/@decoyhq/cli' },
    ],
    llmsUI: { placement: 'outline', viewOptions: ['viewMarkdown'] },
    nav: [{ text: 'Docs', link: '/docs/' }],
    sidebar: {
      '/docs/': Object.entries(groups).map(([text, pages]) => ({
        text,
        items: Object.entries(pages).map(([slug, title]) => ({
          text: title,
          link: slug ? `/docs/${slug}/` : '/docs/',
        })),
      })),
    },
  },
})
