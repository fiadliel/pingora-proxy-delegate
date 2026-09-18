//! Context-preserving request routing for Pingora [`ProxyHttp`] services.
//!
//! Every route and decorator in a graph uses the same application-owned
//! context type. Components access their private state through [`Lens`] values;
//! routing stores each router's selected [`Route`] in a [`RouteSlot`] projected
//! from that context. This keeps routing invisible to decorators outside the
//! router, permits nested routers to share one slot safely, and supports
//! context capabilities parameterized by the context itself.
//!
//! Selection runs once in `early_request_filter`. The selected route is stored
//! before its early filter runs, so failures, retries, caching, response
//! filtering, and logging all dispatch to the same route. Before selection,
//! hooks safely dispatch to the fallback route.
//!
//! Routes are immutable `Arc` handles. A selector may load them from an
//! atomically replaceable application-owned table; a request pins the selected
//! handle, so replacing a complete route affects new requests without changing
//! the route used by in-flight requests.
//!
//! # Example
//!
//! ```
//! use pingora_core::{Result, upstreams::peer::HttpPeer};
//! use pingora_proxy::{ProxyHttp, Session};
//! use pingora_proxy_router::{RouteSlot, Router, context_lens, route};
//!
//! struct AppContext {
//!     route: RouteSlot<Self>,
//!     // MetricsState, TracingState, AuthState, ...
//! }
//!
//! impl Default for AppContext {
//!     fn default() -> Self {
//!         Self { route: RouteSlot::new() }
//!     }
//! }
//!
//! struct Endpoint;
//!
//! #[async_trait::async_trait]
//! impl ProxyHttp for Endpoint {
//!     type CTX = AppContext;
//!
//!     fn new_ctx(&self) -> Self::CTX {
//!         AppContext::default()
//!     }
//!
//!     async fn upstream_peer(
//!         &self,
//!         _: &mut Session,
//!         _: &mut Self::CTX,
//!     ) -> Result<Box<HttpPeer>> {
//!         unimplemented!()
//!     }
//! }
//!
//! let users = route(Endpoint);
//! let fallback = route(Endpoint);
//! let router = Router::new(
//!     move |session: &Session, _ctx: &AppContext| {
//!         Ok(session
//!             .req_header()
//!             .uri
//!             .path()
//!             .starts_with("/users")
//!             .then(|| users.clone()))
//!     },
//!     fallback,
//!     context_lens!(AppContext, ctx => ctx.route),
//! );
//! let _: &dyn ProxyHttp<CTX = AppContext> = &router;
//! ```
//!
//! # Replacing routes
//!
//! The router does not own a mutable route table. A selector may load an
//! immutable snapshot from application-owned storage such as `ArcSwap` and
//! return a cloned route handle. Build and initialize a complete replacement
//! route or table before publishing it. New requests see the replacement;
//! in-flight requests retain the old route through their stored `Arc`.
//!
//! # Component state
//!
//! [`Lens`] describes containment, not delegation. A generic decorator accepts
//! a lens from the application context to its state, reads or modifies that
//! state in its overrides, and delegates the complete context unchanged:
//!
//! ```
//! use pingora_proxy_router::{Lens, context_lens};
//!
//! struct AuthState;
//! struct AppContext {
//!     auth: AuthState,
//! }
//!
//! struct Auth<P> {
//!     inner: P,
//!     state: Lens<AppContext, AuthState>,
//! }
//!
//! impl<P> Auth<P> {
//!     fn new(inner: P, state: Lens<AppContext, AuthState>) -> Self {
//!         Self { inner, state }
//!     }
//! }
//!
//! let inner = ();
//! let auth = Auth::new(
//!     inner,
//!     context_lens!(AppContext, ctx => ctx.auth),
//! );
//! let _ = auth.inner;
//! let _ = auth.state;
//! ```
//!
//! Use traits for semantic capabilities and lenses for locating component-owned
//! state. This permits multiple instances of one component without
//! accessor-trait coherence or tagging problems.
//!
//! # Downstream modules
//!
//! The router exclusively owns downstream HTTP module registration. Pingora
//! builds one server-wide module registry, so route-specific initializers
//! cannot safely be combined automatically. Configure the complete registry
//! with `Router::with_downstream_modules`, calling
//! [`default_downstream_modules`] first when Pingora's default disabled
//! compression module should be retained. If compression is registered on the
//! service instead, use `with_downstream_modules(|_| {})` to avoid registering
//! its module type twice.
//!
//! With the default `upstream_modules` feature, which enables Pingora's, the
//! router likewise owns upstream module registration through
//! `Router::with_upstream_modules`, and forwards `adjust_upstream_modules` to
//! the selected route. Disable default features to build without Pingora's
//! `upstream_modules` feature; if another crate enables Pingora's feature
//! anyway, the build fails rather than silently skipping
//! `adjust_upstream_modules`, and this crate's feature must be enabled.
//!
//! # Keepalive connection context
//!
//! `persist_connection_context` runs after logging, so it reaches the route
//! selected for the finished request. `on_connection_reuse` runs before the
//! next request's `early_request_filter`, so it reaches the fallback route. A
//! value persisted by one route is therefore delivered to the fallback; handle
//! these hooks in a decorator outside the router when routes need them.

