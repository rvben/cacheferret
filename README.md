# cacheferret

[![CI](https://github.com/rvben/cacheferret/actions/workflows/ci.yml/badge.svg)](https://github.com/rvben/cacheferret/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/cacheferret.svg)](https://crates.io/crates/cacheferret)
[![clispec](https://img.shields.io/badge/clispec-v0.2-blue)](https://clispec.dev)

Find and safely clean developer caches across macOS and Linux

## Install

```sh
cargo install cacheferret
```

## Usage

```sh
cacheferret 21          # => 21 doubled is 42   (text on a TTY)
cacheferret 21 | jq .   # => {"value":21,"doubled":42}   (JSON when piped)
```

> This is the scaffolded example command. Replace the `run` logic in
> `src/lib.rs`, the command in `src/schema.rs`, and these docs with your tool.

## Exit codes

| code | meaning |
| --- | --- |
| `0` | success |
| `1` | invalid input |
| `3` | usage error |

## For agents (clispec)

cacheferret follows [The CLI Spec](https://clispec.dev): structured output on
stdout, structured error envelopes on the last line of stderr, and a `schema`
subcommand whose output validates against `clispec.dev/schema/v0.2.json`
(checked by the test suite).

```sh
cacheferret schema
```

## License

MIT
