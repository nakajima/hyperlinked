use proc_macro::TokenStream;
use quote::quote;
use syn::{
    FieldsNamed, Ident, ItemFn, Result, Token, Visibility, braced,
    parse::{Parse, ParseStream},
    parse_macro_input,
};

struct ParamsAttr {
    visibility: Visibility,
    name: Ident,
    fields: FieldsNamed,
}

impl Parse for ParamsAttr {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let visibility = if input.peek(Token![pub]) {
            input.parse()?
        } else {
            Visibility::Inherited
        };
        let name = input.parse()?;
        let content;
        braced!(content in input);
        let fields = FieldsNamed {
            brace_token: Default::default(),
            named: content.parse_terminated(syn::Field::parse_named, Token![,])?,
        };

        Ok(Self {
            visibility,
            name,
            fields,
        })
    }
}

#[proc_macro_attribute]
pub fn params(attr: TokenStream, item: TokenStream) -> TokenStream {
    let params = parse_macro_input!(attr as ParamsAttr);
    let function = parse_macro_input!(item as ItemFn);

    expand_params(params, function)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_params(params: ParamsAttr, function: ItemFn) -> Result<proc_macro2::TokenStream> {
    let ParamsAttr {
        visibility,
        name,
        fields,
    } = params;

    let mut generated_fields = Vec::new();
    for field in fields.named.iter() {
        let attrs = &field.attrs;
        let field_vis = &field.vis;
        let ident = field
            .ident
            .as_ref()
            .ok_or_else(|| syn::Error::new_spanned(field, "params fields must be named"))?;
        let ty = &field.ty;
        generated_fields.push(quote! {
            #(#attrs)*
            #field_vis #ident: #ty
        });
    }

    Ok(quote! {
        #[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
        #visibility struct #name {
            #(#generated_fields,)*
        }

        impl<S> axum::extract::FromRequest<S> for #name
        where
            S: Send + Sync,
        {
            type Rejection = crate::app::controllers::hyperlinks_controller::params::ParamsRejection;

            async fn from_request(
                request: axum::extract::Request,
                state: &S,
            ) -> Result<Self, Self::Rejection> {
                crate::app::controllers::hyperlinks_controller::params::extract(request, state)
                    .await
            }
        }

        #function
    })
}
