# Routes, Cases и Behaviors

```text title="How a Collection selects a Behavior"
collection → route → case → behavior
```

**Route** — это YAML-файл с `id`, одним `transport` и блоком `match`, который указывает, _где_ происходит взаимодействие. **Cases** в Route определяют, _какие данные запроса_ подходят, а у каждого Case есть именованные **Behaviors** — действия Decoy.

```yaml title="mocks/routes/users.yaml"
id: users-by-id # stable id; no ':' allowed
transport: http # or grpc (gRPC guide), ws (WebSocket guide)
match:
  method: GET # GET POST PUT PATCH DELETE HEAD OPTIONS
  path: /api/users/{id} # {name} captures one whole path segment; /files/{name}.json fails startup

cases:
  user-1:
    match:
      pathParams:
        id: '1' # exact value; '*' matches any value

    behaviors:
      ada:
        status: 200
        body:
          id: '1'
          name: Ada Lovelace

      missing:
        status: 404
        body:
          error: user not found
```

Decoy рекурсивно загружает все файлы `.yaml` и `.yml` из каталога Routes, поэтому их можно группировать по папкам. Routes с одинаковыми ID не дадут серверу запуститься.

## Повторное использование полей с помощью якорей YAML

В файле Route, файле Collections или `decoy.yaml` пометьте набор пар ключ–значение якорем `&name` и подмешайте этот набор в другом месте того же файла через `<<: *name`. Здесь оба Behavior возвращают одинаковое тело:

```yaml title="Route excerpt: YAML anchors"
behaviors:
  ok: &ok
    status: 200
    body: { saved: true }

  created:
    <<: *ok
    status: 201
```

Ключи, заданные напрямую, имеют приоритет независимо от их позиции. Чтобы подмешать несколько наборов, используйте `<<: [*first, *second]`. Если оба содержат одинаковый ключ, побеждает первый. Слияние неглубокое: поле `body` в `created` заменяет унаследованное тело целиком.

## Сопоставление по Case {#case-match}

Case без `match` подходит для любого запроса к своему Route. В противном случае `match` задаёт набор полей, и совпасть должны **все** поля:

```yaml title="Route excerpt: request fields"
cases:
  vip:
    match:
      pathParams: { id: '*' } # '*' = present, any value
      query: { priority: high } # ?priority=high (other params ignored)
      headers: { X-Tier: '*' } # header names are case-insensitive
      cookies: { sid: abc }
      body: { items: [{ sku: gold }] } # deep partial match
```

| Поле                                        | Как сопоставляется                                                                                                                                                                                                            |
| ------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `pathParams`, `query`, `headers`, `cookies` | Каждый указанный ключ должен присутствовать. Строки, числа и логические значения сравниваются как строки (`page: 5` подходит для `?page=5`). Если параметр запроса повторяется, достаточно совпадения любого из его значений. |
| `body`                                      | Объекты: указанные ключи должны совпасть, остальные игнорируются. Массивы: массив запроса должен содержать каждый указанный элемент в любом порядке. Скалярные значения: точное равенство (`1` ≠ `"1"`).                      |

Сегменты пути и `pathParams` сравниваются после процентного декодирования, как и `query` (`/users/a%20b` даёт `id` со значением `a b`; `%2F` остаётся внутри одного сегмента). В конверте `path` сохраняется в исходном виде.

`body` содержит разобранный JSON, если `content-type` запроса указывает на JSON (`application/json`, `*+json`); иначе это исходный текст. Повторяющиеся заголовки запроса объединяются через `", "`. Сервер не запустится, если указано неизвестное поле (например, `querry`), если значение в `pathParams`, `query`, `headers` или `cookies` — список или `null`, либо если ключ в `pathParams` не определён путём Route.

### Предикаты {#predicates}

