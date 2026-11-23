# Переключение ответов в тесте

Используйте фикстуру `decoy`, чтобы выбрать Collection до запроса со страницы. Этот тест выбирает `no-users` и проверяет ошибку, которую показывает страница.

```ts title="tests/users.spec.ts"
import { expect, test } from './fixtures'

test('switching the Collection changes what the page shows', async ({ page, decoy }) => {
  await decoy.useCollection('no-users')

  await page.goto('/')
  await page.getByRole('button', { name: 'Load user' }).click()

  await expect(page.getByRole('status')).toHaveText('Error 404: user not found')
})
```

## Жизненный цикл Session

- `setup` (глобальная настройка) запускает `command`, если Decoy ещё не отвечает на проверку работоспособности. При завершении останавливается только то, что было запущено в `setup`. Фикстуры не запускают Decoy.
- Каждый тест получает ID Session (`<file>-<title>-<uuid>`). До перенаправления запросов и выполнения кода теста проверяется работоспособность Decoy и применяется `collection`.
- Запросы браузера, подходящие под `urls`, идут в Decoy с исходными методом, путём и параметрами запроса, заголовками и телом, а также с `x-mock-session`. Ответ Decoy, включая `501` при отсутствии мока, попадает на страницу как есть. Если Decoy недоступен, запрос отменяется, а не уходит в реальную сеть.
- Фикстура `decoy` предоставляет `sessionId`, `useCollection(id)`, `useRoute(address)` с Behavior address `route:case:behavior` (или `useRoute(route, case, behavior)`), `reset()`, `endSession()` и `connection(route)` (см. [WebSocket](../websockets/index.md)). Каждый вызов сразу обращается к Control API и в случае ошибки отклоняется с ошибкой Decoy. При завершении теста фикстуры вызывают `endSession()`, поэтому долго работающий Decoy не хранит по Session для каждого прошедшего теста.
