use pingora_proxy_delegate::proxy_http_delegate;

struct Wrapper {
    inner: (),
}

#[proxy_http_delegate(42)]
impl pingora_proxy::ProxyHttp for Wrapper {
    type CTX = ();
}

fn main() {}