use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use pingora_core::modules::http::{HttpModules, compression::ResponseCompressionBuilder};
use pingora_proxy::{ProxyHttp, Session};
use pingora_proxy_delegate::proxy_http_router_impl;

/// A shareable, immutable proxy selected for one request.
pub type Route<C> = Arc<dyn ProxyHttp<CTX = C> + Send + Sync + 'static>;

/// A server-wide HTTP module initializer. It runs once, when Pingora builds
/// the service, so dynamic dispatch costs nothing per request.
type ModulesInit = Box<dyn Fn(&mut HttpModules) + Send + Sync>;

/// Erases a concrete proxy into a shareable [`Route`].
pub fn route<P>(proxy: P) -> Route<P::CTX>
where
    P: ProxyHttp + Send + Sync + 'static,
{
    Arc::new(proxy)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RouterId(usize);

static NEXT_ROUTER_ID: AtomicUsize = AtomicUsize::new(1);

impl RouterId {
    fn next() -> Self {
        Self(NEXT_ROUTER_ID.fetch_add(1, Ordering::Relaxed))
    }
}

/// Per-request storage for routes selected by one or more [`Router`] instances.
///
/// Put one of these in the application context and pass a [`Lens`] to it when
/// constructing each router. Nested routers may share the same slot: selections
/// are keyed by router identity, so an inner router cannot overwrite the route
/// pinned by an outer router. For access-log or metric labels, have each route
/// record its own identity in separate context state from its
/// `early_request_filter`; selectors intentionally receive only shared context.
pub struct RouteSlot<C> {
    selected: Vec<(RouterId, Route<C>)>,
}

impl<C> RouteSlot<C> {
    /// Creates an empty route slot.
    pub const fn new() -> Self {
        Self {
            selected: Vec::new(),
        }
    }

    /// Returns whether at least one router has completed selection.
    ///
    /// Selecting a fallback route also counts as selection.
    pub fn is_selected(&self) -> bool {
        !self.selected.is_empty()
    }

    fn selected_for(&self, router: RouterId) -> Option<&Route<C>> {
        self.selected
            .iter()
            .find_map(|(id, route)| (*id == router).then_some(route))
    }

    fn clear(&mut self, router: RouterId) {
        self.selected.retain(|(id, _)| *id != router);
    }

    fn select(&mut self, router: RouterId, route: Route<C>) {
        self.clear(router);
        self.selected.push((router, route));
    }
}

impl<C> Default for RouteSlot<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C> fmt::Debug for RouteSlot<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RouteSlot")
            .field("selections", &self.selected.len())
            .finish()
    }
}

