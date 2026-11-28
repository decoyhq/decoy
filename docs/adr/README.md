# Architectural decisions

Read these records for the reasons behind a design choice. For when to add or amend a record, see the [contributor guidelines](../../CONTRIBUTING.md#where-documentation-goes).

| Record                                            | Decision                                                                                                         |
| ------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| [0001](0001-rust-runtime-and-semantic-model.md)   | Use a Rust runtime and model mocks with Routes, Cases, and Behaviors.                                            |
| [0002](0002-pipeline-steps-and-plugin-workers.md) | Use Pipeline steps, JMESPath expressions, and external Transformer workers. ADR 0004 amends the Pipeline design. |
| [0003](0003-runtime-layering.md)                  | Separate the transport-neutral engine, transport implementations, and edge adapters.                             |
| [0004](0004-built-in-steps-only-for-grpc.md)      | Give named Pipelines independent input and output directions and reserve built-in steps for gRPC.                |
| [0005](0005-session-control-commands.md)          | Expose changes to a Session’s Selection as Controller commands.                                                  |
| [0006](0006-shared-pipeline-step-lists.md)        | Let named Pipelines share a step list without losing explicit direction ordering.                                |
