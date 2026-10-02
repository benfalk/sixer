//! `#[sixer::query]`, `#[sixer::command]`, and `#[sixer::workflow]` turn an
//! inherent `run` into the sealed trait impl. `#[sixer::workflow("name")]`
//! sets the registration name. `#[attempt_defaults]` synthesizes the retry
//! policy.

use proc_macro2::{Literal, Span, TokenStream as TokenStream2};
use quote::quote;
use syn::parse::{Parse, Parser};
use syn::parse_quote;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{
    Attribute, Error, Expr, ExprLit, GenericArgument, Ident, ImplItem, ImplItemFn, ItemImpl, Lit,
    LitStr, Meta, PathArguments, Result, ReturnType, Signature, Token, Type, TypeParamBound,
    Visibility,
};

pub enum Kind {
    Query,
    Command,
    Workflow,
}

impl Kind {
    fn label(&self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Command => "command",
            Self::Workflow => "workflow",
        }
    }

    fn env(&self) -> &'static str {
        match self {
            Self::Query => "QueryEnv",
            Self::Command | Self::Workflow => "CommandEnv",
        }
    }

    fn trait_path(&self) -> TokenStream2 {
        match self {
            Self::Query => quote!(crate::service::Query),
            Self::Command => quote!(crate::service::Command),
            Self::Workflow => quote!(crate::service::Workflow),
        }
    }
}

pub fn expand(attr: TokenStream2, item: TokenStream2, kind: Kind) -> Result<TokenStream2> {
    let workflow_name = match &kind {
        Kind::Workflow => workflow_name_arg(attr)?,
        Kind::Query | Kind::Command => {
            if !attr.is_empty() {
                return Err(Error::new_spanned(
                    attr,
                    format!("#[sixer::{}] takes no arguments", kind.label()),
                ));
            }
            None
        }
    };

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

    // `#[workflow]` removes `#[attempt_defaults]` itself. If that attribute
    // expands first, its macro inserts the same method and leaves `#[workflow]`.
    if matches!(kind, Kind::Workflow) {
        install_shorthands(&mut inherent)?;
    } else {
        reject_workflow_shorthands(&inherent)?;
    }

    let name = if matches!(kind, Kind::Workflow) {
        match workflow_name {
            Some(lit) => {
                reject_method(
                    &inherent,
                    "name",
                    "#[sixer::workflow(\"...\")] replaces fn name",
                )?;
                Some(name_method(lit))
            }
            None => take_name(&mut inherent)?,
        }
    } else {
        None
    };
    let attempt_defaults = if matches!(kind, Kind::Workflow) {
        take_attempt_defaults(&mut inherent)?
    } else {
        None
    };
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
    let name_tokens = lifted_fn(name);
    let attempt_defaults_tokens = lifted_fn(attempt_defaults);
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

            #name_tokens
            #attempt_defaults_tokens

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
    match kind {
        Kind::Workflow => expect_workflow_args(signature)?,
        Kind::Query | Kind::Command => expect_env(signature, kind.env())?,
    }
    Ok(())
}

fn lifted_fn(method: Option<ImplItemFn>) -> TokenStream2 {
    method
        .map(|method| {
            let attrs = &method.attrs;
            let signature = &method.sig;
            let block = &method.block;
            quote! {
                #(#attrs)*
                #signature #block
            }
        })
        .unwrap_or_default()
}

fn take_name(inherent: &mut ItemImpl) -> Result<Option<ImplItemFn>> {
    let mut found = None;
    for (index, item) in inherent.items.iter().enumerate() {
        let ImplItem::Fn(method) = item else {
            continue;
        };
        if method.sig.ident != "name" {
            continue;
        }
        if found.is_some() {
            return Err(Error::new_spanned(
                &method.sig.ident,
                "this impl has one name method",
            ));
        }
        check_name(method)?;
        found = Some(index);
    }
    let Some(index) = found else {
        return Ok(None);
    };
    let ImplItem::Fn(method) = inherent.items.remove(index) else {
        unreachable!("take_name only returns a function");
    };
    Ok(Some(method))
}

