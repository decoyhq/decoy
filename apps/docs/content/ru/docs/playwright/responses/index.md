# Переключение ответов в тесте

Выберите коллекцию через фикстуру `decoy` до того, как страница отправит запрос. В этом тесте мы включаем `no-users` и проверяем, что на странице появилась ошибка.

```ts title="tests/users.spec.ts"
import { expect, test } from './fixtures'

test('switching the Collection changes what the page shows', async ({ page, decoy }) => {
  await decoy.useCollection('no-users')

  await page.goto('/')
  await page.getByRole('button', { name: 'Load user' }).click()

  await expect(page.getByRole('status')).toHaveText('Error 404: user not found')
})
```

## Что происходит до и после теста

- Если Decoy не отвечает на проверочный запрос, `setup` запускает `command`. В конце прогона `setup` останавливает только тот процесс, который запустила сама. Фикстуры сервер не запускают.
- Каждый тест получает ID сессии (`<file>-<title>-<uuid>`). Прежде чем перенаправлять запросы или выполнять код теста, фикстура проверяет, что Decoy отвечает, и выбирает `collection`.
- Запросы браузера, подходящие под `urls`, идут в Decoy с исходными методом, путём и параметрами запроса, заголовками и телом, а также с `x-mock-session`. Ответ Decoy, включая `501` при отсутствии мока, попадает на страницу как есть. Если Decoy недоступен, запрос отменяется, а не уходит в реальную сеть.
- Фикстура `decoy` предоставляет `sessionId`, `useCollection(id)`, `useRoute(address)` с Behavior address `route:case:behavior` (или `useRoute(route, case, behavior)`), `reset()`, `endSession()` и `connection(route)` (см. [WebSocket](../websockets/index.md)). Методы фикстуры сразу обращаются к Control API и передают ошибку Decoy в тест. После теста фикстура вызывает `endSession()`, чтобы сервер не хранил сессии завершённых тестов.
