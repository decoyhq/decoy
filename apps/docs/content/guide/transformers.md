# Transformers

A Transformer takes a value and produces another. For example, it might extract an ID from an object, decompress bytes, or turn a timestamp into a date string.

A [Pipeline](transformer-pipelines.md) lets you chain these operations. Each step receives the previous step’s result. The input and output directions have their own lists of steps, so you can choose a different conversion for each direction.

## Choose a command or a worker

A command runs once per call. It reads from stdin and writes its result to stdout. Use a command if an existing executable such as `gzip` already does what you need.

A worker stays running between calls. It can keep state per connection or use the protobuf descriptors Decoy has loaded. Use the [TypeScript SDK](../../../../packages/plugin/README.md) to write one, or implement the [worker protocol](../../../../docs/reference/plugin-protocol.md) in another language.

[Configure a Pipeline](transformer-pipelines.md) to connect the steps and check which Route transports support them.
