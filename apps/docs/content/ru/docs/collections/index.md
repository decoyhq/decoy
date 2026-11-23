# Collections

**Collection** перечисляет Behaviors, которые активны одновременно в одном Scenario. Все Collections находятся в одном YAML-файле.

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

- Каждая запись — **Behavior address** `route:case:behavior`. Работает и структурированная форма `{ route: users-by-id, case: user-1, behavior: ada }`. Неявных значений по умолчанию нет: если Case Route не перечислены, Route неактивен.
- `from` наследует другую Collection. Запись дочерней Collection для той же пары `route+case` заменяет родительскую.
- **Первая** Collection используется по умолчанию. Выберите другую при запуске с помощью `decoy serve --collection <id>` (или `collection:` в [файле конфигурации](../config/index.md)), либо для отдельной Session во время работы через [Control API](../sessions-and-control/index.md).

## Порядок важен

Активные записи проверяются снизу вверх, и побеждает последнее подходящее Case. Сначала размещайте общие запасные варианты, затем — более конкретные Case:

```yaml title="collections.yaml (excerpt)"
- id: z-default
  routes:
    - users:any:fallback # pathParams id: '*'
    - users-by-id:user-123:success # wins for /users/123
```
