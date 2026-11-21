# PRD — Transport transformers

Status: Draft for review

## Problem Statement

I want to mock applications whose transport payloads need to change shape before Case matching or after a Behavior is rendered. A payload might need JSON normalization, compression, or an application-specific envelope. I want to configure these transformations in one place, reuse them across Routes, and use either a persistent SDK worker or a small command. WebSocket messages can arrive or be sent independently, and SSE only sends events. The API needs to express direction relative to Decoy across transports.

## Solution

Define named Transformers and ordered Pipelines in `decoy.yaml`. A Route selects a Pipeline by name. Where a transport transforms incoming payloads, `input` runs before Case matching. `output` runs after Decoy renders the Behavior payload and also applies to Control API sends where the transport supports them. Both lists run top to bottom. Each transport defines what enters and leaves the shared Pipeline.

A Transformer uses a persistent SDK worker or one-shot commands. A Pipeline can compose steps from both modes. Use `input` and `output` in configuration, the SDK, and the language-neutral worker protocol.

## User Stories

1. As a Route author, I want a named Pipeline, so that multiple Routes can share the same transformation.
2. As a Route author, I want to find the Pipeline and its Transformers in `decoy.yaml`, so that I can see what runs without opening each Route.
3. As a Route author, I want to configure just `input` or just `output`, so that I can change only the direction my application uses.
4. As a Route author, I want both directions to run in the order I list them, so that I can read the execution order directly.
5. As a Route author, I want to compose an SDK step and a command, so that I can combine application logic with an existing CLI.
6. As a Route author, I want to normalize JSON on input, so that Cases can match a stable message shape.
7. As a Behavior author, I want to transform rendered output, so that my Behavior can use application fields while the client gets its expected wire format.
8. As a Route author, I want an application Transformer to choose a protobuf schema from the message, so that one Route can carry several message types.
9. As an SDK author, I want `input` and `output` callbacks, so that I can express general transformations with the same vocabulary as the config.
10. As an SDK author, I want a persistent worker with per-connection state, so that I can correlate messages across a conversation.
11. As an SDK author, I want access to Decoy's loaded protobuf schemas, so that my worker can reuse them.
12. As a community SDK author, I want a documented worker protocol and a runnable reference client, so that I can implement a compatible SDK in another language.
13. As a command author, I want payload bytes on stdin and transformed bytes on stdout, so that I can use a conventional executable such as gzip.
14. As a command author, I want to provide arguments in config, so that I can reuse an executable with different settings.
15. As a mock author, I want diagnostics when a transformation fails, so that I can locate the failing Pipeline step.
16. As a tester, I want Behavior messages and Control API pushes to use the same output Pipeline, so that both reach the client in its expected format.
17. As a tester, I want failures to stay within the affected message or connection where applicable, so that I can distinguish a transform failure from a Case miss.
18. As a tester, I want Routes to keep their transport defaults when they omit a Pipeline, so that existing fixtures still work.
19. As an SSE Route author, I want to transform outgoing event data, so that subscribers receive the expected text.
20. As a Route author, I want to reuse a Transformer across transports with compatible payloads, so that I can keep shared transformation logic in one place.

## Implementation Decisions

### Agreed design

- A Transformer is a named, reusable definition. A Pipeline step is a reference to a Transformer in one direction. A Pipeline is two independently ordered lists of these references. Routes use the existing `pipeline` field to select a named Pipeline. The same Transformer can appear in different Pipelines.
- Name the two directions `input` and `output` everywhere, including the SDK callbacks and their worker-protocol operations. The names describe where the step runs. Transformations may work with JSON, text, or bytes. Intermediate results flow from step to step. The input Pipeline produces the value used for Case matching. The output Pipeline starts with the rendered logical message.
- Keep the persistent SDK worker. Its protocol remains language-neutral and retains connection state and protobuf host calls. Document framing, handshake, operations, data types, and errors for community SDK authors. Use the SDK-free worker prototype as the interoperability reference.
- One-shot commands use argv, one process per invocation, and EOF-delimited stdin/stdout. Decoy writes the payload to stdin and closes it. Stdout carries the result, stderr carries diagnostics, and a nonzero exit fails the transform. Use the config directory as the working directory as demonstrated by the prototype.

