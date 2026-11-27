# Подключение WebSocket браузера к Decoy

Задайте `webSocketUrls`, чтобы направить сокеты страницы в Decoy (Playwright 1.48 или новее). Менять страницу не нужно:

```ts title="playwright.config.ts (use excerpt)"
decoyOptions: {
  urls: '**/api/**',
  webSocketUrls: '**/live', // matched against the socket URL, e.g. ws://127.0.0.1:4173/live
}
```

- Для каждого подходящего сокета страницы фикстура открывает соединение с Decoy и сохраняет исходный путь и параметры запроса. Если в `url` указан `https`, она подключается по `wss://`, иначе по `ws://`. Фикстура передаёт ID сессии теста в `x-mock-session`. Текстовые и двоичные фреймы идут в обе стороны. При закрытии одной стороны закрывается и другая, с тем же кодом и причиной, если протокол это позволяет. В конце теста фикстура закрывает все свои оставшиеся сокеты к Decoy.
- Реальный сервер не используется. Если Decoy недоступен, сокет страницы закрывается.
- Сокет остаётся в той же сессии до закрытия. Как настроить маршруты и ответы, описано в разделе [WebSocket](../../websocket/index.md).

Дождитесь подключения страницы, прежде чем отправлять ей сообщение из теста:

```ts title="tests/live.spec.ts (excerpt)"
test('shows a notice the server pushes', async ({ page, decoy }) => {
  await page.goto('/live.html')

  const live = await decoy.connection('friend-socket', { timeout: 5000 })
  await live.send({ type: 'notice', text: 'hello' })

  await expect(page.getByRole('listitem')).toHaveText('hello')
})
```

- `decoy.connection(route, { timeout })` завершается, когда в Session теста открыто соединение на Route (операция `wait` Control API). Вызов отклоняется при истечении ожидания (если `timeout` не задан, используется значение Decoy по умолчанию), неизвестном Route или транспорте без долгоживущих соединений (HTTP, унарный gRPC).
- `.send(message)` отправляет `message` во все активные соединения Route в этой Session, минуя Case (операция [`send`](../../sessions-and-control/index.md#send)). Сокеты других тестов его не получают.
- Ожидание завершается, если в этой сессии есть хотя бы один сокет на нужном маршруте, даже если он уже закрывается. Поэтому после перезагрузки страницы дождитесь её сигнала о новом подключении и только потом вызывайте `send`.
