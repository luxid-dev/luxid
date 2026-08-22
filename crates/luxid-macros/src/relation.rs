//! `#[luxid::model(..)]` — declares a model's relations.
//!
//! Relations are declared in the attribute arguments rather than as bodiless
//! `fn` items: `fn posts();` is not parseable Rust, so that spelling would
//! greet users with a syntax error from inside a macro. Declaring them here
//! keeps the impl block free for ordinary methods.
//!
//! ```ignore
//! #[luxid::model(
//!     has_many(posts = Post, fk = "user_id"),
//!     belongs_to(team = Team),
//! )]
//! impl User {}
//! ```
//!
//! Each declaration generates an accessor reading the model's relations bag,
//! and one arm of `Relatable::load_relation`, which loads that relation for a
//! whole batch of parents in a single query.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{FnArg, ImplItem, ItemImpl, LitStr, Path, Token, Type, parse_macro_input};

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    HasMany,
    HasOne,
    BelongsTo,
}

impl Kind {
    fn parse(ident: &syn::Ident) -> Option<Self> {
        match ident.to_string().as_str() {
            "has_many" => Some(Self::HasMany),
            "has_one" => Some(Self::HasOne),
            "belongs_to" => Some(Self::BelongsTo),
            _ => None,
        }
    }

    fn loader(self) -> TokenStream2 {
        match self {
            Self::HasMany => quote!(load_has_many),
            Self::HasOne => quote!(load_has_one),
            Self::BelongsTo => quote!(load_belongs_to),
        }
    }
}

struct Declaration {
    kind: Kind,
    name: syn::Ident,
    target: Type,
    local_key: syn::Ident,
    foreign_key: syn::Ident,
}

impl Parse for Declaration {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let keyword: syn::Ident = input.parse()?;

        let Some(kind) = Kind::parse(&keyword) else {
            return Err(syn::Error::new_spanned(
                &keyword,
                "expected `has_many`, `has_one`, `belongs_to`, or `crate`",
            ));
        };

        let content;
        syn::parenthesized!(content in input);

        // `name = Target`
        let name: syn::Ident = content.parse()?;
        content.parse::<Token![=]>()?;
        let target: Type = content.parse()?;

        let mut foreign: Option<syn::Ident> = None;
        let mut local: Option<syn::Ident> = None;

        while content.peek(Token![,]) {
            content.parse::<Token![,]>()?;
            if content.is_empty() {
                break;
            }

            let key: syn::Ident = content.parse()?;
            content.parse::<Token![=]>()?;
            let value: LitStr = content.parse()?;
            let column = syn::Ident::new(&value.value(), value.span());

            match key.to_string().as_str() {
                "fk" => foreign = Some(column),
                "local_key" => local = Some(column),
                _ => {
                    return Err(syn::Error::new_spanned(
                        &key,
                        "expected `fk` or `local_key`",
                    ));
                }
            }
        }

        // Conventions: the owner is joined on `id`; the side holding the
        // foreign key names it after the relation.
        let (local_key, foreign_key) = match kind {
            Kind::HasMany | Kind::HasOne => {
                let Some(foreign) = foreign else {
                    return Err(syn::Error::new_spanned(
                        &keyword,
                        "specify the foreign key on the related model, e.g. `fk = \"user_id\"`",
                    ));
                };
                (
                    local.unwrap_or_else(|| syn::Ident::new("id", name.span())),
                    foreign,
                )
            }
            Kind::BelongsTo => (
                foreign.unwrap_or_else(|| format_ident!("{}_id", name)),
                local.unwrap_or_else(|| syn::Ident::new("id", name.span())),
            ),
        };

        Ok(Self {
            kind,
            name,
            target,
            local_key,
            foreign_key,
        })
    }
}

struct Args {
    krate: Option<Path>,
    declarations: Vec<Declaration>,
}

impl Parse for Args {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut krate = None;
        let mut declarations = Vec::new();

