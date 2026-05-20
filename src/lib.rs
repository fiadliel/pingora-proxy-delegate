//! Middleware layers for Pingora's `ProxyHttp` trait.
//!
//! Pingora's `ProxyHttp` trait has ~40 methods and no built-in middleware or
//! layer system. A single monolithic impl handles all concerns. Concerns that
//! span multiple phases (e.g. auth touches `request_filter`,
//! `upstream_request_filter`, and `logging`) are split across methods shared
//! with unrelated logic.
//!
//! Wrapping one `ProxyHttp` in another keeps each concern in one impl, but
//! requires forwarding ~34 default methods. This crate generates them:
//!
//! ```text
//! #[proxy_http_delegate(self.inner)]
//! impl<T: ProxyHttp> ProxyHttp for MyLayer<T> { ... }
//! ```
//!
//! # `#[proxy_http_delegate]`
//!
//! Generates `self.<inner>.method(...)` delegations for every `ProxyHttp`
//! method not already defined in the impl block.
//!
//! Also emits `#[async_trait]` and `#[deny(clippy::missing_trait_methods)]` on
//! the impl — do not add these yourself.
//!
//! ## Syntax
//!
//! ```rust,ignore
//! #[proxy_http_delegate(self.inner)]                  // lint defaults to deny
//! #[proxy_http_delegate(self.inner, lint = warn)]     // downgrade to warning
//! #[proxy_http_delegate(self.inner, lint = allow)]    // silence the lint
//! #[proxy_http_delegate(self.inner, lint = deny)]     // explicit deny (default)
//! #[proxy_http_delegate(self.inner, ctx = ctx.inner)] // map context for delegation
//! ```
//!
//! The `lint` option controls the `clippy::missing_trait_methods` lint level
//! emitted on the impl block. It accepts `deny` (default), `warn`, or `allow`.
//!
//! The `ctx` option remaps the context passed to the inner delegate. When set,
//! delegation calls pass the mapped expression (e.g. `&mut ctx.inner`) instead
//! of `ctx` directly. The `new_ctx` delegation is not generated — you must
//! define it yourself. This is useful when your layer has its own context type
//! that wraps the inner context.
//!
//! ## Example
//!
//! ```rust
//! use pingora_core::prelude::*;
//! use pingora_proxy::{ProxyHttp, Session};
//! use pingora_proxy_delegate::proxy_http_delegate;
//!
//! struct InnerProxy;
//!
//! #[async_trait::async_trait]
//! impl ProxyHttp for InnerProxy {
//!     type CTX = ();
//!
//!     fn new_ctx(&self) -> Self::CTX {
//!         todo!()
//!     }
//!     async fn upstream_peer(
//!         &self,
//!         _: &mut pingora_proxy::Session,
//!         _: &mut Self::CTX,
//!     ) -> Result<Box<HttpPeer>, Box<pingora_core::Error>> {
//!         todo!()
//!     }
//! }
//!
//! struct MyDecorator<Inner> {
//!     inner: Inner,
//! }
//!
//! #[proxy_http_delegate(self.inner)]
//! impl<Inner> ProxyHttp for MyDecorator<Inner>
//! where
//!     Inner: ProxyHttp + Send + Sync,
//!     Inner::CTX: Default + Send + Sync + 'static,
//! {
//!     type CTX = Inner::CTX;
//!
//!     async fn logging(
//!         &self,
//!         session: &mut Session,
//!         error: Option<&pingora_core::Error>,
//!         ctx: &mut Self::CTX,
//!     ) where
//!         Self::CTX: Send + Sync,
//!     {
//!         self.inner.logging(session, error, ctx).await;
//!         // your logic here
//!     }
//! }
//! ```
//!
//! Layers nest: `MyDecorator<AuthLayer<RateLimitLayer<CoreProxy>>>`.
//!
//! ## Compatibility
//!
//! Targets Pingora 0.8. If Pingora adds a new trait method the
//! `missing_trait_methods` deny will surface a compile error — update this
//! crate's method list and re-release.

