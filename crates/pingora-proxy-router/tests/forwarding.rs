use std::{any::Any, time::Duration};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue};
use pingora_core::{Error, ErrorType, Result, upstreams::peer::HttpPeer};
use pingora_http::ResponseHeader;
use pingora_proxy::{ProxyHttp, Session};
use pingora_proxy_delegate::proxy_http_delegate;
use pingora_proxy_router::{RouteSlot, Router, context_lens, route};
use tokio::sync::mpsc;

#[derive(Default)]
struct Context {
    routes: RouteSlot<Self>,
    calls: Vec<(&'static str, &'static str)>,
}

struct Endpoint(&'static str);

impl Endpoint {
    fn record(&self, ctx: &mut Context, hook: &'static str) {
        ctx.calls.push((self.0, hook));
    }
}

#[async_trait::async_trait]
impl ProxyHttp for Endpoint {
    type CTX = Context;

    fn new_ctx(&self) -> Context {
        Context::default()
    }

    async fn upstream_peer(&self, _: &mut Session, ctx: &mut Context) -> Result<Box<HttpPeer>> {
        self.record(ctx, "upstream_peer");
        Ok(Box::new(HttpPeer::new(
            "127.0.0.1:8080",
            false,
            self.0.into(),
        )))
    }

    async fn request_body_filter(
        &self,
        _: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Context,
    ) -> Result<()> {
        self.record(ctx, "request_body_filter");
        assert!(end_of_stream);
        assert_eq!(body.as_deref(), Some(b"request".as_slice()));
        *body = Some(Bytes::from_static(self.0.as_bytes()));
        Ok(())
    }

    async fn response_filter(
        &self,
        _: &mut Session,
        response: &mut ResponseHeader,
        ctx: &mut Context,
    ) -> Result<()> {
        self.record(ctx, "response_filter");
        assert_eq!(response.status.as_u16(), 201);
        response.insert_header("x-route", self.0)?;
        Ok(())
    }

    fn response_body_filter(
        &self,
        _: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Context,
    ) -> Result<Option<Duration>> {
        self.record(ctx, "response_body_filter");
        assert!(!end_of_stream);
        assert_eq!(body.as_deref(), Some(b"response".as_slice()));
        *body = None;
        Ok(Some(Duration::from_millis(37)))
    }

    async fn response_trailer_filter(
        &self,
        _: &mut Session,
        trailers: &mut HeaderMap,
        ctx: &mut Context,
    ) -> Result<Option<Bytes>> {
        self.record(ctx, "response_trailer_filter");
        assert_eq!(trailers["x-input"], "trailer");
        trailers.insert("x-route", HeaderValue::from_static(self.0));
        Ok(Some(Bytes::from_static(b"encoded trailers")))
    }

    fn request_cache_filter(&self, session: &mut Session, ctx: &mut Context) -> Result<()> {
        self.record(ctx, "request_cache_filter");
        session.set_keepalive(Some(23));
        Ok(())
    }

    fn should_serve_stale(
        &self,
        _: &mut Session,
        ctx: &mut Context,
        error: Option<&Error>,
    ) -> bool {
        self.record(ctx, "should_serve_stale");
        error.is_some_and(|error| error.etype() == &ErrorType::InternalError)
    }

    fn fail_to_connect(
        &self,
        _: &mut Session,
        peer: &HttpPeer,
        ctx: &mut Context,
        error: Box<Error>,
    ) -> Box<Error> {
        self.record(ctx, "fail_to_connect");
        assert_eq!(peer.sni, self.0);
        error
    }

    async fn proxy_upstream_filter(&self, _: &mut Session, ctx: &mut Context) -> Result<bool> {
        self.record(ctx, "proxy_upstream_filter");
        Err(Error::explain(ErrorType::InternalError, self.0))
    }

    async fn custom_forwarding(
        &self,
        _: &mut Session,
        ctx: &mut Context,
        upstream: Option<mpsc::Sender<Bytes>>,
        downstream: mpsc::Sender<Bytes>,
    ) -> Result<()> {
        self.record(ctx, "custom_forwarding");
        upstream
            .unwrap()
            .send(Bytes::from_static(b"up"))
            .await
            .unwrap();
        downstream.send(Bytes::from_static(b"down")).await.unwrap();
        Ok(())
    }

    fn persist_connection_context(
        &self,
        _: &Session,
        ctx: &Context,
    ) -> Option<Box<dyn Any + Send + Sync>> {
        Some(Box::new((self.0, ctx.calls.len())))
    }

    fn on_connection_reuse(
        &self,
        _: &mut Session,
        ctx: &mut Context,
        previous: Box<dyn Any + Send + Sync>,
    ) {
        self.record(ctx, "on_connection_reuse");
        assert_eq!(
            *previous.downcast::<(&str, usize)>().unwrap(),
            ("selected", 1)
        );
    }

    #[cfg(feature = "upstream_modules")]
    async fn adjust_upstream_modules(
        &self,
        _: &mut Session,
        response: &ResponseHeader,
        end_of_stream: bool,
        ctx: &mut Context,
    ) -> Result<()> {
        self.record(ctx, "adjust_upstream_modules");
        assert_eq!(response.status.as_u16(), 201);
        assert!(end_of_stream);
        Ok(())
    }
}

struct Decorator<P>(P);

#[proxy_http_delegate(self.0)]
impl<P> ProxyHttp for Decorator<P>
where
    P: ProxyHttp<CTX = Context> + Send + Sync,
{
    type CTX = Context;
}

// Every hook crosses both generated forwarding implementations. Distinct route
// labels and an exact call trace catch fallback dispatch and duplicate calls.
fn router(select: bool) -> impl ProxyHttp<CTX = Context> {
    let selected = route(Decorator(Decorator(Endpoint("selected"))));
    Router::new(
        move |_: &Session, _: &Context| Ok(select.then(|| selected.clone())),
        route(Decorator(Endpoint("fallback"))),
        context_lens!(Context, ctx => ctx.routes),
    )
}

fn session() -> Session {
    let (_, server) = tokio::io::duplex(64);
    Session::new_h1(Box::new(server))
}

#[tokio::test]
async fn forwards_body_header_trailer_and_upstream_hooks() {
    for (select, label) in [(true, "selected"), (false, "fallback")] {
        let proxy = router(select);
        let mut ctx = proxy.new_ctx();
        let mut session = session();
        proxy
            .early_request_filter(&mut session, &mut ctx)
            .await
            .unwrap();

        let peer = proxy.upstream_peer(&mut session, &mut ctx).await.unwrap();
        assert_eq!(peer.sni, label);
        let mut body = Some(Bytes::from_static(b"request"));
        proxy
            .request_body_filter(&mut session, &mut body, true, &mut ctx)
            .await
            .unwrap();
        assert_eq!(body.as_deref(), Some(label.as_bytes()));
        let mut response = ResponseHeader::build(201, None).unwrap();
        proxy
            .response_filter(&mut session, &mut response, &mut ctx)
            .await
            .unwrap();
        assert_eq!(response.headers["x-route"], label);
        body = Some(Bytes::from_static(b"response"));
        assert_eq!(
            proxy
                .response_body_filter(&mut session, &mut body, false, &mut ctx)
                .unwrap(),
            Some(Duration::from_millis(37))
        );
        assert!(body.is_none());
        let mut trailers = HeaderMap::new();
        trailers.insert("x-input", HeaderValue::from_static("trailer"));
        assert_eq!(
            proxy
                .response_trailer_filter(&mut session, &mut trailers, &mut ctx)
                .await
                .unwrap(),
            Some(Bytes::from_static(b"encoded trailers"))
        );
        assert_eq!(trailers["x-route"], label);

        assert_eq!(
            ctx.calls,
            [
                "upstream_peer",
                "request_body_filter",
                "response_filter",
                "response_body_filter",
                "response_trailer_filter"
            ]
            .map(|hook| (label, hook))
        );

        #[cfg(feature = "upstream_modules")]
        {
            ctx.calls.clear();
            proxy
                .adjust_upstream_modules(&mut session, &response, true, &mut ctx)
                .await
                .unwrap();
            assert_eq!(ctx.calls, [(label, "adjust_upstream_modules")]);
        }
    }
}

#[tokio::test]
async fn forwards_cache_errors_and_custom_channels() {
    let proxy = router(true);
    let mut session = session();
    let mut ctx = proxy.new_ctx();
    proxy
        .early_request_filter(&mut session, &mut ctx)
        .await
        .unwrap();
    proxy.request_cache_filter(&mut session, &mut ctx).unwrap();
    assert_eq!(session.get_keepalive(), Some(23));
    let error = proxy
        .proxy_upstream_filter(&mut session, &mut ctx)
        .await
        .unwrap_err();
    assert_eq!(error.etype(), &ErrorType::InternalError);
    assert!(error.to_string().contains("selected"));
    assert!(proxy.should_serve_stale(&mut session, &mut ctx, Some(&error)));
    assert!(!proxy.should_serve_stale(&mut session, &mut ctx, None));
    let peer = HttpPeer::new("127.0.0.1:8080", false, "selected".into());
    let original = &*error as *const Error;
    let returned = proxy.fail_to_connect(&mut session, &peer, &mut ctx, error);
    assert!(std::ptr::eq(original, &*returned));

    let (upstream, mut up_rx) = mpsc::channel(1);
    let (downstream, mut down_rx) = mpsc::channel(1);
    proxy
        .custom_forwarding(&mut session, &mut ctx, Some(upstream), downstream)
        .await
        .unwrap();
    assert_eq!(up_rx.try_recv().unwrap(), Bytes::from_static(b"up"));
    assert_eq!(down_rx.try_recv().unwrap(), Bytes::from_static(b"down"));
    assert_eq!(
        ctx.calls,
        [
            "request_cache_filter",
            "proxy_upstream_filter",
            "should_serve_stale",
            "should_serve_stale",
            "fail_to_connect",
            "custom_forwarding"
        ]
        .map(|hook| ("selected", hook))
    );
}

#[tokio::test]
async fn persisted_selected_route_context_reaches_fallback_before_next_selection() {
    let proxy = router(true);
    let mut session = session();
    let mut ctx = proxy.new_ctx();
    proxy
        .early_request_filter(&mut session, &mut ctx)
        .await
        .unwrap();
    proxy.upstream_peer(&mut session, &mut ctx).await.unwrap();
    let previous = proxy.persist_connection_context(&session, &ctx).unwrap();

    let mut next_ctx = proxy.new_ctx();
    proxy.on_connection_reuse(&mut session, &mut next_ctx, previous);
    assert_eq!(next_ctx.calls, [("fallback", "on_connection_reuse")]);
    assert!(!next_ctx.routes.is_selected());
    proxy
        .early_request_filter(&mut session, &mut next_ctx)
        .await
        .unwrap();
    proxy
        .upstream_peer(&mut session, &mut next_ctx)
        .await
        .unwrap();
    assert_eq!(next_ctx.calls[1], ("selected", "upstream_peer"));
}
