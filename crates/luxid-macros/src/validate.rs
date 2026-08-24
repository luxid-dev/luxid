//! `#[derive(Validate)]`.
//!
//! Synchronous rules expand inline. Asynchronous rules expand into data-layer
//! queries against the ambient connection, which is why this derive needs no
//! database wiring of its own.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Data, DeriveInput, Expr, Fields, LitInt, LitStr, Path, Token, Type, parse_macro_input};

/// One `#[validate(..)]` entry.
///
/// Boxed variants keep the enum small; `Range` carries two `syn::Expr`, which
/// dwarf every other variant.
enum Rule {
    Length {
        min: Option<usize>,
        max: Option<usize>,
        equal: Option<usize>,
        message: Option<String>,
    },
    Email {
        message: Option<String>,
    },
    Range {
        min: Option<Box<Expr>>,
        max: Option<Box<Expr>>,
        message: Option<String>,
    },
    Custom {
        function: Path,
        message: Option<String>,
    },
    Unique {
        model: Path,
        column: syn::Ident,
        except: Option<syn::Ident>,
        message: Option<String>,
    },
    Exists {
        model: Path,
        column: syn::Ident,
        message: Option<String>,
    },
}

impl Rule {
    fn is_async(&self) -> bool {
        matches!(self, Self::Unique { .. } | Self::Exists { .. })
    }
}

struct Rules(Vec<Rule>);

impl Parse for Rules {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut rules = Vec::new();

        while !input.is_empty() {
            let name: syn::Ident = input.parse()?;
            let label = name.to_string();

            let mut min_int = None;
            let mut max_int = None;
            let mut equal_int = None;
            let mut min_expr = None;
            let mut max_expr = None;
            let mut message = None;
            let mut target: Option<Path> = None;
            let mut except: Option<syn::Ident> = None;
            let mut function: Option<Path> = None;

            if input.peek(syn::token::Paren) {
                let args;
                syn::parenthesized!(args in input);

                let mut first = true;
                while !args.is_empty() {
                    // `unique(User::email)` / `exists(Team::id)` lead with a path.
                    if first && matches!(label.as_str(), "unique" | "exists") {
                        target = Some(args.parse()?);
                        first = false;

                        if args.peek(Token![,]) {
                            args.parse::<Token![,]>()?;
                        }
                        continue;
                    }
                    first = false;

                    let key: syn::Ident = args.parse()?;
                    args.parse::<Token![=]>()?;

                    match key.to_string().as_str() {
                        "min" if label == "length" => min_int = Some(parse_usize(&args)?),
                        "max" if label == "length" => max_int = Some(parse_usize(&args)?),
                        "equal" => equal_int = Some(parse_usize(&args)?),
                        "min" => min_expr = Some(Box::new(args.parse::<Expr>()?)),
                        "max" => max_expr = Some(Box::new(args.parse::<Expr>()?)),
                        "message" => message = Some(args.parse::<LitStr>()?.value()),
                        "except" => except = Some(ident_from(&args)?),
                        "function" => function = Some(args.parse()?),
                        other => {
                            return Err(syn::Error::new_spanned(
                                &key,
                                format!("`{other}` is not a recognised option for `{label}`"),
                            ));
                        }
                    }

                    if args.peek(Token![,]) {
                        args.parse::<Token![,]>()?;
                    }
                }
            }

            rules.push(match label.as_str() {
                "length" => Rule::Length {
                    min: min_int,
                    max: max_int,
                    equal: equal_int,
                    message,
                },
                "email" => Rule::Email { message },
                "range" => Rule::Range {
                    min: min_expr,
                    max: max_expr,
                    message,
                },
                "custom" => {
                    let Some(function) = function else {
                        return Err(syn::Error::new_spanned(
                            &name,
                            "`custom` needs `function = path::to::check`",
                        ));
                    };
                    Rule::Custom { function, message }
                }
                "unique" | "exists" => {
                    let Some(path) = target else {
                        return Err(syn::Error::new_spanned(
                            &name,
                            format!("`{label}` needs a column, e.g. `{label}(User::email)`"),
                        ));
                    };
                    let (model, column) = split_column(&path)?;

                    if label == "unique" {
                        Rule::Unique {
                            model,
                            column,
                            except,
                            message,
                        }
                    } else {
                        Rule::Exists {
                            model,
                            column,
                            message,
                        }
                    }
                }
                other => {
                    return Err(syn::Error::new_spanned(
                        &name,
                        format!("`{other}` is not a recognised rule"),
                    ));
                }
            });

            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(Self(rules))
    }
}