Если буквального шаблона недостаточно, используйте предикат [JMESPath](https://jmespath.org): строку ровно вида `${ <expression> }`. Он может заменить весь `match` или отдельное поле и всегда получает доступ ко всему запросу:

```yaml title="Route excerpt: predicates"
cases:
  get-user: # GraphQL: same path, different operation
    match:
      headers: { x-client: web } # literal fields and predicates are ANDed
      body: "${ body.operationName == 'GetUser' }"

  big-order:
    match: '${ body.variables.amount > `1000` }'
```

**Request envelope**, доступный предикатам и шаблонам:

```yaml title="HTTP request envelope"
method: POST
url: http://127.0.0.1:8080/graphql?verbose=true
path: /graphql

pathParams: {}
query: { verbose: 'true' } # a repeated key becomes a list: { tag: [a, b] }
headers: { content-type: application/json, x-client: web } # names lowercased
cookies: { sid: abc }

body: { operationName: GetUser } # parsed JSON, or text, or null
```

Значение считается истинным по правилам JMESPath: `null`, `false`, `""`, `[]` и `{}` не дают совпадения; любое другое значение даёт. В выражениях литералы пишутся в обратных кавычках (`` `1000` ``) или в обычных кавычках (`'GetUser'`). Имена заголовков с дефисами требуют кавычек: `headers."x-tier"`. Некорректное выражение не даст серверу запуститься; ошибка укажет `route:case`.

## Behavior с ответом

Используется по умолчанию, если не указан `kind`. Все поля необязательны:

| Поле      | По умолчанию | Примечания                                                                                                                  |
| --------- | ------------ | --------------------------------------------------------------------------------------------------------------------------- |
| `status`  | `200`        |                                                                                                                             |
| `headers` | Нет          | Словарь строк.                                                                                                              |
| `body`    | Пусто        | Объект или массив превращается в JSON с `content-type: application/json`, если тип не задан; строка отправляется как текст. |

### Задержка

Любое Behavior, будь то ответ или Passthrough, может сделать паузу перед ответом. Задайте `delay` целым числом миллисекунд или длительностью с суффиксом `ms` или `s`, например `500ms`, `2s` или `1.5s`. Пока один запрос ждёт, остальные могут выполняться.

```yaml title="Route excerpt: delayed response"
behaviors:
  slow:
    delay: 2s
    status: 504
```

### Шаблоны {#templates}

В любой строке в `status`, `headers` или `body`, на любой глубине, можно использовать `${ <jmespath> }` с теми же данными запроса, что и в [предикатах](#predicates). Ключи не обрабатываются как шаблоны.

```yaml title="Route excerpt: response templates"
behaviors:
  echo:
    status: '${ query.status || `201` }'
    headers:
      x-user: 'user-${ pathParams.id }'

    body:
      id: '${ pathParams.id }' # exactly one ${ } keeps the type: here a string
      age: '${ body.age }' # …here a number
      greeting: 'Hello, ${ body.name }!' # mixed text interpolates (null → "")
      requestId: '${ uuid() }'
      literal: '\${ not a template }' # \${ is a literal ${
```

Шаблон `status` должен вернуть целое число от 100 до 599. Иначе запрос получит `500` с указанием Behavior, в котором произошла ошибка. Некорректные выражения обнаруживаются при запуске, ошибка указывает `route:case:behavior`. Behavior без `${` используются как есть.

В дополнение к [функциям самого JMESPath](https://jmespath.org/specification.html#built-in-functions) доступны:

| Функция                 | Что возвращает                                            |
| ----------------------- | --------------------------------------------------------- |
| `uuid()`                | Случайный UUID v4 в виде строки.                          |
| `now()` / `now('unix')` | Время UTC по RFC 3339 / число секунд с начала эпохи Unix. |
| `random_int(min, max)`  | Случайное целое число, обе границы включены.              |
| `random_item(array)`    | Случайный элемент или `null` для пустого массива.         |

## Pipelines для тела HTTP {#http-body-pipelines}

Задайте `pipeline` в Route, чтобы преобразовать тело запроса до проверки Cases и сформированное тело ответа до отправки. Например, в [примере с типизированным конвертом](https://github.com/decoyhq/decoy/blob/main/examples/ws-transformer-node/mocks/routes/approvals-post.yaml) один воркер обрабатывает HTTP POST и сообщение WebSocket:

```yaml title="mocks/routes/approvals-post.yaml (excerpt)"
id: approvals-post
transport: http
match: { method: POST, path: /approvals }
pipeline: typed-approvals

cases:
  watch:
    match:
      body: { type: approvals.v1.WatchRequest, data: { ids: [a-1] } }

    behaviors:
      approved:
        headers: { content-type: application/msgpack }
        body:
          type: approvals.v1.Approval
          data:
            id: '${ body.data.ids[0] }'
            state: APPROVED
```

Первый воркер получает разобранный JSON для `application/json` и типов с суффиксом `+json`, текст UTF-8 для `text/*` либо исходные байты для двоичного содержимого и тел, которые Decoy не смог разобрать. В примере клиент отправляет `application/msgpack`. Воркер декодирует байты, после чего Case может проверить `body.data.ids`, а шаблон — прочитать ID. Выходной воркер кодирует сформированный объект в байты MessagePack. Decoy отправляет их без изменений с типом содержимого, указанным в конфигурации.

Case одного Route используют общее преобразованное тело и видят исходные заголовки запроса. При проверке другого Route его Pipeline начинает с исходного запроса. Вывод (`stdout`) последней команды входного Pipeline разбирается как JSON для Cases и шаблонов, если это возможно. Прочие двоичные результаты видны там как `null`. Passthrough передаёт двоичный результат без изменений, а в остальных случаях сериализует преобразованное значение выбранного Route. Если Decoy сериализует его в JSON, вышестоящий сервер получит `application/json`, если только исходный тип содержимого уже не обозначает JSON. HTTP-воркеры могут читать исходные метод, путь и заголовки через `ctx.request` в обоих направлениях. Это не меняет набор пересылаемых заголовков. Преобразователи-команды по-прежнему получают только полезную нагрузку в `stdin`.

Если у Route с Passthrough есть выходные шаги, Decoy буферизует тело ответа вышестоящего сервера и декодирует его в соответствии с типом содержимого. Размер буфера ограничен `maxBodyBytes`. Если `Content-Encoding` отличается от `identity`, первый Transformer получает байты даже для типов JSON. Если Passthrough меняет байты на проводе, Decoy удаляет исходные заголовки `Content-Encoding`, `ETag` и заголовки контрольных сумм и отправляет новую длину тела. Если байты не изменились, эти заголовки сохраняются. Ответы моков сохраняют заданный тип и кодирование. Выходные шаги пропускаются для ответов без тела, например на HEAD или со статусом 204. Ответ `206 Partial Content` можно переслать только в том случае, если выходные шаги не изменили ни одного байта; иначе Decoy вернёт `500`, чтобы не отправить неверный диапазон байтов. Ошибки чтения тела ответа считаются ошибками Passthrough, а не Transformer. Входной воркер может вызвать `miss`, чтобы пропустить свой Route. Ошибка Transformer возвращает HTTP 500. При ошибке на входе Decoy не обращается к вышестоящему серверу.

## Behavior с Passthrough {#passthrough-behavior}

Отправляет совпавший запрос (метод, путь и параметры запроса, заголовки, тело) реальному вышестоящему серверу вместо мока:

```yaml title="mocks/routes/profile.yaml"
id: profile
transport: http
match:
  method: GET
  path: /api/profile

cases:
  any:
    match: {}
    behaviors:
      upstream:
        kind: passthrough
        target:
          baseUrl: http://127.0.0.1:4174
```

Выбирается первый заданный адрес в таком порядке: `target` Behavior, `passthrough: { baseUrl }` на верхнем уровне Route, `decoy serve --passthrough-base-url`, затем `Host` самого запроса. Если пересылка не удалась, Decoy вернёт `502` с `{"error":"passthrough forwarding failed", ...}` — это не ответ об отсутствии мока. Без выходных преобразований тело ответа пересылается по мере поступления, поэтому работают SSE, long polling и большие загрузки. Если сервер не подключился за 10 секунд или не отправил заголовки за 60 секунд, Decoy вернёт `504`; если поступление тела остановилось на 60 секунд, ответ прерывается. Запрос, который вернулся к тому же экземпляру Decoy (например, если его `Host` — сам Decoy), получает `508` вместо зацикливания: каждый экземпляр добавляет свой ID в `x-decoy-via`. Заголовок `x-mock-session` не пересылается, поэтому следующий в цепочке Decoy ответит из Session по умолчанию. Все повторяющиеся заголовки ответа, например `set-cookie`, сохраняются. Путь с сегментом `.` или `..`, даже закодированным (`%2e%2e`, `..%2f`) или отделённым символом `\`, получает `502` и не попадает на вышестоящий сервер: так он не сможет выйти за пределы базового пути.

## Что происходит, если мок не найден {#misses-are-fail-closed}

Если запрос не подходит ни под одно активное Case, Decoy возвращает `501` с `x-mock-miss: true` и телом, которое объясняет причины несовпадения для каждой активной записи Route, в порядке проверки снизу вверх:

```json title="HTTP 501 mock-miss response"
{
  "error": "no active route case matched request",
  "candidates": [
    {
      "address": "get-user:user-123:success",
      "reason": "condition",
      "field": "pathParams.id",
      "expected": "123",
      "actual": "456"
    },
    { "address": "list-users:any:ok", "reason": "path", "expected": "/users" }
  ]
}
```

Возможные значения `reason`: `method`, `path` (со значением `expected` Route), `condition` (первое несовпавшее Case: `field`, `expected` и фактическое значение запроса `actual` или `null`, если его нет; для предиката на всё Case `field` равно `match`, а ошибка вычисления предиката даёт `error`), `decode` (ошибка декодирования gRPC или несовпадение от входного HTTP-преобразователя, с `error`), `transport`, `missingRoute`, `missingCase` или `missingBehavior`. Decoy никогда не пересылает запрос без явно выбранного Behavior с Passthrough.
