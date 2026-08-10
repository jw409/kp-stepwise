# kp-stepwise

Stepwise planning MCP server — a decision ledger for agents. Records
externally-reviewable checkpoints (conclusion, evidence, alternatives, next
action) with branching, confidence tracking, and room/channel isolation so
parallel agents don't overwrite each other's reasoning.

Exposes one tool, `stepwise_plan`.

## Build

```sh
cargo test
cargo build --release   # -> target/release/kp-stepwise
```

## Use

```sh
claude mcp add kp-stepwise --transport stdio -- /path/to/kp-stepwise
```

Environment: `STEPWISE_MODEL`, `KP_STEPWISE_LOG_LEVEL`.

## Relationship to kinderpowers

Extracted from [jw409/kinderpowers](https://github.com/jw409/kinderpowers),
which consumes this repo as a submodule at `mcp-servers/stepwise` and ships a
pre-built binary at `mcp-servers/bin/kp-stepwise`. The plugin's `plugin.json`
points at that binary, so **installing the plugin does not require this repo** —
it's a build-time dependency only.

Source changes land here; kinderpowers then bumps its submodule pointer and
rebuilds binaries via its `mcp-v*` tag workflow.
