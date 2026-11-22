import { definePlugin } from '@decoyhq/plugin'

export default definePlugin({
  name: 'http-context',
  version: '1.0.0',
  input(_body, { request }) {
    const preview = request?.headers.cookie
      ?.split(';')
      .some((cookie) => cookie.trim() === 'preview=1')
    return { preview: preview ?? false }
  },
  output(result, { request }) {
    return {
      result,
      client: request?.headers['x-demo-client'],
      method: request?.method,
      path: request?.path,
    }
  },
})
