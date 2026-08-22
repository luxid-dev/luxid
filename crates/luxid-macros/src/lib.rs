//! Procedural macros for Luxid.
//!
//! Neither macro emits salvo types — generated code depends only on Luxid's own
//! traits, which keeps the substrate sealed even in macro output.

mod model;
mod relation;
mod testing;
mod validate;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{
    Attribute, FnArg, ImplItem, ImplItemFn, ItemImpl, LitInt, LitStr, Token, Type,
    parse_macro_input, parse_quote,
};

/// Turn an inherent `impl` block into routable actions.
///
/// Every `async fn name(ctx: HttpContext) -> Result<Response>` becomes a
/// zero-sized handler exposed as an associated constant, so routes read
/// `UsersController::index` with no parentheses. Other items are left alone.
#[proc_macro_attribute]
pub fn controller(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut block = parse_macro_input!(item as ItemImpl);

    let self_ty = block.self_ty.clone();
    let ty_ident = match inherent_ident(&block) {
        Ok(ident) => ident,
        Err(err) => return err.to_compile_error().into(),
    };

    let mut handlers: Vec<TokenStream2> = Vec::new();
    let mut constants: Vec<TokenStream2> = Vec::new();
    let mut resource: Vec<TokenStream2> = Vec::new();

    for item in &mut block.items {
        let ImplItem::Fn(func) = item else { continue };
        if !is_action(func) {
            continue;
        }

        // `#[openapi(..)]` is not a real attribute; the controller macro owns it.
        let documentation = match take_openapi(&mut func.attrs) {
            Ok(documentation) => documentation,
            Err(err) => return err.to_compile_error().into(),
        };

        let action = func.sig.ident.clone();
        let inner = format_ident!("__luxid_action_{}", action);
        func.sig.ident = inner.clone();
        func.vis = parse_quote!(pub);

        let handler_ty = format_ident!("__LuxidAction_{}_{}", ty_ident, action);
        let docs = format!("Route handler for [`{ty_ident}::{action}`].");
        let action_label = format!("{ty_ident}::{action}");

        handlers.push(quote! {
            #[doc = #docs]
            #[doc(hidden)]
            #[allow(non_camel_case_types)]
            #[derive(Clone, Copy, Debug, Default)]
            pub struct #handler_ty;

            impl ::luxid::__private::Action for #handler_ty {
                fn call(
                    &self,
                    ctx: ::luxid::HttpContext,
                ) -> ::luxid::__private::BoxFuture<'static, ::luxid::Result<::luxid::Response>> {
                    ::std::boxed::Box::pin(#self_ty::#inner(ctx))
                }

                fn name(&self) -> &'static str {
                    #action_label
                }

                #documentation
            }
        });

        constants.push(quote! {
            #[allow(non_upper_case_globals)]
            pub const #action: #handler_ty = #handler_ty;
        });

        // Only the actions that exist become routes.
        let registration = match action.to_string().as_str() {
            "index" => Some(quote!(router.get("", Self::index);)),
            "store" => Some(quote!(router.post("", Self::store);)),
            "show" => Some(quote!(router.get("/{id}", Self::show);)),
            "update" => Some(quote!(router.put("/{id}", Self::update);)),
            "destroy" => Some(quote!(router.delete("/{id}", Self::destroy);)),
            _ => None,
        };

        if let Some(registration) = registration {
            resource.push(registration);
        }
    }

    // A controller with none of the five resource actions gets no impl, so
    // `r.resource(..)` on it is a compile error rather than a route table that
    // silently registers nothing.
    let resource_impl = if resource.is_empty() {
        quote!()
    } else {
        quote! {
            impl ::luxid::__private::ResourceRoutes for #self_ty {
                fn register(router: &mut ::luxid::Router) {
                    #(#resource)*
                }
            }
        }
    };

    quote! {
        #block

        #(#handlers)*

        impl #self_ty {
            #(#constants)*
        }

        #resource_impl
    }
    .into()
}

