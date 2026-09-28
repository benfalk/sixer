//! `#[sixer::query]` and `#[sixer::command]` turn an inherent `run` into the
//! sealed trait impl.

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    Error, GenericArgument, ImplItem, ImplItemFn, ItemImpl, PathArguments, Result, ReturnType,
    Signature, Type, TypeParamBound, Visibility,
};

pub enum Kind {
    Query,
    Command,
}

impl Kind {
    fn label(&self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Command => "command",
        }
    }

    fn env(&self) -> &'static str {
        match self {
            Self::Query => "QueryEnv",
            Self::Command => "CommandEnv",
        }
    }

    fn trait_path(&self) -> TokenStream2 {
        match self {
            Self::Query => quote!(crate::service::Query),
            Self::Command => quote!(crate::service::Command),
        }
    }
}

pub fn expand(attr: TokenStream2, item: TokenStream2, kind: Kind) -> Result<TokenStream2> {
    if !attr.is_empty() {
        return Err(Error::new_spanned(
            attr,
            format!("#[sixer::{}] takes no arguments", kind.label()),
        ));
    }

    let mut inherent: ItemImpl = syn::parse2(item)?;
    if inherent.trait_.is_some() {
        return Err(Error::new_spanned(
            &inherent.self_ty,
            format!("put #[sixer::{}] on an inherent impl", kind.label()),
        ));
    }
    if inherent.unsafety.is_some() {
        return Err(Error::new_spanned(
            &inherent.self_ty,
            "use-case impls are safe",
        ));
    }
    inherent.modifiers.require_empty()?;
    if !inherent.generics.params.is_empty() || inherent.generics.where_clause.is_some() {
        return Err(Error::new_spanned(
            &inherent.generics,
            "use-case impls are not generic",
        ));
    }

    let run_index = find_run(&inherent)?;
    let ImplItem::Fn(run) = inherent.items.remove(run_index) else {
        unreachable!("find_run only returns a function");
    };
    check_run(&run, &kind)?;
    let (output, error) = split_result(&run.sig)?;

    let attrs = &inherent.attrs;
    let self_ty = &inherent.self_ty;
    let trait_path = kind.trait_path();
    let run_attrs = &run.attrs;
    let signature = &run.sig;
    let body = &run.block;
    let helpers = &inherent.items;
    let inherent_impl = if helpers.is_empty() {
        quote! {}
    } else {
        quote! {
            #(#attrs)*
            impl #self_ty {
                #(#helpers)*
            }
        }
    };

    Ok(quote! {
        #inherent_impl

        #(#attrs)*
        impl #trait_path for #self_ty {
            type Output = #output;
            type Error = #error;

            #(#run_attrs)*
            #signature #body
        }
    })
}

fn find_run(inherent: &ItemImpl) -> Result<usize> {
    let mut found = None;
    for (index, item) in inherent.items.iter().enumerate() {
        let ImplItem::Fn(method) = item else {
            continue;
        };
        if method.sig.ident != "run" {
            continue;
        }
        if found.is_some() {
            return Err(Error::new_spanned(
                &method.sig.ident,
                "this impl has one run method",
            ));
        }
        found = Some(index);
    }
    found.ok_or_else(|| Error::new_spanned(&inherent.self_ty, "this impl needs an async fn run"))
}

fn check_run(method: &ImplItemFn, kind: &Kind) -> Result<()> {
    method.modifiers.require_empty()?;
    if !matches!(method.vis, Visibility::Inherited) {
        return Err(Error::new_spanned(
            &method.vis,
            "run has no visibility modifier",
        ));
    }
    let signature = &method.sig;
    if signature.asyncness.is_none() {
        return Err(Error::new_spanned(signature.fn_token, "run is an async fn"));
    }
    if signature.constness.is_some()
        || signature.abi.is_some()
        || !matches!(signature.safety, syn::Safety::Default)
    {
        return Err(Error::new_spanned(signature.fn_token, "run is an async fn"));
    }
    if !signature.generics.params.is_empty() || signature.generics.where_clause.is_some() {
        return Err(Error::new_spanned(
            &signature.generics,
            "run is not generic",
        ));
    }
    expect_value_self(signature)?;
    expect_env(signature, kind.env())?;
    Ok(())
}

fn expect_value_self(signature: &Signature) -> Result<()> {
    let Some(receiver) = signature.receiver() else {
        return Err(Error::new_spanned(
            &signature.ident,
            "run takes self by value",
        ));
    };
    if receiver.mutability.is_some() {
        return Err(Error::new_spanned(receiver, "run takes self by value"));
    }
    match &receiver.kind {
        syn::ReceiverKind::Value => Ok(()),
        _ => Err(Error::new_spanned(receiver, "run takes self by value")),
    }
}

