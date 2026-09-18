// Expanding the macro without its feature is rejected, as the dev-dependency
// enables `pingora-proxy/upstream_modules`.
#![cfg(feature = "upstream_modules")]

use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use pingora_core::{Result, upstreams::peer::HttpPeer};
use pingora_proxy::{ProxyHttp, Session};
use pingora_proxy_delegate::proxy_http_delegate;

struct ImmediateInner {
    calls: Arc<AtomicUsize>,
}

// These must never be selected by generated ProxyHttp forwarding.
#[allow(dead_code)]
impl ImmediateInner {
    fn new_ctx(&self) {
        panic!("called inherent new_ctx");
    }

    async fn request_filter(&self, _: &mut Session, _: &mut ()) -> Result<bool> {
        panic!("called inherent request_filter");
    }
}

struct ConcreteWrapper {
    inner: ImmediateInner,
}

#[proxy_http_delegate(self.inner)]
impl ProxyHttp for ConcreteWrapper {
    type CTX = ();
}

struct GetterWrapper {
    inner: Arc<ImmediateInner>,
}

impl GetterWrapper {
    fn inner(&self) -> &Arc<ImmediateInner> {
        &self.inner
    }
}

#[proxy_http_delegate(self.inner())]
impl ProxyHttp for GetterWrapper {
    type CTX = ();
}

#[async_trait::async_trait]
impl ProxyHttp for ImmediateInner {
    type CTX = ();

    fn new_ctx(&self) -> Self::CTX {}

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        _ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        unimplemented!()
    }

    fn request_filter<'life0, 'life1, 'life2, 'async_trait>(
        &'life0 self,
        _session: &'life1 mut Session,
        _ctx: &'life2 mut Self::CTX,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'async_trait>>
    where
        Self::CTX: Send + Sync,
        'life0: 'async_trait,
        'life1: 'async_trait,
        'life2: 'async_trait,
        Self: 'async_trait,
    {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(Ok(false)))
    }
}

struct Wrapper<Inner> {
    inner: Inner,
}

#[proxy_http_delegate(self.inner)]
impl<Inner> ProxyHttp for Wrapper<Inner>
where
    Inner: ProxyHttp + Send + Sync,
    Inner::CTX: Send + Sync,
{
    type CTX = Inner::CTX;
}

#[tokio::test]
async fn generated_async_forwards_return_the_inner_future_immediately() {
    let calls = Arc::new(AtomicUsize::new(0));
    let proxy = Wrapper {
        inner: Wrapper {
            inner: ImmediateInner {
                calls: Arc::clone(&calls),
            },
        },
    };
    let (_client, server) = tokio::io::duplex(1);
    let mut session = Session::new_h1(Box::new(server));
    let mut ctx = ();

    let future = proxy.request_filter(&mut session, &mut ctx);

    // An `async fn` forwarding body would not call the inner method until this
    // future was polled. Direct forwarding reaches it synchronously instead.
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!future.await.unwrap());
}

#[tokio::test]
async fn generated_forwards_ignore_inherent_methods() {
    let calls = Arc::new(AtomicUsize::new(0));
    let concrete = ConcreteWrapper {
        inner: ImmediateInner {
            calls: Arc::clone(&calls),
        },
    };
    let getter = GetterWrapper {
        inner: Arc::new(ImmediateInner {
            calls: Arc::clone(&calls),
        }),
    };
    for proxy in [&concrete as &(dyn ProxyHttp<CTX = ()> + Sync), &getter] {
        let (_client, server) = tokio::io::duplex(1);
        let mut session = Session::new_h1(Box::new(server));
        proxy.new_ctx();
        let before = calls.load(Ordering::SeqCst);
        let mut ctx = ();
        let future = proxy.request_filter(&mut session, &mut ctx);
        assert_eq!(calls.load(Ordering::SeqCst), before + 1);
        assert!(!future.await.unwrap());
    }
}