        while !input.is_empty() {
            if input.peek(Token![crate]) {
                input.parse::<Token![crate]>()?;
                input.parse::<Token![=]>()?;
                krate = Some(input.parse()?);
            } else {
                declarations.push(input.parse()?);
            }

            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(Self {
            krate,
            declarations,
        })
    }
}

pub fn model(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as Args);
    let block = parse_macro_input!(item as ItemImpl);

    match expand(args, block) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// A `#[scope]` function: takes the query, returns the query, plus any extra
/// arguments the caller supplies.
struct Scope {
    name: syn::Ident,
    inner: syn::Ident,
    query_ty: Type,
    extra: Vec<(syn::Ident, Type)>,
}

/// Pull `#[scope]` functions out of the impl block, renaming them so the
/// generated wrappers can own the public names.
fn take_scopes(block: &mut ItemImpl) -> syn::Result<Vec<Scope>> {
    let mut scopes = Vec::new();

    for item in &mut block.items {
        let ImplItem::Fn(func) = item else { continue };

        let Some(index) = func
            .attrs
            .iter()
            .position(|attr| attr.path().is_ident("scope"))
        else {
            continue;
        };
        func.attrs.remove(index);

        let name = func.sig.ident.clone();

        let Some(FnArg::Typed(first)) = func.sig.inputs.first() else {
            return Err(syn::Error::new_spanned(
                &func.sig,
                "a #[scope] takes the query as its first argument, \
                 e.g. `fn active(query: Query<users::Entity>) -> Query<users::Entity>`",
            ));
        };
        let query_ty = (*first.ty).clone();

        let mut extra = Vec::new();
        for argument in func.sig.inputs.iter().skip(1) {
            let FnArg::Typed(argument) = argument else {
                return Err(syn::Error::new_spanned(
                    argument,
                    "a #[scope] is an associated function and cannot take `self`",
                ));
            };
            let syn::Pat::Ident(ident) = &*argument.pat else {
                return Err(syn::Error::new_spanned(
                    &argument.pat,
                    "#[scope] arguments must be plain identifiers",
                ));
            };

            extra.push((ident.ident.clone(), (*argument.ty).clone()));
        }

        let inner = format_ident!("__luxid_scope_{}", name);
        func.sig.ident = inner.clone();

        scopes.push(Scope {
            name,
            inner,
            query_ty,
            extra,
        });
    }

    Ok(scopes)
}

