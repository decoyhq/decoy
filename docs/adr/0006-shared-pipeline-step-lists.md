# 0006. Let named Pipelines share a step list

Accepted 2026-11-27. Amends [ADR 0004](0004-built-in-steps-only-for-grpc.md).

Nested wire formats usually need opposite execution orders: input removes the outer encoding
first, and output restores it last. Requiring two lists repeats the same steps in reverse.
Requiring every step to work both ways would also rule out useful one-way steps, such as adding
a synthetic request ID to a reply.

A named Pipeline may use one shared step list. Input runs it left to right; output runs it right
to left. Each direction silently skips steps that do not support it. Explicit `input` and `output`
lists still run in the order written and reject steps that do not support the requested direction.
Use explicit lists when the directions need different steps or an order that is not the reverse.

Shared lists can reference configured and built-in Transformers. Built-in implementations live in
`transformers/`; transports run the Pipeline and supply message types. Both forms reject unknown
names. Transformer definitions must have unique names, even when unused. `grpc-frame` and `protobuf`
are reserved globally: existing configurations that shadow either name must rename their Transformer.
Repeating a step reference does not define a new Transformer.

A named Pipeline must declare at least one step. Decoy rejects empty values, empty shared lists,
and mappings with no steps. An omitted or empty direction in an otherwise valid Pipeline still
uses the existing transport behavior.
