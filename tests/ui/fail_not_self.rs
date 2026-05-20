use pingora_proxy_delegate::proxy_http_delegate;

struct Wrapper {
    inner: (),
}

#[proxy_http_delegate(foo.bar)]
impl pingora_proxy::ProxyHttp for Wrapper {
    type CTX = ();
}

fn main() {}
