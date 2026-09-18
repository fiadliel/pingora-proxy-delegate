use pingora_proxy_delegate::proxy_http_delegate;

struct Wrapper {
    inner: (),
}

trait NotProxyHttp {}

#[proxy_http_delegate(self.inner)]
impl NotProxyHttp for Wrapper {}

fn main() {}
