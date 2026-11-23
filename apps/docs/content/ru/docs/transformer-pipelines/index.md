# Настройка Pipeline преобразований

Определите Transformer один раз в `decoy.yaml`, чтобы использовать его в нескольких именованных Pipelines. В примере ниже `gzip` запускает команду, а `approvals` использует постоянный воркер. Pipeline `approvals-feed` распаковывает входящие данные и передаёт их воркеру. На выходе он сначала запускает воркер, а затем сжимает результат.

```yaml title="decoy.yaml (excerpt)"
transformers:
  approvals:
    worker: [node, ../workers/approvals.ts]

  gzip:
    input: [gzip, -dc]
    output: [gzip, -c]

pipelines:
  approvals-feed:
    input: [gzip, approvals]
    output: [approvals, gzip]
```

## Выберите Pipeline для Route

Задайте `pipeline` рядом с `id`, `transport` и `match` в файле Route. В обоих направлениях шаги выполняются в порядке записи. Несколько совместимых Routes могут использовать один именованный Pipeline.

```yaml title="Route excerpt: select a named Pipeline"
pipeline: approvals-feed
```

| Транспорт                                              | Что преобразует Pipeline                                                                                                                         |
| ------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| [HTTP](../routes/index.md#http-body-pipelines)         | Тела запросов и ответов.                                                                                                                         |
| [gRPC](../grpc/index.md#pipelines)                     | Байты запросов и ответов в кадрах. Если Transformer работает с декодированными сообщениями, добавьте `grpc-frame` и `protobuf` в нужном порядке. |
| [WebSocket](../websocket/index.md#websocket-pipelines) | Полезную нагрузку входящих фреймов и исходящие сообщения, в том числе отправленные через Control API.                                            |

В именованном Pipeline gRPC настроенный Transformer имеет приоритет над одноимённым встроенным шагом. Рабочие настройки есть в [общем примере для HTTP и WebSocket](https://github.com/decoyhq/decoy/tree/main/examples/ws-transformer-node) и в [примере для gRPC](https://github.com/decoyhq/decoy/tree/main/examples/grpc-node).

## Проверка и ограничения выполнения

При загрузке Routes Decoy проверяет указанные Transformers и направления. Если воркер стоит первым на выходе, он также может проверить литерал из конфигурации при запуске. Чтобы применить изменения конфигурации воркера, перезапустите Decoy.

Одноразовая команда преобразования должна завершиться за 5 секунд. Преобразование завершится ошибкой, если объём `stdout` превысит 16 МиБ или `stderr` превысит 64 КиБ. В процессе Decoy могут одновременно работать не более восьми таких команд. Если все места заняты, следующая команда завершит преобразование ошибкой, даже не запустившись.
