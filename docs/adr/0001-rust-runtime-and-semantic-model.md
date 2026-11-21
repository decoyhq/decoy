# 0001. Share one native runtime and one mock model across clients

Accepted 2026-08-25.

Decoy began as a TypeScript server. Requiring Node made it harder to use from other ecosystems,
and a separate engine for each language would have to reproduce the same matching rules.
We chose a Rust CLI and server that adapters can launch as a native binary.

The runtime owns matching and Selection. A Route identifies the interaction, a Case matches its
input, and a Behavior defines the outcome. Collections select Behaviors by `route:case:behavior`
address. A client can switch a Collection for its Session without implementing any matching logic.
Changing that model affects both authored YAML and every adapter that drives the runtime.