/// Selects an immutable route handle for the current request.
///
/// Returning `Ok(None)` selects the fallback. Returning an error leaves the
/// slot empty, so Pingora's ensuing error callbacks also use the fallback.
/// `ctx` may contain state populated by decorators outside the router. It is
/// shared because route selection is observational; the router alone updates
/// its [`RouteSlot`] after selection.
/// A selector may read an application-owned atomic route-table snapshot and
/// return a cloned route handle from it.
pub trait RouteSelector<C>: Send + Sync + 'static {
    /// Selects the route that should handle `session`, using application context
    /// prepared by any outer layers.
    fn select(&self, session: &Session, ctx: &C) -> pingora_core::Result<Option<Route<C>>>;
}

impl<C, F> RouteSelector<C> for F
where
    F: Fn(&Session, &C) -> pingora_core::Result<Option<Route<C>>> + Send + Sync + 'static,
{
    fn select(&self, session: &Session, ctx: &C) -> pingora_core::Result<Option<Route<C>>> {
        (self)(session, ctx)
    }
}

/// A shared and mutable projection into an application context.
///
/// Lenses describe containment. A decorator uses a lens to reach its own state
/// while preserving the complete application context when it delegates.
pub struct Lens<Outer, Target: ?Sized> {
    get: for<'a> fn(&'a Outer) -> &'a Target,
    get_mut: for<'a> fn(&'a mut Outer) -> &'a mut Target,
}

impl<Outer, Target: ?Sized> Clone for Lens<Outer, Target> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Outer, Target: ?Sized> Copy for Lens<Outer, Target> {}

impl<Outer, Target: ?Sized> Lens<Outer, Target> {
    /// Creates a lens from shared and mutable projections.
    pub const fn new(
        get: for<'a> fn(&'a Outer) -> &'a Target,
        get_mut: for<'a> fn(&'a mut Outer) -> &'a mut Target,
    ) -> Self {
        Self { get, get_mut }
    }

    /// Projects a shared reference.
    pub fn get<'a>(&self, outer: &'a Outer) -> &'a Target {
        (self.get)(outer)
    }

    /// Projects a mutable reference.
    pub fn get_mut<'a>(&self, outer: &'a mut Outer) -> &'a mut Target {
        (self.get_mut)(outer)
    }
}

impl<Outer, Target: ?Sized> fmt::Debug for Lens<Outer, Target> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Lens").finish_non_exhaustive()
    }
}

/// Creates a [`Lens`] from one field or place projection.
///
/// ```
/// use pingora_proxy_router::context_lens;
///
/// struct Outer {
///     inner: usize,
/// }
///
/// let lens = context_lens!(Outer, ctx => ctx.inner);
/// let mut outer = Outer { inner: 1 };
/// *lens.get_mut(&mut outer) = 2;
/// assert_eq!(*lens.get(&outer), 2);
/// ```
#[macro_export]
macro_rules! context_lens {
    ($outer:ty, $binding:ident => $projection:expr) => {
        $crate::Lens::new(
            |$binding: &$outer| &$projection,
            |$binding: &mut $outer| &mut $projection,
        )
    };
}

/// Initializes Pingora's default server-wide downstream modules.
///
/// Call this from a custom initializer before adding other shared modules when
/// the default disabled response-compression module should be retained.
pub fn default_downstream_modules(modules: &mut HttpModules) {
    modules.add_module(ResponseCompressionBuilder::enable(0));
}

/// Routes requests without changing the application context type.
///
/// `S` selects complete immutable route handles. `F` constructs the one
/// application context shared by global decorators, the router, route-specific
/// decorators, and endpoint proxies. [`Router::new`] uses [`Default`]; use
/// [`Router::with_context_factory`] when context construction needs captured
/// configuration. The selected handle is stored through `route_slot` and
/// remains pinned for the request lifetime.
#[must_use]
pub struct Router<C, S, F> {
    id: RouterId,
    selector: S,
    fallback: Route<C>,
    context_factory: F,
    route_slot: Lens<C, RouteSlot<C>>,
    downstream_modules: ModulesInit,
    #[cfg(feature = "upstream_modules")]
    upstream_modules: ModulesInit,
}

