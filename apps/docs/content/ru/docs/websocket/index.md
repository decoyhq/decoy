# WebSocket

Decoy принимает запросы на переключение протокола WebSocket на обычном порту, наравне с HTTP, gRPC и Control API. Укажите в клиенте URL сокета на Decoy. Если включён [TLS](../tls/index.md), тот же порт обслуживает `wss://`.

Поддерживаются текстовые фреймы с JSON или обычным текстом, Case `on: connect`, ответы с задержкой, `close`, `send` в Control API и двоичные форматы через [Pipelines WebSocket](#websocket-pipelines).

## Routes WebSocket

На сокет приходится один Route. В `match` задаются `path` запроса на переключение протокола и, при необходимости, параметры `query`, которые должны быть в этом запросе. Это позволяет использовать один путь для нескольких сокетов:

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
- **`on: connect`:** не более одного такого Case на Route. У него нет `match`, и оно срабатывает при открытии сокета. Его шаблоны видят запрос на переключение протокола (`${ query.service }`). Без такого Case сервер молчит, пока не придёт сообщение или тест не вызовет `send`.
- **Case для сообщений** сопоставляют каждый входящий текстовый фрейм. В `match` можно задать `message` (частичное сопоставление в глубину, как у HTTP `body`) и `pathParams`, `query`, `headers`, `cookies` исходного запроса либо один предикат `${ }`. Фрейм, который разбирается как JSON, становится JSON; всё остальное остаётся строкой: ``match: '${ message == `"ping"` }'``.
- **Нет совпадения:** Decoy ничего не отправляет, записывает несовпадение в журнал и оставляет сокет открытым. Без `pipeline` двоичный фрейм никогда не совпадает.

Collections и Sessions работают как в HTTP. Cases `on: connect` добавляются в Collection так же, как остальные.

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

У каждого ответа свой таймер. Если пришло сообщение, а в предыдущем ответе ещё остались отложенные элементы, элементы обоих ответов перемежаются по мере срабатывания таймеров. При закрытии сокета ожидающие элементы отбрасываются.

## Sessions

Сокет остаётся в Session, с которой был открыт (`x-mock-session` в запросе на переключение протокола). `useCollection` и `useRoute` применяются к следующему сообщению, а изменение `on: connect` — к следующему сокету в Session. Браузеры не позволяют задать заголовки в `new WebSocket()`; в Playwright заголовок добавляет мост фикстуры (см. [WebSocket в Playwright](../playwright/websockets/index.md)).

## Отправка из теста

[`send`](../sessions-and-control/index.md#send) отправляет сообщение во все открытые сокеты Route в Session вызывающей стороны, минуя Case:

```sh title="Push a text message in test-42"
curl -X POST localhost:8080/__decoy__/control/send \
  -H 'x-mock-session: test-42' \
  -H 'content-type: application/json' \
  -d '{"route":"friend-socket","message":"ping"}'
```

[Пример `ws-node`](https://github.com/decoyhq/decoy/tree/main/examples/ws-node) показывает всё это с настоящим клиентом.

## Pipelines WebSocket {#websocket-pipelines}

[Настройка Pipeline преобразований](../transformer-pipelines/index.md) одинакова для HTTP, gRPC и WebSocket. В Route WebSocket список `input` получает полезную нагрузку фрейма до проверки Cases; команда в конце входного Pipeline должна вывести JSON. Список `output` выполняется после формирования ответа или при отправке сообщения через Control API. Итоговая строка становится текстовым фреймом, итоговые байты — двоичным. Воркеры могут хранить [состояние для каждого соединения](../transformer-sdk/index.mdx).

`send` в Control API подсчитывает сообщения, поставленные в очередь на отправку. Выходной Pipeline запускается по одному разу для каждого соединения-получателя. При ошибке Decoy указывает Transformer в журнале и пропускает фрейм.

Во время преобразования каждый сокет может накопить до 64 следующих фреймов данных общим объёмом не более 16 МиБ. Общий лимит опережающего чтения для всех сокетов процесса — 64 МиБ; учёт ведётся целыми КиБ, минимум 1 КиБ за фрейм. При превышении лимита закрывается сокет, запросивший дополнительное место.

Открытые сокеты сохраняют Pipeline, выбранный при переключении протокола. Вызовы хоста для protobuf используют текущие загруженные дескрипторы, в том числе после перезагрузки. События запросов WebSocket зафиксируют сформированный ответ до выходных преобразований и задержек. Поэтому записанный ответ может отличаться от фреймов, которые получит клиент.
