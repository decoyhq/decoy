# Подключение WebSocket браузера к Decoy

Задайте `webSocketUrls`, чтобы направить сокеты страницы в Decoy (Playwright 1.48 или новее). Менять страницу не нужно:

```ts title="playwright.config.ts (use excerpt)"
decoyOptions: {
  urls: '**/api/**',
  webSocketUrls: '**/live', // matched against the socket URL, e.g. ws://127.0.0.1:4173/live
}
```

- Для каждого подходящего сокета страницы фикстура открывает свой сокет к Decoy с теми же путём и параметрами запроса (`ws://` или `wss://`, если `url` использует `https`) и устанавливает `x-mock-session` с ID Session теста. Текстовые и двоичные фреймы идут в обе стороны. При закрытии одной стороны закрывается и другая, с тем же кодом и причиной, если протокол это позволяет. В конце теста фикстура закрывает все свои оставшиеся сокеты к Decoy.
- Реальный сервер не используется. Если Decoy недоступен, сокет страницы закрывается.
- Сокет остаётся в Session, с которой был открыт. Настройку Routes и Behaviors см. в разделе [WebSocket](../../websocket/index.md).

Чтобы отправить сообщение из теста, дождитесь сокета страницы и отправьте в него сообщение:

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
- Ожидание завершается, если активен _любой_ сокет Session на Route, в том числе закрывающийся. После перезагрузки дождитесь сигнала от страницы, что новый сокет открыт, и только затем вызывайте `send`.
