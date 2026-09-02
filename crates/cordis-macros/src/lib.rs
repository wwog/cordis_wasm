use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{
    Attribute, Expr, ExprLit, FnArg, GenericArgument, ImplItem, ItemImpl, ItemStruct, ItemTrait,
    Lit, Meta, MetaNameValue, Pat, PathArguments, ReturnType, Token, TraitItem, TraitItemFn, Type,
    parse_macro_input,
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
    match kind {
        SpecKind::Service => match expand_service(&mut item, &name) {
            Ok(expanded) => expanded.into(),
            Err(error) => error.into_compile_error().into(),
        },
        SpecKind::Event => {
            let ident = &item.ident;
            let marker = format_ident!("{ident}Event");
            let visibility = &item.vis;
            let tokens = quote!(#item);
            let hash = hash_tokens(&name, &tokens);
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
                    #item

                    #[derive(Clone, Copy, Debug, Default)]
                    #visibility struct #marker;
                    impl ::cordis::EventSpec for #marker {
                        type Input = #input;
                        type Output = #output;
                        const NAME: &'static str = #name;
                        const ABI_HASH: [u8; 32] = [#(#hash),*];
                    }
                },
                _ => syn::Error::new_spanned(
                    &item.ident,
                    "event trait must declare `type Input = ...;` and `type Output = ...;`",
                )
                .into_compile_error(),
            }
        }
        .into(),
    }
}

struct ServiceMethod {
    method: syn::Ident,
    method_id: u32,
    arguments: Vec<(syn::Ident, Type)>,
    ok: Type,
    error: Type,
}

