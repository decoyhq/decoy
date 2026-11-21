# Transformer benchmark notes

These measurements used the prototype runtime on macOS arm64. They compare strategies with the same
small WebSocket payloads. See [the benchmark PR](https://github.com/decoyhq/decoy/pull/411) for the
original harness and results.

The release comparison used eight sockets, one message in flight per socket, and 1,024 checked
request/reply round trips per mode. Each binary payload used the same MessagePack envelope and
framed protobuf message.

| Execution mode                                        |       p95 | Round trips/s |
| ----------------------------------------------------- | --------: | ------------: |
| Plain JSON WebSocket                                  |   0.39 ms |        38,017 |
| Persistent Node worker with local protobuf            |   0.47 ms |        32,591 |
| Persistent Node worker with Decoy protobuf host calls |   0.37 ms |        33,234 |
| One-shot Rust with a precompiled descriptor set       |   7.80 ms |         1,165 |
| One-shot Node, one command per direction              | 187.76 ms |            48 |
| One-shot Node, two commands per direction             | 356.76 ms |            25 |

In a follow-up with compiled Rust message types, the one-shot path reached 1,326 round trips/s
at eight sockets (p95 6.90 ms) and 1,420 at 32 sockets (p95 33.47 ms). A separate soak with 64
sockets reached 52,017 round trips/s using a persistent worker with local protobuf.

The Rust byte-copy command took 2.70 ms per launch at the median. The full transform took 3.03 ms.
A round trip with one command in each direction pays process startup twice. Workers pay that cost
when Decoy starts.
