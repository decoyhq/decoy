# gRPC

Decoy обслуживает унарные вызовы gRPC и вызовы с серверной потоковой передачей на обычном порту. Его делят gRPC поверх незашифрованного HTTP/2 (h2c), обычный HTTP и Control API. Направьте gRPC-клиент в Decoy вместо реального сервиса: сгенерированный код и серверные заглушки не нужны. Клиентская и двунаправленная потоковая передача пока не поддерживаются. Для gRPC поверх TLS [включите TLS](../tls/index.md), и этот же порт будет обслуживать gRPC по HTTPS.

## Корневые каталоги proto

Decoy читает `.proto`-файлы при запуске. Перечислите корневые каталоги поиска импортов в `decoy.yaml`:

```yaml title="mocks/decoy.yaml"
routes: routes
collections: collections.yaml
port: 8080

protos:
  roots: [../proto] # import paths; every .proto under them is compiled
  # descriptorSets: [build/api.binpb]   # precompiled FileDescriptorSets, if compiling is awkward
```

Пути отсчитываются от `decoy.yaml`. Если корневой каталог отсутствует или proto-файл не компилируется, запуск прервётся с указанием файла и причины.

Если в корневом каталоге есть лишние файлы, например в папке `proto/` монорепозитория лежат сервисы, которые нельзя скомпилировать вместе, перечислите входные файлы в `files`. Decoy скомпилирует только их и разрешит их импорты через `roots`:

```yaml title="mocks/decoy.yaml (excerpt)"
protos:
  roots: [../proto]
  files: [../proto/services/catalog/**/*.proto] # `*` stays in one folder, `**` crosses folders
```

Каждый шаблон должен найти хотя бы один `.proto` в одном из корневых каталогов.

## Routes gRPC

```yaml title="mocks/routes/approvals.yaml"
id: approvals-get
transport: grpc
match:
  service: approvals.v1.Approvals # fully qualified, as in the proto
  method: GetApproval

cases:
  pending:
    match:
      message: { id: a-1 }

    behaviors:
      ok:
        message: { id: a-1, state: PENDING, approvers: [alice] }

  any:
    behaviors:
      echo:
        message: { id: '${ message.id }', state: PENDING }

      not-found:
        status: NOT_FOUND
        message: 'approval ${ message.id } not found'
        metadata: { x-reason: missing }

      slow:
        delay: 2s
        message: { id: '${ message.id }', state: PENDING }
```

Collections, Sessions и Control API работают так же, как в HTTP; адреса по-прежнему имеют вид `route:case:behavior`.

## Request envelope

Cases и шаблоны видят один вызов в таком виде:

```json title="gRPC request envelope"
{
  "service": "approvals.v1.Approvals",
  "method": "GetApproval",
  "metadata": { "x-user": "carol", "content-type": "application/grpc" },
  "message": { "id": "a-1" }
}
```

- По умолчанию `message` — декодированный запрос в формате protobuf JSON: имена полей в camelCase, элементы перечислений по именам, 64-битные целые в виде строк, `bytes` как base64. Значения по умолчанию включены (`id: ''`, `state: STATE_UNSPECIFIED`).
- `metadata` — заголовки запроса с именами в нижнем регистре.

В `match` можно указать `message` (частичное сопоставление в глубину, как для `body` в HTTP) и `metadata` (как `headers`, `'*'` означает любое значение) либо использовать предикаты `${ }` по всему конверту.

## Behaviors

| Behavior                        | Ответ                                                              |
| ------------------------------- | ------------------------------------------------------------------ |
| `message: { … }`                | `OK` с указанным сообщением.                                       |
| `status: NAME`, `message: text` | Указанный статус gRPC (`NOT_FOUND`, `UNAVAILABLE`, …) и текст.     |
| `metadata: { name: value }`     | Отправляются как завершающие заголовки (trailers) в обоих случаях. |
| `delay: 500ms`                  | Пауза перед ответом.                                               |

Любая строка может быть шаблоном `${ }`, в том числе `status`.

