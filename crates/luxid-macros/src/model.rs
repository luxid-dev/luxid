//! `#[derive(Model)]` — turns a SeaORM entity model into a Luxid model.
//!
//! Emits three things:
//!
//! * `impl Record`, so `User::find`, `find_or_fail`, `query` and friends exist.
//! * A zero-sized type per column carrying that column's Rust type, exposed as
//!   an associated constant so it reads `User::team_id`.
//! * `impl ColumnRef` for the entity's own `Column` enum, keeping the untyped
//!   escape hatch available.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, Path, Type, parse_macro_input};

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
            &input.ident,
            "#[derive(Model)] expects a struct — apply it to a SeaORM entity model",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "#[derive(Model)] expects named fields",
        ));
    };

    let krate = crate_path(&input)?;
    let hooks = hook_bindings(&input)?;
    let model_name = match explicit_name(&input)? {
        Some(name) => name,
        None => derive_model_name(&input, &ident)?,
    };

    let mut columns = Vec::new();
    let mut constants = Vec::new();

    for field in &fields.named {
        // Non-column fields (the relations bag, anything `#[sea_orm(ignore)]`)
        // have no column to reference.
        if has_sea_orm_flag(&field.attrs, "ignore") {
            continue;
        }

        let Some(field_ident) = field.ident.as_ref() else {
            continue;
        };

        let variant = format_ident!("{}", pascal_case(&field_ident.to_string()));
        let holder = format_ident!("__LuxidColumn_{}_{}", ident, field_ident);

        // `Option<T>` compares against `T`; absence is expressed with
        // `where_null`, not by comparing to a null.
        let value_type = unwrap_option(&field.ty);
        let docs = format!("The `{field_ident}` column of [`{ident}`].");

        columns.push(quote! {
            #[doc = #docs]
            #[doc(hidden)]
            #[allow(non_camel_case_types)]
            #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
            pub struct #holder;

            impl #krate::ColumnRef<Entity> for #holder {
                type Value = #value_type;

                fn column(&self) -> <Entity as #krate::sea_orm::EntityTrait>::Column {
                    Column::#variant
                }
            }
        });

        constants.push(quote! {
            #[doc = #docs]
            #[allow(non_upper_case_globals)]
            pub const #field_ident: #holder = #holder;
        });
    }

    let hook_impls = hooks.iter().map(|(kind, target)| {
        let method = format_ident!("{}", kind);

        if kind.starts_with("before") {
            quote! {
                fn #method(
                    active: &mut Self::Active,
                ) -> #krate::__private::BoxFuture<'_, #krate::Result<()>> {
                    ::std::boxed::Box::pin(#target(active))
                }
            }
        } else {
            quote! {
                fn #method(model: &Self) -> #krate::__private::BoxFuture<'_, #krate::Result<()>> {
                    ::std::boxed::Box::pin(#target(model))
                }
            }
        }
    });

    Ok(quote! {
        impl #krate::__private::Hooks for #ident {
            type Active = ActiveModel;

            #(#hook_impls)*
        }

        impl #krate::Record for #ident {
            type Entity = Entity;
            const MODEL: &'static str = #model_name;
        }

        /// Untyped escape hatch: the entity's own `Column` enum accepts any
        /// value, exactly as SeaORM does.
        impl #krate::ColumnRef<Entity> for Column {
            type Value = #krate::sea_orm::Value;

            fn column(&self) -> Column {
                *self
            }
        }

        #(#columns)*

        impl #ident {
            #(#constants)*
        }
    })
}

/// `#[luxid(before_create = path, after_save = path, ..)]`.
///
/// Hooks are named here rather than marked on their functions so the derive —
/// which every model has — can generate the `Hooks` impl. See the module docs
/// in `luxid-orm::hooks` for why that matters.
fn hook_bindings(input: &DeriveInput) -> syn::Result<Vec<(String, Path)>> {
    const KINDS: [&str; 6] = [
        "before_save",
        "before_create",
        "before_update",
        "after_create",
        "after_update",
        "after_save",
    ];

    let mut bindings: Vec<(String, Path)> = Vec::new();

    for attr in &input.attrs {
        if !attr.path().is_ident("luxid") {
            continue;
        }

        let mut error = None;

        attr.parse_nested_meta(|meta| {
            let Some(name) = meta.path.get_ident().map(ToString::to_string) else {
                let _ = meta.value().and_then(|value| value.parse::<syn::Expr>());
                return Ok(());
            };

            if !KINDS.contains(&name.as_str()) {
                let _ = meta.value().and_then(|value| value.parse::<syn::Expr>());
                return Ok(());
            }

            let target: Path = meta.value()?.parse()?;

            if bindings.iter().any(|(kind, _)| kind == &name) {
                error = Some(syn::Error::new_spanned(
                    &target,
                    format!("`{name}` is declared more than once"),
                ));
            }

            bindings.push((name, target));
            Ok(())
        })?;

        if let Some(error) = error {
            return Err(error);
        }
    }

    Ok(bindings)
}

