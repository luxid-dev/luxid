//! `#[luxid::test]` — a test that leaves the database as it found it.
//!
//! ```ignore
//! #[luxid::test(db = crate::support::database)]
//! async fn it_lists_users(db: Db) -> luxid::Result<()> {
//!     // every query here runs inside a transaction that is rolled back
//! }
//! ```
//!
//! Without `db = ..` this is `#[tokio::test]` with `Result` unwrapping. With
//! it, the named factory supplies a database, the body runs inside a
//! transaction, and the transaction is rolled back afterwards — so tests share
//! one database, run in parallel, and need no truncation or fixtures.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{ItemFn, Path, ReturnType, Token, parse_macro_input};

struct Args {
    db: Option<Path>,
}

impl Parse for Args {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut db = None;

        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;

            if key == "db" {
                db = Some(input.parse()?);
            } else {
                return Err(syn::Error::new_spanned(&key, "expected `db = <path>`"));
            }

            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(Self { db })
    }
}

pub fn test(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as Args);
    let function = parse_macro_input!(item as ItemFn);

    match expand(args, function) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand(args: Args, function: ItemFn) -> syn::Result<TokenStream2> {
    if function.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &function.sig,
            "#[luxid::test] expects an async fn",
        ));
    }

    let name = function.sig.ident.clone();
    let attrs = function.attrs.clone();
    let returns_value = !matches!(function.sig.output, ReturnType::Default);

    let mut inner = function;
    inner.sig.ident = syn::Ident::new("__luxid_test_body", inner.sig.ident.span());
    inner.attrs.clear();

    let takes_db = !inner.sig.inputs.is_empty();

    let Some(factory) = args.db else {
        if takes_db {
            return Err(syn::Error::new_spanned(
                &inner.sig,
                "this test takes an argument, so it needs a database factory: \
                 `#[luxid::test(db = path::to::factory)]`",
            ));
        }

        let call = unwrap(quote!(__luxid_test_body().await), returns_value);

        return Ok(quote! {
            #[::tokio::test]
            #(#attrs)*
            async fn #name() {
                #inner
                #call
            }
        });
    };

    let invocation = if takes_db {
        quote!(__luxid_test_body(__luxid_db.clone()).await)
    } else {
        quote!(__luxid_test_body().await)
    };
    let call = unwrap(quote!(__luxid_outcome), returns_value);

    Ok(quote! {
        #[::tokio::test]
        #(#attrs)*
        async fn #name() {
            #inner

            let __luxid_db = #factory().await;

            // The body runs inside a transaction; the transaction is discarded
            // whether the body passed or failed.
            let __luxid_outcome = __luxid_db
                .rollback_scope(async || #invocation)
                .await
                .expect("the test transaction could not be rolled back");

            #call
        }
    })
}

/// A body returning `Result` should fail the test on `Err`, with the error
/// visible rather than swallowed.
fn unwrap(expression: TokenStream2, returns_value: bool) -> TokenStream2 {
    if returns_value {
        quote! {
            if let ::std::result::Result::Err(error) = #expression {
                ::std::panic!("test returned an error: {error}");
            }
        }
    } else {
        quote!(let _ = #expression;)
    }
}