use std::collections::HashSet;

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::{
    Expr, Ident, ImplItem, ItemImpl, Token,
    parse::{Parse, ParseStream},
    parse_macro_input,
};

struct Args {
    inner: Expr,
    ctx: Option<Expr>,
    lint: Option<Ident>,
}

impl Parse for Args {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let inner: Expr = input.parse()?;
        let mut ctx = None;
        let mut lint = None;
        while input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let key: Ident = input.parse()?;
            if key == "ctx" {
                if ctx.is_some() {
                    return Err(syn::Error::new(key.span(), "duplicate `ctx`"));
                }
                input.parse::<Token![=]>()?;
                ctx = Some(input.parse()?);
            } else if key == "lint" {
                if lint.is_some() {
                    return Err(syn::Error::new(key.span(), "duplicate `lint`"));
                }
                input.parse::<Token![=]>()?;
                let level: Ident = input.parse()?;
                if level != "warn" && level != "deny" && level != "allow" {
                    return Err(syn::Error::new(
                        level.span(),
                        "expected one of `warn`, `deny`, or `allow`",
                    ));
                }
                lint = Some(level);
            } else {
                return Err(syn::Error::new(key.span(), "expected `ctx` or `lint`"));
            }
        }
        Ok(Args { inner, ctx, lint })
    }
}

#[proc_macro_attribute]
pub fn proxy_http_delegate(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as Args);

    let is_self_field = matches!(
        &args.inner,
        Expr::Field(f) if matches!(&*f.base, Expr::Path(p) if p.path.is_ident("self"))
    );
    if !is_self_field {
        return syn::Error::new_spanned(&args.inner, "expected a field access like `self.inner`")
            .to_compile_error()
            .into();
    }

    let mut impl_block = parse_macro_input!(item as ItemImpl);

    // Validate that this impl is for a trait whose name ends in `ProxyHttp`.
    let is_proxy_http = impl_block
        .trait_
        .as_ref()
        .and_then(|(_, path, _)| path.segments.last())
        .is_some_and(|seg| seg.ident == "ProxyHttp");
    if !is_proxy_http {
        return syn::Error::new_spanned(
            &impl_block.self_ty,
            "proxy_http_delegate can only be applied to a `ProxyHttp` impl",
        )
        .to_compile_error()
        .into();
    }

    // Collect names of methods the caller has already written.
    let defined: HashSet<String> = impl_block
        .items
        .iter()
        .filter_map(|item| {
            if let ImplItem::Fn(m) = item {
                Some(m.sig.ident.to_string())
            } else {
                None
            }
        })
        .collect();

    for tokens in delegations(&args.inner, &defined, args.ctx.as_ref()) {
        impl_block.items.push(ImplItem::Verbatim(tokens));
    }

    let lint = args
        .lint
        .unwrap_or_else(|| Ident::new("deny", Span::call_site()));

    quote! {
        #[#lint(clippy::missing_trait_methods)]
        #[::async_trait::async_trait]
        #impl_block
    }
    .into()
}