fn check_name(method: &ImplItemFn) -> Result<()> {
    let message = "name is fn name() -> &'static str";
    method.modifiers.require_empty()?;
    if !matches!(method.vis, Visibility::Inherited) {
        return Err(Error::new_spanned(
            &method.vis,
            "name has no visibility modifier",
        ));
    }
    let signature = &method.sig;
    if signature.constness.is_some()
        || signature.asyncness.is_some()
        || signature.abi.is_some()
        || !matches!(signature.safety, syn::Safety::Default)
        || !signature.generics.params.is_empty()
        || signature.generics.where_clause.is_some()
        || signature.receiver().is_some()
        || !signature.inputs.is_empty()
    {
        return Err(Error::new_spanned(&signature.ident, message));
    }
    let ReturnType::Type(_, return_type) = &signature.output else {
        return Err(Error::new_spanned(&signature.ident, message));
    };
    let Type::Reference(reference) = return_type.as_ref() else {
        return Err(Error::new_spanned(return_type, message));
    };
    let is_static = reference
        .lifetime
        .as_ref()
        .is_some_and(|lifetime| lifetime.ident == "static");
    let Type::Path(path) = reference.elem.as_ref() else {
        return Err(Error::new_spanned(return_type, message));
    };
    let is_str = reference.mutability.is_none()
        && is_static
        && path.qself.is_none()
        && path
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "str");
    if !is_str {
        return Err(Error::new_spanned(return_type, message));
    }
    Ok(())
}

fn take_attempt_defaults(inherent: &mut ItemImpl) -> Result<Option<ImplItemFn>> {
    let mut found = None;
    for (index, item) in inherent.items.iter().enumerate() {
        let ImplItem::Fn(method) = item else {
            continue;
        };
        if method.sig.ident != "attempt_defaults" {
            continue;
        }
        if found.is_some() {
            return Err(Error::new_spanned(
                &method.sig.ident,
                "this impl has one attempt_defaults method",
            ));
        }
        check_attempt_defaults(method)?;
        found = Some(index);
    }
    let Some(index) = found else {
        return Ok(None);
    };
    let ImplItem::Fn(method) = inherent.items.remove(index) else {
        unreachable!("take_attempt_defaults only returns a function");
    };
    Ok(Some(method))
}

fn check_attempt_defaults(method: &ImplItemFn) -> Result<()> {
    let message = "attempt_defaults is fn attempt_defaults() -> AttemptDefaults";
    method.modifiers.require_empty()?;
    if !matches!(method.vis, Visibility::Inherited) {
        return Err(Error::new_spanned(
            &method.vis,
            "attempt_defaults has no visibility modifier",
        ));
    }
    let signature = &method.sig;
    if signature.constness.is_some()
        || signature.asyncness.is_some()
        || signature.abi.is_some()
        || !matches!(signature.safety, syn::Safety::Default)
        || !signature.generics.params.is_empty()
        || signature.generics.where_clause.is_some()
        || signature.receiver().is_some()
        || !signature.inputs.is_empty()
    {
        return Err(Error::new_spanned(&signature.ident, message));
    }
    let ReturnType::Type(_, return_type) = &signature.output else {
        return Err(Error::new_spanned(&signature.ident, message));
    };
    let Type::Path(path) = return_type.as_ref() else {
        return Err(Error::new_spanned(return_type, message));
    };
    let is_defaults = path.qself.is_none()
        && path
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "AttemptDefaults");
    if !is_defaults {
        return Err(Error::new_spanned(return_type, message));
    }
    Ok(())
}