// Keeping the mutually dependent client/adapter/dispatcher template contiguous makes the
// generated API substantially easier to audit than splitting it across state-carrying helpers.
#[allow(clippy::too_many_lines)]
fn expand_service(item: &mut ItemTrait, name: &str) -> syn::Result<proc_macro2::TokenStream> {
    if !item.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &item.generics,
            "service traits cannot have generic parameters",
        ));
    }

    let methods = parse_service_methods(item, name)?;
    let hash = service_abi_hash(name, &methods);
    let ident = &item.ident;
    let visibility = &item.vis;
    let marker = format_ident!("{ident}Service");
    let client = format_ident!("{ident}Client");
    let client_backend = format_ident!("__Cordis{ident}ClientBackend");
    let native_client = format_ident!("__Cordis{ident}NativeClient");
    let native_adapter = format_ident!("__Cordis{ident}NativeClientAdapter");
    let dispatcher = format_ident!("{ident}Dispatcher");

    let client_methods = methods.iter().map(|method| {
        let method_name = &method.method;
        let method_id = method.method_id;
        let arguments = method
            .arguments
            .iter()
            .map(|(ident, ty)| quote!(#ident: #ty));
        let argument_names = method
            .arguments
            .iter()
            .map(|(ident, _)| ident)
            .collect::<Vec<_>>();
        let native_arguments = &argument_names;
        let wire_arguments = &argument_names;
        let ok = &method.ok;
        let error = &method.error;
        quote! {
            #visibility async fn #method_name(
                &self,
                #(#arguments),*
            ) -> Result<#ok, ::cordis::ServiceCallError<#error>> {
                match &self.backend {
                    #client_backend::Native(client) => client
                        .#method_name(#(#native_arguments),*)
                        .await
                        .map_err(::cordis::ServiceCallError::Service),
                    #client_backend::Dynamic(client) => {
                        let payload =
                            ::cordis::encode_service_payload(&(#(#wire_arguments,)*))?;
                        let response = client.call(#method_id, payload).await?;
                        let result: Result<#ok, #error> =
                            ::cordis::decode_service_payload(&response)?;
                        result.map_err(::cordis::ServiceCallError::Service)
                    }
                }
            }
        }
    });

    let native_client_methods = methods.iter().map(|method| {
        let method_name = &method.method;
        let arguments = method
            .arguments
            .iter()
            .map(|(ident, ty)| quote!(#ident: #ty));
        let ok = &method.ok;
        let error = &method.error;
        quote! {
            fn #method_name(
                &self,
                #(#arguments),*
            ) -> ::std::pin::Pin<Box<
                dyn ::std::future::Future<Output = Result<#ok, #error>> + Send + '_
            >>;
        }
    });

    let native_adapter_methods = methods.iter().map(|method| {
        let method_name = &method.method;
        let arguments = method
            .arguments
            .iter()
            .map(|(ident, ty)| quote!(#ident: #ty));
        let argument_names = method.arguments.iter().map(|(ident, _)| ident);
        let ok = &method.ok;
        let error = &method.error;
        quote! {
            fn #method_name(
                &self,
                #(#arguments),*
            ) -> ::std::pin::Pin<Box<
                dyn ::std::future::Future<Output = Result<#ok, #error>> + Send + '_
            >> {
                Box::pin(<T as #ident>::#method_name(
                    self.service.as_ref(),
                    #(#argument_names),*
                ))
            }
        }
    });

    let dispatch_arms = methods.iter().map(|method| {
        let method_name = &method.method;
        let method_id = method.method_id;
        let argument_names = method
            .arguments
            .iter()
            .map(|(ident, _)| ident)
            .collect::<Vec<_>>();
        let decoded_names = &argument_names;
        let called_names = &argument_names;
        let argument_types = method
            .arguments
            .iter()
            .map(|(_, ty)| ty)
            .collect::<Vec<_>>();
        quote! {
            #method_id => {
                let service = ::std::sync::Arc::clone(&self.service);
                Box::pin(async move {
                    let (#(#decoded_names,)*) : (#(#argument_types,)*) =
                        ::cordis::decode_service_payload(&payload)?;
                    let result = service.#method_name(#(#called_names),*).await;
                    ::cordis::encode_service_payload(&result)
                })
            }
        }
    });

    Ok(quote! {
        #item

        #[derive(Clone, Copy, Debug, Default)]
        #visibility struct #marker;

        impl #marker {
            #visibility const NAME: &'static str = #name;
            #visibility const ABI_HASH: [u8; 32] = [#(#hash),*];
        }

        impl ::cordis::ServiceKey for #marker {
            const NAME: &'static str = #name;
            const ABI_HASH: [u8; 32] = [#(#hash),*];
        }

        #[derive(Clone)]
        #visibility struct #client {
            service: ::cordis::ServiceId,
            backend: #client_backend,
        }

        impl #client {
            #visibility fn new(
                dispatcher: ::std::sync::Arc<dyn ::cordis::ServiceDispatcher>,
            ) -> Result<Self, ::cordis::CordisError> {
                let client = ::cordis::ServiceClient::new::<#marker>(dispatcher)?;
                Ok(Self {
                    service: client.service_id().clone(),
                    backend: #client_backend::Dynamic(client),
                })
            }

            #visibility fn from_native<T>(service: ::std::sync::Arc<T>) -> Self
            where
                T: #ident + Send + Sync + 'static,
            {
                Self {
                    service: <#marker as ::cordis::ServiceSpec>::service_id(),
                    backend: #client_backend::Native(::std::sync::Arc::new(
                        #native_adapter { service }
                    )),
                }
            }

            #visibility fn service_id(&self) -> &::cordis::ServiceId {
                &self.service
            }

            #(#client_methods)*
        }

        impl ::std::fmt::Debug for #client {
            fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                formatter
                    .debug_struct(stringify!(#client))
                    .field("service", &self.service)
                    .finish_non_exhaustive()
            }
        }

        #[derive(Clone)]
        enum #client_backend {
            Native(::std::sync::Arc<dyn #native_client>),
            Dynamic(::cordis::ServiceClient),
        }

        trait #native_client: Send + Sync + 'static {
            #(#native_client_methods)*
        }

        struct #native_adapter<T> {
            service: ::std::sync::Arc<T>,
        }

        impl<T> #native_client for #native_adapter<T>
        where
            T: #ident + Send + Sync + 'static,
        {
            #(#native_adapter_methods)*
        }

        #[derive(Clone)]
        #visibility struct #dispatcher<T> {
            service: ::std::sync::Arc<T>,
        }

        impl<T> #dispatcher<T> {
            #visibility fn new(service: ::std::sync::Arc<T>) -> Self {
                Self { service }
            }
        }

        impl<T> ::std::fmt::Debug for #dispatcher<T> {
            fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                formatter
                    .debug_struct(stringify!(#dispatcher))
                    .finish_non_exhaustive()
            }
        }

        impl<T> ::cordis::ServiceDispatcher for #dispatcher<T>
        where
            T: #ident + Send + Sync + 'static,
        {
            fn service_id(&self) -> ::cordis::ServiceId {
                <#marker as ::cordis::ServiceSpec>::service_id()
            }

            fn dispatch(
                &self,
                method_id: u32,
                payload: ::std::vec::Vec<u8>,
            ) -> ::cordis::ServiceFuture {
                match method_id {
                    #(#dispatch_arms,)*
                    _ => {
                        let service = <Self as ::cordis::ServiceDispatcher>::service_id(self);
                        Box::pin(async move {
                            Err(::cordis::CordisError::UnknownServiceMethod {
                                service,
                                method_id,
                            })
                        })
                    }
                }
            }
        }
    })
}