impl<C, S> Router<C, S, fn() -> C>
where
    C: Default,
    S: RouteSelector<C>,
{
    /// Creates a context-preserving router using `C::default` and Pingora's
    /// default downstream modules.
    pub fn new(selector: S, fallback: Route<C>, route_slot: Lens<C, RouteSlot<C>>) -> Self {
        Self {
            id: RouterId::next(),
            selector,
            fallback,
            context_factory: C::default,
            route_slot,
            downstream_modules: Box::new(default_downstream_modules),
            #[cfg(feature = "upstream_modules")]
            upstream_modules: Box::new(|_| {}),
        }
    }
}

impl<C, S, F> Router<C, S, F>
where
    S: RouteSelector<C>,
    F: Fn() -> C + Send + Sync + 'static,
{
    /// Creates a context-preserving router with a custom context factory and
    /// Pingora's default downstream modules.
    pub fn with_context_factory(
        selector: S,
        fallback: Route<C>,
        context_factory: F,
        route_slot: Lens<C, RouteSlot<C>>,
    ) -> Self {
        Self {
            id: RouterId::next(),
            selector,
            fallback,
            context_factory,
            route_slot,
            downstream_modules: Box::new(default_downstream_modules),
            #[cfg(feature = "upstream_modules")]
            upstream_modules: Box::new(|_| {}),
        }
    }
}

impl<C, S, F> Router<C, S, F> {
    /// Replaces the complete server-wide downstream-module initializer.
    ///
    /// Route initializers are intentionally not invoked. Put the complete
    /// shared module configuration in this closure. If response compression is
    /// already registered on the service, use `|_| {}` (or another initializer
    /// that omits [`default_downstream_modules`]); Pingora rejects duplicate
    /// module types when it builds the module context.
    pub fn with_downstream_modules(
        mut self,
        modules: impl Fn(&mut HttpModules) + Send + Sync + 'static,
    ) -> Self {
        self.downstream_modules = Box::new(modules);
        self
    }

    /// Replaces the complete server-wide upstream-module initializer.
    ///
    /// As with downstream modules, route initializers are intentionally not
    /// invoked. The default registers no upstream modules, matching Pingora.
    #[cfg(feature = "upstream_modules")]
    pub fn with_upstream_modules(
        mut self,
        modules: impl Fn(&mut HttpModules) + Send + Sync + 'static,
    ) -> Self {
        self.upstream_modules = Box::new(modules);
        self
    }

    fn selected_route(&self, ctx: &C) -> Route<C> {
        self.route_slot
            .get(ctx)
            .selected_for(self.id)
            .cloned()
            .unwrap_or_else(|| Arc::clone(&self.fallback))
    }
}

impl<C, S, F> fmt::Debug for Router<C, S, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Router").finish_non_exhaustive()
    }
}