/// `#[attempt_defaults]` belongs under `#[workflow]`.
///
/// The workflow macro strips it and inserts the method. The attribute macro
/// does the same insert when it runs first, then leaves `#[workflow]` on the
/// impl so this pass still lifts `run`.
pub fn apply_attempt_defaults(attr: TokenStream2, item: TokenStream2) -> Result<TokenStream2> {
    let mut inherent: ItemImpl = syn::parse2(item)?;
    require_workflow_impl(&inherent, "attempt_defaults")?;
    if let Some(other) = find_attr(&inherent, "attempt_defaults") {
        return Err(Error::new_spanned(other, "one attempt_defaults attribute"));
    }
    reject_method(
        &inherent,
        "attempt_defaults",
        "#[attempt_defaults] replaces fn attempt_defaults",
    )?;
    let method = attempt_defaults_method(assigns_from_tokens(attr)?)?;
    inherent.items.insert(0, ImplItem::Fn(method));
    Ok(quote!(#inherent))
}

fn install_shorthands(inherent: &mut ItemImpl) -> Result<()> {
    let mut attrs = Vec::new();
    let mut defaults_attr = None;
    for attr in inherent.attrs.drain(..) {
        if is_sixer_attr(&attr, "attempt_defaults") {
            if defaults_attr.is_some() {
                return Err(Error::new_spanned(attr, "one attempt_defaults attribute"));
            }
            defaults_attr = Some(attr);
        } else {
            attrs.push(attr);
        }
    }
    inherent.attrs = attrs;

    if let Some(attr) = defaults_attr {
        reject_method(
            inherent,
            "attempt_defaults",
            "#[attempt_defaults] replaces fn attempt_defaults",
        )?;
        let method = attempt_defaults_method(assigns_from_attr(&attr)?)?;
        inherent.items.insert(0, ImplItem::Fn(method));
    }
    Ok(())
}

fn reject_workflow_shorthands(inherent: &ItemImpl) -> Result<()> {
    for attr in &inherent.attrs {
        if is_sixer_attr(attr, "attempt_defaults") {
            return Err(Error::new_spanned(
                attr,
                "#[attempt_defaults] belongs on a #[sixer::workflow] impl",
            ));
        }
    }
    Ok(())
}

fn require_workflow_impl(inherent: &ItemImpl, helper: &str) -> Result<()> {
    if inherent
        .attrs
        .iter()
        .any(|attr| is_sixer_attr(attr, "workflow"))
    {
        return Ok(());
    }
    Err(Error::new_spanned(
        &inherent.self_ty,
        format!("#[{helper}] belongs on a #[sixer::workflow] impl"),
    ))
}

fn is_sixer_attr(attr: &Attribute, name: &str) -> bool {
    let path = attr.path();
    if path
        .segments
        .iter()
        .any(|segment| !matches!(segment.arguments, PathArguments::None))
    {
        return false;
    }
    let mut segments = path.segments.iter();
    match (segments.next(), segments.next(), segments.next()) {
        (Some(segment), None, None) => segment.ident == name,
        (Some(prefix), Some(segment), None) => prefix.ident == "sixer" && segment.ident == name,
        _ => false,
    }
}

fn find_attr<'a>(inherent: &'a ItemImpl, name: &str) -> Option<&'a Attribute> {
    inherent.attrs.iter().find(|attr| is_sixer_attr(attr, name))
}

fn find_method<'a>(inherent: &'a ItemImpl, name: &str) -> Option<&'a ImplItemFn> {
    inherent.items.iter().find_map(|item| match item {
        ImplItem::Fn(method) if method.sig.ident == name => Some(method),
        _ => None,
    })
}

fn reject_method(inherent: &ItemImpl, name: &str, message: &str) -> Result<()> {
    if let Some(method) = find_method(inherent, name) {
        return Err(Error::new_spanned(&method.sig.ident, message));
    }
    Ok(())
}

fn name_method(lit: LitStr) -> ImplItemFn {
    parse_quote! {
        fn name() -> &'static str {
            #lit
        }
    }
}

fn workflow_name_arg(attr: TokenStream2) -> Result<Option<LitStr>> {
    if attr.is_empty() {
        return Ok(None);
    }
    let message = "#[sixer::workflow] takes an optional name string";
    let lit: LitStr = syn::parse2(attr).map_err(|err| Error::new(err.span(), message))?;
    if lit.value().is_empty() {
        return Err(Error::new_spanned(
            lit,
            "#[sixer::workflow] takes a non-empty name",
        ));
    }
    Ok(Some(lit))
}

struct AttemptAssign {
    name: Ident,
    expr: Expr,
}

impl Parse for AttemptAssign {
    fn parse(input: syn::parse::ParseStream<'_>) -> Result<Self> {
        let name = input.parse()?;
        input.parse::<Token![=]>()?;
        let expr = input.parse()?;
        Ok(Self { name, expr })
    }
}

fn attempt_options_message() -> &'static str {
    "attempt_defaults options are max_retries, backoff_factor, base_interval, and max_interval"
}