fn parse_service_methods(
    item: &mut ItemTrait,
    service_name: &str,
) -> syn::Result<Vec<ServiceMethod>> {
    let mut methods = Vec::new();
    let mut method_ids = std::collections::BTreeMap::new();
    for trait_item in &mut item.items {
        let TraitItem::Fn(method) = trait_item else {
            return Err(syn::Error::new_spanned(
                trait_item,
                "service traits may only contain methods",
            ));
        };
        let parsed = parse_service_method(method, service_name)?;
        if let Some(previous) = method_ids.insert(parsed.method_id, parsed.method.clone()) {
            return Err(syn::Error::new_spanned(
                &method.sig.ident,
                format!(
                    "service method id collision between `{previous}` and `{}`",
                    method.sig.ident
                ),
            ));
        }
        methods.push(parsed);
    }
    if methods.is_empty() {
        return Err(syn::Error::new_spanned(
            &item.ident,
            "service trait must declare at least one method",
        ));
    }
    Ok(methods)
}

fn parse_service_method(
    method: &mut TraitItemFn,
    service_name: &str,
) -> syn::Result<ServiceMethod> {
    if method.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &method.sig.ident,
            "service methods must be async",
        ));
    }
    if method.default.is_some() {
        return Err(syn::Error::new_spanned(
            &method.sig.ident,
            "service methods cannot have a default implementation",
        ));
    }
    if !method.sig.generics.params.is_empty() || method.sig.generics.where_clause.is_some() {
        return Err(syn::Error::new_spanned(
            &method.sig.generics,
            "service methods cannot be generic",
        ));
    }

    let mut inputs = method.sig.inputs.iter();
    let Some(FnArg::Receiver(receiver)) = inputs.next() else {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "service methods must take `&self`",
        ));
    };
    if receiver.reference.is_none() || receiver.mutability.is_some() {
        return Err(syn::Error::new_spanned(
            receiver,
            "service methods must take `&self`",
        ));
    }

    let mut arguments = Vec::new();
    for input in inputs {
        let FnArg::Typed(argument) = input else {
            return Err(syn::Error::new_spanned(input, "unexpected receiver"));
        };
        let Pat::Ident(pattern) = argument.pat.as_ref() else {
            return Err(syn::Error::new_spanned(
                &argument.pat,
                "service argument must use a simple identifier",
            ));
        };
        if pattern.by_ref.is_some() || pattern.mutability.is_some() || pattern.subpat.is_some() {
            return Err(syn::Error::new_spanned(
                pattern,
                "service argument must use an immutable owned identifier",
            ));
        }
        if matches!(argument.ty.as_ref(), Type::Reference(_)) {
            return Err(syn::Error::new_spanned(
                &argument.ty,
                "service arguments must be owned so native and Wasm dispatch use the same ABI",
            ));
        }
        arguments.push((pattern.ident.clone(), (*argument.ty).clone()));
    }

    let (ok, error, declared_output) = result_output(&method.sig.output)?;
    let canonical = canonical_service_method(&method.sig.ident, &arguments, &ok, &error);
    let digest = hash_text(&format!("{service_name}\n{canonical}"));
    let method_id = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]);

    method.sig.asyncness = None;
    method.sig.output = syn::parse_quote!(
        -> impl ::std::future::Future<Output = #declared_output> + Send
    );

    Ok(ServiceMethod {
        method: method.sig.ident.clone(),
        method_id,
        arguments,
        ok,
        error,
    })
}