fn expect_env(signature: &Signature, env_name: &str) -> Result<()> {
    let mut inputs = signature.inputs.iter();
    inputs.next();
    let Some(syn::FnArg::Typed(argument)) = inputs.next() else {
        return Err(Error::new_spanned(
            &signature.ident,
            format!("run takes self and &impl {env_name}"),
        ));
    };
    if inputs.next().is_some() {
        return Err(Error::new_spanned(
            &argument.pat,
            format!("run takes self and &impl {env_name}"),
        ));
    }

    let SynType::Reference(reference) = &*argument.ty else {
        return Err(Error::new_spanned(
            &*argument.ty,
            format!("run takes &impl {env_name}"),
        ));
    };
    if reference.mutability.is_some() {
        return Err(Error::new_spanned(
            &*argument.ty,
            format!("run takes &impl {env_name}"),
        ));
    }
    let SynType::ImplTrait(impl_trait) = &*reference.elem else {
        return Err(Error::new_spanned(
            &*argument.ty,
            format!("run takes &impl {env_name}"),
        ));
    };
    let matches_env = impl_trait.bounds.iter().any(|bound| match bound {
        TypeParamBound::Trait(trait_bound) => trait_bound
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == env_name),
        _ => false,
    });
    if !matches_env {
        return Err(Error::new_spanned(
            &*argument.ty,
            format!("run takes &impl {env_name}"),
        ));
    }
    Ok(())
}

fn split_result(signature: &Signature) -> Result<(&Type, &Type)> {
    let ReturnType::Type(_, return_type) = &signature.output else {
        return Err(Error::new_spanned(
            &signature.ident,
            "run returns Result<Output, Error>",
        ));
    };
    let SynType::Path(path) = &**return_type else {
        return Err(Error::new_spanned(
            return_type,
            "run returns Result<Output, Error>",
        ));
    };
    if path.qself.is_some() {
        return Err(Error::new_spanned(
            return_type,
            "run returns Result<Output, Error>",
        ));
    }
    let Some(last) = path.path.segments.last() else {
        return Err(Error::new_spanned(
            return_type,
            "run returns Result<Output, Error>",
        ));
    };
    if last.ident != "Result" {
        return Err(Error::new_spanned(
            return_type,
            "run returns Result<Output, Error>",
        ));
    }
    let PathArguments::AngleBracketed(arguments) = &last.arguments else {
        return Err(Error::new_spanned(
            return_type,
            "run returns Result<Output, Error>",
        ));
    };
    let mut types = Vec::new();
    for argument in &arguments.args {
        match argument {
            GenericArgument::Type(ty) => types.push(ty),
            _ => {
                return Err(Error::new_spanned(
                    return_type,
                    "run returns Result<Output, Error>",
                ));
            }
        }
    }
    let [output, error] = types.as_slice() else {
        return Err(Error::new_spanned(
            return_type,
            "run returns Result<Output, Error>",
        ));
    };
    Ok((*output, *error))
}

type SynType = Type;

#[cfg(test)]
mod tests {
    use syn::{ImplItem, Item, ItemImpl};

    use super::*;

    struct Items(Vec<Item>);

    impl syn::parse::Parse for Items {
        fn parse(input: syn::parse::ParseStream<'_>) -> Result<Self> {
            let mut items = Vec::new();
            while !input.is_empty() {
                items.push(input.parse()?);
            }
            Ok(Self(items))
        }
    }

    fn impls(tokens: TokenStream2) -> Vec<ItemImpl> {
        let Items(items) = syn::parse2(tokens).unwrap();
        items
            .into_iter()
            .map(|item| match item {
                Item::Impl(impl_item) => impl_item,
                _ => panic!("expected an impl"),
            })
            .collect()
    }

    #[test]
    fn lifts_run_and_reads_result() {
        let output = expand(
            TokenStream2::new(),
            quote! {
                impl CreateWidget {
                    fn label(&self) -> &str {
                        self.name.as_str()
                    }

                    async fn run(
                        self,
                        env: &impl CommandEnv,
                    ) -> Result<(u32, u32), crate::Error> {
                        let _ = env;
                        Ok((self.name.len() as u32, 1))
                    }
                }
            },
            Kind::Command,
        )
        .unwrap();
        let impls = impls(output);
        assert_eq!(impls.len(), 2);

        let helper = &impls[0];
        assert!(helper.trait_.is_none());
        assert!(matches!(&helper.items[0], ImplItem::Fn(method) if method.sig.ident == "label"));

        let (trait_path, _) = impls[1].trait_.as_ref().unwrap();
        assert_eq!(trait_path.segments.last().unwrap().ident, "Command");
        let types: Vec<_> = impls[1]
            .items
            .iter()
            .filter_map(|item| match item {
                ImplItem::Type(assoc) => Some(assoc.ident.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(types, ["Output", "Error"]);
        let ImplItem::Fn(run) = impls[1]
            .items
            .iter()
            .find(|item| matches!(item, ImplItem::Fn(method) if method.sig.ident == "run"))
            .unwrap()
        else {
            unreachable!();
        };
        assert!(run.sig.asyncness.is_some());
    }

    #[test]
    fn rejects_the_wrong_environment() {
        let err = expand(
            TokenStream2::new(),
            quote! {
                impl FetchWidget {
                    async fn run(
                        self,
                        env: &impl CommandEnv,
                    ) -> Result<(), crate::Error> {
                        let _ = env;
                        Ok(())
                    }
                }
            },
            Kind::Query,
        )
        .unwrap_err();
        assert!(err.to_string().contains("QueryEnv"));
    }
}
