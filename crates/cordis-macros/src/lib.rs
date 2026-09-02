use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{
    Attribute, Expr, ExprLit, ImplItem, ItemImpl, ItemStruct, ItemTrait, Lit, Meta, MetaNameValue,
    Token, Type, parse_macro_input,
};

#[proc_macro_attribute]
pub fn service(args: TokenStream, input: TokenStream) -> TokenStream {
    expand_spec(args, input, SpecKind::Service)
}

#[proc_macro_attribute]
pub fn event(args: TokenStream, input: TokenStream) -> TokenStream {
    expand_spec(args, input, SpecKind::Event)
}

#[proc_macro_attribute]
pub fn inject(args: TokenStream, input: TokenStream) -> TokenStream {
    let mut item = parse_macro_input!(input as ItemStruct);
    let args = proc_macro2::TokenStream::from(args);
    item.attrs.push(syn::parse_quote!(#[cordis_inject(#args)]));
    quote!(#item).into()
}

#[proc_macro_attribute]
pub fn component(args: TokenStream, input: TokenStream) -> TokenStream {
    let mut item = parse_macro_input!(input as ItemStruct);
    match expand_component(args, &mut item) {
        Ok(expanded) => expanded.into(),
        Err(error) => error.into_compile_error().into(),
    }
}

#[proc_macro_attribute]
pub fn component_impl(_args: TokenStream, input: TokenStream) -> TokenStream {
    let mut item = parse_macro_input!(input as ItemImpl);
    match expand_component_impl(&mut item) {
        Ok(expanded) => expanded.into(),
        Err(error) => error.into_compile_error().into(),
    }
}

#[proc_macro_attribute]
pub fn apply(_args: TokenStream, input: TokenStream) -> TokenStream {
    input
}

#[derive(Clone, Copy)]
enum SpecKind {
    Service,
    Event,
}

fn expand_spec(args: TokenStream, input: TokenStream, kind: SpecKind) -> TokenStream {
    let mut item = parse_macro_input!(input as ItemTrait);
    let name = match named_string(args, "name") {
        Ok(Some(name)) => name,
        Ok(None) => item.ident.to_string(),
        Err(error) => return error.into_compile_error().into(),
    };
    let ident = &item.ident;
    let marker = match kind {
        SpecKind::Service => format_ident!("{ident}Service"),
        SpecKind::Event => format_ident!("{ident}Event"),
    };
    let tokens = quote!(#item);
    let hash = hash_tokens(&name, &tokens);
    let implementation = match kind {
        SpecKind::Service => quote! {
            #[derive(Clone, Copy, Debug, Default)]
            pub struct #marker;
            impl ::cordis::ServiceSpec for #marker {
                const NAME: &'static str = #name;
                const ABI_HASH: [u8; 32] = [#(#hash),*];
            }
        },
        SpecKind::Event => {
            let input = associated_type(&item, "Input");
            let output = associated_type(&item, "Output");
            for trait_item in &mut item.items {
                if let syn::TraitItem::Type(ty) = trait_item
                    && (ty.ident == "Input" || ty.ident == "Output")
                {
                    ty.default = None;
                }
            }
            match (input, output) {
                (Some(input), Some(output)) => quote! {
                    #[derive(Clone, Copy, Debug, Default)]
                    pub struct #marker;
                    impl ::cordis::EventSpec for #marker {
                        type Input = #input;
                        type Output = #output;
                        const NAME: &'static str = #name;
                        const ABI_HASH: [u8; 32] = [#(#hash),*];
                    }
                },
                _ => {
                    return syn::Error::new_spanned(
                        &item.ident,
                        "event trait must declare `type Input = ...;` and `type Output = ...;`",
                    )
                    .into_compile_error()
                    .into();
                }
            }
        }
    };
    quote!(#item #implementation).into()
}

fn expand_component(
    args: TokenStream,
    item: &mut ItemStruct,
) -> syn::Result<proc_macro2::TokenStream> {
    let metas = parse_metas(args)?;
    let name = meta_string(&metas, "name")?.unwrap_or_else(|| item.ident.to_string());
    let config = meta_type(&metas, "config")?.unwrap_or_else(|| syn::parse_quote!(()));
    let injects = take_injects(&mut item.attrs)?;
    let injects = injects
        .into_iter()
        .map(service_marker)
        .collect::<syn::Result<Vec<_>>>()?;
    let ident = &item.ident;
    let deps = format_ident!("{ident}Dependencies");
    Ok(quote! {
        #item

        #[derive(Clone, Copy, Debug, Default)]
        pub struct #deps;

        impl ::cordis::DependencySet for #deps {
            fn injects() -> ::std::vec::Vec<::cordis::InjectSpec> {
                vec![
                    #(::cordis::InjectSpec::required(
                        <#injects as ::cordis::ServiceSpec>::service_id()
                    )),*
                ]
            }
        }

        impl ::cordis::ComponentDefinition for #ident {
            type Config = #config;
            type Deps = #deps;

            fn descriptor() -> &'static ::cordis::ComponentDescriptor {
                static DESCRIPTOR: ::std::sync::OnceLock<::cordis::ComponentDescriptor> =
                    ::std::sync::OnceLock::new();
                DESCRIPTOR.get_or_init(|| ::cordis::ComponentDescriptor {
                    name: #name,
                    injects: <#deps as ::cordis::DependencySet>::injects(),
                    config_schema: ::cordis::config_schema::<#config>,
                })
            }
        }
    })
}