fn result_output(output: &ReturnType) -> syn::Result<(Type, Type, Type)> {
    let ReturnType::Type(_, declared) = output else {
        return Err(syn::Error::new_spanned(
            output,
            "service method must return `Result<T, E>`",
        ));
    };
    let Type::Path(path) = declared.as_ref() else {
        return Err(syn::Error::new_spanned(
            declared,
            "service method must return `Result<T, E>`",
        ));
    };
    let Some(segment) = path.path.segments.last() else {
        return Err(syn::Error::new_spanned(
            declared,
            "service method must return `Result<T, E>`",
        ));
    };
    if segment.ident != "Result" {
        return Err(syn::Error::new_spanned(
            declared,
            "service method must return `Result<T, E>`",
        ));
    }
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            declared,
            "service method must return `Result<T, E>`",
        ));
    };
    let types = arguments
        .args
        .iter()
        .filter_map(|argument| match argument {
            GenericArgument::Type(ty) => Some(ty.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if types.len() != 2 || arguments.args.len() != 2 {
        return Err(syn::Error::new_spanned(
            declared,
            "service method must return `Result<T, E>`",
        ));
    }
    Ok((types[0].clone(), types[1].clone(), (**declared).clone()))
}

fn expand_component(
    args: TokenStream,
    item: &mut ItemStruct,
) -> syn::Result<proc_macro2::TokenStream> {
    let metas = parse_metas(args)?;
    let name = meta_string(&metas, "name")?.unwrap_or_else(|| item.ident.to_string());
    let config = meta_type(&metas, "config")?.unwrap_or_else(|| syn::parse_quote!(()));
    let injected_services = take_injects(&mut item.attrs)?;
    let injects = injected_services
        .iter()
        .cloned()
        .map(service_marker)
        .collect::<syn::Result<Vec<_>>>()?;
    let clients = injected_services
        .iter()
        .cloned()
        .map(service_client)
        .collect::<syn::Result<Vec<_>>>()?;
    let fields = injected_services
        .iter()
        .map(service_field)
        .collect::<syn::Result<Vec<_>>>()?;
    let mut unique_fields = std::collections::BTreeSet::new();
    for field in &fields {
        if !unique_fields.insert(field.to_string()) {
            return Err(syn::Error::new_spanned(
                field,
                "injected services must have distinct client field names",
            ));
        }
    }
    let ident = &item.ident;
    let visibility = &item.vis;
    let deps = format_ident!("{ident}Dependencies");
    let derives = if fields.is_empty() {
        quote!(#[derive(Clone, Debug, Default)])
    } else {
        quote!(#[derive(Clone, Debug)])
    };
    Ok(quote! {
        #item

        #derives
        #visibility struct #deps {
            #(#visibility #fields: #clients),*
        }

        impl #deps {
            #visibility fn new(#(#fields: #clients),*) -> Self {
                Self { #(#fields),* }
            }
        }

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
    hash_text(&format!("{name}\n{tokens}"))
}

fn hash_text(value: &str) -> [u8; 32] {
    *blake3::hash(value.as_bytes()).as_bytes()
}

fn service_abi_hash(name: &str, methods: &[ServiceMethod]) -> [u8; 32] {
    let mut canonical = String::from(name);
    let mut signatures = methods
        .iter()
        .map(|method| {
            canonical_service_method(&method.method, &method.arguments, &method.ok, &method.error)
        })
        .collect::<Vec<_>>();
    signatures.sort_unstable();
    for signature in signatures {
        canonical.push('\n');
        canonical.push_str(&signature);
    }
    hash_text(&canonical)
}

fn canonical_service_method(
    method: &syn::Ident,
    arguments: &[(syn::Ident, Type)],
    ok: &Type,
    error: &Type,
) -> String {
    let arguments = arguments
        .iter()
        .map(|(_, ty)| quote!(#ty).to_string())
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{method}({arguments})->Result<{},{}>",
        quote!(#ok),
        quote!(#error)
    )
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
    replace_service_suffix(&mut ty, "Service")?;
    Ok(ty)
}

fn service_client(mut ty: Type) -> syn::Result<Type> {
    replace_service_suffix(&mut ty, "Client")?;
    Ok(ty)
}

fn replace_service_suffix(ty: &mut Type, suffix: &str) -> syn::Result<()> {
    let Type::Path(path) = &mut *ty else {
        return Err(syn::Error::new_spanned(
            &*ty,
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
    if !matches!(segment.arguments, PathArguments::None) {
        return Err(syn::Error::new_spanned(
            segment,
            "injected service path cannot have generic arguments",
        ));
    }
    segment.ident = format_ident!("{}{suffix}", segment.ident);
    Ok(())
}

fn service_field(ty: &Type) -> syn::Result<syn::Ident> {
    let Type::Path(path) = ty else {
        return Err(syn::Error::new_spanned(
            ty,
            "injected service must be a trait path",
        ));
    };
    let segment = path.path.segments.last().ok_or_else(|| {
        syn::Error::new_spanned(&path.path, "injected service path cannot be empty")
    })?;
    let snake = to_snake_case(&segment.ident.to_string());
    syn::parse_str(&snake).map_err(|_| {
        syn::Error::new_spanned(
            &segment.ident,
            "injected service name cannot be converted to a Rust field name",
        )
    })
}

fn to_snake_case(value: &str) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    let mut result = String::with_capacity(value.len());
    for (index, character) in chars.iter().copied().enumerate() {
        let previous_is_lower_or_digit =
            index > 0 && (chars[index - 1].is_lowercase() || chars[index - 1].is_ascii_digit());
        let next_is_lower = chars.get(index + 1).is_some_and(|next| next.is_lowercase());
        if character.is_uppercase() && index > 0 && (previous_is_lower_or_digit || next_is_lower) {
            result.push('_');
        }
        result.extend(character.to_lowercase());
    }
    result
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
