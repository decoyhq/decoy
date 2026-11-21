# 0002. Transform wire data before matching Cases

Accepted 2026-10-17. [ADR 0004](0004-built-in-steps-only-for-grpc.md) amends the ordering of steps,
the set of built-ins, and how external transforms run.

A WebSocket frame can contain a msgpack envelope around a protobuf payload. Giving each encoding
its own matcher or config field would make the order of decoding ambiguous and couple Case
definitions to wire formats.

We chose an ordered Pipeline between the transport and the values a Case matches. Cases and
Behavior templates use JMESPath over decoded data. A Plugin can run as a persistent external
worker, so authors can use their language's libraries while Decoy keeps control of matching,
Behavior selection, and Sessions.

The original design used one list of steps and ran it in reverse on output. ADR 0004 replaced
that rule for named Pipelines because an outgoing message can need different transforms.
