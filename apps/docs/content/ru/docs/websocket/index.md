# WebSocket

Decoy принимает подключения WebSocket на том же порту, что и HTTP, gRPC и Control API. В клиенте укажите адрес Decoy вместо адреса реального сервиса. С включённым [TLS](../tls/index.md) можно подключаться по `wss://`.

Поддерживаются текстовые фреймы с JSON или обычным текстом, Case `on: connect`, ответы с задержкой, `close`, `send` в Control API и двоичные форматы через [Pipelines WebSocket](#websocket-pipelines).

## Routes WebSocket

Decoy выбирает один маршрут на соединение. В `match` укажите путь (`path`), а при необходимости и параметры `query` из запроса на открытие WebSocket. Так можно различать соединения с одним и тем же путём:

```yaml title="mocks/routes/friend-socket.yaml"
id: friend-socket
transport: ws
match:
  path: /subscribe
  query: { service: my-friend-webservice } # other params (client=browser) are ignored

cases:
  hello:
    on: connect # runs once, when the socket opens

    behaviors:
      ping: { message: ping }
      silent: { messages: [] } # say nothing; the test sends ping

      unauthorized:
        message: { type: error, reason: unauthorized }
        close: { code: 4001, reason: unauthorized }

  status:
    match: { message: { type: get-status } } # runs on each matching message
    behaviors:
      online: { message: { id: '${ message.id }', status: online } }
      offline: { message: { id: '${ message.id }', status: offline } }

  subscribe:
    match: { message: { type: subscribe } }
    behaviors:
      updates:
        messages:
          - { payload: { id: '${ message.id }', type: subscribed, topic: '${ message.topic }' } }
          - { payload: { type: update, topic: '${ message.topic }', count: 1 }, delay: 1s }
```

- **Выбор Route:** путь проверяется так же, как для HTTP (`{param}` извлекает сегмент). Каждый указанный параметр `query` должен присутствовать с заданным значением (`'*'` — любое значение). Если подходят несколько Routes, побеждает тот, где больше ключей `query`. При наличии двух Routes, которые могут совпасть с равным приоритетом, сервер не запустится. Если ни один Route `ws` не подходит, запрос обрабатывается как HTTP GET: отвечает HTTP-маршрут по этому пути либо возвращается `501`. То же происходит, если повторяющийся ключ (`?service=a&service=b`) даёт совпадение с двумя Routes равного приоритета.
- **`on: connect`.** В маршруте может быть только одно такое условие. У него нет `match`: оно срабатывает при подключении. Шаблоны получают данные запроса на открытие WebSocket, например `${ query.service }`. Без этого условия Decoy ничего не отправит, пока клиент не пришлёт сообщение или тест не вызовет `send`.
- **Условия для сообщений** проверяют каждый входящий текстовый кадр. В `match` можно указать поля `message` (Decoy сверяет только заданные поля, в том числе вложенные, как для HTTP `body`), а также `pathParams`, `query`, `headers` и `cookies` исходного запроса. Вместо набора полей можно задать один предикат `${ }`. Если текст кадра можно разобрать как JSON, Decoy подставит полученное значение, иначе оставит строку: ``match: '${ message == `"ping"` }'``.
- **Нет совпадения:** Decoy ничего не отправляет, записывает несовпадение в журнал и оставляет сокет открытым. Без `pipeline` ни одно условие не подходит для двоичного кадра.

Коллекции и сессии работают так же, как в HTTP. Добавляйте `on: connect` в коллекцию наравне с другими условиями.

```yaml title="mocks/collections.yaml"
- id: happy-path
  routes:
    - friend-socket:hello:ping
    - friend-socket:status:online
    - friend-socket:subscribe:updates

- id: manual-start
  from: happy-path
  routes:
    - friend-socket:hello:silent
```

## Behaviors

| Поле       | Что делает                                                                                                         |
| ---------- | ------------------------------------------------------------------------------------------------------------------ |
| `message`  | Один ответ. Строка отправляется как есть; словарь или список — как текст JSON.                                     |
| `messages` | Список элементов `{ payload, delay? }`: каждый отправляется через `delay` после предыдущего.                       |
| `close`    | `{ code, reason }`: закрывает сокет после отправки сообщений. `code` — `1000` или число в диапазоне `3000`–`4999`. |
| `delay`    | Начальная задержка, применяется один раз до первой отправки.                                                       |

Используйте либо `message`, либо `messages`. Все строки обрабатываются как шаблоны относительно Message envelope: `{ path, pathParams, query, headers, cookies, message }`.

Поле `delay` на уровне Behavior работает и с `message`, и с `messages`. Задержка элемента отсчитывается от предыдущей отправки. Для первого элемента обе задержки складываются. Оба поля принимают неотрицательное целое число миллисекунд или длительность с суффиксом `ms` либо `s`.

```yaml title="mocks/routes/friend-socket.yaml (Behavior excerpt)"
updates:
  delay: 1000
  messages:
    - { payload: subscribed, delay: 250ms }
    - { payload: update, delay: 500ms }
```

Decoy отправит `subscribed` через 1,25 секунды, а `update` — ещё через 0,5 секунды.

У каждого ответа свой таймер. Если клиент прислал новое сообщение, пока Decoy ещё отправляет предыдущий ответ, сообщения из двух ответов могут прийти вперемешку. После закрытия сокета Decoy отбрасывает всё, что не успел отправить.

## Sessions

Decoy привязывает сокет к сессии по заголовку `x-mock-session` при подключении. Изменения через `useCollection` и `useRoute` действуют со следующего сообщения. Изменения `on: connect` сработают только при новом подключении в этой сессии. В браузере передать заголовки через `new WebSocket()` нельзя. В Playwright их добавляет фикстура (см. [WebSocket в Playwright](../playwright/websockets/index.md)).

## Отправка из теста

[`send`](../sessions-and-control/index.md#send) отправляет сообщение во все открытые сокеты Route в Session вызывающей стороны, минуя Case:

```sh title="Push a text message in test-42"
curl -X POST localhost:8080/__decoy__/control/send \
  -H 'x-mock-session: test-42' \
  -H 'content-type: application/json' \
  -d '{"route":"friend-socket","message":"ping"}'
```

[Пример `ws-node`](https://github.com/decoyhq/decoy/tree/main/examples/ws-node) показывает всё это с настоящим клиентом.

## Преобразования WebSocket {#websocket-pipelines}

[Цепочки преобразований](../transformer-pipelines/index.md) одинаково настраиваются для HTTP, gRPC и WebSocket. Decoy передаёт содержимое кадра в `input` до проверки условий. Если последним входным шагом стоит команда, она должна вывести JSON. Decoy запускает `output`, когда ответ готов или когда отправляет сообщение через Control API. Строку он отправляет текстовым кадром, байты двоичным. Воркеры могут хранить [состояние для каждого соединения](../transformer-sdk/index.mdx).

`send` в Control API подсчитывает сообщения, поставленные в очередь на отправку. Выходной Pipeline запускается по одному разу для каждого соединения-получателя. При ошибке Decoy указывает Transformer в журнале и пропускает фрейм.

Во время преобразования каждый сокет может накопить до 64 следующих фреймов данных общим объёмом не более 16 МиБ. Общий лимит опережающего чтения для всех сокетов процесса — 64 МиБ; учёт ведётся целыми КиБ, минимум 1 КиБ за фрейм. При превышении лимита закрывается сокет, запросивший дополнительное место.

Открытые сокеты сохраняют Pipeline, выбранный при переключении протокола. Вызовы хоста для protobuf используют текущие загруженные дескрипторы, в том числе после перезагрузки. В журнал WebSocket попадает ответ до преобразований и задержек. Поэтому запись в журнале может отличаться от кадров, которые получит клиент.