/// Returns delegation bodies for every `ProxyHttp` method not present in `skip`.
///
/// Audited against: `pingora-proxy` 0.8.x.
#[allow(clippy::too_many_lines)]
fn delegations(d: &Expr, skip: &HashSet<String>, ctx_map: Option<&Expr>) -> Vec<TokenStream2> {
    let mut out = Vec::new();

    let ctx_mut = match ctx_map {
        Some(expr) => quote! { &mut #expr },
        None => quote! { ctx },
    };
    let ctx_ref = match ctx_map {
        Some(expr) => quote! { &#expr },
        None => quote! { ctx },
    };

    macro_rules! delegate {
        ($name:literal, $body:expr) => {
            if !skip.contains($name) {
                out.push($body);
            }
        };
    }

    if ctx_map.is_none() {
        delegate!(
            "new_ctx",
            quote! {
                fn new_ctx(&self) -> Self::CTX {
                    #d.new_ctx()
                }
            }
        );
    }

    delegate!(
        "init_downstream_modules",
        quote! {
            fn init_downstream_modules(
                &self,
                modules: &mut ::pingora_core::modules::http::HttpModules,
            ) {
                #d.init_downstream_modules(modules);
            }
        }
    );

    delegate!(
        "early_request_filter",
        quote! {
            async fn early_request_filter(
                &self,
                session: &mut ::pingora_proxy::Session,
                ctx: &mut Self::CTX,
            ) -> ::pingora_core::Result<()>
            where
                Self::CTX: Send + Sync,
            {
                #d.early_request_filter(session, #ctx_mut).await
            }
        }
    );

    delegate!(
        "request_filter",
        quote! {
            async fn request_filter(
                &self,
                session: &mut ::pingora_proxy::Session,
                ctx: &mut Self::CTX,
            ) -> ::pingora_core::Result<bool>
            where
                Self::CTX: Send + Sync,
            {
                #d.request_filter(session, #ctx_mut).await
            }
        }
    );

    delegate!(
        "allow_spawning_subrequest",
        quote! {
            fn allow_spawning_subrequest(
                &self,
                session: &::pingora_proxy::Session,
                ctx: &Self::CTX,
            ) -> bool
            where
                Self::CTX: Send + Sync,
            {
                #d.allow_spawning_subrequest(session, #ctx_ref)
            }
        }
    );

    delegate!(
        "request_body_filter",
        quote! {
            async fn request_body_filter(
                &self,
                session: &mut ::pingora_proxy::Session,
                body: &mut ::core::option::Option<::bytes::Bytes>,
                end_of_stream: bool,
                ctx: &mut Self::CTX,
            ) -> ::pingora_core::Result<()>
            where
                Self::CTX: Send + Sync,
            {
                #d.request_body_filter(session, body, end_of_stream, #ctx_mut).await
            }
        }
    );

    delegate!(
        "request_cache_filter",
        quote! {
            fn request_cache_filter(
                &self,
                session: &mut ::pingora_proxy::Session,
                ctx: &mut Self::CTX,
            ) -> ::pingora_core::Result<()>
            where
                Self::CTX: Send + Sync,
            {
                #d.request_cache_filter(session, #ctx_mut)
            }
        }
    );

    delegate!(
        "cache_key_callback",
        quote! {
            fn cache_key_callback(
                &self,
                session: &::pingora_proxy::Session,
                ctx: &mut Self::CTX,
            ) -> ::pingora_core::Result<::pingora_cache::CacheKey> {
                #d.cache_key_callback(session, #ctx_mut)
            }
        }
    );

    delegate!(
        "cache_miss",
        quote! {
            fn cache_miss(
                &self,
                session: &mut ::pingora_proxy::Session,
                ctx: &mut Self::CTX,
            ) {
                #d.cache_miss(session, #ctx_mut);
            }
        }
    );

    delegate!(
        "cache_hit_filter",
        quote! {
            async fn cache_hit_filter(
                &self,
                session: &mut ::pingora_proxy::Session,
                meta: &::pingora_cache::CacheMeta,
                hit_handler: &mut ::pingora_cache::HitHandler,
                is_fresh: bool,
                ctx: &mut Self::CTX,
            ) -> ::pingora_core::Result<::core::option::Option<::pingora_cache::ForcedFreshness>>
            where
                Self::CTX: Send + Sync,
            {
                #d.cache_hit_filter(session, meta, hit_handler, is_fresh, #ctx_mut).await
            }
        }
    );

    delegate!(
        "proxy_upstream_filter",
        quote! {
            async fn proxy_upstream_filter(
                &self,
                session: &mut ::pingora_proxy::Session,
                ctx: &mut Self::CTX,
            ) -> ::pingora_core::Result<bool>
            where
                Self::CTX: Send + Sync,
            {
                #d.proxy_upstream_filter(session, #ctx_mut).await
            }
        }
    );

    delegate!(
        "response_cache_filter",
        quote! {
            fn response_cache_filter(
                &self,
                session: &::pingora_proxy::Session,
                resp: &::pingora_http::ResponseHeader,
                ctx: &mut Self::CTX,
            ) -> ::pingora_core::Result<::pingora_cache::RespCacheable> {
                #d.response_cache_filter(session, resp, #ctx_mut)
            }
        }
    );

    delegate!(
        "cache_vary_filter",
        quote! {
            fn cache_vary_filter(
                &self,
                meta: &::pingora_cache::CacheMeta,
                ctx: &mut Self::CTX,
                req: &::pingora_http::RequestHeader,
            ) -> ::core::option::Option<::pingora_cache::key::HashBinary> {
                #d.cache_vary_filter(meta, #ctx_mut, req)
            }
        }
    );

    delegate!(
        "cache_not_modified_filter",
        quote! {
            fn cache_not_modified_filter(
                &self,
                session: &::pingora_proxy::Session,
                resp: &::pingora_http::ResponseHeader,
                ctx: &mut Self::CTX,
            ) -> ::pingora_core::Result<bool> {
                #d.cache_not_modified_filter(session, resp, #ctx_mut)
            }
        }
    );

    delegate!(
        "range_header_filter",
        quote! {
            fn range_header_filter(
                &self,
                session: &mut ::pingora_proxy::Session,
                resp: &mut ::pingora_http::ResponseHeader,
                ctx: &mut Self::CTX,
            ) -> ::pingora_proxy::RangeType {
                #d.range_header_filter(session, resp, #ctx_mut)
            }
        }
    );

    delegate!(
        "upstream_peer",
        quote! {
            async fn upstream_peer(
                &self,
                session: &mut ::pingora_proxy::Session,
                ctx: &mut Self::CTX,
            ) -> ::pingora_core::Result<::std::boxed::Box<::pingora_core::upstreams::peer::HttpPeer>>
            {
                #d.upstream_peer(session, #ctx_mut).await
            }
        }
    );

    delegate!(
        "upstream_request_filter",
        quote! {
            async fn upstream_request_filter(
                &self,
                session: &mut ::pingora_proxy::Session,
                upstream_request: &mut ::pingora_http::RequestHeader,
                ctx: &mut Self::CTX,
            ) -> ::pingora_core::Result<()>
            where
                Self::CTX: Send + Sync,
            {
                #d.upstream_request_filter(session, upstream_request, #ctx_mut).await
            }
        }
    );

    delegate!(
        "upstream_response_filter",
        quote! {
            async fn upstream_response_filter(
                &self,
                session: &mut ::pingora_proxy::Session,
                upstream_response: &mut ::pingora_http::ResponseHeader,
                ctx: &mut Self::CTX,
            ) -> ::pingora_core::Result<()>
            where
                Self::CTX: Send + Sync,
            {
                #d.upstream_response_filter(session, upstream_response, #ctx_mut).await
            }
        }
    );

    delegate!(
        "response_filter",
        quote! {
            async fn response_filter(
                &self,
                session: &mut ::pingora_proxy::Session,
                upstream_response: &mut ::pingora_http::ResponseHeader,
                ctx: &mut Self::CTX,
            ) -> ::pingora_core::Result<()>
            where
                Self::CTX: Send + Sync,
            {
                #d.response_filter(session, upstream_response, #ctx_mut).await
            }
        }
    );

    delegate!(
        "custom_forwarding",
        quote! {
            async fn custom_forwarding(
                &self,
                session: &mut ::pingora_proxy::Session,
                ctx: &mut Self::CTX,
                custom_message_to_upstream: ::core::option::Option<
                    ::tokio::sync::mpsc::Sender<::bytes::Bytes>,
                >,
                custom_message_to_downstream: ::tokio::sync::mpsc::Sender<::bytes::Bytes>,
            ) -> ::pingora_core::Result<()>
            where
                Self::CTX: Send + Sync,
            {
                #d.custom_forwarding(
                    session,
                    #ctx_mut,
                    custom_message_to_upstream,
                    custom_message_to_downstream,
                )
                .await
            }
        }
    );

    delegate!(
        "downstream_custom_message_proxy_filter",
        quote! {
            async fn downstream_custom_message_proxy_filter(
                &self,
                session: &mut ::pingora_proxy::Session,
                custom_message: ::bytes::Bytes,
                ctx: &mut Self::CTX,
                final_hop: bool,
            ) -> ::pingora_core::Result<::core::option::Option<::bytes::Bytes>>
            where
                Self::CTX: Send + Sync,
            {
                #d.downstream_custom_message_proxy_filter(session, custom_message, #ctx_mut, final_hop)
                    .await
            }
        }
    );

    delegate!(
        "upstream_custom_message_proxy_filter",
        quote! {
            async fn upstream_custom_message_proxy_filter(
                &self,
                session: &mut ::pingora_proxy::Session,
                custom_message: ::bytes::Bytes,
                ctx: &mut Self::CTX,
                final_hop: bool,
            ) -> ::pingora_core::Result<::core::option::Option<::bytes::Bytes>>
            where
                Self::CTX: Send + Sync,
            {
                #d.upstream_custom_message_proxy_filter(session, custom_message, #ctx_mut, final_hop)
                    .await
            }
        }
    );

    delegate!(
        "upstream_response_body_filter",
        quote! {
            fn upstream_response_body_filter(
                &self,
                session: &mut ::pingora_proxy::Session,
                body: &mut ::core::option::Option<::bytes::Bytes>,
                end_of_stream: bool,
                ctx: &mut Self::CTX,
            ) -> ::pingora_core::Result<::core::option::Option<::std::time::Duration>> {
                #d.upstream_response_body_filter(session, body, end_of_stream, #ctx_mut)
            }
        }
    );

    delegate!(
        "upstream_response_trailer_filter",
        quote! {
            fn upstream_response_trailer_filter(
                &self,
                session: &mut ::pingora_proxy::Session,
                upstream_trailers: &mut ::http::HeaderMap,
                ctx: &mut Self::CTX,
            ) -> ::pingora_core::Result<()> {
                #d.upstream_response_trailer_filter(session, upstream_trailers, #ctx_mut)
            }
        }
    );

    delegate!(
        "response_body_filter",
        quote! {
            fn response_body_filter(
                &self,
                session: &mut ::pingora_proxy::Session,
                body: &mut ::core::option::Option<::bytes::Bytes>,
                end_of_stream: bool,
                ctx: &mut Self::CTX,
            ) -> ::pingora_core::Result<::core::option::Option<::std::time::Duration>>
            where
                Self::CTX: Send + Sync,
            {
                #d.response_body_filter(session, body, end_of_stream, #ctx_mut)
            }
        }
    );

    delegate!(
        "response_trailer_filter",
        quote! {
            async fn response_trailer_filter(
                &self,
                session: &mut ::pingora_proxy::Session,
                upstream_trailers: &mut ::http::HeaderMap,
                ctx: &mut Self::CTX,
            ) -> ::pingora_core::Result<::core::option::Option<::bytes::Bytes>>
            where
                Self::CTX: Send + Sync,
            {
                #d.response_trailer_filter(session, upstream_trailers, #ctx_mut).await
            }
        }
    );

    delegate!(
        "logging",
        quote! {
            async fn logging(
                &self,
                session: &mut ::pingora_proxy::Session,
                error: ::core::option::Option<&::pingora_core::Error>,
                ctx: &mut Self::CTX,
            ) where
                Self::CTX: Send + Sync,
            {
                #d.logging(session, error, #ctx_mut).await;
            }
        }
    );

    delegate!(
        "suppress_error_log",
        quote! {
            fn suppress_error_log(
                &self,
                session: &::pingora_proxy::Session,
                ctx: &Self::CTX,
                error: &::pingora_core::Error,
            ) -> bool {
                #d.suppress_error_log(session, #ctx_ref, error)
            }
        }
    );

    delegate!(
        "error_while_proxy",
        quote! {
            fn error_while_proxy(
                &self,
                peer: &::pingora_core::upstreams::peer::HttpPeer,
                session: &mut ::pingora_proxy::Session,
                e: ::std::boxed::Box<::pingora_core::Error>,
                ctx: &mut Self::CTX,
                client_reused: bool,
            ) -> ::std::boxed::Box<::pingora_core::Error> {
                #d.error_while_proxy(peer, session, e, #ctx_mut, client_reused)
            }
        }
    );

    delegate!(
        "fail_to_connect",
        quote! {
            fn fail_to_connect(
                &self,
                session: &mut ::pingora_proxy::Session,
                peer: &::pingora_core::upstreams::peer::HttpPeer,
                ctx: &mut Self::CTX,
                e: ::std::boxed::Box<::pingora_core::Error>,
            ) -> ::std::boxed::Box<::pingora_core::Error> {
                #d.fail_to_connect(session, peer, #ctx_mut, e)
            }
        }
    );

    delegate!(
        "fail_to_proxy",
        quote! {
            async fn fail_to_proxy(
                &self,
                session: &mut ::pingora_proxy::Session,
                e: &::pingora_core::Error,
                ctx: &mut Self::CTX,
            ) -> ::pingora_proxy::FailToProxy
            where
                Self::CTX: Send + Sync,
            {
                #d.fail_to_proxy(session, e, #ctx_mut).await
            }
        }
    );

    delegate!(
        "should_serve_stale",
        quote! {
            fn should_serve_stale(
                &self,
                session: &mut ::pingora_proxy::Session,
                ctx: &mut Self::CTX,
                error: ::core::option::Option<&::pingora_core::Error>,
            ) -> bool {
                #d.should_serve_stale(session, #ctx_mut, error)
            }
        }
    );

    // Platform-specific fd/sock parameter requires cfg-gated call sites.
    delegate!(
        "connected_to_upstream",
        quote! {
            async fn connected_to_upstream(
                &self,
                session: &mut ::pingora_proxy::Session,
                reused: bool,
                peer: &::pingora_core::upstreams::peer::HttpPeer,
                #[cfg(unix)] fd: ::std::os::unix::io::RawFd,
                #[cfg(windows)] sock: ::std::os::windows::io::RawSocket,
                digest: ::core::option::Option<&::pingora_core::protocols::Digest>,
                ctx: &mut Self::CTX,
            ) -> ::pingora_core::Result<()>
            where
                Self::CTX: Send + Sync,
            {
                #[cfg(unix)]
                {
                    #d.connected_to_upstream(session, reused, peer, fd, digest, #ctx_mut).await
                }
                #[cfg(windows)]
                {
                    #d.connected_to_upstream(session, reused, peer, sock, digest, #ctx_mut).await
                }
            }
        }
    );

    delegate!(
        "request_summary",
        quote! {
            fn request_summary(
                &self,
                session: &::pingora_proxy::Session,
                ctx: &Self::CTX,
            ) -> ::std::string::String {
                #d.request_summary(session, #ctx_ref)
            }
        }
    );

    delegate!(
        "is_purge",
        quote! {
            fn is_purge(&self, session: &::pingora_proxy::Session, ctx: &Self::CTX) -> bool {
                #d.is_purge(session, #ctx_ref)
            }
        }
    );

    delegate!(
        "purge_response_filter",
        quote! {
            fn purge_response_filter(
                &self,
                session: &::pingora_proxy::Session,
                ctx: &mut Self::CTX,
                purge_status: ::pingora_proxy::PurgeStatus,
                purge_response: &mut ::std::borrow::Cow<'static, ::pingora_http::ResponseHeader>,
            ) -> ::pingora_core::Result<()> {
                #d.purge_response_filter(session, #ctx_mut, purge_status, purge_response)
            }
        }
    );

    out
}