proxy_http_router_impl!();

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, RwLock,
        atomic::{AtomicUsize, Ordering},
    };

    use pingora_core::{Error, ErrorType, Result, upstreams::peer::HttpPeer};
    use pingora_proxy_delegate::proxy_http_delegate;
    use tokio::io::{AsyncWriteExt, DuplexStream};

    use super::*;

    #[derive(Default)]
    struct AppContext {
        route: RouteSlot<Self>,
        value: usize,
        auth_calls: usize,
    }

    fn route_lens() -> Lens<AppContext, RouteSlot<AppContext>> {
        context_lens!(AppContext, ctx => ctx.route)
    }

    async fn session(path: &str) -> (Session, DuplexStream) {
        let raw = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n");
        let (mut client, server) = tokio::io::duplex(4096);
        client.write_all(raw.as_bytes()).await.unwrap();
        let mut session = Session::new_h1(Box::new(server));
        session.downstream_session.read_request().await.unwrap();
        (session, client)
    }

    #[derive(Default)]
    struct Counters {
        early: AtomicUsize,
        request: AtomicUsize,
        fail: AtomicUsize,
        summary: AtomicUsize,
        logging: AtomicUsize,
    }

    struct ProbeProxy {
        id: usize,
        fail_early: bool,
        counters: Arc<Counters>,
    }

    #[async_trait::async_trait]
    impl ProxyHttp for ProbeProxy {
        type CTX = AppContext;

        fn new_ctx(&self) -> Self::CTX {
            AppContext::default()
        }

        async fn early_request_filter(
            &self,
            _session: &mut Session,
            ctx: &mut Self::CTX,
        ) -> Result<()> {
            self.counters.early.fetch_add(1, Ordering::SeqCst);
            ctx.value = self.id;
            if self.fail_early {
                Err(Error::explain(ErrorType::InternalError, "early failure"))
            } else {
                Ok(())
            }
        }

        async fn request_filter(
            &self,
            _session: &mut Session,
            ctx: &mut Self::CTX,
        ) -> Result<bool> {
            assert_eq!(ctx.value, self.id);
            self.counters.request.fetch_add(1, Ordering::SeqCst);
            Ok(false)
        }

        async fn upstream_peer(
            &self,
            _session: &mut Session,
            _ctx: &mut Self::CTX,
        ) -> Result<Box<HttpPeer>> {
            unimplemented!()
        }

        async fn fail_to_proxy(
            &self,
            _session: &mut Session,
            _error: &Error,
            _ctx: &mut Self::CTX,
        ) -> pingora_proxy::FailToProxy {
            self.counters.fail.fetch_add(1, Ordering::SeqCst);
            pingora_proxy::FailToProxy {
                error_code: 500,
                can_reuse_downstream: false,
            }
        }

        fn request_summary(&self, _session: &Session, _ctx: &Self::CTX) -> String {
            self.counters.summary.fetch_add(1, Ordering::SeqCst);
            self.id.to_string()
        }

        async fn logging(
            &self,
            _session: &mut Session,
            _error: Option<&Error>,
            _ctx: &mut Self::CTX,
        ) {
            self.counters.logging.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn probe(id: usize, fail_early: bool, counters: &Arc<Counters>) -> Route<AppContext> {
        route(ProbeProxy {
            id,
            fail_early,
            counters: Arc::clone(counters),
        })
    }

    #[tokio::test]
    async fn selects_once_and_preserves_the_application_context_type() {
        let selected_counters = Arc::new(Counters::default());
        let fallback_counters = Arc::new(Counters::default());
        let selected = probe(7, false, &selected_counters);
        let fallback = probe(99, false, &fallback_counters);
        let router = Router::new(
            {
                let selected = Arc::clone(&selected);
                move |session: &Session, _ctx: &AppContext| {
                    Ok((session.req_header().uri.path() == "/selected")
                        .then(|| Arc::clone(&selected)))
                }
            },
            fallback,
            route_lens(),
        );
        let (mut session, _client) = session("/selected").await;
        let mut ctx = router.new_ctx();

        router
            .early_request_filter(&mut session, &mut ctx)
            .await
            .unwrap();
        router.request_filter(&mut session, &mut ctx).await.unwrap();

        assert_eq!(ctx.value, 7);
        assert!(ctx.route.is_selected());
        assert_eq!(selected_counters.early.load(Ordering::SeqCst), 1);
        assert_eq!(selected_counters.request.load(Ordering::SeqCst), 1);
        assert_eq!(fallback_counters.early.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn selector_can_read_application_context() {
        let selected_counters = Arc::new(Counters::default());
        let fallback_counters = Arc::new(Counters::default());
        let selected = probe(7, false, &selected_counters);
        let router = Router::new(
            move |_session: &Session, ctx: &AppContext| {
                Ok((ctx.value == 42).then(|| Arc::clone(&selected)))
            },
            probe(99, false, &fallback_counters),
            route_lens(),
        );
        let (mut session, _client) = session("/selected-by-context").await;
        let mut ctx = router.new_ctx();
        ctx.value = 42;

        router
            .early_request_filter(&mut session, &mut ctx)
            .await
            .unwrap();

        assert_eq!(selected_counters.early.load(Ordering::SeqCst), 1);
        assert_eq!(fallback_counters.early.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn selected_route_is_pinned_across_table_replacement() {
        let old_counters = Arc::new(Counters::default());
        let new_counters = Arc::new(Counters::default());
        let fallback_counters = Arc::new(Counters::default());
        let current = Arc::new(RwLock::new(probe(1, false, &old_counters)));
        let router = Router::new(
            {
                let current = Arc::clone(&current);
                move |_session: &Session, _ctx: &AppContext| {
                    Ok(Some(Arc::clone(&current.read().unwrap())))
                }
            },
            probe(99, false, &fallback_counters),
            route_lens(),
        );
        let (mut session, _client) = session("/route").await;
        let mut ctx = router.new_ctx();

        router
            .early_request_filter(&mut session, &mut ctx)
            .await
            .unwrap();
        *current.write().unwrap() = probe(2, false, &new_counters);
        router.request_filter(&mut session, &mut ctx).await.unwrap();

        assert_eq!(ctx.value, 1);
        assert_eq!(old_counters.request.load(Ordering::SeqCst), 1);
        assert_eq!(new_counters.request.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn preselection_error_hooks_use_the_fallback() {
        let fallback_counters = Arc::new(Counters::default());
        let router = Router::new(
            |_session: &Session, _ctx: &AppContext| Ok(None),
            probe(99, false, &fallback_counters),
            route_lens(),
        );
        let (mut session, _client) = session("/before-selection").await;
        let mut ctx = router.new_ctx();
        let error = Error::explain(ErrorType::InternalError, "outer layer failed");

        router.fail_to_proxy(&mut session, &error, &mut ctx).await;
        assert_eq!(router.request_summary(&session, &ctx), "99");
        router.logging(&mut session, Some(&error), &mut ctx).await;

        assert_eq!(fallback_counters.fail.load(Ordering::SeqCst), 1);
        assert_eq!(fallback_counters.summary.load(Ordering::SeqCst), 1);
        assert_eq!(fallback_counters.logging.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn selector_errors_leave_error_callbacks_on_the_fallback() {
        let fallback_counters = Arc::new(Counters::default());
        let router = Router::new(
            |_session: &Session, _ctx: &AppContext| {
                Err(Error::explain(ErrorType::InternalError, "selection failed"))
            },
            probe(99, false, &fallback_counters),
            route_lens(),
        );
        let (mut session, _client) = session("/bad-host").await;
        let mut ctx = router.new_ctx();

        let error = router
            .early_request_filter(&mut session, &mut ctx)
            .await
            .unwrap_err();
        router.fail_to_proxy(&mut session, &error, &mut ctx).await;

        assert!(!ctx.route.is_selected());
        assert_eq!(fallback_counters.fail.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn selected_route_handles_callbacks_after_its_early_error() {
        let selected_counters = Arc::new(Counters::default());
        let fallback_counters = Arc::new(Counters::default());
        let selected = probe(4, true, &selected_counters);
        let router = Router::new(
            move |_session: &Session, _ctx: &AppContext| Ok(Some(Arc::clone(&selected))),
            probe(99, false, &fallback_counters),
            route_lens(),
        );
        let (mut session, _client) = session("/error").await;
        let mut ctx = router.new_ctx();

        let error = router
            .early_request_filter(&mut session, &mut ctx)
            .await
            .unwrap_err();
        router.fail_to_proxy(&mut session, &error, &mut ctx).await;
        assert_eq!(router.request_summary(&session, &ctx), "4");

        assert_eq!(selected_counters.fail.load(Ordering::SeqCst), 1);
        assert_eq!(selected_counters.summary.load(Ordering::SeqCst), 1);
        assert_eq!(fallback_counters.fail.load(Ordering::SeqCst), 0);
    }

    struct AuthLayer<P: ProxyHttp> {
        inner: P,
        state: Lens<P::CTX, usize>,
    }

    #[proxy_http_delegate(self.inner)]
    impl<P> ProxyHttp for AuthLayer<P>
    where
        P: ProxyHttp + Send + Sync,
        P::CTX: Send + Sync,
    {
        type CTX = P::CTX;

        async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool> {
            *self.state.get_mut(ctx) += 1;
            self.inner.request_filter(session, ctx).await
        }
    }

    #[tokio::test]
    async fn route_specific_decorator_uses_a_lens_without_changing_context() {
        let inner_counters = Arc::new(Counters::default());
        let fallback_counters = Arc::new(Counters::default());
        let authenticated = route(AuthLayer {
            inner: ProbeProxy {
                id: 3,
                fail_early: false,
                counters: Arc::clone(&inner_counters),
            },
            state: context_lens!(AppContext, ctx => ctx.auth_calls),
        });
        let router = Router::new(
            move |_session: &Session, _ctx: &AppContext| Ok(Some(Arc::clone(&authenticated))),
            probe(99, false, &fallback_counters),
            route_lens(),
        );
        let (mut session, _client) = session("/auth").await;
        let mut ctx = router.new_ctx();

        router
            .early_request_filter(&mut session, &mut ctx)
            .await
            .unwrap();
        router.request_filter(&mut session, &mut ctx).await.unwrap();

        assert_eq!(ctx.auth_calls, 1);
        assert_eq!(inner_counters.request.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn decorator_above_a_nested_router_is_not_bypassed() {
        let leaf_counters = Arc::new(Counters::default());
        let inner_fallback_counters = Arc::new(Counters::default());
        let outer_fallback_counters = Arc::new(Counters::default());
        let leaf = probe(3, false, &leaf_counters);
        let inner = Router::new(
            move |_session: &Session, _ctx: &AppContext| Ok(Some(Arc::clone(&leaf))),
            probe(98, false, &inner_fallback_counters),
            route_lens(),
        );
        let decorated = route(AuthLayer {
            inner,
            state: context_lens!(AppContext, ctx => ctx.auth_calls),
        });
        let outer = Router::new(
            move |_session: &Session, _ctx: &AppContext| Ok(Some(Arc::clone(&decorated))),
            probe(99, false, &outer_fallback_counters),
            route_lens(),
        );
        let (mut session, _client) = session("/nested").await;
        let mut ctx = outer.new_ctx();

        outer
            .early_request_filter(&mut session, &mut ctx)
            .await
            .unwrap();
        outer.request_filter(&mut session, &mut ctx).await.unwrap();

        assert_eq!(ctx.auth_calls, 1);
        assert_eq!(leaf_counters.early.load(Ordering::SeqCst), 1);
        assert_eq!(leaf_counters.request.load(Ordering::SeqCst), 1);
        assert_eq!(ctx.route.selected.len(), 2);
    }

    #[test]
    fn router_owns_downstream_module_initialization() {
        let fallback_counters = Arc::new(Counters::default());
        let initializations = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&initializations);
        let router = Router::new(
            |_session: &Session, _ctx: &AppContext| Ok(None),
            probe(99, false, &fallback_counters),
            route_lens(),
        )
        .with_downstream_modules(move |_modules| {
            observed.fetch_add(1, Ordering::SeqCst);
        });
        let mut modules = HttpModules::new();

        router.init_downstream_modules(&mut modules);

        assert_eq!(initializations.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn custom_context_factory_is_used() {
        let fallback_counters = Arc::new(Counters::default());
        let router = Router::with_context_factory(
            |_session: &Session, _ctx: &AppContext| Ok(None),
            probe(99, false, &fallback_counters),
            || AppContext {
                value: 42,
                ..AppContext::default()
            },
            route_lens(),
        );

        assert_eq!(router.new_ctx().value, 42);
    }

    #[test]
    fn lens_projects_shared_mutable_and_unsized_state() {
        struct Outer {
            inner: [u8; 3],
        }

        let lens: Lens<Outer, [u8]> = context_lens!(Outer, ctx => ctx.inner[..]);
        let mut outer = Outer { inner: [1, 2, 3] };

        lens.get_mut(&mut outer)[1] = 4;
        assert_eq!(lens.get(&outer), &[1, 4, 3]);
    }
}
