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
const russianLabels = {
  Docs: 'Документация',
  'Get started': 'Начало работы',
  Overview: 'Обзор',
  Install: 'Установка',
  'Quick start': 'Быстрый старт',
  'Runnable examples': 'Примеры для запуска',
  Glossary: 'Глоссарий',
  'Write mocks': 'Настройка моков',
  'Configure a Pipeline': 'Настройка Pipeline',
  'Worker protocol': 'Протокол Transformer worker',
  'Run Decoy': 'Запуск Decoy',
  Configuration: 'Конфигурация',
  'Terminal UI': 'Терминальный интерфейс',
  Troubleshooting: 'Устранение неполадок',
  'Set up Playwright': 'Настройка Playwright',
  'Switch responses in a test': 'Переключение ответов в тесте',
  'Bridge browser WebSockets': 'Подключение WebSocket браузера',
  'Set up Testplane': 'Настройка Testplane',
  Reference: 'Справочник',
  'Sessions and the Control API': 'Sessions и Control API',
}
const sidebar = Object.entries(groups).map(([text, pages]) => ({
  text,
  items: Object.entries(pages).map(([slug, title]) => ({
    text: title,
    link: slug ? `/docs/${slug}/` : '/docs/',
  })),
}))
const base = process.env.DOCS_BASE ?? '/'

export default defineConfig({
  root: 'content',
  outDir: 'doc-build',
  title: 'Decoy',
  lang: 'en',
  locales: [
    { lang: 'en', label: 'English' },
    { lang: 'ru', label: 'Русский' },
  ],
  i18nSource: Object.fromEntries(Object.entries(russianLabels).map(([en, ru]) => [en, { en, ru }])),
  base,
  builderConfig: { performance: { buildCache: { cacheDigest: [base] } } },
  logo: '/brand/decoy.svg',
  logoText: 'Decoy',
  icon: '/brand/decoy.svg',
  route: { cleanUrls: true },
  llms: true,
  webMcp: true,
  globalStyles: resolve('theme/site.css'),
  themeConfig: {
    darkMode: 'dark',
    socialLinks: [
      { icon: 'github', mode: 'link', content: 'https://github.com/decoyhq/decoy' },
      { icon: 'npm', mode: 'link', content: 'https://www.npmjs.com/package/@decoyhq/cli' },
    ],
    llmsUI: { placement: 'outline', viewOptions: ['viewMarkdown'] },
    nav: [{ text: 'Docs', link: '/docs/' }],
    sidebar: { '/docs/': sidebar, '/ru/docs/': sidebar },
  },
})
