# 0003. Keep request resolution pure and I/O at the runtime edges

Accepted 2026-10-19.

The original runtime mixed matching, per-Session Selection, wire conversion, and network I/O in the
same modules. Adding a transport required editing those modules together, and checking which
Behavior won could involve starting a server.

The engine owns the Catalog and per-Session Selection. Each transport uses both to resolve a
request to a match or miss. Its execution code performs delays, forwarding, and wire
I/O. Server and Control API adapters call inward. The engine can use transport request and plan
types, but transport execution and server dependencies stay outside it.

This keeps matching testable without a clock or network and lets transports use different
execution models without a common transport trait. The [runtime layering rules](../../crates/decoy-runtime/AGENTS.md)
give the dependency boundaries for code changes.
