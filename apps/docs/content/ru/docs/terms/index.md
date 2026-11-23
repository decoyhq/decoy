---
title: Глоссарий
description: Термины из файлов моков, запросов и тестов Decoy.
---

## Route {#route}

Route определяет, какое взаимодействие должен обработать Decoy. В HTTP его задают метод и путь. В [быстром старте](../quick-start/index.mdx) Route `users-by-id` обрабатывает `GET /users/{id}`. И `/users/123`, и `/users/456` подходят под этот путь.

## Case {#case}

Case определяет, какие входные данные важны для Route. Case `user-123` проверяет извлечённый `id`, поэтому ему соответствует только запрос к `/users/123`. Cases также позволяют различать запросы по параметрам строки запроса, заголовкам и телу. [Как проверяются Cases](../routes/index.md#case-match).

## Behavior {#behavior}

Behavior указывает Decoy, что делать, когда запрос соответствует Case. Для Case `user-123` можно задать два Behaviors: `success` возвращает данные Ады, а `missing` — ответ 404. Переключая активное Behavior, можно проверить оба исхода с одним и тем же запросом.

## Behavior address {#behavior-address}

Behavior address содержит в таком порядке Route, Case и Behavior. `users-by-id:user-123:missing` означает: «в Route users-by-id для Case user-123 использовать Behavior missing».

## Collection {#collection}

Collection выбирает Behaviors, которые должны работать вместе. Тест может переключиться с `happy-path` на `user-missing` одним вызовом, даже если в этих Collections указаны Behaviors сразу нескольких Routes. [Как написать Collection](../collections/index.md).

## Scenario

Scenario — ситуация, которую вы хотите воспроизвести в приложении, например оформление заказа, когда пользователь не найден. Collection — то, что вы описываете, чтобы воспроизвести эту ситуацию.

## Session

Session позволяет одному тесту менять ответы, не затрагивая другие тесты на том же сервере. Передавайте одинаковый заголовок `x-mock-session` в запросе управления и в запросе, который должен увидеть изменение. [Session в параллельных тестах](../sessions-and-control/index.md).

## Selection

Selection — это активная Collection и переопределения Behaviors для одной Session. Например, тест может оставить `happy-path` активной, но переопределить Behavior одного Case, чтобы получить 404.

## Catalog

Catalog содержит Routes, Cases, Behaviors, Collections и настройки, загруженные Decoy. Sessions используют общий Catalog, но независимо выбирают, какие Behaviors активировать.

## Fail-closed

Если запрос не сопоставлен, Decoy возвращает диагностический ответ. Например, HTTP-запрос по ненастроенному пути получает ответ о том, что мок не найден. Для обращения к реальному серверу требуется явно выбрать Behavior [Passthrough](#passthrough).

## Passthrough {#passthrough}

Passthrough — Behavior, которое отправляет совпавший запрос вышестоящему серверу. Например, можно имитировать `GET /users/123`, а запросы по другим пользователям отправлять на сервер разработки. [Как настроить Passthrough](../routes/index.md#passthrough-behavior).

## Order-based matching

Decoy проверяет активные Cases снизу вверх по Collection. Если подходят и общее, и более конкретное Case, выигрывает то, которое указано позже. [Как работает порядок в Collection](../collections/index.md).

## Request envelope

Request envelope содержит значения одного запроса, доступные Case или шаблону. Для `GET /users/123` в Request envelope HTTP входят путь и извлечённый ID. В gRPC туда входят метод и декодированное сообщение.

## Message envelope

В открытом WebSocket каждое входящее сообщение получает свой Message envelope. Case может использовать декодированное сообщение вместе с данными соединения, например с его путём или параметрами запроса. [Сопоставление сообщений WebSocket](../websocket/index.md).

## Template context

Template context предоставляет значения, когда Decoy формирует ответ. Например, в тело ответа можно подставить ID из пути входящего запроса. [Как написать шаблон ответа](../routes/index.md#templates).

## Standard function

Standard function встроена в язык выражений Decoy. Например, `uuid()` может сгенерировать ID запроса прямо в шаблоне ответа.

## Controller

Controller меняет Selection для Session. Вызовите `useCollection`, чтобы переключить весь Scenario, или `useRoute`, чтобы выбрать отдельное Behavior.

## Control API

Control API позволяет вызывать операции работающего сервера по HTTP. Например, POST-запрос к `/__decoy__/control/useCollection` переключает Collection для указанной в запросе Session. [Запросы к Control API](../sessions-and-control/index.md#control-api).

## Router

Router — объект, через который тест управляет своей Session. Например, в Playwright вызов `decoy.useCollection('happy-path')` меняет ответы только для этого теста. [Как использовать фикстуру Playwright](../playwright/index.mdx).

## Connection

Объект Connection представляет все активные соединения одного Route в одной Session. После того как браузер откроет WebSocket, тест может через этот объект отправить сообщение подключённому клиенту. [Как отправить сообщение из теста](../sessions-and-control/index.md#send).

## Pipeline

Pipeline задаёт порядок преобразований полезной нагрузки транспорта. Например, Pipeline WebSocket может декодировать двоичные данные перед проверкой Case. У именованного Pipeline есть отдельные списки входных и выходных шагов. [Как настроить Pipeline](../transformer-pipelines/index.md).

## Transformer

Transformer принимает одно значение и возвращает другое. Можно менять формат, содержимое или и то и другое. Например, первый шаг распаковывает байты, а следующий разбирает их в объект. Ещё один Transformer может взять объект и вернуть только его `id`. Каждый шаг передаёт результат следующему. Transformer может работать как команда или как постоянный воркер. [Как написать воркер с TypeScript SDK](../transformer-sdk/index.mdx) или [реализовать протокол воркера](../transformer-worker-protocol/index.md).

## Pipeline step

Pipeline step — одно применение Transformer в списке входных или выходных шагов либо встроенное преобразование gRPC. Позиция в списке определяет, какое значение приходит с предыдущего шага.

## Transformer worker

Transformer worker — постоянный внешний процесс, который выполняет входные или выходные преобразования. Используйте [TypeScript SDK](../transformer-sdk/index.mdx) или реализуйте [протокол воркера](../transformer-worker-protocol/index.md) напрямую.
