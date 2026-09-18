<!-- cargo-reedme: start -->

<!-- cargo-reedme: info-start

    Do not edit this region by hand
    ===============================

    This region was generated from Rust documentation comments by `cargo-reedme` using this command:

        cargo +nightly reedme -p pingora-proxy-router

    for more info: https://github.com/nik-rev/cargo-reedme

cargo-reedme: info-end -->

Context-preserving request routing for Pingora [`ProxyHttp`](https://docs.rs/pingora_proxy/latest/pingora_proxy/proxy_trait/trait.ProxyHttp.html) services.

Every route and decorator in a graph uses the same application-owned
context type. Components access their private state through [`Lens`](https://docs.rs/pingora-proxy-router/latest/pingora_proxy_router/struct.Lens.html) values;
routing stores each router’s selected [`Route`](https://docs.rs/pingora-proxy-router/latest/pingora_proxy_router/type.Route.html) in a [`RouteSlot`](https://docs.rs/pingora-proxy-router/latest/pingora_proxy_router/struct.RouteSlot.html) projected
from that context. This keeps routing invisible to decorators outside the
router, permits nested routers to share one slot safely, and supports
context capabilities parameterized by the context itself.

Selection runs once in `early_request_filter`. The selected route is stored
before its early filter runs, so failures, retries, caching, response
filtering, and logging all dispatch to the same route. Before selection,
hooks safely dispatch to the fallback route.

Routes are immutable `Arc` handles. A selector may load them from an
atomically replaceable application-owned table; a request pins the selected
handle, so replacing a complete route affects new requests without changing
the route used by in-flight requests.

# Example

```rust
use pingora_core::{Result, upstreams::peer::HttpPeer};
use pingora_proxy::{ProxyHttp, Session};
use pingora_proxy_router::{RouteSlot, Router, context_lens, route};

struct AppContext {
    route: RouteSlot<Self>,
    // MetricsState, TracingState, AuthState, ...
}

impl Default for AppContext {
    fn default() -> Self {
        Self { route: RouteSlot::new() }
    }
}

struct Endpoint;

#[async_trait::async_trait]
impl ProxyHttp for Endpoint {
    type CTX = AppContext;

    fn new_ctx(&self) -> Self::CTX {
        AppContext::default()
    }

    async fn upstream_peer(
        &self,
        _: &mut Session,
        _: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        unimplemented!()
    }
}

let users = route(Endpoint);
let fallback = route(Endpoint);
let router = Router::new(
    move |session: &Session, _ctx: &AppContext| {
        Ok(session
            .req_header()
            .uri
            .path()
            .starts_with("/users")
            .then(|| users.clone()))
    },
    fallback,
    context_lens!(AppContext, ctx => ctx.route),
);
let _: &dyn ProxyHttp<CTX = AppContext> = &router;
```

# Replacing routes

The router does not own a mutable route table. A selector may load an
immutable snapshot from application-owned storage such as `ArcSwap` and
return a cloned route handle. Build and initialize a complete replacement
route or table before publishing it. New requests see the replacement;
in-flight requests retain the old route through their stored `Arc`.

# Component state

[`Lens`](https://docs.rs/pingora-proxy-router/latest/pingora_proxy_router/struct.Lens.html) describes containment, not delegation. A generic decorator accepts
a lens from the application context to its state, reads or modifies that
state in its overrides, and delegates the complete context unchanged:

```rust
use pingora_proxy_router::{Lens, context_lens};

struct AuthState;
struct AppContext {
    auth: AuthState,
}

struct Auth<P> {
    inner: P,
    state: Lens<AppContext, AuthState>,
}

impl<P> Auth<P> {
    fn new(inner: P, state: Lens<AppContext, AuthState>) -> Self {
        Self { inner, state }
    }
}

let inner = ();
let auth = Auth::new(
    inner,
    context_lens!(AppContext, ctx => ctx.auth),
);
let _ = auth.inner;
let _ = auth.state;
```

Use traits for semantic capabilities and lenses for locating component-owned
state. This permits multiple instances of one component without
accessor-trait coherence or tagging problems.

# Downstream modules

The router exclusively owns downstream HTTP module registration. Pingora
builds one server-wide module registry, so route-specific initializers
cannot safely be combined automatically. Configure the complete registry
with `Router::with_downstream_modules`, calling
[`default_downstream_modules`](https://docs.rs/pingora-proxy-router/latest/pingora_proxy_router/fn.default_downstream_modules.html) first when Pingora’s default disabled
compression module should be retained. If compression is registered on the
service instead, use `with_downstream_modules(|_| {})` to avoid registering
its module type twice.

With the default `upstream_modules` feature, which enables Pingora’s, the
router likewise owns upstream module registration through
`Router::with_upstream_modules`, and forwards `adjust_upstream_modules` to
the selected route. Disable default features to build without Pingora’s
`upstream_modules` feature; if another crate enables Pingora’s feature
anyway, the build fails rather than silently skipping
`adjust_upstream_modules`, and this crate’s feature must be enabled.

# Keepalive connection context

`persist_connection_context` runs after logging, so it reaches the route
selected for the finished request. `on_connection_reuse` runs before the
next request’s `early_request_filter`, so it reaches the fallback route. A
value persisted by one route is therefore delivered to the fallback; handle
these hooks in a decorator outside the router when routes need them.

<!-- cargo-reedme: end -->
