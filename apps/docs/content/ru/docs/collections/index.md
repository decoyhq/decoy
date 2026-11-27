# Collections

В **Collection** перечислены правила поведения (Behaviors), которые Decoy включает вместе для одного сценария. Все коллекции хранятся в одном YAML-файле.

```yaml title="collections.yaml"
- id: happy-path
  routes:
    - users-by-id:user-1:ada
    - profile:any:upstream

- id: no-users
  from: happy-path
  routes:
    - users-by-id:user-1:missing
```

- Каждая запись содержит **адрес Behavior** в формате `route:case:behavior`. Его можно записать и как объект: `{ route: users-by-id, case: user-1, behavior: ada }`. Маршрут неактивен, если в списке нет ни одного его Case.
- Поле `from` указывает, из какой коллекции взять записи. Если в дочерней и родительской коллекциях есть запись для одной пары `route+case`, Decoy берёт дочернюю.
- По умолчанию Decoy берёт **первую** коллекцию из файла. Чтобы выбрать другую при запуске, передайте `decoy serve --collection <id>` или задайте `collection:` в [конфигурации](../config/index.md). Пока сервер работает, коллекцию отдельной сессии можно сменить через [Control API](../sessions-and-control/index.md).

## Порядок важен

Decoy проверяет активные записи снизу вверх и останавливается на первом подходящем Case. Поэтому общие условия ставьте выше, а частные ниже:

```yaml title="collections.yaml (excerpt)"
- id: z-default
  routes:
    - users:any:fallback # pathParams id: '*'
    - users-by-id:user-123:success # wins for /users/123
```
