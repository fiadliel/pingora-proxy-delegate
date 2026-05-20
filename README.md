<!-- cargo-reedme: start -->

<!-- cargo-reedme: info-start

    Do not edit this region by hand
    ===============================

    This region was generated from Rust documentation comments by `cargo-reedme` using this command:

        cargo +nightly reedme

    for more info: https://github.com/nik-rev/cargo-reedme

cargo-reedme: info-end -->

Middleware layers for Pingora’s `ProxyHttp` trait.

Pingora’s `ProxyHttp` trait has ~40 methods and no built-in middleware or
layer system. A single monolithic impl handles all concerns. Concerns that
span multiple phases (e.g. auth touches `request_filter`,
`upstream_request_filter`, and `logging`) are split across methods shared
with unrelated logic.

Wrapping one `ProxyHttp` in another keeps each concern in one impl, but
requires forwarding ~34 default methods. This crate generates them:

```text
#[proxy_http_delegate(self.inner)]
impl<T: ProxyHttp> ProxyHttp for MyLayer<T> { ... }
```

# `#[proxy_http_delegate]`

Generates `self.<inner>.method(...)` delegations for every `ProxyHttp`
method not already defined in the impl block.

Also emits `#[async_trait]` and `#[deny(clippy::missing_trait_methods)]` on
the impl — do not add these yourself.

## Syntax

```rust
#[proxy_http_delegate(self.inner)]                  // lint defaults to deny
#[proxy_http_delegate(self.inner, lint = warn)]     // downgrade to warning
#[proxy_http_delegate(self.inner, lint = allow)]    // silence the lint
#[proxy_http_delegate(self.inner, lint = deny)]     // explicit deny (default)
#[proxy_http_delegate(self.inner, ctx = ctx.inner)] // map context for delegation
```

The `lint` option controls the `clippy::missing_trait_methods` lint level
emitted on the impl block. It accepts `deny` (default), `warn`, or `allow`.

The `ctx` option remaps the context passed to the inner delegate. When set,
delegation calls pass the mapped expression (e.g. `&mut ctx.inner`) instead
of `ctx` directly. The `new_ctx` delegation is not generated — you must
define it yourself. This is useful when your layer has its own context type
that wraps the inner context.

## Example

```rust
use pingora_core::prelude::*;
use pingora_proxy::{ProxyHttp, Session};
use pingora_proxy_delegate::proxy_http_delegate;

struct InnerProxy;

#[async_trait::async_trait]
impl ProxyHttp for InnerProxy {
    type CTX = ();

    fn new_ctx(&self) -> Self::CTX {
        todo!()
    }
    async fn upstream_peer(
        &self,
        _: &mut pingora_proxy::Session,
        _: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>, Box<pingora_core::Error>> {
        todo!()
    }
}

struct MyDecorator<Inner> {
    inner: Inner,
}

#[proxy_http_delegate(self.inner)]
impl<Inner> ProxyHttp for MyDecorator<Inner>
where
    Inner: ProxyHttp + Send + Sync,
    Inner::CTX: Default + Send + Sync + 'static,
{
    type CTX = Inner::CTX;

    async fn logging(
        &self,
        session: &mut Session,
        error: Option<&pingora_core::Error>,
        ctx: &mut Self::CTX,
    ) where
        Self::CTX: Send + Sync,
    {
        self.inner.logging(session, error, ctx).await;
        // your logic here
    }
}
```

Layers nest: `MyDecorator<AuthLayer<RateLimitLayer<CoreProxy>>>`.

## Compatibility

Targets Pingora 0.8. If Pingora adds a new trait method the
`missing_trait_methods` deny will surface a compile error — update this
crate’s method list and re-release.

<!-- cargo-reedme: end -->
