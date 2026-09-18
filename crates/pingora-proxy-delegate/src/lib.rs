//! Complete, context-preserving delegation for Pingora's `ProxyHttp` trait.
//!
//! Pingora 0.9's `ProxyHttp` trait has 42 methods (40 without its
//! `upstream_modules` feature). Wrapping one implementation
//! in another keeps a concern in one type, but requires forwarding every hook
//! that the wrapper does not override. [`proxy_http_delegate`] generates those
//! forwards and rejects a `ProxyHttp` surface change when Clippy runs.
//!
//! This crate deliberately does not define middleware ordering or reshape the
//! request context. A composable proxy graph uses one application-owned `CTX`;
//! decorators access their own state separately and pass the complete context
//! unchanged when they delegate.
//!
//! # Syntax
//!
//! ```text
//! #[proxy_http_delegate(self.inner)]
//! #[proxy_http_delegate(self.inner, lint = warn)]
//! #[proxy_http_delegate(self.inner, lint = allow)]
//! ```
//!
//! The first argument is an expression resolving to the inner `ProxyHttp`, such
//! as `self.inner`, `self.layers.proxy`, `*self.boxed`, or `self.inner()`. It
//! must borrow from `self`: forwarded async hooks return the inner future, which
//! cannot outlive a temporary, so an owned value such as `self.inner.clone()`
//! fails with "cannot return value referencing temporary value". `lint`
//! controls the generated `clippy::missing_trait_methods` lint and defaults to
//! `deny`.
//!
//! The macro also emits `#[async_trait]`; do not add that attribute to the same
//! impl yourself. Pure async forwards return the inner boxed future unchanged,
//! so each pass-through decorator does not allocate a second future.
//!
//! Method-level `#[cfg]` and `#[cfg_attr]` attributes are rejected, regardless
//! of `lint`: disabling an override could silently use Pingora's default
//! instead of delegating. Put conditional logic inside an unconditional method
//! and explicitly delegate in the other branch, or conditionally compile entire
//! impl blocks, each annotated with this macro.
//!
//! # Example
//!
//! ```rust
//! use pingora_core::{Result, upstreams::peer::HttpPeer};
//! use pingora_proxy::{ProxyHttp, Session};
//! use pingora_proxy_delegate::proxy_http_delegate;
//!
//! struct Inner;
//!
//! #[async_trait::async_trait]
//! impl ProxyHttp for Inner {
//!     type CTX = usize;
//!
//!     fn new_ctx(&self) -> Self::CTX {
//!         0
//!     }
//!
//!     async fn upstream_peer(&self, _: &mut Session, _: &mut Self::CTX) -> Result<Box<HttpPeer>> {
//!         unimplemented!()
//!     }
//! }
//!
//! struct CountRequests<P> {
//!     inner: P,
//! }
//!
//! #[proxy_http_delegate(self.inner)]
//! impl<P> ProxyHttp for CountRequests<P>
//! where
//!     P: ProxyHttp<CTX = usize> + Send + Sync,
//! {
//!     type CTX = P::CTX;
//!
//!     async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool> {
//!         *ctx += 1;
//!         self.inner.request_filter(session, ctx).await
//!     }
//! }
//! ```
//!
//! # Generated-code dependencies
//!
//! Macro expansions name the crates used by Pingora's complete trait surface.
//! Consuming crates therefore need direct dependencies on `async-trait`,
//! `bytes`, `http`, `pingora-cache`, `pingora-core`, `pingora-http`,
//! `pingora-proxy`, and `tokio`, in addition to this crate. Keep all Pingora
//! packages on the same 0.9.x release.
//!
//! # Upstream modules
//!
//! Pingora declares `init_upstream_modules` and `adjust_upstream_modules` only
//! with its `upstream_modules` feature. This crate's default `upstream_modules`
//! feature forwards them, and must match Pingora's. Features are unified
//! across the dependency graph, so either can be enabled by another crate; a
//! mismatch in either direction fails to compile:
//!
//! - Pingora's feature off, this crate's on: "method is not a member of trait
//!   `ProxyHttp`". Set `default-features = false` on this crate.
//! - Pingora's feature on, this crate's off: "multiple applicable items in
//!   scope", naming `EnableUpstreamModulesFeatureOfPingoraProxyDelegate`. Enable
//!   this crate's `upstream_modules` feature.
//!
//! The second check exists because omitting hooks that do exist would
//! otherwise silently run Pingora's defaults instead of the inner
//! implementation.
//!
//! [`pingora-proxy-router`](https://docs.rs/pingora-proxy-router) provides a
//! context-preserving router, route slots, and lenses for component-owned state.
//!
//! This release targets Pingora 0.9. Applications should run Clippy in CI so a
//! newly added default trait method triggers the generated compatibility lint.

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
    lint: Option<Ident>,
}