/// Implement `Middleware` from an inherent
/// `async fn handle(&self, ctx: HttpContext, next: Next) -> Result<Response>`.
#[proc_macro_attribute]
pub fn middleware(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut block = parse_macro_input!(item as ItemImpl);

    let self_ty = block.self_ty.clone();
    if let Err(err) = inherent_ident(&block) {
        return err.to_compile_error().into();
    }

    let mut found = false;

    for item in &mut block.items {
        let ImplItem::Fn(func) = item else { continue };
        if func.sig.ident != "handle" {
            continue;
        }

        if func.sig.asyncness.is_none() {
            return syn::Error::new_spanned(&func.sig, "`handle` must be an async fn")
                .to_compile_error()
                .into();
        }
        if !matches!(func.sig.inputs.first(), Some(FnArg::Receiver(_))) {
            return syn::Error::new_spanned(
                &func.sig,
                "`handle` must take `&self` so configured middleware can hold state",
            )
            .to_compile_error()
            .into();
        }

        func.sig.ident = format_ident!("__luxid_middleware_handle");
        found = true;
        break;
    }

    if !found {
        return syn::Error::new_spanned(
            &self_ty,
            "#[luxid::middleware] expects `async fn handle(&self, ctx: HttpContext, next: Next) -> Result<Response>`",
        )
        .to_compile_error()
        .into();
    }

    quote! {
        #block

        impl ::luxid::__private::Middleware for #self_ty {
            fn handle<'__luxid>(
                &'__luxid self,
                ctx: ::luxid::HttpContext,
                next: ::luxid::Next,
            ) -> ::luxid::__private::BoxFuture<'__luxid, ::luxid::Result<::luxid::Response>> {
                ::std::boxed::Box::pin(Self::__luxid_middleware_handle(self, ctx, next))
            }
        }
    }
    .into()
}

/// Actions are `async fn name(ctx: HttpContext) -> Result<Response>`. Anything
/// else in the block is a helper and stays untouched.
fn is_action(func: &ImplItemFn) -> bool {
    func.sig.asyncness.is_some()
        && func.sig.inputs.len() == 1
        && !matches!(func.sig.inputs.first(), Some(FnArg::Receiver(_)))
}

fn inherent_ident(block: &ItemImpl) -> syn::Result<syn::Ident> {
    if block.trait_.is_some() {
        return Err(syn::Error::new_spanned(
            &block.self_ty,
            "expected a plain inherent impl, not a trait implementation",
        ));
    }

    match &*block.self_ty {
        Type::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.clone())
            .ok_or_else(|| syn::Error::new_spanned(&block.self_ty, "expected a named type")),
        other => Err(syn::Error::new_spanned(
            other,
            "expected a named type, e.g. `impl UsersController`",
        )),
    }
}

/// Derive Lucid model operations and typed columns for a SeaORM entity model.
///
/// The model name used in 404s comes from `#[sea_orm(table_name = "...")]`,
/// singularized. Override it with `#[luxid(name = "Person")]` when the naive
/// rules get it wrong.
#[proc_macro_derive(Model, attributes(luxid))]
pub fn derive_model(item: TokenStream) -> TokenStream {
    model::derive(item)
}

/// Declare a model's relations.
///
/// ```ignore
/// #[luxid::model(
///     has_many(posts = Post, fk = "user_id"),
///     belongs_to(team = Team),
/// )]
/// impl User {}
/// ```
#[proc_macro_attribute]
pub fn model(attr: TokenStream, item: TokenStream) -> TokenStream {
    relation::model(attr, item)
}

/// A test that leaves the database as it found it.
///
/// See [`testing`](crate) for the full form; without `db = ..` this is
/// `#[tokio::test]` with `Result` unwrapping.
#[proc_macro_attribute]
pub fn test(attr: TokenStream, item: TokenStream) -> TokenStream {
    testing::test(attr, item)
}