## Серверная потоковая передача {#server-streaming}

Метод с серверной потоковой передачей принимает один запрос и отвечает потоком сообщений `stream`. Каждое отправляется через `delay` после предыдущего:

```yaml title="mocks/routes/approvals-watch.yaml"
id: approvals-watch
transport: grpc
match: { service: approvals.v1.Approvals, method: WatchApprovals }

cases:
  any:
    behaviors:
      progress:
        stream:
          - { payload: { id: '${ message.ids[0] }', state: PENDING } }
          - { payload: { id: '${ message.ids[0] }', state: APPROVED }, delay: 500ms }
        status: OK # end the call here

      gives-up:
        stream: [{ payload: { id: a-1, state: PENDING } }]
        status: ABORTED
        message: gave up

      live:
        stream: [{ payload: { id: a-1, state: PENDING } }] # no status: stays open
```

- Каждый элемент `stream` содержит данные в `payload`.
- Если указан `status`, вызов завершается после списка: `OK` или ошибка с текстом `message` и `metadata`, как в унарном вызове.
- Без `status` вызов остаётся открытым после списка. Каждый вызов [`send`](../sessions-and-control/index.md#send) из Control API к этому Route из Session вызова добавляет ещё одно сообщение ответа, пока клиент не отменит вызов. Если `send` поступит во время отправки списка, это сообщение придёт после списка. `{}` открывает поток без заранее заданных сообщений. [`wait`](../sessions-and-control/index.md#wait) с `connected: true` завершается, когда такой поток открыт.
- `message` вне списка — только текст ошибки `status`, а для отправки `metadata` нужен `status`. Если указать `stream` у унарного метода, сервер не запустится.
- `delay` на уровне Behavior задаёт паузу перед первым сообщением.
- Событие запроса записывается один раз, при открытии потока.

## Проверки при запуске

- Сервис и метод должны существовать в загруженных proto-файлах.
- Литеральные значения `message` ответа и `stream[].payload` должны соответствовать выходному типу метода: без неизвестных полей, с допустимыми именами элементов перечислений, типами и диапазонами скаляров. Сообщение без `${ }` также разбирается как protobuf JSON.
- Литеральные значения `match.message` должны соответствовать входному типу в форме конверта, описанной выше. `created_at_ms` или `createdAtMs: 5` не совпадут с декодированным запросом и потому вызывают ошибку: используйте имена JSON и записывайте 64-битные числа строками (`createdAtMs: '5'`). Стандартные типы тоже надо записывать в том виде, в каком они представлены в запросе: `Int64Value` как `'5'`, а `Timestamp` в UTC как `'2024-01-01T00:00:00Z'`. Ошибка подскажет правильный формат.
- Имена метаданных — имена заголовков в нижнем регистре, не начинающиеся с `grpc-*` (префикс `grpc-` зарезервирован).
- Литеральные значения метаданных должны содержать только печатные символы ASCII (`0x20`–`0x7E`). Если результат шаблона выходит за этот диапазон, Decoy удалит его из завершающих заголовков и выдаст предупреждение.

Именованный Pipeline может менять форму `message` перед сопоставлением или кодированием в protobuf. Decoy всё равно проверяет метод и структуру Behavior при запуске. Первый выходной воркер может проверить литералы из конфигурации. Шаг `protobuf` сверяет реальные значения с дескриптором во время выполнения Pipeline.

Значения шаблонов проверяются после вычисления. Ошибка вычисления или кодирования даёт `INTERNAL` с Behavior address. Для потока Decoy вычисляет весь список при получении вызова, поэтому в случае ошибки отправляет `INTERNAL` до первого сообщения. Сообщение `send`, не соответствующее выходному типу, отклоняется с `400`, а поток остаётся открытым.

## Несовпадения и ошибки {#misses-and-errors}

| Ситуация                                                 | Статус             | Примечание                                 |
| -------------------------------------------------------- | ------------------ | ------------------------------------------ |
| Ни одно активное Case не совпало с вызовом.              | `UNIMPLEMENTED`    | Завершающий заголовок `x-mock-miss: true`. |
| Вызов сжат.                                              | `INVALID_ARGUMENT` | Отключите сжатие в клиенте.                |
| Тело запроса не декодируется.                            | `INVALID_ARGUMENT` | В сообщении указаны Route и причина.       |
| Не удалось сформировать или закодировать ответ Behavior. | `INTERNAL`         | В сообщении указан `route:case:behavior`.  |

Запросы не пересылаются реальному сервису: Routes gRPC не поддерживают Passthrough.

## Sessions

Передайте `x-mock-session` в метаданных вызова. Это обычный заголовок gRPC, поэтому его может задать любой клиент:

```js title="Client call with Session metadata"
const metadata = new grpc.Metadata()
metadata.set('x-mock-session', 'test-42')

client.getApproval({ id: 'a-1' }, metadata, callback)
```

Затем переключите Collection или Behavior этой Session через [Control API](../sessions-and-control/index.md) по обычному HTTP на том же порту.

## Pipelines {#pipelines}

По умолчанию Route gRPC декодирует вызовы Pipeline `[grpc-frame, protobuf]`. Явно заданный список заменяет его. Decoy декодирует в порядке списка, а кодирует в обратном порядке. Каждый шаг можно использовать один раз. `grpc-frame` должен быть первым, так как тела gRPC всегда содержат кадр; `protobuf`, если указан, идёт после него. Без `protobuf` Route принимает и отдаёт исходные байты: `message` — строка base64.

```yaml title="mocks/routes/approvals.yaml (excerpt)"
pipeline: [grpc-frame] # message is base64 bytes, e.g. `message: CgNhLTE=`
```

Чтобы добавить Transformer, выберите именованный Pipeline через `pipeline: approval-ids` в Route. Например, сохраните этот воркер рядом с `decoy.yaml`. Он нормализует ID до сопоставления и добавляет префикс к каждому ответу:

```js title="approval-ids.mjs"
import { defineTransformer } from '@decoyhq/transformer'

defineTransformer({
  name: 'approval-ids',
  version: '1',
  input: (request) => ({ ...request, id: request.id.toLowerCase() }),
  output: (reply) => ({ ...reply, id: 'ref-' + reply.id }),
})
```

```yaml title="decoy.yaml"
transformers:
  ids: { worker: [node, approval-ids.mjs] }

pipelines:
  approval-ids:
    input: [grpc-frame, protobuf, ids]
    output: [ids, protobuf, grpc-frame]
```

В каждом направлении шаги выполняются в порядке записи. На вход поступают байты запроса в кадре. Здесь `grpc-frame` снимает кадр, `protobuf` декодирует запрос по входному типу метода, а Case получает результат воркера как `message`. Выход начинается со сформированного ответа. Воркер меняет его, `protobuf` кодирует по выходному типу метода, а `grpc-frame` добавляет кадр, ожидаемый клиентом. Те же выходные шаги применяются к каждому сообщению серверного потока и к сообщениям, отправленным через Control API. Непустой выходной список должен вернуть байты в сетевом формате. Если список пропущен или пуст, в этом направлении используется стандартный кодек gRPC и выполняется проверка protobuf при запуске. Непустые списки идут в заданном порядке, включая пользовательские шаги между встроенными. Decoy отклоняет несовместимые соседние встроенные шаги при запуске; пользовательский шаг может изменить тип значения. Команда в конце входного списка должна вывести JSON для сопоставления Cases. Воркер может вернуть значение JSON или байты; Cases видят байты как строку base64. Вызов `miss` в воркере пропускает его Route. Если другие Cases не совпали, клиент получит `UNIMPLEMENTED`. Ошибка именованного входного шага завершает вызов с `INVALID_ARGUMENT`. Если заданный ответ не кодируется, Decoy вернёт `INTERNAL`.

Рабочий клиент Node.js есть в [`examples/grpc-node`](https://github.com/decoyhq/decoy/tree/main/examples/grpc-node).