impl Parse for Args {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let inner: Expr = input.parse()?;
        let mut lint = None;
        while input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let key: Ident = input.parse()?;
            if key == "lint" {
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
                return Err(syn::Error::new(key.span(), "expected `lint`"));
            }
        }
        Ok(Args { inner, lint })
    }
}

#[proc_macro_attribute]
pub fn proxy_http_delegate(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as Args);

    if matches!(&args.inner, Expr::Lit(_)) {
        return syn::Error::new_spanned(
            &args.inner,
            "expected an expression resolving to a `ProxyHttp` implementation",
        )
        .to_compile_error()
        .into();
    }

    let inner = args.inner;

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

    // Conditional methods must not suppress forwarding and then disappear
    // during cfg expansion, leaving Pingora's default implementation active.
    for item in &impl_block.items {
        if let ImplItem::Fn(method) = item {
            for attr in &method.attrs {
                if attr.path().is_ident("cfg") || attr.path().is_ident("cfg_attr") {
                    return syn::Error::new_spanned(
                        attr,
                        "conditional methods are not supported by `proxy_http_delegate`: disabling this method could silently use Pingora's default instead of delegating; move conditional logic into the method body, or conditionally compile the entire impl",
                    )
                    .to_compile_error()
                    .into();
                }
            }
        }
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

    for tokens in delegations(&inner, &defined, false) {
        impl_block.items.push(ImplItem::Verbatim(tokens));
    }

    let lint = args
        .lint
        .unwrap_or_else(|| Ident::new("deny", Span::call_site()));

    let probe = upstream_modules_probe();

    quote! {
        #[#lint(clippy::missing_trait_methods)]
        #[::async_trait::async_trait]
        #impl_block

        #probe
    }
    .into()
}

/// Generates the context-preserving ProxyHttp implementation for
/// pingora-proxy-router.
///
/// This is an implementation detail of that runtime crate.
#[doc(hidden)]
#[proc_macro]
pub fn proxy_http_router_impl(input: TokenStream) -> TokenStream {
    if !input.is_empty() {
        return syn::Error::new(Span::call_site(), "this macro accepts no arguments")
            .to_compile_error()
            .into();
    }

    let skip = [
        "new_ctx",
        "init_downstream_modules",
        "init_upstream_modules",
        "early_request_filter",
    ]
    .map(str::to_owned)
    .into_iter()
    .collect::<HashSet<_>>();
    let delegate: Expr = syn::parse_quote!(self.selected_route(ctx));
    let forwarded = delegations(&delegate, &skip, true);
    let probe = upstream_modules_probe();
    // Dependency features can enable this crate's hooks without enabling the
    // router's matching initializer and configuration API. Check the router's
    // feature in the expansion, independently of the Pingora compatibility probe.
    let router_feature_guard = cfg!(feature = "upstream_modules").then(|| {
        quote! {
            #[cfg(not(feature = "upstream_modules"))]
            compile_error!("enable pingora-proxy-router's `upstream_modules` feature when pingora-proxy-delegate's `upstream_modules` feature is enabled");
        }
    });

    quote! {
        #router_feature_guard

        #[::async_trait::async_trait]
        #[deny(clippy::missing_trait_methods)]
        impl<C, S, F> ::pingora_proxy::ProxyHttp for Router<C, S, F>
        where
            C: Send + Sync + 'static,
            S: RouteSelector<C>,
            F: Fn() -> C + Send + Sync + 'static,
        {
            type CTX = C;

            fn new_ctx(&self) -> Self::CTX {
                (self.context_factory)()
            }

            fn init_downstream_modules(
                &self,
                modules: &mut ::pingora_core::modules::http::HttpModules,
            ) {
                (self.downstream_modules)(modules);
            }

            // Gated on the router crate's feature, which enables this crate's.
            #[cfg(feature = "upstream_modules")]
            fn init_upstream_modules(
                &self,
                modules: &mut ::pingora_core::modules::http::HttpModules,
            ) {
                (self.upstream_modules)(modules);
            }

            async fn early_request_filter(
                &self,
                session: &mut ::pingora_proxy::Session,
                ctx: &mut Self::CTX,
            ) -> ::pingora_core::Result<()>
            where
                Self::CTX: Send + Sync,
            {
                self.route_slot.get_mut(ctx).clear(self.id);
                let selected = self
                    .selector
                    .select(session, ctx)?
                    .unwrap_or_else(|| ::std::sync::Arc::clone(&self.fallback));
                self.route_slot
                    .get_mut(ctx)
                    .select(self.id, ::std::sync::Arc::clone(&selected));
                selected.early_request_filter(session, ctx).await
            }

            #(#forwarded)*
        }

        #probe
    }
    .into()
}

