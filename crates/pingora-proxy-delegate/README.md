<!-- cargo-reedme: start -->

<!-- cargo-reedme: info-start

    Do not edit this region by hand
    ===============================

    This region was generated from Rust documentation comments by `cargo-reedme` using this command:

        cargo +nightly reedme -p pingora-proxy-delegate

    for more info: https://github.com/nik-rev/cargo-reedme

cargo-reedme: info-end -->

Complete, context-preserving delegation for Pingora’s `ProxyHttp` trait.

Pingora 0.9’s `ProxyHttp` trait has 42 methods (40 without its
`upstream_modules` feature). Wrapping one implementation
in another keeps a concern in one type, but requires forwarding every hook
that the wrapper does not override. [`proxy_http_delegate`](https://docs.rs/pingora-proxy-delegate/latest/pingora_proxy_delegate/attr.proxy_http_delegate.html) generates those
forwards and rejects a `ProxyHttp` surface change when Clippy runs.

This crate deliberately does not define middleware ordering or reshape the
request context. A composable proxy graph uses one application-owned `CTX`;
decorators access their own state separately and pass the complete context
unchanged when they delegate.

# Syntax

```text
#[proxy_http_delegate(self.inner)]
#[proxy_http_delegate(self.inner, lint = warn)]
#[proxy_http_delegate(self.inner, lint = allow)]
```

The first argument is an expression resolving to the inner `ProxyHttp`, such
as `self.inner`, `self.layers.proxy`, `*self.boxed`, or `self.inner()`. It
must borrow from `self`: forwarded async hooks return the inner future, which
cannot outlive a temporary, so an owned value such as `self.inner.clone()`
fails with “cannot return value referencing temporary value”. `lint`
controls the generated `clippy::missing_trait_methods` lint and defaults to
`deny`.

The macro also emits `#[async_trait]`; do not add that attribute to the same
impl yourself. Pure async forwards return the inner boxed future unchanged,
so each pass-through decorator does not allocate a second future.

Method-level `#[cfg]` and `#[cfg_attr]` attributes are rejected, regardless
of `lint`: disabling an override could silently use Pingora’s default
instead of delegating. Put conditional logic inside an unconditional method
and explicitly delegate in the other branch, or conditionally compile entire
impl blocks, each annotated with this macro.

# Example

```rust
use pingora_core::{Result, upstreams::peer::HttpPeer};
use pingora_proxy::{ProxyHttp, Session};
use pingora_proxy_delegate::proxy_http_delegate;

struct Inner;

#[async_trait::async_trait]
impl ProxyHttp for Inner {
    type CTX = usize;

    fn new_ctx(&self) -> Self::CTX {
        0
    }

    async fn upstream_peer(&self, _: &mut Session, _: &mut Self::CTX) -> Result<Box<HttpPeer>> {
        unimplemented!()
    }
}

struct CountRequests<P> {
    inner: P,
}

#[proxy_http_delegate(self.inner)]
impl<P> ProxyHttp for CountRequests<P>
where
    P: ProxyHttp<CTX = usize> + Send + Sync,
{
    type CTX = P::CTX;

    async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool> {
        *ctx += 1;
        self.inner.request_filter(session, ctx).await
    }
}
```

# Generated-code dependencies

Macro expansions name the crates used by Pingora’s complete trait surface.
Consuming crates therefore need direct dependencies on `async-trait`,
`bytes`, `http`, `pingora-cache`, `pingora-core`, `pingora-http`,
`pingora-proxy`, and `tokio`, in addition to this crate. Keep all Pingora
packages on the same 0.9.x release.

# Upstream modules

Pingora declares `init_upstream_modules` and `adjust_upstream_modules` only
with its `upstream_modules` feature. This crate’s default `upstream_modules`
feature forwards them, and must match Pingora’s. Features are unified
across the dependency graph, so either can be enabled by another crate; a
mismatch in either direction fails to compile:

- Pingora’s feature off, this crate’s on: “method is not a member of trait
  `ProxyHttp`”. Set `default-features = false` on this crate.
- Pingora’s feature on, this crate’s off: “multiple applicable items in
  scope”, naming `EnableUpstreamModulesFeatureOfPingoraProxyDelegate`. Enable
  this crate’s `upstream_modules` feature.

The second check exists because omitting hooks that do exist would
otherwise silently run Pingora’s defaults instead of the inner
implementation.

[`pingora-proxy-router`](https://docs.rs/pingora-proxy-router) provides a
context-preserving router, route slots, and lenses for component-owned state.

This release targets Pingora 0.9. Applications should run Clippy in CI so a
newly added default trait method triggers the generated compatibility lint.

<!-- cargo-reedme: end -->

# Migrating from 0.1

- **Pingora 0.9.** Update every Pingora crate to the same 0.9.x release.
- **`ctx = ...` is removed.** Wrappers now pass the complete context to the
  inner proxy, so `type CTX = P::CTX` and there is no mapped context or
  hand-written `new_ctx`. Move per-layer state into the application-owned
  context and reach it from the layer, for example with a
  [`pingora-proxy-router`](https://docs.rs/pingora-proxy-router) `Lens`.
- **Any expression borrowing from `self` is accepted** as the delegate, not
  only `self.field`; `self.inner()` and `*self.boxed` both work.
- **New default `upstream_modules` feature.** If your build does not enable
  `pingora-proxy/upstream_modules`, set `default-features = false`; see
  [Upstream modules](#upstream-modules).
