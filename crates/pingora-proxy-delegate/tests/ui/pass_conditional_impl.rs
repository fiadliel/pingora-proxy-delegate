use pingora_proxy::ProxyHttp;
use pingora_proxy_delegate::proxy_http_delegate;

struct Inner;

#[async_trait::async_trait]
impl ProxyHttp for Inner {
    type CTX = ();

    fn new_ctx(&self) -> Self::CTX {}

    async fn upstream_peer(
        &self,
        _: &mut pingora_proxy::Session,
        _: &mut Self::CTX,
    ) -> pingora_core::Result<Box<pingora_core::upstreams::peer::HttpPeer>> {
        unimplemented!()
    }
}

struct Wrapper {
    inner: Inner,
}

#[cfg(all())]
#[proxy_http_delegate(self.inner)]
impl ProxyHttp for Wrapper {
    type CTX = ();
}

#[cfg(not(all()))]
#[proxy_http_delegate(self.inner)]
impl ProxyHttp for Wrapper {
    type CTX = ();
}

fn main() {
    let wrapper = Wrapper { inner: Inner };
    wrapper.new_ctx();
}