/// Derive form-request validation.
///
/// Synchronous rules (`length`, `email`, `range`, `custom`) run first;
/// asynchronous ones (`unique`, `exists`) run afterwards against the request's
/// ambient database connection, skipping fields that already failed.
#[proc_macro_derive(Validate, attributes(validate, luxid))]
pub fn derive_validate(item: TokenStream) -> TokenStream {
    validate::derive(item)
}

/// One `#[openapi(..)]` entry.
struct OpenApi {
    summary: Option<String>,
    tag: Option<String>,
    body: Option<Type>,
    responses: Vec<(u16, Option<Type>)>,
}

impl Parse for OpenApi {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut documentation = OpenApi {
            summary: None,
            tag: None,
            body: None,
            responses: Vec::new(),
        };

        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            let label = key.to_string();

            match label.as_str() {
                // `no_content` is a bare flag: 204 carries nothing by definition.
                "no_content" => documentation.responses.push((204, None)),

                "errors" => {
                    input.parse::<Token![=]>()?;
                    let statuses;
                    syn::bracketed!(statuses in input);

                    while !statuses.is_empty() {
                        let status: LitInt = statuses.parse()?;
                        documentation.responses.push((status.base10_parse()?, None));

                        if statuses.peek(Token![,]) {
                            statuses.parse::<Token![,]>()?;
                        }
                    }
                }

                "summary" | "tag" => {
                    input.parse::<Token![=]>()?;
                    let value: LitStr = input.parse()?;

                    if label == "summary" {
                        documentation.summary = Some(value.value());
                    } else {
                        documentation.tag = Some(value.value());
                    }
                }

                "body" => {
                    input.parse::<Token![=]>()?;
                    documentation.body = Some(input.parse()?);
                }

                "ok" | "created" | "accepted" => {
                    input.parse::<Token![=]>()?;
                    let status = match label.as_str() {
                        "ok" => 200,
                        "created" => 201,
                        _ => 202,
                    };
                    documentation.responses.push((status, Some(input.parse()?)));
                }

                other => {
                    return Err(syn::Error::new_spanned(
                        &key,
                        format!(
                            "`{other}` is not a recognised #[openapi] key. Expected one of: \
                             summary, tag, body, ok, created, accepted, no_content, errors"
                        ),
                    ));
                }
            }

            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(documentation)
    }
}

/// Remove `#[openapi(..)]` from an action and render it as an `Action::openapi`
/// implementation.
fn take_openapi(attrs: &mut Vec<Attribute>) -> syn::Result<TokenStream2> {
    let Some(index) = attrs
        .iter()
        .position(|attr| attr.path().is_ident("openapi"))
    else {
        return Ok(TokenStream2::new());
    };

    let attr = attrs.remove(index);
    let documentation: OpenApi = attr.parse_args()?;

    let summary = match &documentation.summary {
        Some(text) => quote!(::std::option::Option::Some(#text)),
        None => quote!(::std::option::Option::None),
    };
    let tag = match &documentation.tag {
        Some(text) => quote!(::std::option::Option::Some(#text)),
        None => quote!(::std::option::Option::None),
    };
    let body = match &documentation.body {
        Some(ty) => quote!(::std::option::Option::Some(::luxid::__private::schema_of::<#ty>)),
        None => quote!(::std::option::Option::None),
    };

    let responses = documentation.responses.iter().map(|(status, ty)| {
        let schema = match ty {
            Some(ty) => quote!(::std::option::Option::Some(::luxid::__private::schema_of::<#ty>)),
            None => quote!(::std::option::Option::None),
        };
        quote! {
            ::luxid::__private::ResponseSpec { status: #status, schema: #schema }
        }
    });

    Ok(quote! {
        fn openapi(&self) -> ::std::option::Option<::luxid::__private::Operation> {
            ::std::option::Option::Some(::luxid::__private::Operation {
                summary: #summary,
                tag: #tag,
                body: #body,
                responses: ::std::vec![#(#responses),*],
            })
        }
    })
}