The config shape combines the named Pipeline prototype in [#413](https://github.com/decoyhq/decoy/pull/413) with the agreed Transformer names and direction lists:

```yaml
transformers:
  compression:
    input: [gzip, -dc]
    output: [gzip, -c]
  approvals:
    worker: [node, ./approvals.ts]

pipelines:
  compressed-approvals:
    input: [compression, approvals]
    output: [approvals, compression]
```

### Proposed runtime contract

- Each named worker definition exposes one Pipeline step with either or both directions. The same worker process serves its configured directions. One-shot commands are opt-in. A reference to a Transformer must support the direction that uses it.
- Document conversion between typed worker values and byte-oriented commands at intermediate boundaries. Each transport specifies the payload accepted at both ends of its Pipeline.
- Run the output Pipeline once per recipient connection when sending a message. For the Control API, `delivered` counts queue acceptance. If transformation fails after queueing, Decoy reports the failure and skips the message. This would remove the extra encode preflight present in the worker prototype.
- A one-shot command runs when its Pipeline reaches that step. Decoy consumes stdout through EOF. Startup checks validate the authored payload through the first output step when it is a worker. Preserve a distinct failure diagnostic for a transform error and a Case miss.

### Transport integration

- **WebSocket.** The [WebSocket Routes PRD](websocket-routes-prd.md) defines the Route and Behavior contract. `input` runs on an incoming message payload before the Message envelope is built. A terminal input command emits JSON for Case matching. On `output`, terminal worker text or bytes select the frame kind. A terminal command emits a binary message.
- **SSE.** The event stream uses `output` on each event's `data`. The transport handles text framing and the `event`, `id`, and `retry` fields. Define how a terminal command's bytes become event text when integrating the SSE transport. [#218](https://github.com/decoyhq/decoy/issues/218)
- **Other transports.** Reuse the same Pipeline configuration and SDK contract when adding an integration. Define its payload boundaries and supported directions there. The HTTP and gRPC defaults stay as they are.

## Testing Decisions

- Test through a running Decoy and real transport clients. For WebSocket, assert received messages, frame kinds, and diagnostics. Use the live socket fixtures from [#406](https://github.com/decoyhq/decoy/pull/406) and the approval and telemetry flows from [#414](https://github.com/decoyhq/decoy/pull/414) as prior art. With the SSE integration, assert that a subscriber receives transformed event data with its event metadata intact.
- Exercise worker-only, command-only, and mixed Pipelines. Use non-commuting operations in a mixed Pipeline to prove each list's order. Include a plain JSON transformation alongside binary examples.
- Verify a Behavior and a Control API push go through the same output Pipeline. Cover per-connection worker state, Session isolation, a command failure, and transport defaults when a Route omits a direction or Pipeline.
- Verify the published worker contract with an SDK-free worker over a real WebSocket. Use the interoperability fixture from [#412](https://github.com/decoyhq/decoy/pull/412).
- Use config loading tests for invalid references and direction mismatches. Keep throughput measurements reproducible with the same workload across modes, as in [#411](https://github.com/decoyhq/decoy/pull/411).

## Out of Scope

- Implementing the SSE transport itself. Its output Pipeline integration is covered here and depends on the transport milestone.
- Changes to default HTTP or gRPC behavior, new built-in transformations, plugin-defined template functions, and changes to Route, Case, Behavior, Collection, or Session semantics.

## Further Notes

- Treat the open PR stack as draft work. [#402](https://github.com/decoyhq/decoy/pull/402) through [#406](https://github.com/decoyhq/decoy/pull/406) provide the design, SDK, host, runtime, and test starting points. [#411](https://github.com/decoyhq/decoy/pull/411) through [#414](https://github.com/decoyhq/decoy/pull/414) provide command experiments, measurements, an SDK-free worker, centralized config, and application examples. Rework the stack against this PRD.
- Amend [ADR 0002](../adr/0002-pipeline-steps-and-plugin-workers.md) when the decision is accepted. It currently specifies one list with reversed outbound order. This PRD proposes separate lists with explicit order and an additional one-shot execution mode. Keep the gRPC built-ins boundary proposed in [ADR 0004 in PR #402](https://github.com/decoyhq/decoy/pull/402). Update its WebSocket wording when it lands.
