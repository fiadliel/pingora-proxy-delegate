# Pingora proxy composition

Two crates for composing Pingora 0.9 `ProxyHttp` implementations while preserving
one application-owned request context:

| Crate | Purpose |
| --- | --- |
| [pingora-proxy-delegate](crates/pingora-proxy-delegate/README.md) | Generates forwarding hooks for decorators, with explicit overrides for their own behavior. |
| [pingora-proxy-router](crates/pingora-proxy-router/README.md) | Selects and pins routes per request, and provides context lenses for component state. |

See each crate's README for usage, dependencies, and feature configuration.

## Development

The repository root is a virtual Cargo workspace. Both crates live under
`crates/` and share the root lockfile, CI, and formatting configuration.
The minimum supported Rust version is 1.85.1.

```sh
cargo test --workspace --all-targets
cargo test --workspace --doc
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

Use `-p pingora-proxy-delegate` or `-p pingora-proxy-router` to select one crate.
Feature-disabled checks run separately because workspace dev-dependencies enable
Pingora's upstream modules:

```sh
cargo test -p pingora-proxy-router --all-targets --no-default-features
cargo test -p pingora-proxy-delegate --all-targets --no-default-features
```

The second command verifies that mismatched upstream-module features produce a
compile error. Run `mise run fmt` to apply the nightly formatting configuration.

Crate READMEs are generated from their Rust documentation:

```sh
cargo +nightly reedme -p pingora-proxy-delegate
cargo +nightly reedme -p pingora-proxy-router
```

## License

MIT OR Apache-2.0; see [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE).
