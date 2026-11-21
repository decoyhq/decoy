// Render the existing manual and example READMEs from their canonical sources.
import {
  cpSync,
  existsSync,
  globSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import { dirname, join, posix } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('../../../', import.meta.url))
const output = fileURLToPath(new URL('../.rspress-content/', import.meta.url))
const sourceRoot = 'https://github.com/decoyhq/decoy/blob/main/'

export function importGuides() {
  rmSync(output, { recursive: true, force: true })
  cpSync(join(root, 'apps/docs/public'), join(output, 'public'), { recursive: true })
  const pages = new Map([
    ['packages/plugin/README.md', 'docs/transformer-sdk'],
    ['docs/reference/plugin-protocol.md', 'docs/transformer-worker-protocol'],
  ])
  for (const source of globSync('apps/docs/content/guide/**/*.md', { cwd: root })) {
    pages.set(source, source.replace('apps/docs/content/guide/', 'docs/').replace(/\.md$/, ''))
  }
  for (const source of globSync('examples/*/*.md', { cwd: root })) {
    pages.set(source, 'docs/' + source.replace(/\/README\.md$|\.md$/g, ''))
  }
  const routes = new Map([
    ...pages,
    ['CONTEXT.md', 'docs/terms'],
    ['apps/docs/content/terms.md', 'docs/terms'],
    ['apps/docs/content/index.md', 'docs'],
    ['apps/docs/content/quickstart.mdx', 'docs/quickstart'],
  ])
  function render(slug, title, body) {
    const tabs = (commands) =>
      '<Tabs groupId="package-manager">\n' +
      Object.entries(commands)
        .map(
          ([name, command]) =>
            '<Tab label="' + name + '">\n\n```sh\n' + command + '\n```\n\n</Tab>',
        )
        .join('\n') +
      '\n</Tabs>'
    const install = (packages) =>
      tabs({
        npm: 'npm install -D ' + packages,
        pnpm: 'pnpm add -D ' + packages,
        Yarn: 'yarn add -D ' + packages,
        Bun: 'bun add -d ' + packages,
      })
    const serve =
      'decoy serve --routes mocks/routes --collections mocks/collections.yaml --port 8080 --watch'
    body = body
      .replace(/^```sh\nnpm i -D ([@\w/ .-]+)\n```$/m, (_, packages) => install(packages))
      .replace(
        '```sh\nnpm exec --package=@decoyhq/cli -- ' + serve + '\n```',
        tabs({
          npm: 'npm exec --package=@decoyhq/cli -- ' + serve,
          pnpm: 'pnpm exec ' + serve,
          Yarn: 'yarn ' + serve,
          Bun: 'bun run ' + serve,
          Standalone: serve,
        }),
      )
    const hasTabs = body.includes('<Tabs ')
    const ext = hasTabs ? 'mdx' : 'md'
    const prefix = hasTabs ? "import { Tabs, Tab } from '@rspress/core/theme'\n\n" : ''
    const dest = join(output, slug, 'index.' + ext)
    mkdirSync(dirname(dest), { recursive: true })
    const article = '# ' + title + '\n\n' + body
    writeFileSync(dest, prefix + article)
    const translated = join(root, 'apps/docs/content/ru', slug, 'index.' + ext)
    const ru = join(output, 'ru', slug, 'index.' + ext)
    mkdirSync(dirname(ru), { recursive: true })
    if (existsSync(translated)) cpSync(translated, ru)
    else
      writeFileSync(ru, prefix + '> Эта страница пока доступна только на английском.\n\n' + article)
  }
  function articleLinks(file, slug, body) {
    return body.replace(/\]\(([^)\s]+)\)/g, (link, href) => {
      if (href.startsWith('#')) return link
      const repoLink = href.startsWith(sourceRoot)
      if (!repoLink && /^\w+:/.test(href)) return link
      const [path, fragment] = href.replace(sourceRoot, '').split('#')
      const target = repoLink ? path : posix.join(dirname(file), path)
      const route = routes.get(target) || routes.get(target + '/README.md')
      if (!route) throw new Error(`Missing docs route for ${href} in ${file}`)
      const relative = posix.relative(slug, route) || '.'
      return `](${relative}/index.md${fragment ? '#' + fragment : ''})`
    })
  }
  for (const [file, slug] of pages) {
    const [heading, ...lines] = readFileSync(join(root, file), 'utf8').split('\n')
    const title = heading.replace(/^# /, '')
    render(slug, title, articleLinks(file, slug, lines.join('\n')))
  }
  for (const [file, slug] of [
    ['index.md', 'docs'],
    ['quickstart.mdx', 'docs/quickstart'],
    ['terms.md', 'docs/terms'],
  ]) {
    const source = readFileSync(join(root, 'apps/docs/content', file), 'utf8')
    const [, title, body] = source.match(/^---\ntitle: ([^\n]+)[\s\S]*?\n---\n([\s\S]*)$/)
    render(slug, title, articleLinks('apps/docs/content/' + file, slug, body))
  }
  cpSync(join(root, 'apps/docs/content/home.mdx'), join(output, 'index.mdx'))
  cpSync(join(root, 'apps/docs/content/ru/home.mdx'), join(output, 'ru/index.mdx'))
}
