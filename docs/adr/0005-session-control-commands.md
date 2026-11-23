# 0005. Expose Session control as commands

Accepted 2026-08-26 for the native HTTP runtime. This records the [original Control API decision](https://github.com/decoyhq/decoy/blob/8edafdfc562e064ab8ea289d969c275fa01efadd/docs/design/native-control-api-shape.md).

Switching a Collection or overriding a Behavior changes one Session's Selection. We considered
modeling these operations as edits to REST resources. That would have required defining resource
identities and lifetimes for the Selection that Decoy drops when a test ends its Session.

The Control API exposes the Controller verbs as commands under `/__decoy__/control/`. Adapters
can express the same operation across process boundaries, and the Session header identifies the Session
whose Selection changes. This decision covers mutations. It leaves room for separate inspection endpoints.

For request bodies, status codes, and examples, see [Sessions and the Control API](../../apps/docs/content/docs/sessions-and-control/index.md).
