use pingora_proxy_delegate::proxy_http_delegate;

struct Wrapper;

#[proxy_http_delegate(self.inner, lint = allow)]
impl pingora_proxy::ProxyHttp for Wrapper {
    type CTX = ();

    // Even cfg_attr that only adds a harmless attribute is rejected.
    #[cfg_attr(all(), allow(dead_code))]
    fn new_ctx(&self) -> Self::CTX {}
}

fn main() {}
