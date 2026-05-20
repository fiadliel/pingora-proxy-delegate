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

#[proxy_http_delegate(self.inner,)]
impl ProxyHttp for Wrapper {
    type CTX = ();
}

fn main() {}