fn assigns_from_attr(attr: &Attribute) -> Result<Vec<AttemptAssign>> {
    let Meta::List(list) = &attr.meta else {
        return Err(Error::new_spanned(attr, attempt_options_message()));
    };
    if list.tokens.is_empty() {
        return Err(Error::new_spanned(attr, attempt_options_message()));
    }
    assigns_from_tokens(list.tokens.clone())
}

fn assigns_from_tokens(tokens: TokenStream2) -> Result<Vec<AttemptAssign>> {
    if tokens.is_empty() {
        return Err(Error::new(Span::call_site(), attempt_options_message()));
    }
    let parsed = Punctuated::<AttemptAssign, Token![,]>::parse_terminated.parse2(tokens)?;
    if parsed.is_empty() {
        return Err(Error::new(Span::call_site(), attempt_options_message()));
    }
    Ok(parsed.into_iter().collect())
}

fn attempt_defaults_method(options: Vec<AttemptAssign>) -> Result<ImplItemFn> {
    let mut seen = Vec::new();
    let mut calls = Vec::new();
    for option in options {
        if !is_attempt_field(&option.name) {
            return Err(Error::new_spanned(option.name, attempt_options_message()));
        }
        if seen.iter().any(|prior: &Ident| prior == &option.name) {
            return Err(Error::new_spanned(
                &option.name,
                format!("attempt_defaults sets {} once", option.name),
            ));
        }
        seen.push(option.name.clone());
        let value = option_value(&option.name, option.expr)?;
        let name = option.name;
        calls.push(quote!(.#name(#value)));
    }
    Ok(parse_quote! {
        fn attempt_defaults() -> crate::service::AttemptDefaults {
            crate::service::AttemptDefaults::default()
                #(#calls)*
        }
    })
}

fn is_attempt_field(name: &Ident) -> bool {
    name == "max_retries"
        || name == "backoff_factor"
        || name == "base_interval"
        || name == "max_interval"
}

fn option_value(name: &Ident, expr: Expr) -> Result<TokenStream2> {
    match name.to_string().as_str() {
        "max_retries" => integer_value(name, expr),
        "backoff_factor" => factor_value(name, expr),
        "base_interval" | "max_interval" => duration_value(name, expr),
        _ => Err(Error::new_spanned(name, attempt_options_message())),
    }
}

fn integer_value(name: &Ident, expr: Expr) -> Result<TokenStream2> {
    let message = format!("{name} takes an integer");
    match &expr {
        Expr::Lit(ExprLit {
            lit: Lit::Int(int), ..
        }) if is_duration_unit(int.suffix()) => Err(Error::new_spanned(expr, message)),
        Expr::Lit(ExprLit {
            lit: Lit::Float(_) | Lit::Str(_),
            ..
        }) => Err(Error::new_spanned(expr, message)),
        _ => Ok(quote!(#expr)),
    }
}

fn factor_value(name: &Ident, expr: Expr) -> Result<TokenStream2> {
    let message = format!("{name} takes a number such as 2.0");
    if let Expr::Lit(ExprLit { lit, .. }) = &expr {
        match lit {
            Lit::Int(int) if int.suffix().is_empty() => {
                let value: u64 = int.base10_parse()?;
                let mut literal = Literal::f64_unsuffixed(value as f64);
                literal.set_span(int.span());
                return Ok(quote!(#literal));
            }
            Lit::Int(_) | Lit::Str(_) => return Err(Error::new_spanned(&expr, message)),
            _ => {}
        }
    }
    Ok(quote!(#expr))
}

fn duration_value(name: &Ident, expr: Expr) -> Result<TokenStream2> {
    let message = format!("{name} takes a duration such as 1ms, or a Duration");
    match &expr {
        Expr::Lit(ExprLit {
            lit: Lit::Int(int), ..
        }) => {
            let unit = int.suffix();
            if unit.is_empty() {
                return Err(Error::new_spanned(&expr, message));
            }
            let value: u64 = int.base10_parse()?;
            duration_call(name, unit, value, &expr)
        }
        Expr::Lit(ExprLit {
            lit: Lit::Str(text),
            ..
        }) => duration_from_text(name, &text.value(), &expr),
        Expr::Lit(ExprLit {
            lit: Lit::Float(_), ..
        }) => Err(Error::new_spanned(
            &expr,
            format!("{name} takes a whole number of ns, us, ms, or s"),
        )),
        Expr::Lit(_) => Err(Error::new_spanned(&expr, message)),
        _ => Ok(quote!(#expr)),
    }
}

fn duration_from_text(name: &Ident, text: &str, span: &Expr) -> Result<TokenStream2> {
    let message = format!("{name} takes a duration such as 1ms, or a Duration");
    let Some(unit_at) = text.find(|ch: char| ch.is_ascii_alphabetic()) else {
        return Err(Error::new_spanned(span, message));
    };
    let (digits, unit) = text.split_at(unit_at);
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit() || ch == '_') {
        return Err(Error::new_spanned(span, message));
    }
    let cleaned: String = digits.chars().filter(|ch| *ch != '_').collect();
    let Ok(value) = cleaned.parse::<u64>() else {
        return Err(Error::new_spanned(span, message));
    };
    duration_call(name, unit, value, span)
}

fn duration_call(name: &Ident, unit: &str, value: u64, span: &Expr) -> Result<TokenStream2> {
    let mut literal = Literal::u64_unsuffixed(value);
    literal.set_span(span.span());
    let call = match unit {
        "ns" => quote!(::std::time::Duration::from_nanos(#literal)),
        "us" => quote!(::std::time::Duration::from_micros(#literal)),
        "ms" => quote!(::std::time::Duration::from_millis(#literal)),
        "s" => quote!(::std::time::Duration::from_secs(#literal)),
        _ => {
            return Err(Error::new_spanned(
                span,
                format!("{name} unit is ns, us, ms, or s"),
            ));
        }
    };
    Ok(call)
}

fn is_duration_unit(suffix: &str) -> bool {
    matches!(suffix, "ns" | "us" | "ms" | "s")
}

fn expect_workflow_args(signature: &Signature) -> Result<()> {
    let message = "run takes self, &WorkflowContext, and &impl CommandEnv";
    let mut inputs = signature.inputs.iter();
    inputs.next();
    let Some(syn::FnArg::Typed(context)) = inputs.next() else {
        return Err(Error::new_spanned(&signature.ident, message));
    };
    let Some(syn::FnArg::Typed(env)) = inputs.next() else {
        return Err(Error::new_spanned(&signature.ident, message));
    };
    if inputs.next().is_some() {
        return Err(Error::new_spanned(&env.pat, message));
    }
    expect_context_ref(&context.ty)?;
    expect_impl_env(&env.ty, "CommandEnv", message)?;
    Ok(())
}

fn expect_context_ref(ty: &Type) -> Result<()> {
    let message = "run takes self, &WorkflowContext, and &impl CommandEnv";
    let Type::Reference(reference) = ty else {
        return Err(Error::new_spanned(ty, message));
    };
    if reference.mutability.is_some() {
        return Err(Error::new_spanned(ty, message));
    }
    let Type::Path(path) = reference.elem.as_ref() else {
        return Err(Error::new_spanned(ty, message));
    };
    let matches = path.qself.is_none()
        && path
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "WorkflowContext");
    if !matches {
        return Err(Error::new_spanned(ty, message));
    }
    Ok(())
}

fn expect_impl_env(ty: &Type, env_name: &str, message: &str) -> Result<()> {
    let Type::Reference(reference) = ty else {
        return Err(Error::new_spanned(ty, message));
    };
    if reference.mutability.is_some() {
        return Err(Error::new_spanned(ty, message));
    }
    let Type::ImplTrait(impl_trait) = reference.elem.as_ref() else {
        return Err(Error::new_spanned(ty, message));
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
        return Err(Error::new_spanned(ty, message));
    }
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

    #[test]
    fn workflow_lifts_run_without_inventing_name() {
        let output = expand(
            TokenStream2::new(),
            quote! {
                impl Copy {
                    async fn run(
                        self,
                        ctx: &WorkflowContext,
                        env: &impl CommandEnv,
                    ) -> Result<(), crate::Error> {
                        let _ = (ctx, env);
                        Ok(())
                    }
                }
            },
            Kind::Workflow,
        )
        .unwrap();
        let impls = impls(output);
        assert_eq!(impls.len(), 1);
        let (trait_path, _) = impls[0].trait_.as_ref().unwrap();
        assert_eq!(trait_path.segments.last().unwrap().ident, "Workflow");
        let names: Vec<_> = impls[0]
            .items
            .iter()
            .map(|item| match item {
                ImplItem::Fn(method) => method.sig.ident.to_string(),
                ImplItem::Type(assoc) => assoc.ident.to_string(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(names, ["Output", "Error", "run"]);
    }

    #[test]
    fn workflow_lifts_name_and_leaves_other_methods() {
        let output = expand(
            TokenStream2::new(),
            quote! {
                impl Copy {
                    fn label(&self) -> &str {
                        "copy"
                    }

                    fn name() -> &'static str {
                        "copy"
                    }

                    async fn run(
                        self,
                        ctx: &WorkflowContext,
                        env: &impl CommandEnv,
                    ) -> Result<(), crate::Error> {
                        let _ = (ctx, env, self.label());
                        Ok(())
                    }
                }
            },
            Kind::Workflow,
        )
        .unwrap();
        let impls = impls(output);
        assert_eq!(impls.len(), 2);
        assert!(impls[0].trait_.is_none());
        assert!(matches!(&impls[0].items[0], ImplItem::Fn(method) if method.sig.ident == "label"));

        let names: Vec<_> = impls[1]
            .items
            .iter()
            .map(|item| match item {
                ImplItem::Fn(method) => method.sig.ident.to_string(),
                ImplItem::Type(assoc) => assoc.ident.to_string(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(names, ["Output", "Error", "name", "run"]);
    }

    #[test]
    fn workflow_rejects_a_second_name() {
        let err = expand(
            TokenStream2::new(),
            quote! {
                impl Copy {
                    fn name() -> &'static str {
                        "copy"
                    }

                    fn name() -> &'static str {
                        "again"
                    }

                    async fn run(
                        self,
                        ctx: &WorkflowContext,
                        env: &impl CommandEnv,
                    ) -> Result<(), crate::Error> {
                        let _ = (ctx, env);
                        Ok(())
                    }
                }
            },
            Kind::Workflow,
        )
        .unwrap_err();
        assert!(err.to_string().contains("one name method"));
    }

    #[test]
    fn workflow_lifts_attempt_defaults() {
        let output = expand(
            TokenStream2::new(),
            quote! {
                impl Copy {
                    fn attempt_defaults() -> crate::service::AttemptDefaults {
                        crate::service::AttemptDefaults::default()
                    }

                    async fn run(
                        self,
                        ctx: &WorkflowContext,
                        env: &impl CommandEnv,
                    ) -> Result<(), crate::Error> {
                        let _ = (ctx, env);
                        Ok(())
                    }
                }
            },
            Kind::Workflow,
        )
        .unwrap();
        let impls = impls(output);
        assert_eq!(impls.len(), 1);
        let names: Vec<_> = impls[0]
            .items
            .iter()
            .map(|item| match item {
                ImplItem::Fn(method) => method.sig.ident.to_string(),
                ImplItem::Type(assoc) => assoc.ident.to_string(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(names, ["Output", "Error", "attempt_defaults", "run"]);
    }

    #[test]
    fn workflow_rejects_a_second_attempt_defaults() {
        let err = expand(
            TokenStream2::new(),
            quote! {
                impl Copy {
                    fn attempt_defaults() -> AttemptDefaults {
                        AttemptDefaults::default()
                    }

                    fn attempt_defaults() -> AttemptDefaults {
                        AttemptDefaults::default()
                    }

                    async fn run(
                        self,
                        ctx: &WorkflowContext,
                        env: &impl CommandEnv,
                    ) -> Result<(), crate::Error> {
                        let _ = (ctx, env);
                        Ok(())
                    }
                }
            },
            Kind::Workflow,
        )
        .unwrap_err();
        assert!(err.to_string().contains("one attempt_defaults method"));
    }

    #[test]
    fn workflow_rejects_attempt_defaults_with_a_receiver() {
        let err = expand(
            TokenStream2::new(),
            quote! {
                impl Copy {
                    fn attempt_defaults(&self) -> AttemptDefaults {
                        AttemptDefaults::default()
                    }

                    async fn run(
                        self,
                        ctx: &WorkflowContext,
                        env: &impl CommandEnv,
                    ) -> Result<(), crate::Error> {
                        let _ = (ctx, env);
                        Ok(())
                    }
                }
            },
            Kind::Workflow,
        )
        .unwrap_err();
        assert!(err.to_string().contains("fn attempt_defaults()"));
    }

    #[test]
    fn workflow_rejects_name_with_a_receiver() {
        let err = expand(
            TokenStream2::new(),
            quote! {
                impl Copy {
                    fn name(&self) -> &'static str {
                        "copy"
                    }

                    async fn run(
                        self,
                        ctx: &WorkflowContext,
                        env: &impl CommandEnv,
                    ) -> Result<(), crate::Error> {
                        let _ = (ctx, env);
                        Ok(())
                    }
                }
            },
            Kind::Workflow,
        )
        .unwrap_err();
        assert!(err.to_string().contains("name is fn name()"));
    }

    #[test]
    fn workflow_rejects_run_without_the_context() {
        let err = expand(
            TokenStream2::new(),
            quote! {
                impl Copy {
                    async fn run(self, env: &impl CommandEnv) -> Result<(), crate::Error> {
                        let _ = env;
                        Ok(())
                    }
                }
            },
            Kind::Workflow,
        )
        .unwrap_err();
        assert!(err.to_string().contains("WorkflowContext"));
    }

    fn method_tokens(impls: &[ItemImpl], name: &str) -> String {
        let method = impls
            .iter()
            .flat_map(|item| &item.items)
            .find_map(|item| match item {
                ImplItem::Fn(method) if method.sig.ident == name => Some(method),
                _ => None,
            })
            .expect(name);
        quote!(#method).to_string()
    }

    fn workflow_run() -> TokenStream2 {
        quote! {
            async fn run(
                self,
                ctx: &WorkflowContext,
                env: &impl CommandEnv,
            ) -> Result<(), crate::Error> {
                let _ = (ctx, env);
                Ok(())
            }
        }
    }

    #[test]
    fn workflow_argument_emits_name() {
        let run = workflow_run();
        let output = expand(
            quote!("copy"),
            quote! {
                #[allow(dead_code)]
                impl Copy {
                    #run
                }
            },
            Kind::Workflow,
        )
        .unwrap();
        let impls = impls(output);
        assert_eq!(impls.len(), 1);
        let attrs: Vec<_> = impls[0]
            .attrs
            .iter()
            .map(|attr| attr.path().segments.last().unwrap().ident.to_string())
            .collect();
        assert_eq!(attrs, ["allow"]);
        let names: Vec<_> = impls[0]
            .items
            .iter()
            .map(|item| match item {
                ImplItem::Fn(method) => method.sig.ident.to_string(),
                ImplItem::Type(assoc) => assoc.ident.to_string(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(names, ["Output", "Error", "name", "run"]);
        assert!(method_tokens(&impls, "name").contains("\"copy\""));
    }

    #[test]
    fn workflow_argument_keeps_attempt_defaults() {
        let run = workflow_run();
        let output = expand(
            quote!("copy"),
            quote! {
                #[::sixer::attempt_defaults(max_retries = 3)]
                impl Copy {
                    #run
                }
            },
            Kind::Workflow,
        )
        .unwrap();
        let impls = impls(output);
        assert!(method_tokens(&impls, "name").contains("\"copy\""));
        assert!(method_tokens(&impls, "attempt_defaults").contains("max_retries (3)"));
    }

    #[test]
    fn attempt_defaults_attribute_emits_the_policy() {
        let run = workflow_run();
        let output = expand(
            TokenStream2::new(),
            quote! {
                #[attempt_defaults(
                    max_retries = 3,
                    backoff_factor = 2,
                    base_interval = 1ms,
                    max_interval = "5s",
                )]
                impl Copy {
                    #run
                }
            },
            Kind::Workflow,
        )
        .unwrap();
        let impls = impls(output);
        let tokens = method_tokens(&impls, "attempt_defaults");
        assert!(tokens.contains("crate :: service :: AttemptDefaults :: default ()"));
        assert!(tokens.contains("max_retries (3)"));
        assert!(tokens.contains("backoff_factor (2.0)"));
        assert!(tokens.contains("from_millis (1)"));
        assert!(tokens.contains("from_secs (5)"));
        assert!(!tokens.contains("retry_if"));
    }

    #[test]
    fn attempt_defaults_attribute_keeps_a_duration_expression() {
        let run = workflow_run();
        let output = expand(
            TokenStream2::new(),
            quote! {
                #[::sixer::attempt_defaults(
                    base_interval = ::std::time::Duration::from_millis(7),
                )]
                impl Copy {
                    #run
                }
            },
            Kind::Workflow,
        )
        .unwrap();
        let tokens = method_tokens(&impls(output), "attempt_defaults");
        assert!(tokens.contains(":: std :: time :: Duration :: from_millis (7)"));
    }

    #[test]
    fn workflow_argument_rejects_a_handwritten_name() {
        let run = workflow_run();
        let err = expand(
            quote!("copy"),
            quote! {
                impl Copy {
                    fn name() -> &'static str {
                        "copy"
                    }

                    #run
                }
            },
            Kind::Workflow,
        )
        .unwrap_err();
        assert!(err.to_string().contains("replaces fn name"));
    }

    #[test]
    fn attempt_defaults_attribute_rejects_a_handwritten_method() {
        let run = workflow_run();
        let err = expand(
            TokenStream2::new(),
            quote! {
                #[attempt_defaults(max_retries = 3)]
                impl Copy {
                    fn attempt_defaults() -> AttemptDefaults {
                        AttemptDefaults::default()
                    }

                    #run
                }
            },
            Kind::Workflow,
        )
        .unwrap_err();
        assert!(err.to_string().contains("replaces fn attempt_defaults"));
    }

    #[test]
    fn workflow_argument_rejects_an_empty_string() {
        let run = workflow_run();
        let err = expand(
            quote!(""),
            quote! {
                impl Copy {
                    #run
                }
            },
            Kind::Workflow,
        )
        .unwrap_err();
        assert!(err.to_string().contains("non-empty"));
    }

    #[test]
    fn workflow_argument_rejects_a_non_string() {
        let run = workflow_run();
        let err = expand(
            quote!(copy),
            quote! {
                impl Copy {
                    #run
                }
            },
            Kind::Workflow,
        )
        .unwrap_err();
        assert!(err.to_string().contains("optional name string"));
    }

    #[test]
    fn attempt_defaults_attribute_rejects_an_unknown_option() {
        let run = workflow_run();
        let err = expand(
            TokenStream2::new(),
            quote! {
                #[attempt_defaults(retry_if = false)]
                impl Copy {
                    #run
                }
            },
            Kind::Workflow,
        )
        .unwrap_err();
        assert!(err.to_string().contains("max_retries"));
        assert!(!err.to_string().contains("retry_if is"));
    }

    #[test]
    fn attempt_defaults_attribute_rejects_a_repeated_option() {
        let run = workflow_run();
        let err = expand(
            TokenStream2::new(),
            quote! {
                #[attempt_defaults(max_retries = 3, max_retries = 4)]
                impl Copy {
                    #run
                }
            },
            Kind::Workflow,
        )
        .unwrap_err();
        assert!(err.to_string().contains("sets max_retries once"));
    }

    #[test]
    fn attempt_defaults_attribute_rejects_a_bare_integer_duration() {
        let run = workflow_run();
        let err = expand(
            TokenStream2::new(),
            quote! {
                #[attempt_defaults(base_interval = 5)]
                impl Copy {
                    #run
                }
            },
            Kind::Workflow,
        )
        .unwrap_err();
        assert!(err.to_string().contains("duration"));
    }

    #[test]
    fn attempt_defaults_attribute_rejects_an_unknown_duration_unit() {
        let run = workflow_run();
        let err = expand(
            TokenStream2::new(),
            quote! {
                #[attempt_defaults(max_interval = 2m)]
                impl Copy {
                    #run
                }
            },
            Kind::Workflow,
        )
        .unwrap_err();
        assert!(err.to_string().contains("unit is"));
    }

    #[test]
    fn command_rejects_a_name_argument() {
        let err = expand(
            quote!("copy"),
            quote! {
                impl Copy {
                    async fn run(self, env: &impl CommandEnv) -> Result<(), crate::Error> {
                        let _ = env;
                        Ok(())
                    }
                }
            },
            Kind::Command,
        )
        .unwrap_err();
        assert!(err.to_string().contains("takes no arguments"));
    }
}