fn parse_usize(input: ParseStream) -> syn::Result<usize> {
    input.parse::<LitInt>()?.base10_parse()
}

fn ident_from(input: ParseStream) -> syn::Result<syn::Ident> {
    let value: LitStr = input.parse()?;
    Ok(syn::Ident::new(&value.value(), value.span()))
}

/// `User::email` → (`User`, `email`).
fn split_column(path: &Path) -> syn::Result<(Path, syn::Ident)> {
    if path.segments.len() < 2 {
        return Err(syn::Error::new_spanned(
            path,
            "expected `Model::column`, e.g. `User::email`",
        ));
    }

    let mut model = path.clone();
    let column = model
        .segments
        .pop()
        .expect("checked above")
        .into_value()
        .ident;
    // Drop the trailing separator left by `pop`.
    let model = syn::Path {
        leading_colon: model.leading_colon,
        segments: model
            .segments
            .into_iter()
            .collect::<Punctuated<_, Token![::]>>(),
    };

    Ok((model, column))
}

pub fn derive(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);

    match expand(input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand(input: DeriveInput) -> syn::Result<TokenStream2> {
    let ident = input.ident.clone();

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &ident,
            "#[derive(Validate)] expects a struct",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &ident,
            "#[derive(Validate)] expects named fields",
        ));
    };

    let krate = crate::model::crate_path(&input)?;

    let mut sync = Vec::new();
    let mut asynchronous = Vec::new();

    for field in &fields.named {
        let Some(name) = field.ident.as_ref() else {
            continue;
        };
        let label = name.to_string();
        let optional = is_option(&field.ty);

        for attr in &field.attrs {
            if !attr.path().is_ident("validate") {
                continue;
            }

            let Rules(rules) = attr.parse_args()?;

            for rule in rules {
                let body = expand_rule(&rule, &label, name, &krate);

                // An `Option` field is validated only when present; absence is
                // the business of a `required` rule, not of `length`.
                let guarded = if optional {
                    let inner = quote!(value);
                    let body = expand_rule_on(&rule, &label, &inner, &krate);
                    quote! {
                        if let ::std::option::Option::Some(value) = &self.#name {
                            #body
                        }
                    }
                } else {
                    body
                };

                if rule.is_async() {
                    asynchronous.push(quote! {
                        if !skip.contains(&#label) {
                            #guarded
                        }
                    });
                } else {
                    sync.push(guarded);
                }
            }
        }
    }

    let async_impl = if asynchronous.is_empty() {
        quote!()
    } else {
        quote! {
            fn validate_async<'__a>(
                &'__a self,
                skip: &'__a [&'__a str],
            ) -> #krate::__private::BoxFuture<'__a, #krate::Result<#krate::ValidationErrors>> {
                ::std::boxed::Box::pin(async move {
                    let mut errors = #krate::ValidationErrors::new();
                    #(#asynchronous)*
                    ::std::result::Result::Ok(errors)
                })
            }
        }
    };

    Ok(quote! {
        impl #krate::__private::Validate for #ident {
            fn validate_sync(&self) -> #krate::ValidationErrors {
                let mut errors = #krate::ValidationErrors::new();
                #(#sync)*
                errors
            }

            #async_impl
        }
    })
}

