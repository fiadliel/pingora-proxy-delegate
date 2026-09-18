use pingora_proxy::ProxyHttp;
use pingora_proxy_delegate::proxy_http_delegate;

struct Inner;

#[async_trait::async_trait]
impl ProxyHttp for Inner {
    type CTX = ();

    fn new_ctx(&self) -> Self::CTX {}

    async fn upstream_peer(
        &self,
        _session: &mut pingora_proxy::Session,
        _ctx: &mut Self::CTX,
    ) -> pingora_core::Result<Box<pingora_core::upstreams::peer::HttpPeer>> {
        unimplemented!()
    }
}

struct Wrapper {
    inner: Inner,
}

impl Wrapper {
    fn inner(&self) -> &Inner {
        &self.inner
    }
}

#[proxy_http_delegate(self.inner())]
impl ProxyHttp for Wrapper {
    type CTX = ();
}

// Prefix operators bind more loosely than `.`, so the macro must
// parenthesize the receiver.
struct Boxed {
    inner: Box<Inner>,
}

#[proxy_http_delegate(*self.inner)]
impl ProxyHttp for Boxed {
    type CTX = ();
}

fn main() {}