/// Rejects builds where Pingora declares the upstream-module hooks but this
/// crate's `upstream_modules` feature is off, which would otherwise silently
/// leave those hooks on Pingora's defaults instead of forwarding them.
///
/// The probe trait declares a method with the same name as the gated hook. If
/// `ProxyHttp` also declares it, the call through a type bounded by both traits
/// is ambiguous (E0034), and the error names the probe trait.
fn upstream_modules_probe() -> TokenStream2 {
    if cfg!(feature = "upstream_modules") {
        return TokenStream2::new();
    }
    quote! {
        const _: () = {
            trait EnableUpstreamModulesFeatureOfPingoraProxyDelegate {
                fn init_upstream_modules(
                    &self,
                    _modules: &mut ::pingora_core::modules::http::HttpModules,
                ) {
                }
            }

            impl<T: ?Sized> EnableUpstreamModulesFeatureOfPingoraProxyDelegate for T {}

            #[allow(dead_code)]
            fn probe<P>(proxy: &P, modules: &mut ::pingora_core::modules::http::HttpModules)
            where
                P: ::pingora_proxy::ProxyHttp + EnableUpstreamModulesFeatureOfPingoraProxyDelegate,
            {
                proxy.init_upstream_modules(modules);
            }
        };
    }
}

/// Returns delegation bodies for every `ProxyHttp` method not present in `skip`.
///
/// Audited against: `pingora-proxy` 0.9.x.
#[allow(clippy::too_many_lines)]
fn delegations(d: &Expr, skip: &HashSet<String>, wrap_async: bool) -> Vec<TokenStream2> {
    let mut out = Vec::new();
    // Preserve method-receiver autoderef (references, Box, Arc, and getters)
    // while resolving every hook explicitly through ProxyHttp. This local
    // extension trait returns the concrete target without dynamic dispatch.
    let receiver: Expr = if wrap_async {
        syn::parse_quote!(&*(#d))
    } else {
        syn::parse_quote!({
            trait __PingoraProxyDelegateTarget: ::pingora_proxy::ProxyHttp {
                fn __pingora_proxy_delegate_target(&self) -> &Self {
                    self
                }
            }
            impl<T: ::pingora_proxy::ProxyHttp + ?Sized> __PingoraProxyDelegateTarget for T {}
            (#d).__pingora_proxy_delegate_target()
        })
    };
    let async_delegate: Expr = if wrap_async {
        syn::parse_quote!(&*delegate)
    } else {
        receiver.clone()
    };

    macro_rules! delegate {
        ($name:literal, $body:expr) => {
            if !skip.contains($name) {
                out.push($body);
            }
        };
    }

    // `ProxyHttp` uses `async-trait`, whose ABI is a pinned boxed future. Emit
    // that ABI directly for pure forwarding methods so returning the inner
    // future does not allocate and box a second future at every layer.
    macro_rules! delegate_async {
        (
            $name:literal,
            [$($lifetime:lifetime),+ $(,)?],
            fn $method:ident($($argument:tt)*) -> $output:ty,
            [$($where_clause:tt)*],
            $body:expr
        ) => {{
            let body = $body;
            let body = if wrap_async {
                quote! {
                    ::std::boxed::Box::pin(async move {
                        let delegate = #d;
                        let future = { #body };
                        future.await
                    })
                }
            } else {
                body
            };
            delegate!(
                $name,
                quote! {
                    #[allow(clippy::type_complexity, clippy::type_repetition_in_bounds)]
                    fn $method<$($lifetime,)* 'async_trait>(
                        $($argument)*
                    ) -> ::core::pin::Pin<
                        ::std::boxed::Box<
                            dyn ::core::future::Future<Output = $output>
                                + ::core::marker::Send
                                + 'async_trait,
                        >,
                    >
                    where
                        $($where_clause)*
                        $($lifetime: 'async_trait,)*
                        Self: 'async_trait,
                    {
                        #body
                    }
                }
            );
        }};
    }

    delegate!(
        "new_ctx",
        quote! {
            fn new_ctx(&self) -> Self::CTX {
                ::pingora_proxy::ProxyHttp::new_ctx(#receiver)
            }
        }
    );

    delegate!(
        "init_downstream_modules",
        quote! {
            fn init_downstream_modules(
                &self,
                modules: &mut ::pingora_core::modules::http::HttpModules,
            ) {
                ::pingora_proxy::ProxyHttp::init_downstream_modules(#receiver, modules);
            }
        }
    );

    // Pingora only declares the upstream-module hooks with its
    // `upstream_modules` feature, which a proc macro cannot observe, so this
    // crate mirrors that feature. A mismatch fails to compile in either
    // direction: here when the hooks are missing, and in
    // `upstream_modules_probe` when they exist but are not forwarded.
    if cfg!(feature = "upstream_modules") {
        delegate!(
            "init_upstream_modules",
            quote! {
                fn init_upstream_modules(
                    &self,
                    modules: &mut ::pingora_core::modules::http::HttpModules,
                ) {
                    ::pingora_proxy::ProxyHttp::init_upstream_modules(#receiver, modules);
                }
            }
        );
    }

    delegate_async!(
        "early_request_filter",
        ['life0, 'life1, 'life2],
        fn early_request_filter(
            &'life0 self,
            session: &'life1 mut ::pingora_proxy::Session,
            ctx: &'life2 mut Self::CTX,
        ) -> ::pingora_core::Result<()>,
        [Self::CTX: Send + Sync,],
        quote! {
            ::pingora_proxy::ProxyHttp::early_request_filter(#async_delegate, session, ctx)
        }
    );

    delegate_async!(
        "request_filter",
        ['life0, 'life1, 'life2],
        fn request_filter(
            &'life0 self,
            session: &'life1 mut ::pingora_proxy::Session,
            ctx: &'life2 mut Self::CTX,
        ) -> ::pingora_core::Result<bool>,
        [Self::CTX: Send + Sync,],
        quote! {
            ::pingora_proxy::ProxyHttp::request_filter(#async_delegate, session, ctx)
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
                ::pingora_proxy::ProxyHttp::allow_spawning_subrequest(#receiver, session, ctx)
            }
        }
    );

    delegate_async!(
        "request_body_filter",
        ['life0, 'life1, 'life2, 'life3],
        fn request_body_filter(
            &'life0 self,
            session: &'life1 mut ::pingora_proxy::Session,
            body: &'life2 mut ::core::option::Option<::bytes::Bytes>,
            end_of_stream: bool,
            ctx: &'life3 mut Self::CTX,
        ) -> ::pingora_core::Result<()>,
        [Self::CTX: Send + Sync,],
        quote! {
            ::pingora_proxy::ProxyHttp::request_body_filter(#async_delegate, session, body, end_of_stream, ctx)
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
                ::pingora_proxy::ProxyHttp::request_cache_filter(#receiver, session, ctx)
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
                ::pingora_proxy::ProxyHttp::cache_key_callback(#receiver, session, ctx)
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
                ::pingora_proxy::ProxyHttp::cache_miss(#receiver, session, ctx);
            }
        }
    );

    delegate_async!(
        "cache_hit_filter",
        ['life0, 'life1, 'life2, 'life3, 'life4],
        fn cache_hit_filter(
            &'life0 self,
            session: &'life1 mut ::pingora_proxy::Session,
            meta: &'life2 ::pingora_cache::CacheMeta,
            hit_handler: &'life3 mut ::pingora_cache::HitHandler,
            is_fresh: bool,
            ctx: &'life4 mut Self::CTX,
        ) -> ::pingora_core::Result<::core::option::Option<::pingora_cache::ForcedFreshness>>,
        [Self::CTX: Send + Sync,],
        quote! {
            ::pingora_proxy::ProxyHttp::cache_hit_filter(#async_delegate, session, meta, hit_handler, is_fresh, ctx)
        }
    );

    delegate_async!(
        "proxy_upstream_filter",
        ['life0, 'life1, 'life2],
        fn proxy_upstream_filter(
            &'life0 self,
            session: &'life1 mut ::pingora_proxy::Session,
            ctx: &'life2 mut Self::CTX,
        ) -> ::pingora_core::Result<bool>,
        [Self::CTX: Send + Sync,],
        quote! {
            ::pingora_proxy::ProxyHttp::proxy_upstream_filter(#async_delegate, session, ctx)
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
                ::pingora_proxy::ProxyHttp::response_cache_filter(#receiver, session, resp, ctx)
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
                ::pingora_proxy::ProxyHttp::cache_vary_filter(#receiver, meta, ctx, req)
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
                ::pingora_proxy::ProxyHttp::cache_not_modified_filter(#receiver, session, resp, ctx)
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
                ::pingora_proxy::ProxyHttp::range_header_filter(#receiver, session, resp, ctx)
            }
        }
    );

    delegate_async!(
        "upstream_peer",
        ['life0, 'life1, 'life2],
        fn upstream_peer(
            &'life0 self,
            session: &'life1 mut ::pingora_proxy::Session,
            ctx: &'life2 mut Self::CTX,
        ) -> ::pingora_core::Result<
            ::std::boxed::Box<::pingora_core::upstreams::peer::HttpPeer>
        >,
        [],
        quote! {
            ::pingora_proxy::ProxyHttp::upstream_peer(#async_delegate, session, ctx)
        }
    );

    delegate_async!(
        "upstream_request_filter",
        ['life0, 'life1, 'life2, 'life3],
        fn upstream_request_filter(
            &'life0 self,
            session: &'life1 mut ::pingora_proxy::Session,
            upstream_request: &'life2 mut ::pingora_http::RequestHeader,
            ctx: &'life3 mut Self::CTX,
        ) -> ::pingora_core::Result<()>,
        [Self::CTX: Send + Sync,],
        quote! {
            ::pingora_proxy::ProxyHttp::upstream_request_filter(#async_delegate, session, upstream_request, ctx)
        }
    );

    delegate_async!(
        "upstream_response_filter",
        ['life0, 'life1, 'life2, 'life3],
        fn upstream_response_filter(
            &'life0 self,
            session: &'life1 mut ::pingora_proxy::Session,
            upstream_response: &'life2 mut ::pingora_http::ResponseHeader,
            ctx: &'life3 mut Self::CTX,
        ) -> ::pingora_core::Result<()>,
        [Self::CTX: Send + Sync,],
        quote! {
            ::pingora_proxy::ProxyHttp::upstream_response_filter(#async_delegate, session, upstream_response, ctx)
        }
    );

    if cfg!(feature = "upstream_modules") {
        delegate_async!(
            "adjust_upstream_modules",
            ['life0, 'life1, 'life2, 'life3],
            fn adjust_upstream_modules(
                &'life0 self,
                session: &'life1 mut ::pingora_proxy::Session,
                upstream_response: &'life2 ::pingora_http::ResponseHeader,
                end_of_stream: bool,
                ctx: &'life3 mut Self::CTX,
            ) -> ::pingora_core::Result<()>,
            [Self::CTX: Send + Sync,],
            quote! {
                ::pingora_proxy::ProxyHttp::adjust_upstream_modules(#async_delegate, session, upstream_response, end_of_stream, ctx)
            }
        );
    }

    delegate_async!(
        "response_filter",
        ['life0, 'life1, 'life2, 'life3],
        fn response_filter(
            &'life0 self,
            session: &'life1 mut ::pingora_proxy::Session,
            upstream_response: &'life2 mut ::pingora_http::ResponseHeader,
            ctx: &'life3 mut Self::CTX,
        ) -> ::pingora_core::Result<()>,
        [Self::CTX: Send + Sync,],
        quote! {
            ::pingora_proxy::ProxyHttp::response_filter(#async_delegate, session, upstream_response, ctx)
        }
    );

    delegate_async!(
        "custom_forwarding",
        ['life0, 'life1, 'life2],
        fn custom_forwarding(
            &'life0 self,
            session: &'life1 mut ::pingora_proxy::Session,
            ctx: &'life2 mut Self::CTX,
            custom_message_to_upstream: ::core::option::Option<
                ::tokio::sync::mpsc::Sender<::bytes::Bytes>,
            >,
            custom_message_to_downstream: ::tokio::sync::mpsc::Sender<::bytes::Bytes>,
        ) -> ::pingora_core::Result<()>,
        [Self::CTX: Send + Sync,],
        quote! {
            ::pingora_proxy::ProxyHttp::custom_forwarding(
                #async_delegate,
                session,
                ctx,
                custom_message_to_upstream,
                custom_message_to_downstream,
            )
        }
    );

    delegate_async!(
        "downstream_custom_message_proxy_filter",
        ['life0, 'life1, 'life2],
        fn downstream_custom_message_proxy_filter(
            &'life0 self,
            session: &'life1 mut ::pingora_proxy::Session,
            custom_message: ::bytes::Bytes,
            ctx: &'life2 mut Self::CTX,
            final_hop: bool,
        ) -> ::pingora_core::Result<::core::option::Option<::bytes::Bytes>>,
        [Self::CTX: Send + Sync,],
        quote! {
            ::pingora_proxy::ProxyHttp::downstream_custom_message_proxy_filter(
                #async_delegate,
                session,
                custom_message,
                ctx,
                final_hop,
            )
        }
    );

    delegate_async!(
        "upstream_custom_message_proxy_filter",
        ['life0, 'life1, 'life2],
        fn upstream_custom_message_proxy_filter(
            &'life0 self,
            session: &'life1 mut ::pingora_proxy::Session,
            custom_message: ::bytes::Bytes,
            ctx: &'life2 mut Self::CTX,
            final_hop: bool,
        ) -> ::pingora_core::Result<::core::option::Option<::bytes::Bytes>>,
        [Self::CTX: Send + Sync,],
        quote! {
            ::pingora_proxy::ProxyHttp::upstream_custom_message_proxy_filter(
                #async_delegate,
                session,
                custom_message,
                ctx,
                final_hop,
            )
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
                ::pingora_proxy::ProxyHttp::upstream_response_body_filter(#receiver, session, body, end_of_stream, ctx)
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
                ::pingora_proxy::ProxyHttp::upstream_response_trailer_filter(#receiver, session, upstream_trailers, ctx)
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
                ::pingora_proxy::ProxyHttp::response_body_filter(#receiver, session, body, end_of_stream, ctx)
            }
        }
    );

    delegate_async!(
        "response_trailer_filter",
        ['life0, 'life1, 'life2, 'life3],
        fn response_trailer_filter(
            &'life0 self,
            session: &'life1 mut ::pingora_proxy::Session,
            upstream_trailers: &'life2 mut ::http::HeaderMap,
            ctx: &'life3 mut Self::CTX,
        ) -> ::pingora_core::Result<::core::option::Option<::bytes::Bytes>>,
        [Self::CTX: Send + Sync,],
        quote! {
            ::pingora_proxy::ProxyHttp::response_trailer_filter(#async_delegate, session, upstream_trailers, ctx)
        }
    );

    delegate_async!(
        "logging",
        ['life0, 'life1, 'life2, 'life3],
        fn logging(
            &'life0 self,
            session: &'life1 mut ::pingora_proxy::Session,
            error: ::core::option::Option<&'life2 ::pingora_core::Error>,
            ctx: &'life3 mut Self::CTX,
        ) -> (),
        [Self::CTX: Send + Sync,],
        quote! {
            ::pingora_proxy::ProxyHttp::logging(#async_delegate, session, error, ctx)
        }
    );

    delegate!(
        "persist_connection_context",
        quote! {
            fn persist_connection_context(
                &self,
                session: &::pingora_proxy::Session,
                ctx: &Self::CTX,
            ) -> ::core::option::Option<
                ::std::boxed::Box<
                    dyn ::std::any::Any + ::core::marker::Send + ::core::marker::Sync,
                >,
            > {
                ::pingora_proxy::ProxyHttp::persist_connection_context(#receiver, session, ctx)
            }
        }
    );

    delegate!(
        "on_connection_reuse",
        quote! {
            fn on_connection_reuse(
                &self,
                session: &mut ::pingora_proxy::Session,
                ctx: &mut Self::CTX,
                prev_ctx: ::std::boxed::Box<
                    dyn ::std::any::Any + ::core::marker::Send + ::core::marker::Sync,
                >,
            ) {
                ::pingora_proxy::ProxyHttp::on_connection_reuse(#receiver, session, ctx, prev_ctx);
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
                ::pingora_proxy::ProxyHttp::suppress_error_log(#receiver, session, ctx, error)
            }
        }
    );

    delegate!(
        "suppress_proxy_warn_log",
        quote! {
            fn suppress_proxy_warn_log(
                &self,
                session: &::pingora_proxy::Session,
                ctx: &Self::CTX,
                error: &::pingora_core::Error,
                context: ::pingora_proxy::ProxyWarnLogContext,
            ) -> bool {
                ::pingora_proxy::ProxyHttp::suppress_proxy_warn_log(#receiver, session, ctx, error, context)
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
                ::pingora_proxy::ProxyHttp::error_while_proxy(#receiver, peer, session, e, ctx, client_reused)
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
                ::pingora_proxy::ProxyHttp::fail_to_connect(#receiver, session, peer, ctx, e)
            }
        }
    );

    delegate_async!(
        "fail_to_proxy",
        ['life0, 'life1, 'life2, 'life3],
        fn fail_to_proxy(
            &'life0 self,
            session: &'life1 mut ::pingora_proxy::Session,
            e: &'life2 ::pingora_core::Error,
            ctx: &'life3 mut Self::CTX,
        ) -> ::pingora_proxy::FailToProxy,
        [Self::CTX: Send + Sync,],
        quote! {
            ::pingora_proxy::ProxyHttp::fail_to_proxy(#async_delegate, session, e, ctx)
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
                ::pingora_proxy::ProxyHttp::should_serve_stale(#receiver, session, ctx, error)
            }
        }
    );

    // Platform-specific fd/sock parameter requires cfg-gated call sites.
    delegate_async!(
        "connected_to_upstream",
        ['life0, 'life1, 'life2, 'life3, 'life4],
        fn connected_to_upstream(
            &'life0 self,
            session: &'life1 mut ::pingora_proxy::Session,
            reused: bool,
            peer: &'life2 ::pingora_core::upstreams::peer::HttpPeer,
            #[cfg(unix)] fd: ::std::os::unix::io::RawFd,
            #[cfg(windows)] sock: ::std::os::windows::io::RawSocket,
            digest: ::core::option::Option<&'life3 ::pingora_core::protocols::Digest>,
            ctx: &'life4 mut Self::CTX,
        ) -> ::pingora_core::Result<()>,
        [Self::CTX: Send + Sync,],
        quote! {
            #[cfg(unix)]
            {
                ::pingora_proxy::ProxyHttp::connected_to_upstream(#async_delegate, session, reused, peer, fd, digest, ctx)
            }
            #[cfg(windows)]
            {
                ::pingora_proxy::ProxyHttp::connected_to_upstream(#async_delegate, session, reused, peer, sock, digest, ctx)
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
                ::pingora_proxy::ProxyHttp::request_summary(#receiver, session, ctx)
            }
        }
    );

    delegate!(
        "is_purge",
        quote! {
            fn is_purge(&self, session: &::pingora_proxy::Session, ctx: &Self::CTX) -> bool {
                ::pingora_proxy::ProxyHttp::is_purge(#receiver, session, ctx)
            }
        }
    );

    delegate!(
        "purge_action",
        quote! {
            fn purge_action(
                &self,
                session: &::pingora_proxy::Session,
                ctx: &Self::CTX,
            ) -> ::pingora_cache::PurgeAction {
                ::pingora_proxy::ProxyHttp::purge_action(#receiver, session, ctx)
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
                ::pingora_proxy::ProxyHttp::purge_response_filter(#receiver, session, ctx, purge_status, purge_response)
            }
        }
    );

    out
}
