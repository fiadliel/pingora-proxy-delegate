use pingora_proxy_delegate::proxy_http_delegate;

struct Wrapper;

#[proxy_http_delegate(self.inner, lint = allow)]
impl pingora_proxy::ProxyHttp for Wrapper {
    type CTX = ();

    #[cfg(any())]
    async fn request_filter(
        &self,
        _: &mut pingora_proxy::Session,
        _: &mut Self::CTX,
    ) -> pingora_core::Result<bool> {
        Ok(false)
    }
}

fn main() {}
