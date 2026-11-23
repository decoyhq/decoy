import { resolve } from 'node:path'
import { defineConfig } from '@rspress/core'
import { importGuides } from './scripts/import-guides.mjs'

importGuides()

const groups = {
  'Get started': {
    '': 'Overview',
    install: 'Install',
    quickstart: 'Quick start',
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
const russian: Record<string, string> = {
  'Get started': 'Начало работы',
  Overview: 'Обзор',
  Install: 'Установка',
  'Quick start': 'Первый мок',
  'Runnable examples': 'Готовые примеры',
  Glossary: 'Термины',
  'Write mocks': 'Написание моков',
  Routes: 'Route',
  Collections: 'Collection',
  Transformers: 'Трансформации',
  'Configure a Pipeline': 'Настройка Pipeline',
  'TypeScript SDK': 'SDK для TypeScript',
  'Worker protocol': 'Протокол воркера',
  'Run Decoy': 'Запуск Decoy',
  Configuration: 'Конфигурация',
  'Terminal UI': 'Терминальный интерфейс',
  Troubleshooting: 'Устранение неполадок',
  'Set up Playwright': 'Настройка Playwright',
  'Set up Testplane': 'Настройка Testplane',
  'Switch responses in a test': 'Смена ответа в тесте',
  'Bridge browser WebSockets': 'WebSocket в браузере',
  Reference: 'Справочник',
  'Sessions and the Control API': 'Session и Control API',
}
function sidebar(prefix: string, labels: Record<string, string> = {}) {
  return {
    [prefix]: Object.entries(groups).map(([text, pages]) => ({
      text: labels[text] ?? text,
      items: Object.entries(pages).map(([slug, title]) => ({
        text: labels[title] ?? title,
        link: prefix + (slug ? slug + '/' : ''),
      })),
    })),
  }
}
const base = process.env.DOCS_BASE ?? '/'

export default defineConfig({
  root: '.rspress-content',
  title: 'Decoy',
  lang: 'en',
  base,
  builderConfig: { performance: { buildCache: { cacheDigest: [base] } } },
  logo: '/brand/decoy.svg',
  logoText: 'Decoy',
  icon: '/brand/decoy.svg',
  route: { cleanUrls: true, localeRedirect: 'never' },
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
    locales: [
      {
        lang: 'en',
        label: 'English',
        sidebar: sidebar('/docs/'),
        nav: [{ text: 'Docs', link: '/docs/' }],
      },
      {
        lang: 'ru',
        label: 'Русский',
        sidebar: sidebar('/ru/docs/', russian),
        nav: [{ text: 'Документация', link: '/ru/docs/' }],
      },
    ],
  },
})
