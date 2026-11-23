# TLS

Некоторые клиенты работают только с HTTPS: например, BFF обращается к своим сервисам через TLS, приложение открыто в браузере по HTTPS или клиент gRPC использует TLS. Decoy может включить TLS на одних точках прослушивания и оставить обычный HTTP на других.

## Включение TLS

```yaml title="decoy.yaml (excerpt)"
tls: auto # or: tls: { cert: certs/dev.pem, key: certs/dev-key.pem }

port: 8443 # https
listeners:
  payments: 9443 # https
  legacy-crm: { port: 8080, tls: false } # http
```

- `tls` действует на все точки прослушивания. `{ port, tls: false }` отключает TLS на одной из них.
- Один порт с TLS обслуживает HTTP/1.1, HTTP/2, gRPC и Control API. ALPN выбирает протокол для каждого соединения.
- `tls: auto` при каждом запуске подписывает новый сертификат для `localhost`, `127.0.0.1` и `::1` сроком на год. Локальный центр сертификации (CA) создаётся один раз в `.decoy/` рядом с конфигурацией и используется при следующих запусках. Decoy записывает туда `.gitignore`, чтобы ключ CA не попал в Git.
- `tls: { cert, key }` использует ваши PEM-файлы, например сертификат, уже используемый сервером разработки.

Точные имена ключей см. в разделе [Конфигурация](../config/index.md).

## Доверие к сертификату

Клиенты отклоняют недоверенные сертификаты. Укажите им локальный CA (`tls: auto`) или свой сертификат:

| Клиент                             | Настройка                                                                                                                                                                                                                                               |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Node.js (`fetch`, `https`)         | `NODE_EXTRA_CA_CERTS=mocks/.decoy/ca.pem node …` (переменная читается при запуске процесса).                                                                                                                                                            |
| `@grpc/grpc-js`                    | `credentials.createSsl()` использует доверенные сертификаты Node.js, поэтому подходит тот же `NODE_EXTRA_CA_CERTS`.                                                                                                                                     |
| Playwright (`@decoyhq/playwright`) | Запустите Playwright с `NODE_EXTRA_CA_CERTS=mocks/.decoy/ca.pem`: фикстура обращается к Decoy из Node.js. `use: { ignoreHTTPSErrors: true }` действует только на браузер и `context.request`. Или направьте `decoyOptions.url` на точку с `tls: false`. |
| curl                               | `curl --cacert mocks/.decoy/ca.pem https://localhost:8443/…`                                                                                                                                                                                            |

Подключайтесь к `localhost` или `127.0.0.1`: эти адреса есть в сгенерированном сертификате.

## Пример

[`examples/tls-bff-node`](https://github.com/decoyhq/decoy/tree/main/examples/tls-bff-node) — BFF на Node.js, который обращается к платёжному API по HTTPS, к старой CRM по обычному HTTP и к сервису Inventory по gRPC поверх TLS. Все три сервиса имитирует один экземпляр Decoy; одна Session переключает ответы всех трёх.