fn expand_rule(rule: &Rule, label: &str, name: &syn::Ident, krate: &TokenStream2) -> TokenStream2 {
    let accessor = quote!(&self.#name);
    expand_rule_on(rule, label, &accessor, krate)
}

fn expand_rule_on(
    rule: &Rule,
    label: &str,
    value: &TokenStream2,
    krate: &TokenStream2,
) -> TokenStream2 {
    match rule {
        Rule::Length {
            min,
            max,
            equal,
            message,
        } => {
            let mut checks = Vec::new();

            if let Some(min) = min {
                let text = message.clone().unwrap_or_default();
                checks.push(quote! {
                    if #krate::__private::rules::length(#value) < #min {
                        let message = if #text.is_empty() {
                            #krate::__private::rules::too_short(#min)
                        } else {
                            #text.to_owned()
                        };
                        errors.add(#label, message);
                    }
                });
            }
            if let Some(max) = max {
                let text = message.clone().unwrap_or_default();
                checks.push(quote! {
                    if #krate::__private::rules::length(#value) > #max {
                        let message = if #text.is_empty() {
                            #krate::__private::rules::too_long(#max)
                        } else {
                            #text.to_owned()
                        };
                        errors.add(#label, message);
                    }
                });
            }
            if let Some(equal) = equal {
                let text = message.clone().unwrap_or_default();
                checks.push(quote! {
                    if #krate::__private::rules::length(#value) != #equal {
                        let message = if #text.is_empty() {
                            #krate::__private::rules::wrong_length(#equal)
                        } else {
                            #text.to_owned()
                        };
                        errors.add(#label, message);
                    }
                });
            }

            quote!(#(#checks)*)
        }

        Rule::Email { message } => {
            let text = message
                .clone()
                .unwrap_or_else(|| "must be a valid email address".to_owned());
            quote! {
                if !#krate::__private::rules::is_email(#value) {
                    errors.add(#label, #text);
                }
            }
        }

        Rule::Range { min, max, message } => {
            let mut checks = Vec::new();

            if let Some(min) = min {
                let text = message
                    .clone()
                    .unwrap_or_else(|| format!("must be at least {}", quote!(#min)));
                checks.push(quote! {
                    if *#value < #min { errors.add(#label, #text); }
                });
            }
            if let Some(max) = max {
                let text = message
                    .clone()
                    .unwrap_or_else(|| format!("must be at most {}", quote!(#max)));
                checks.push(quote! {
                    if *#value > #max { errors.add(#label, #text); }
                });
            }

            quote!(#(#checks)*)
        }

        Rule::Custom { function, message } => {
            let text = message.clone().unwrap_or_else(|| "is invalid".to_owned());
            quote! {
                if !#function(#value) {
                    errors.add(#label, #text);
                }
            }
        }

        Rule::Unique {
            model,
            column,
            except,
            message,
        } => {
            let text = message
                .clone()
                .unwrap_or_else(|| "has already been taken".to_owned());

            let exclusion = match except {
                Some(field) => quote! {
                    let query = query.where_ne(<#model>::id, self.#field.clone());
                },
                None => quote!(),
            };

            quote! {
                {
                    let query = <#model as #krate::Record>::query()
                        .where_eq(<#model>::#column, ::std::clone::Clone::clone(#value));
                    #exclusion

                    if query.exists().await? {
                        errors.add(#label, #text);
                    }
                }
            }
        }

        Rule::Exists {
            model,
            column,
            message,
        } => {
            let text = message
                .clone()
                .unwrap_or_else(|| "does not exist".to_owned());
            quote! {
                {
                    let found = <#model as #krate::Record>::query()
                        .where_eq(<#model>::#column, ::std::clone::Clone::clone(#value))
                        .exists()
                        .await?;

                    if !found {
                        errors.add(#label, #text);
                    }
                }
            }
        }
    }
}

fn is_option(ty: &Type) -> bool {
    matches!(ty, Type::Path(path)
        if path.path.segments.last().is_some_and(|segment| segment.ident == "Option"))
}