/// `#[luxid(crate = ...)]`, defaulting to the `luxid` facade.
pub(crate) fn crate_path(input: &DeriveInput) -> syn::Result<TokenStream2> {
    for attr in &input.attrs {
        if !attr.path().is_ident("luxid") {
            continue;
        }

        let mut found = None;
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("crate") {
                let value: syn::Path = meta.value()?.parse()?;
                found = Some(quote!(#value));
                return Ok(());
            }

            // Consume any other key's value, so a sibling like
            // `name = "Person"` does not abort the walk.
            let _ = meta.value().and_then(|value| value.parse::<syn::Expr>());
            Ok(())
        })?;

        if let Some(path) = found {
            return Ok(path);
        }
    }

    Ok(quote!(::luxid))
}

/// `#[luxid(name = "User")]`.
fn explicit_name(input: &DeriveInput) -> syn::Result<Option<String>> {
    for attr in &input.attrs {
        if !attr.path().is_ident("luxid") {
            continue;
        }

        let mut found = None;
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                let value: syn::LitStr = meta.value()?.parse()?;
                found = Some(value.value());
                return Ok(());
            }

            let _ = meta.value().and_then(|value| value.parse::<syn::Expr>());
            Ok(())
        })?;

        if found.is_some() {
            return Ok(found);
        }
    }

    Ok(None)
}

/// Fall back to the table name, singularized. The rules are deliberately
/// simple; anything irregular should use `#[luxid(name = "...")]`.
fn derive_model_name(input: &DeriveInput, ident: &syn::Ident) -> syn::Result<String> {
    let Some(table) = sea_orm_table_name(&input.attrs)? else {
        // No table name to work from: use the struct's own name, which for a
        // SeaORM entity is `Model` and deserves an explicit override.
        return Ok(ident.to_string());
    };

    Ok(pascal_case(&singularize(&table)))
}

fn sea_orm_table_name(attrs: &[syn::Attribute]) -> syn::Result<Option<String>> {
    for attr in attrs {
        if !attr.path().is_ident("sea_orm") {
            continue;
        }

        let mut found = None;
        // Other `sea_orm` keys are none of our business; ignore parse issues
        // rather than rejecting attributes SeaORM understands and we do not.
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("table_name")
                && let Ok(value) = meta.value()
                && let Ok(name) = value.parse::<syn::LitStr>()
            {
                found = Some(name.value());
            }
            Ok(())
        });

        if found.is_some() {
            return Ok(found);
        }
    }

    Ok(None)
}

fn has_sea_orm_flag(attrs: &[syn::Attribute], flag: &str) -> bool {
    for attr in attrs {
        if !attr.path().is_ident("sea_orm") {
            continue;
        }

        let mut present = false;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident(flag) {
                present = true;
            }
            // Consume a value if there is one, so keys like `column_type = ".."`
            // do not abort the walk before reaching our flag.
            let _ = meta.value().and_then(|value| value.parse::<syn::Expr>());
            Ok(())
        });

        if present {
            return true;
        }
    }

    false
}

fn unwrap_option(ty: &Type) -> TokenStream2 {
    if let Type::Path(path) = ty
        && let Some(segment) = path.path.segments.last()
        && segment.ident == "Option"
        && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
        && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
    {
        return quote!(#inner);
    }

    quote!(#ty)
}

fn pascal_case(input: &str) -> String {
    input
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

fn singularize(table: &str) -> String {
    let (head, last) = match table.rsplit_once('_') {
        Some((head, last)) => (Some(head), last),
        None => (None, table),
    };

    let singular = if let Some(stem) = last.strip_suffix("ies") {
        format!("{stem}y")
    } else if last.ends_with("sses") || last.ends_with("shes") || last.ends_with("ches") {
        last.trim_end_matches("es").to_owned()
    } else if last.ends_with("ss") {
        last.to_owned()
    } else if let Some(stem) = last.strip_suffix('s') {
        stem.to_owned()
    } else {
        last.to_owned()
    };

    match head {
        Some(head) => format!("{head}_{singular}"),
        None => singular,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn singularizes_common_plurals() {
        assert_eq!(singularize("posts"), "post");
        assert_eq!(singularize("users"), "user");
        assert_eq!(singularize("categories"), "category");
        assert_eq!(singularize("addresses"), "address");
        assert_eq!(singularize("branches"), "branch");
        assert_eq!(singularize("people"), "people");
        assert_eq!(singularize("user_profiles"), "user_profile");
        assert_eq!(singularize("course_enrollments"), "course_enrollment");
    }

    #[test]
    fn pascal_cases_snake_case() {
        assert_eq!(pascal_case("team_id"), "TeamId");
        assert_eq!(pascal_case("id"), "Id");
        assert_eq!(pascal_case("created_at"), "CreatedAt");
        assert_eq!(pascal_case("user_profile"), "UserProfile");
    }
}
