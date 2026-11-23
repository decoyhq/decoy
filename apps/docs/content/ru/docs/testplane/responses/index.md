# Переключение ответов в тесте

Выберите коллекцию через `decoy(browser)` до того, как страница отправит запрос. В этом тесте мы включаем `no-users` и проверяем, что на странице появилась ошибка.

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

## Что происходит до и после теста

- Перед тестами плагин запускает `command`, если Decoy ещё не отвечает на проверку работоспособности. В конце прогона он останавливает только тот процесс, который запустил сам.
- Каждый тест получает ID сессии (`<browser>-<full title>-<uuid>`). Прежде чем запускать тест или его хуки, в том числе `beforeEach` верхнего уровня файла, плагин проверяет, что Decoy отвечает, и выбирает `collection`. После теста и его хуков `afterEach` плагин завершает сессию.
- Запросы браузера, подходящие под `urls`, из любой вкладки, всплывающего окна, iframe другого сайта или воркера (dedicated, shared, service) направляются в Decoy с исходными методом, путём и параметрами запроса, заголовками, cookie и телом, а также с `x-mock-session`. URL переписывает сам Chrome, поэтому для страницы запрос остаётся на том же origin и CORS не нужен. Страница получает ответ Decoy без изменений, включая `501`, если мок не найден. Если в ответе есть относительный `Location`, браузер отсчитывает его от URL страницы, а не от адреса Decoy. Если Decoy недоступен, запрос завершается ошибкой и не уходит в реальную сеть. Новые вкладки остаются приостановленными, пока перенаправление не включится, поэтому оно охватывает даже их первый запрос. Подходящие запросы между тестами отменяются.
- `decoy(browser)` предоставляет `sessionId`, `useCollection(id)`, `useRoute(address)` с Behavior address `route:case:behavior` (или `useRoute(route, case, behavior)`), `reset()` и `endSession()`. Методы сразу обращаются к Control API и передают ошибку Decoy в тест. Вне теста `decoy(browser)` выбрасывает исключение.
