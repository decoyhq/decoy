# Переключение ответов в тесте

Используйте `decoy(browser)`, чтобы выбрать Collection до запроса со страницы. Этот тест выбирает `no-users` и проверяет ошибку, которую показывает страница.

```ts title="tests/users.testplane.ts"
import { decoy } from '@decoyhq/testplane'
import { it } from 'testplane'

it('switching the Collection changes what the page shows', async ({ browser }) => {
  await decoy(browser).useCollection('no-users')

  await browser.url('/')
  await browser.$('#load-user').click()

  await expect(browser.$('#result')).toHaveText('Error 404: user not found')
})
```

## Жизненный цикл Session

- Перед тестами плагин запускает `command`, если Decoy ещё не отвечает на проверку работоспособности. В конце прогона он останавливает только тот процесс, который запустил сам.
- Каждый тест получает ID Session (`<browser>-<full title>-<uuid>`). До теста и его хуков, включая `beforeEach` верхнего уровня файла, проверяется работоспособность Decoy и применяется `collection`. После теста и его хуков `afterEach` Session завершается.
- Запросы браузера, подходящие под `urls`, из любой вкладки, всплывающего окна, iframe другого сайта или воркера (dedicated, shared, service) направляются в Decoy с исходными методом, путём и параметрами запроса, заголовками, cookie и телом, а также с `x-mock-session`. Chrome сам переписывает URL, поэтому страница видит собственный origin (без CORS). Ответ Decoy, включая `501` при отсутствии мока, попадает на страницу как есть; относительный `Location` при перенаправлении разрешается относительно URL страницы, а не Decoy. Если Decoy недоступен, запрос завершается ошибкой и не уходит в реальную сеть. Новые вкладки остаются приостановленными, пока перенаправление не включится, поэтому оно охватывает даже их первый запрос. Подходящие запросы между тестами отменяются.
- `decoy(browser)` предоставляет `sessionId`, `useCollection(id)`, `useRoute(address)` с Behavior address `route:case:behavior` (или `useRoute(route, case, behavior)`), `reset()` и `endSession()`. Каждый вызов сразу обращается к Control API и в случае ошибки отклоняется с ошибкой Decoy. Вне теста `decoy(browser)` выбрасывает исключение.
