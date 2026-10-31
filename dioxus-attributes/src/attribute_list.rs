//! Parsing and code generation for the `attributes!` macro

use dioxus_rsx::{Attribute, AttributeName, ElementName};
use proc_macro2::TokenStream as TokenStream2;
use quote::{quote, quote_spanned, ToTokens};
use syn::{
    braced,
    parse::{Parse, ParseStream},
    Expr, Ident, Token,
};

/// A parsed list of attributes from the `attributes!` macro
pub struct AttributeList {
    /// The element name for namespace/volatility lookup
    pub element_name: ElementName,
    /// Regular attributes (not spreads)
    pub attributes: Vec<Attribute>,
    /// Spread expressions that provide existing collections of attributes
    pub spreads: Vec<SpreadExpr>,
}

/// A spread expression like `..existing_attrs`
pub struct SpreadExpr {
    #[allow(dead_code)]
    pub dots: Token![..],
    pub expr: Expr,
}

impl Parse for AttributeList {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Parse element name first (e.g., `div`, `input`, `button`)
        let element_name: ElementName = if input.peek(Ident) {
            ElementName::Ident(input.parse()?)
        } else {
            return Err(input.error("expected element name (e.g., div, input, button)"));
        };

        // Parse the braced content
        let content;
        braced!(content in input);

        let mut attributes = Vec::new();
        let mut spreads = Vec::new();

        while !content.is_empty() {
            // Check for spread attribute first
            if content.peek(Token![..]) {
                let dots = content.parse::<Token![..]>()?;
                let expr = content.parse::<Expr>()?;
                // Consume optional trailing comma
                let _ = content.parse::<Token![,]>();
                spreads.push(SpreadExpr { dots, expr });
                continue;
            }

            // Parse regular attribute using dioxus_rsx's parser
            let mut attr: Attribute = content.parse()?;
            // Set the element name for proper namespace/volatility lookup
            attr.el_name = Some(element_name.clone());
            attributes.push(attr);
        }

        Ok(AttributeList {
            element_name,
            attributes,
            spreads,
        })
    }
}

impl ToTokens for AttributeList {
    fn to_tokens(&self, tokens: &mut TokenStream2) {
        // PATCH(rust-ui, Dioxus 0.8): dioxus-rsx 0.8 no longer resolves a built-in attribute's
        // HTML name (`aria_label` -> `aria-label`, `view_box` -> `viewBox`) when it renders it
        // on its own; elements get the name from typed builder methods instead. So built-in,
        // non-event attributes go through the same methods here, on a small list type that opts
        // into the element's and the global attribute traits (as a `#[props(extends = ...)]`
        // builder does). Events, string names and spreads keep the rsx rendering.
        let el_ident = match &self.element_name {
            ElementName::Ident(ident) => ident.clone(),
            ElementName::Custom(_) => unreachable!("attributes! only parses ident element names"),
        };
        let el_raw = el_ident.to_string();
        let el_raw = el_raw.strip_prefix("r#").unwrap_or(&el_raw);
        let el_marker = Ident::new(
            &format!("{}SpreadTarget", upper_camel(el_raw)),
            el_ident.span(),
        );

        let attr_tokens: Vec<TokenStream2> = self
            .attributes
            .iter()
            .map(|attr| match &attr.name {
                AttributeName::BuiltIn(method)
                    if !method.to_string().starts_with("on") && !attr.name.is_likely_key() =>
                {
                    let value = &attr.value;
                    quote_spanned! { method.span() =>
                        __attrs = __attrs.#method(#value);
                    }
                }
                _ => {
                    let rendered = attr.rendered_as_dynamic_attr();
                    // dioxus-rsx 0.8 renders an attribute as `Box<[Attribute; 1]>` or
                    // `Box<[Attribute]>`; only the second is an iterator, so coerce to it first.
                    quote! {
                        __attrs.0.extend({
                            let __boxed: ::std::boxed::Box<[_]> = #rendered;
                            __boxed
                        });
                    }
                }
            })
            .collect();

        let spread_tokens: Vec<TokenStream2> = self
            .spreads
            .iter()
            .map(|spread| {
                let expr = &spread.expr;
                quote! {
                    __attrs.0.extend(#expr);
                }
            })
            .collect();

        let output = quote! {
            {
                #[allow(unused_imports)]
                use ::dioxus::html::extensions::*;
                struct __DioxusAttributeList(::std::vec::Vec<dioxus_core::Attribute>);
                impl dioxus_core::HasAttributes for __DioxusAttributeList {
                    fn push_attribute<__L>(
                        mut self,
                        name: &'static str,
                        ns: ::std::option::Option<&'static str>,
                        attr: impl dioxus_core::IntoAttributeValue<__L>,
                        volatile: bool,
                    ) -> Self {
                        self.0.push(dioxus_core::Attribute::new(
                            name,
                            dioxus_core::IntoAttributeValue::<__L>::into_value(attr),
                            ns,
                            volatile,
                        ));
                        self
                    }
                }
                impl ::dioxus::html::extensions::GlobalAttributesSpreadTarget for __DioxusAttributeList {}
                impl ::dioxus::html::extensions::#el_marker for __DioxusAttributeList {}
                #[allow(unused_mut)]
                let mut __attrs = __DioxusAttributeList(::std::vec::Vec::new());
                #( #attr_tokens )*
                #( #spread_tokens )*
                __attrs.0
            }
        };

        tokens.extend(output);
    }
}

fn upper_camel(name: &str) -> String {
    name.split(['_', '-'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect()
}