fn expand_component_impl(item: &mut ItemImpl) -> syn::Result<proc_macro2::TokenStream> {
    if item.trait_.is_some() {
        return Err(syn::Error::new_spanned(
            &item.self_ty,
            "component_impl must be used on an inherent impl",
        ));
    }
    let mut apply_method = None;
    for impl_item in &mut item.items {
        let ImplItem::Fn(method) = impl_item else {
            continue;
        };
        if take_marker(&mut method.attrs, "apply") {
            if apply_method.is_some() {
                return Err(syn::Error::new_spanned(
                    &method.sig.ident,
                    "component_impl requires exactly one #[cordis::apply] method",
                ));
            }
            if method.sig.asyncness.is_none() {
                return Err(syn::Error::new_spanned(
                    &method.sig.ident,
                    "#[cordis::apply] method must be async",
                ));
            }
            let receiver = method.sig.receiver().ok_or_else(|| {
                syn::Error::new_spanned(&method.sig, "#[cordis::apply] method must take self")
            })?;
            if receiver.reference.is_some() || method.sig.inputs.len() != 3 {
                return Err(syn::Error::new_spanned(
                    &method.sig,
                    "#[cordis::apply] signature must be `async fn name(self, context, config) -> Result<(), CordisError>`",
                ));
            }
            apply_method = Some(method.sig.ident.clone());
        }
    }
    let method = apply_method.ok_or_else(|| {
        syn::Error::new_spanned(
            &item.self_ty,
            "component_impl requires one #[cordis::apply] method",
        )
    })?;
    let self_ty = &item.self_ty;
    Ok(quote! {
        #item

        impl ::cordis::Component for #self_ty {
            fn apply(
                self,
                context: ::cordis::ComponentContext<Self::Deps>,
                config: Self::Config,
            ) -> impl ::std::future::Future<
                Output = Result<::cordis::ComponentEffects, ::cordis::CordisError>
            > + Send {
                let effects = context.effect_set();
                async move {
                    Self::#method(self, context, config).await?;
                    Ok(::cordis::ComponentEffects::new(effects))
                }
            }
        }
    })
}

fn associated_type(item: &ItemTrait, name: &str) -> Option<Type> {
    item.items.iter().find_map(|item| {
        let syn::TraitItem::Type(ty) = item else {
            return None;
        };
        (ty.ident == name).then(|| ty.default.as_ref().map(|(_, ty)| ty.clone()))?
    })
}

fn hash_tokens(name: &str, tokens: &proc_macro2::TokenStream) -> [u8; 32] {
    *blake3::hash(format!("{name}\n{tokens}").as_bytes()).as_bytes()
}

fn named_string(args: TokenStream, key: &str) -> syn::Result<Option<String>> {
    meta_string(&parse_metas(args)?, key)
}

fn parse_metas(args: TokenStream) -> syn::Result<Vec<Meta>> {
    Punctuated::<Meta, Token![,]>::parse_terminated
        .parse(args)
        .map(|values| values.into_iter().collect())
}

fn meta_string(metas: &[Meta], key: &str) -> syn::Result<Option<String>> {
    let Some(meta) = metas.iter().find(|meta| meta.path().is_ident(key)) else {
        return Ok(None);
    };
    let Meta::NameValue(MetaNameValue {
        value: Expr::Lit(ExprLit {
            lit: Lit::Str(value),
            ..
        }),
        ..
    }) = meta
    else {
        return Err(syn::Error::new_spanned(
            meta,
            format!("`{key}` must be a string"),
        ));
    };
    Ok(Some(value.value()))
}

fn meta_type(metas: &[Meta], key: &str) -> syn::Result<Option<Type>> {
    let Some(meta) = metas.iter().find(|meta| meta.path().is_ident(key)) else {
        return Ok(None);
    };
    let Meta::NameValue(value) = meta else {
        return Err(syn::Error::new_spanned(
            meta,
            format!("`{key}` must name a type"),
        ));
    };
    let value = &value.value;
    syn::parse2(quote!(#value)).map(Some)
}

fn service_marker(mut ty: Type) -> syn::Result<Type> {
    let Type::Path(path) = &mut ty else {
        return Err(syn::Error::new_spanned(
            ty,
            "injected service must be a trait path",
        ));
    };
    if path.path.segments.is_empty() {
        return Err(syn::Error::new_spanned(
            &path.path,
            "injected service path cannot be empty",
        ));
    }
    let segment = path
        .path
        .segments
        .last_mut()
        .expect("path was checked as non-empty");
    segment.ident = format_ident!("{}Service", segment.ident);
    Ok(ty)
}

fn take_injects(attrs: &mut Vec<Attribute>) -> syn::Result<Vec<Type>> {
    let mut injects = Vec::new();
    let mut retained = Vec::new();
    for attr in std::mem::take(attrs) {
        if attr
            .path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "inject" || segment.ident == "cordis_inject")
        {
            injects.extend(attr.parse_args_with(Punctuated::<Type, Token![,]>::parse_terminated)?);
        } else {
            retained.push(attr);
        }
    }
    *attrs = retained;
    Ok(injects)
}

fn take_marker(attrs: &mut Vec<Attribute>, name: &str) -> bool {
    let found = attrs.iter().any(|attr| {
        attr.path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == name)
    });
    attrs.retain(|attr| {
        attr.path()
            .segments
            .last()
            .is_none_or(|segment| segment.ident != name)
    });
    found
}