fn expand(args: Args, block: ItemImpl) -> syn::Result<TokenStream2> {
    if block.trait_.is_some() {
        return Err(syn::Error::new_spanned(
            &block.self_ty,
            "#[luxid::model] expects a plain inherent impl, e.g. `impl User {}`",
        ));
    }

    let mut block = block;
    let scopes = take_scopes(&mut block)?;

    let self_ty = block.self_ty.clone();
    let krate = args
        .krate
        .map(|path| quote!(#path))
        .unwrap_or_else(|| quote!(::luxid));

    let mut accessors = Vec::new();
    let mut arms = Vec::new();

    for declaration in &args.declarations {
        let Declaration {
            kind,
            name,
            target,
            local_key,
            foreign_key,
        } = declaration;

        let relation = name.to_string();
        let docs = format!("The `{relation}` relation. Load it with `.with(\"{relation}\")`.");

        accessors.push(match kind {
            Kind::HasMany => quote! {
                #[doc = #docs]
                pub fn #name(&self) -> #krate::Result<&[#target]> {
                    #krate::__private::Relatable::relations(self)
                        .many::<#target>(<Self as #krate::__private::Lucid>::MODEL, #relation)
                }
            },
            Kind::HasOne | Kind::BelongsTo => quote! {
                #[doc = #docs]
                pub fn #name(&self) -> #krate::Result<::std::option::Option<&#target>> {
                    #krate::__private::Relatable::relations(self)
                        .one::<#target>(<Self as #krate::__private::Lucid>::MODEL, #relation)
                }
            },
        });

        let loader = kind.loader();

        arms.push(quote! {
            #relation => {
                #krate::__private::#loader::<Self, #target>(
                    parents,
                    #krate::__private::ColumnRef::column(&Self::#local_key),
                    #krate::__private::ColumnRef::column(&<#target>::#foreign_key),
                    #relation,
                )
                .await
            }
        });
    }

    let declared: Vec<String> = args
        .declarations
        .iter()
        .map(|d| d.name.to_string())
        .collect();
    let declared = declared.join(", ");

    let scope_tokens = expand_scopes(&self_ty, &scopes, &krate)?;

    // A model with no declared relations gets no `Relatable` impl, so `.with(..)`
    // on it is a trait-bound error rather than a runtime "no such relation".
    let relatable = if args.declarations.is_empty() {
        quote!()
    } else {
        quote! {
        impl #krate::__private::Relatable for #self_ty {
            fn relations(&self) -> &#krate::__private::Relations {
                &self.relations
            }

            fn relations_mut(&mut self) -> &mut #krate::__private::Relations {
                &mut self.relations
            }

            fn load_relation(
                name: ::std::string::String,
                parents: &mut ::std::vec::Vec<Self>,
            ) -> #krate::__private::BoxFuture<'_, #krate::Result<()>> {
                ::std::boxed::Box::pin(async move {
                    match name.as_str() {
                        #(#arms)*
                        unknown => ::std::result::Result::Err(#krate::Error::internal(
                            ::std::format!(
                                "`{}` has no relation `{}`. Declared relations: [{}].",
                                <Self as #krate::__private::Lucid>::MODEL,
                                unknown,
                                #declared,
                            ),
                        )),
                    }
                })
            }
        }
        }
    };

    Ok(quote! {
        #block

        impl #self_ty {
            #(#accessors)*
        }

        #relatable

        #scope_tokens
    })
}

/// Scopes become two things: a starter on the model (`User::active()`) that
/// needs no import, and a trait on the query (`User::query().active()`) for
/// mid-chain use, which does.
fn expand_scopes(
    self_ty: &Type,
    scopes: &[Scope],
    krate: &TokenStream2,
) -> syn::Result<TokenStream2> {
    if scopes.is_empty() {
        return Ok(quote!());
    }

    let Type::Path(path) = self_ty else {
        return Err(syn::Error::new_spanned(self_ty, "expected a named type"));
    };
    let Some(ident) = path.path.segments.last().map(|segment| &segment.ident) else {
        return Err(syn::Error::new_spanned(self_ty, "expected a named type"));
    };

    let trait_ident = format_ident!("{}Scopes", ident);
    let query_ty = scopes[0].query_ty.clone();

    let mut signatures = Vec::new();
    let mut implementations = Vec::new();
    let mut starters = Vec::new();

    for scope in scopes {
        let Scope {
            name, inner, extra, ..
        } = scope;

        let params = extra.iter().map(|(name, ty)| quote!(#name: #ty));
        let params2 = params.clone();
        let params3 = params.clone();
        let forwarded = extra.iter().map(|(name, _)| quote!(#name));
        let forwarded2 = forwarded.clone();

        let docs = format!("The `{name}` scope.");

        signatures.push(quote! {
            #[doc = #docs]
            fn #name(self, #(#params),*) -> Self;
        });

        implementations.push(quote! {
            fn #name(self, #(#params2),*) -> Self {
                <#self_ty>::#inner(self, #(#forwarded),*)
            }
        });

        starters.push(quote! {
            #[doc = #docs]
            pub fn #name(#(#params3),*) -> #query_ty {
                <#self_ty>::#inner(<#self_ty as #krate::__private::Lucid>::query(), #(#forwarded2),*)
            }
        });
    }

    Ok(quote! {
        #[doc = "Scopes for this model. Import it to chain them onto a query."]
        pub trait #trait_ident {
            #(#signatures)*
        }

        impl #trait_ident for #query_ty {
            #(#implementations)*
        }

        impl #self_ty {
            #(#starters)*
        }
    })
}
