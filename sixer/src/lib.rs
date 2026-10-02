//! Macros that expand a command/query runtime into the calling crate.
//!
//! [`runtime!`] generates `Service`, the port bag, and the sealed traits.
//! Invoke it at the crate root. Use cases are ordinary `impl` blocks in the
//! calling crate, so the editor can see inside `run`.
//!
//! Annotate each port with [`port`]. That keeps the trait the adapter
//! implements, and emits `{Trait}Query` and `{Trait}Command`. `QueryEnv`
//! accessors return the query view. `CommandEnv` accessors return the command
//! view, which includes the query methods.
//!
//! [`query`], [`command`], and [`workflow`] sit on an inherent impl whose
//! `async fn run` returns `Result<Output, Error>`. They emit the sealed trait
//! impl. [`workflow`] is generated only when `runtime!` opts in. Pass a name,
//! `#[workflow("copy")]`, to override `fn name`. [`attempt_defaults`] on that
//! impl fills in `fn attempt_defaults`.
//!
//! `cargo test` of the calling crate needs `mockall` as a dev-dependency.
//! Production builds do not.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{Ident, Path, PathArguments, Result, Token, Type, braced};

mod port_view;
mod use_case;
mod workflow_gen;

mod kw {
    syn::custom_keyword!(error);
    syn::custom_keyword!(context);
    syn::custom_keyword!(workflow);
    syn::custom_keyword!(workflows);
    syn::custom_keyword!(ports);
}

struct RuntimeInput {
    error: Type,
    context: Option<Type>,
    workflow: bool,
    workflows: Vec<Path>,
    ports: Vec<Port>,
}

struct Port {
    field: Ident,
    assoc: Ident,
    bound: Path,
    mock: Path,
    query_view: Path,
    command_view: Path,
}

impl Parse for RuntimeInput {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        input.parse::<kw::error>()?;
        input.parse::<Token![=]>()?;
        let error = input.parse()?;
        let _ = input.parse::<Option<Token![,]>>()?;

        let context = if input.peek(kw::context) {
            input.parse::<kw::context>()?;
            input.parse::<Token![=]>()?;
            let context = input.parse()?;
            let _ = input.parse::<Option<Token![,]>>()?;
            Some(context)
        } else {
            None
        };

        let workflow = if input.peek(kw::workflow) {
            if context.is_none() {
                return Err(input.error("workflow requires context"));
            }
            input.parse::<kw::workflow>()?;
            let _ = input.parse::<Option<Token![,]>>()?;
            true
        } else {
            false
        };

        input.parse::<kw::ports>()?;
        let body;
        braced!(body in input);
        let mut ports = Vec::new();
        while !body.is_empty() {
            ports.push(body.parse()?);
            if body.is_empty() {
                break;
            }
            body.parse::<Token![,]>()?;
        }
        if ports.is_empty() {
            return Err(body.error("runtime! needs at least one port"));
        }
        let _ = input.parse::<Option<Token![,]>>()?;

        let workflows = if input.peek(kw::workflows) {
            if !workflow {
                return Err(input.error("workflows requires workflow"));
            }
            input.parse::<kw::workflows>()?;
            let list;
            braced!(list in input);
            let mut workflows = Vec::new();
            while !list.is_empty() {
                workflows.push(list.parse()?);
                if list.is_empty() {
                    break;
                }
                list.parse::<Token![,]>()?;
            }
            let _ = input.parse::<Option<Token![,]>>()?;
            workflows
        } else {
            Vec::new()
        };

        Ok(Self {
            error,
            context,
            workflow,
            workflows,
            ports,
        })
    }
}

impl Parse for Port {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let field = input.parse()?;
        input.parse::<Token![:]>()?;
        let assoc = input.parse()?;
        input.parse::<Token![:]>()?;
        let bound: Path = input.parse()?;
        let query_view = view_for_bound(&bound, "Query")?;
        let command_view = view_for_bound(&bound, "Command")?;
        let mock = if input.peek(Token![=>]) {
            input.parse::<Token![=>]>()?;
            input.parse()?
        } else {
            mock_for_bound(&bound)?
        };
        Ok(Self {
            field,
            assoc,
            bound,
            mock,
            query_view,
            command_view,
        })
    }
}

/// Marks each method of a port trait as a query or a command.
///
/// The adapter still implements the trait you wrote. The macro also emits
/// `{Trait}Query` and `{Trait}Command` wrappers in the same module. Those
/// wrappers are `pub(crate)`. The command wrapper includes the query
/// methods. Methods on the wrappers are inherent, so a use case in the host
/// crate can call them without importing the view.
///
/// Put `#[sixer::port]` above other attribute macros, such as
/// `mockall::automock`, so it sees the methods first. Every method needs
/// `#[query]` or `#[command]`.
///
/// `#[sixer::port(async_send)]` rewrites each `async fn` to
/// `fn ... -> impl Future<Output = ...> + Send`. `#[sixer::port]` leaves
/// signatures as written, so a method can spell that future by hand. Sync
/// methods stay ordinary functions either way. Adapters still implement a
/// rewritten method with `async fn`.
///
/// ```
/// use sixer::port;
///
/// #[port]
/// pub trait Database: Send + Sync {
///     #[query]
///     fn get(&self, id: u32) -> u32;
///
///     #[command]
///     fn put(&self, id: u32, value: u32);
/// }
///
/// struct Mem;
///
/// impl Database for Mem {
///     fn get(&self, id: u32) -> u32 {
///         id
///     }
///     fn put(&self, _: u32, _: u32) {}
/// }
///
/// fn read(db: &impl Database) -> u32 {
///     DatabaseQuery::new(db).get(1)
/// }
///
/// fn write(db: &impl Database) {
///     let view = DatabaseCommand::new(db);
///     let _ = view.get(1);
///     view.put(1, 2);
/// }
///
/// let db = Mem;
/// assert_eq!(read(&db), 1);
/// write(&db);
/// ```
///
/// A query view has no command methods.
///
/// ```compile_fail
/// use sixer::port;
///
/// #[port]
/// trait Database: Send + Sync {
///     #[query]
///     fn get(&self) -> u32;
///
///     #[command]
///     fn put(&self, value: u32);
/// }
///
/// fn write_from_query(db: &impl Database) {
///     DatabaseQuery::new(db).put(1);
/// }
/// ```
///
/// ```
/// use sixer::port;
///
/// #[port(async_send)]
/// trait Database: Send + Sync {
///     #[query]
///     async fn get(&self, id: u32) -> u32;
///
///     #[command]
///     fn put(&self, value: u32);
/// }
///
/// struct Mem;
///
/// impl Database for Mem {
///     async fn get(&self, id: u32) -> u32 {
///         id
///     }
///
///     fn put(&self, _: u32) {}
/// }
///
/// fn assert_send<T: Send>(_: T) {}
///
/// assert_send(DatabaseQuery::new(&Mem).get(1));
/// ```
#[proc_macro_attribute]
pub fn port(attr: TokenStream, item: TokenStream) -> TokenStream {
    match port_view::expand(attr.into(), item.into()) {
        Ok(expanded) => expanded.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Lifts `async fn run` from an inherent impl onto `Query`.
///
/// `Output` and `Error` are the two type arguments of the function's
/// `Result`. The environment parameter is `&impl QueryEnv`. Other methods in
/// the impl stay inherent.
///
/// ```ignore
/// #[sixer::query]
/// impl FetchWidget {
///     async fn run(
///         self,
///         env: &impl QueryEnv,
///     ) -> Result<Option<Widget>, crate::Error> {
///         env.database().get(self.0).await
///     }
/// }
/// ```
///
/// The environment has to be `QueryEnv`.
///
/// ```compile_fail
/// use sixer::query;
///
/// struct Tick;
///
/// #[query]
/// impl Tick {
///     async fn run(self, _env: &impl CommandEnv) -> Result<(), ()> {
///         Ok(())
///     }
/// }
/// ```
#[proc_macro_attribute]
pub fn query(attr: TokenStream, item: TokenStream) -> TokenStream {
    match use_case::expand(attr.into(), item.into(), use_case::Kind::Query) {
        Ok(expanded) => expanded.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Lifts `async fn run` from an inherent impl onto `Command`.
///
/// `Output` and `Error` are the two type arguments of the function's
/// `Result`. The environment parameter is `&impl CommandEnv`. Other methods
/// in the impl stay inherent.
#[proc_macro_attribute]
pub fn command(attr: TokenStream, item: TokenStream) -> TokenStream {
    match use_case::expand(attr.into(), item.into(), use_case::Kind::Command) {
        Ok(expanded) => expanded.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Lifts `async fn run` from an inherent impl onto `Workflow`.
///
/// `run` takes `self`, `&WorkflowContext`, and `&impl CommandEnv`. `Output`
/// and `Error` are the two type arguments of the function's `Result`. An
/// optional name, `#[workflow("copy")]`, or `fn name() -> &'static str`,
/// overrides the registration name. Optional
/// `fn attempt_defaults() -> AttemptDefaults`, or [`attempt_defaults`], sets
/// the policy for every `ctx.attempt`. Other methods in the impl stay
/// inherent. `runtime!` must opt in with `workflow` or the `Workflow` trait
/// is not generated.
#[proc_macro_attribute]
pub fn workflow(attr: TokenStream, item: TokenStream) -> TokenStream {
    match use_case::expand(attr.into(), item.into(), use_case::Kind::Workflow) {
        Ok(expanded) => expanded.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Sets the retry policy for every `ctx.attempt` in a workflow.
///
/// Place it on the same inherent impl as [`workflow`]. The macro emits
/// `fn attempt_defaults() -> AttemptDefaults`. Options are `max_retries`,
/// `backoff_factor`, `base_interval`, and `max_interval`. A duration is a
/// whole number of `ns`, `us`, `ms`, or `s` (`50ms` or `"50ms"`), or any
/// `Duration` expression. Omitted options keep the trait default. A
/// handwritten `fn attempt_defaults` does the same job.
#[proc_macro_attribute]
pub fn attempt_defaults(attr: TokenStream, item: TokenStream) -> TokenStream {
    match use_case::apply_attempt_defaults(attr.into(), item.into()) {
        Ok(expanded) => expanded.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// `path::Database` becomes `path::DatabaseQuery` or `path::DatabaseCommand`.
fn view_for_bound(bound: &Path, suffix: &str) -> Result<Path> {
    let mut view = bound.clone();
    let Some(last) = view.segments.last_mut() else {
        return Err(syn::Error::new_spanned(bound, "port bound must be a path"));
    };
    if !matches!(last.arguments, PathArguments::None) {
        return Err(syn::Error::new_spanned(
            bound,
            "generic port bounds have no query/command view",
        ));
    }
    last.ident = format_ident!("{}{suffix}", last.ident, span = last.ident.span());
    Ok(view)
}

/// `path::Database` becomes `path::MockDatabase`, matching `mockall::automock`.
fn mock_for_bound(bound: &Path) -> Result<Path> {
    let mut mock = bound.clone();
    let Some(last) = mock.segments.last_mut() else {
        return Err(syn::Error::new_spanned(bound, "port bound must be a path"));
    };
    if !matches!(last.arguments, syn::PathArguments::None) {
        return Err(syn::Error::new_spanned(
            bound,
            "cannot prefix Mock onto a bound with generic arguments; name the mock with `=>`",
        ));
    }
    last.ident = format_ident!("Mock{}", last.ident, span = last.ident.span());
    Ok(mock)
}

/// Expand `Service`, the port bag, and the sealed command/query traits.
///
/// ```ignore
/// sixer::runtime! {
///     error = crate::Error,
///     context = crate::Context,
///     ports {
///         database: DB: crate::port::Database,
///         clock: Clock: crate::port::Clock => crate::port::FakeClock,
///     }
/// }
/// ```
///
/// Each port line is `field: Assoc: Bound`. The bound is a `#[sixer::port]`
/// trait. `QueryEnv::{field}` returns the `{Bound}Query` wrapper.
/// `CommandEnv::{field}` returns the `{Bound}Command` wrapper.
/// `Service::builder()` takes those fields by
/// name, in any order, and `build` is available only once every field is set.
/// `Service::new` remains the positional form. Under `cfg(test)`, the mock
/// type is `Mock` prefixed to the bound's last segment, in the same module
/// (`Database` becomes `MockDatabase`). Write `=> path` when the mock is named
/// differently.
///
/// `context = Type` is optional and comes after `error`. The type must be
/// `Default + Send + Sync`. `Service::query` and `Service::command` install
/// `Default::default()`. `Service::with_context` borrows another value for
/// that call, and nested queries and commands see the same borrow. A use case
/// reads it with `env.ctx()`. Without `context`, those methods are not
/// generated and `Env` implements the environment traits directly.
///
/// `workflow` comes after `context` and requires it. `workflows { Type, ... }`
/// comes after `ports` and lists the types `Service::launch` registers. An
/// omitted list is empty. The application crate depends on `durare` and
/// `serde`. This crate does not.
///
/// Without `workflow`, a command env has no `start_workflow`.
///
/// ```compile_fail,E0599
/// use sixer::port;
///
/// #[port]
/// trait Store: Send + Sync {
///     #[command]
///     fn put(&self, value: u32);
/// }
///
/// sixer::runtime! {
///     error = (),
///     ports {
///         store: Store: crate::Store,
///     }
/// }
///
/// fn start(env: &impl service::CommandEnv) {
///     let _ = env.start_workflow("id", ());
/// }
///
/// fn main() {}
/// ```
#[proc_macro]
pub fn runtime(input: TokenStream) -> TokenStream {
    match syn::parse(input) {
        Ok(parsed) => expand(parsed).into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand(input: RuntimeInput) -> TokenStream2 {
    let error = &input.error;
    let port_types: Vec<_> = input
        .ports
        .iter()
        .map(|port| {
            let assoc = &port.assoc;
            let bound = &port.bound;
            quote! { type #assoc: #bound; }
        })
        .collect();
    let new_params: Vec<_> = input
        .ports
        .iter()
        .map(|port| {
            let field = &port.field;
            let assoc = &port.assoc;
            quote! { #field: P::#assoc }
        })
        .collect();
    let env_init: Vec<_> = input
        .ports
        .iter()
        .map(|port| {
            let field = &port.field;
            quote! { #field }
        })
        .collect();
    let env_fields: Vec<_> = input
        .ports
        .iter()
        .map(|port| {
            let field = &port.field;
            let assoc = &port.assoc;
            quote! { #field: P::#assoc }
        })
        .collect();
    let query_accessors: Vec<_> = input
        .ports
        .iter()
        .map(|port| accessor(&port.field, &port.query_view, &port.assoc))
        .collect();
    let command_accessors: Vec<_> = input
        .ports
        .iter()
        .map(|port| accessor(&port.field, &port.command_view, &port.assoc))
        .collect();
    let query_bodies: Vec<_> = input
        .ports
        .iter()
        .map(|port| accessor_body(&port.field, &port.query_view, &port.assoc))
        .collect();
    let command_bodies: Vec<_> = input
        .ports
        .iter()
        .map(|port| accessor_body(&port.field, &port.command_view, &port.assoc))
        .collect();
    let mock_fields: Vec<_> = input
        .ports
        .iter()
        .map(|port| {
            let field = &port.field;
            let mock = &port.mock;
            quote! { pub #field: #mock }
        })
        .collect();
    let mock_assocs: Vec<_> = input
        .ports
        .iter()
        .map(|port| {
            let assoc = &port.assoc;
            let mock = &port.mock;
            quote! { type #assoc = #mock; }
        })
        .collect();
    let builder_params: Vec<_> = input
        .ports
        .iter()
        .map(|port| {
            let assoc = &port.assoc;
            quote! { #assoc = Unset }
        })
        .collect();
    let builder_fields: Vec<_> = input
        .ports
        .iter()
        .map(|port| {
            let field = &port.field;
            let assoc = &port.assoc;
            quote! { #field: #assoc }
        })
        .collect();
    let builder_unset: Vec<_> = input
        .ports
        .iter()
        .map(|port| {
            let field = &port.field;
            quote! { #field: Unset }
        })
        .collect();
    let ready_params: Vec<_> = input
        .ports
        .iter()
        .map(|port| {
            let assoc = &port.assoc;
            quote! { <P as Ports>::#assoc }
        })
        .collect();
    let ready_moves: Vec<_> = input
        .ports
        .iter()
        .map(|port| {
            let field = &port.field;
            quote! { #field: self.#field }
        })
        .collect();
    let setters: Vec<_> = input
        .ports
        .iter()
        .enumerate()
        .map(|(index, port)| setter_impl(&input.ports, index, port, input.workflow))
        .collect();
    let context_query_bodies: Vec<_> = input
        .ports
        .iter()
        .map(|port| context_accessor_body(&port.field, &port.query_view, &port.assoc))
        .collect();
    let context_command_bodies: Vec<_> = input
        .ports
        .iter()
        .map(|port| context_accessor_body(&port.field, &port.command_view, &port.assoc))
        .collect();
    let ctx_method = input.context.as_ref().map(|ty| {
        quote! {
            /// Context installed for this command or query.
            fn ctx(&self) -> &#ty;
        }
    });
    let service_calls = service_calls(input.context.as_ref(), input.workflow);
    let direct_env_impls = if input.context.is_none() {
        quote! {
            impl<P: Ports> EnvExt for Env<P> {
                type Ports = P;
            }

            impl<P: Ports> QueryRuntime for Env<P> {
                fn query<Q>(
                    &self,
                    query: Q,
                ) -> impl ::core::future::Future<
                    Output = ::core::result::Result<Q::Output, Q::Error>,
                > + ::core::marker::Send
                where
                    Q: Query,
                {
                    query.run(self)
                }
            }

            impl<P: Ports> CommandRuntime for Env<P> {
                fn command<C>(
                    &self,
                    cmd: C,
                ) -> impl ::core::future::Future<
                    Output = ::core::result::Result<C::Output, C::Error>,
                > + ::core::marker::Send
                where
                    C: Command,
                {
                    cmd.run(self)
                }
            }

            impl<P: Ports> QueryEnv for Env<P> {
                #(#query_bodies)*
            }

            impl<P: Ports> CommandEnv for Env<P> {
                #(#command_bodies)*
            }
        }
    } else {
        quote! {}
    };
    let context_runtime = context_runtime(
        input.context.as_ref(),
        &context_query_bodies,
        &context_command_bodies,
        input.workflow,
    );
    let mock_ctx_field = input.context.as_ref().map(|ty| quote! { pub ctx: #ty, });
    let mock_ctx_body = input.context.as_ref().map(|ty| {
        quote! {
            fn ctx(&self) -> &#ty {
                &self.ctx
            }
        }
    });
    let service_struct = if input.workflow {
        quote! {
            struct Shared<P: Ports> {
                env: ::std::sync::Arc<Env<P>>,
                provider: ::std::sync::Arc<dyn ::durare::StateProvider>,
                engine: ::std::sync::OnceLock<::durare::DurableEngine>,
            }

            pub struct Service<P: Ports> {
                shared: ::std::sync::Arc<Shared<P>>,
            }
        }
    } else {
        quote! {
            pub struct Service<P: Ports> {
                env: ::std::sync::Arc<Env<P>>,
            }
        }
    };
    let service_new = if input.workflow {
        quote! {
            pub fn new(
                #(#new_params,)*
                provider: ::std::sync::Arc<dyn ::durare::StateProvider>,
            ) -> Self {
                Self {
                    shared: ::std::sync::Arc::new(Shared {
                        env: ::std::sync::Arc::new(Env { #(#env_init,)* }),
                        provider,
                        engine: ::std::sync::OnceLock::new(),
                    }),
                }
            }
        }
    } else {
        quote! {
            pub fn new(#(#new_params),*) -> Self {
                Self {
                    env: ::std::sync::Arc::new(Env { #(#env_init,)* }),
                }
            }
        }
    };
    let builder_struct = if input.workflow {
        quote! {
            pub struct ServiceBuilder<P, #(#builder_params),*, Provider = Unset> {
                #(#builder_fields,)*
                provider: Provider,
                _ports: ::core::marker::PhantomData<fn() -> P>,
            }
        }
    } else {
        quote! {
            pub struct ServiceBuilder<P, #(#builder_params),*> {
                #(#builder_fields,)*
                _ports: ::core::marker::PhantomData<fn() -> P>,
            }
        }
    };
    let builder_fn = if input.workflow {
        quote! {
            /// Name each port, in any order, and set the workflow provider.
            /// `build` is only implemented once every port and the provider
            /// are set.
            #[must_use]
            pub fn builder() -> ServiceBuilder<P> {
                ServiceBuilder {
                    #(#builder_unset,)*
                    provider: Unset,
                    _ports: ::core::marker::PhantomData,
                }
            }
        }
    } else {
        quote! {
            /// Name each port, in any order. `build` is only implemented
            /// once every port has been set.
            #[must_use]
            pub fn builder() -> ServiceBuilder<P> {
                ServiceBuilder {
                    #(#builder_unset,)*
                    _ports: ::core::marker::PhantomData,
                }
            }
        }
    };
    let clone_impl = if input.workflow {
        quote! {
            impl<P: Ports> Clone for Service<P> {
                fn clone(&self) -> Self {
                    Self {
                        shared: ::std::sync::Arc::clone(&self.shared),
                    }
                }
            }
        }
    } else {
        quote! {
            impl<P: Ports> Clone for Service<P> {
                fn clone(&self) -> Self {
                    Self {
                        env: ::std::sync::Arc::clone(&self.env),
                    }
                }
            }
        }
    };
    let build_impl = if input.workflow {
        quote! {
            impl<P: Ports> ServiceBuilder<
                P,
                #(#ready_params),*,
                ::std::sync::Arc<dyn ::durare::StateProvider>,
            > {
                pub fn build(self) -> Service<P> {
                    Service {
                        shared: ::std::sync::Arc::new(Shared {
                            env: ::std::sync::Arc::new(Env {
                                #(#ready_moves,)*
                            }),
                            provider: self.provider,
                            engine: ::std::sync::OnceLock::new(),
                        }),
                    }
                }
            }
        }
    } else {
        quote! {
            impl<P: Ports> ServiceBuilder<P, #(#ready_params),*> {
                pub fn build(self) -> Service<P> {
                    Service {
                        env: ::std::sync::Arc::new(Env {
                            #(#ready_moves,)*
                        }),
                    }
                }
            }
        }
    };
    let workflow_provider = if input.workflow {
        workflow_provider_impl(&input.ports)
    } else {
        quote! {}
    };
    let launch_shutdown = if input.workflow {
        workflow_gen::service_methods(
            error,
            input.context.as_ref().expect("workflow requires context"),
            &input.workflows,
        )
    } else {
        quote! {}
    };
    let command_env_bounds = if input.workflow {
        quote! { EnvExt + QueryRuntime + CommandRuntime + WorkflowRuntime }
    } else {
        quote! { EnvExt + QueryRuntime + CommandRuntime }
    };
    let workflow_items = if input.workflow {
        let context = input.context.as_ref().expect("workflow requires context");
        let runtime_trait = workflow_gen::runtime_trait(error);
        let items = workflow_gen::items(
            error,
            context,
            &context_query_bodies,
            &context_command_bodies,
        );
        let start = workflow_gen::context_start(error, context);
        quote! {
            #runtime_trait
            #items
            #start
        }
    } else {
        quote! {}
    };
    let (expect_start_alias, mock_workflow_field, expect_start_method, mock_workflow_impl) =
        if input.workflow {
            let (alias, field, expect) = workflow_gen::mock_pieces();
            (alias, field, expect, workflow_gen::mock_impl(error))
        } else {
            (quote! {}, quote! {}, quote! {}, quote! {})
        };
    let reexport = if input.workflow {
        quote! {
            pub use service::{
                Attempt, AttemptDefaults, ContextRuntime, Ports, Service, Workflow,
                WorkflowContext, WorkflowRun,
            };
        }
    } else if input.context.is_some() {
        quote! { pub use service::{ContextRuntime, Ports, Service}; }
    } else {
        quote! { pub use service::{Ports, Service}; }
    };

    quote! {
        mod service {
            pub trait Ports {
                #(#port_types)*
            }

            #service_struct

            #[doc(hidden)]
            pub struct Unset;

            #builder_struct

            impl<P: Ports> Service<P> {
                #service_new

                #builder_fn

                #service_calls

                #launch_shutdown
            }

            #clone_impl

            #(#setters)*

            #workflow_provider

            #build_impl

            struct Env<P: Ports> {
                #(#env_fields,)*
            }

            pub trait EnvExt: ::core::marker::Send + ::core::marker::Sync {
                type Ports: Ports;
                #ctx_method
            }

            #[cfg_attr(test, ::mockall::automock)]
            pub trait QueryRuntime: ::core::marker::Send + ::core::marker::Sync {
                fn query<Q>(
                    &self,
                    query: Q,
                ) -> impl ::core::future::Future<
                    Output = ::core::result::Result<Q::Output, Q::Error>,
                > + ::core::marker::Send
                where
                    Q: Query;
            }

            #[cfg_attr(test, ::mockall::automock)]
            pub trait CommandRuntime: ::core::marker::Send + ::core::marker::Sync {
                fn command<C>(
                    &self,
                    cmd: C,
                ) -> impl ::core::future::Future<
                    Output = ::core::result::Result<C::Output, C::Error>,
                > + ::core::marker::Send
                where
                    C: Command;
            }

            /// Accessors return the query view of each port (`TraitQuery`).
            pub trait QueryEnv: EnvExt + QueryRuntime {
                #(#query_accessors;)*
            }

            /// Accessors return the command view of each port (`TraitCommand`).
            ///
            /// This does not extend `QueryEnv`. Both traits name the same
            /// accessors, and the views have different types.
            pub trait CommandEnv: #command_env_bounds {
                #(#command_accessors;)*
            }

            pub trait Command: ::core::fmt::Debug + ::core::marker::Send + 'static {
                type Output;
                type Error: ::core::convert::Into<#error>;

                fn run(
                    self,
                    env: &impl CommandEnv,
                ) -> impl ::core::future::Future<
                    Output = ::core::result::Result<Self::Output, Self::Error>,
                > + ::core::marker::Send;
            }

            pub trait Query: ::core::fmt::Debug + ::core::marker::Send + 'static {
                type Output;
                type Error: ::core::convert::Into<#error>;

                fn run(
                    self,
                    env: &impl QueryEnv,
                ) -> impl ::core::future::Future<
                    Output = ::core::result::Result<Self::Output, Self::Error>,
                > + ::core::marker::Send;
            }

            #direct_env_impls

            #context_runtime

            #workflow_items

            #[cfg(test)]
            mod tests_doubles {
                use super::*;

                type ExpectQuery<Q> =
                    super::__mock_MockQueryRuntime_QueryRuntime::__query::Expectation<Q>;
                type ExpectCommand<C> =
                    super::__mock_MockCommandRuntime_CommandRuntime::__command::Expectation<C>;
                #expect_start_alias

                #[derive(Default)]
                pub struct MockEnv {
                    #(#mock_fields,)*
                    #mock_ctx_field
                    queries: MockQueryRuntime,
                    commands: MockCommandRuntime,
                    #mock_workflow_field
                }

                impl MockEnv {
                    pub fn expect_query<Q>(&mut self) -> &mut ExpectQuery<Q>
                    where
                        Q: Query,
                    {
                        self.queries.expect_query::<Q>()
                    }

                    #[allow(dead_code)]
                    pub fn expect_command<C>(&mut self) -> &mut ExpectCommand<C>
                    where
                        C: Command,
                    {
                        self.commands.expect_command::<C>()
                    }

                    #expect_start_method
                }

                impl Ports for MockEnv {
                    #(#mock_assocs)*
                }

                impl EnvExt for MockEnv {
                    type Ports = MockEnv;
                    #mock_ctx_body
                }

                impl QueryRuntime for MockEnv {
                    async fn query<Q>(
                        &self,
                        query: Q,
                    ) -> ::core::result::Result<Q::Output, Q::Error>
                    where
                        Q: Query,
                    {
                        self.queries.query(query).await
                    }
                }

                impl CommandRuntime for MockEnv {
                    async fn command<C>(
                        &self,
                        cmd: C,
                    ) -> ::core::result::Result<C::Output, C::Error>
                    where
                        C: Command,
                    {
                        self.commands.command(cmd).await
                    }
                }

                impl QueryEnv for MockEnv {
                    #(#query_bodies)*
                }

                impl CommandEnv for MockEnv {
                    #(#command_bodies)*
                }

                #mock_workflow_impl
            }

            #[cfg(test)]
            pub use tests_doubles::MockEnv;
        }

        #reexport
    }
}

fn service_calls(context: Option<&Type>, workflow: bool) -> TokenStream2 {
    let Some(ty) = context else {
        return quote! {
            /// Run a command. Nested queries share this environment.
            pub async fn command<C>(
                &self,
                cmd: C,
            ) -> ::core::result::Result<C::Output, C::Error>
            where
                C: Command,
            {
                cmd.run(self.env.as_ref()).await
            }

            /// Run a query. The query environment cannot dispatch commands.
            pub async fn query<Q>(
                &self,
                query: Q,
            ) -> ::core::result::Result<Q::Output, Q::Error>
            where
                Q: Query,
            {
                query.run(self.env.as_ref()).await
            }
        };
    };

    let with_context_fields = if workflow {
        quote! {
            env: self.shared.env.as_ref(),
            engine: &self.shared.engine,
            ctx,
        }
    } else {
        quote! {
            env: self.env.as_ref(),
            ctx,
        }
    };

    quote! {
        /// Run a command. A default context is installed for this call.
        /// Nested queries share that context.
        pub async fn command<C>(
            &self,
            cmd: C,
        ) -> ::core::result::Result<C::Output, C::Error>
        where
            C: Command,
        {
            let ctx = <#ty as ::core::default::Default>::default();
            self.with_context(&ctx).command(cmd).await
        }

        /// Run a query. A default context is installed for this call.
        /// The query environment cannot dispatch commands.
        pub async fn query<Q>(
            &self,
            query: Q,
        ) -> ::core::result::Result<Q::Output, Q::Error>
        where
            Q: Query,
        {
            let ctx = <#ty as ::core::default::Default>::default();
            self.with_context(&ctx).query(query).await
        }

        /// Borrow a context for one command or query.
        /// Nested calls see this same context.
        #[must_use]
        pub fn with_context<'a>(
            &'a self,
            ctx: &'a #ty,
        ) -> ContextRuntime<'a, P> {
            ContextRuntime { #with_context_fields }
        }
    }
}

fn context_runtime(
    context: Option<&Type>,
    query_bodies: &[TokenStream2],
    command_bodies: &[TokenStream2],
    workflow: bool,
) -> TokenStream2 {
    let Some(ty) = context else {
        return quote! {};
    };
    let engine_field = if workflow {
        quote! { engine: &'a ::std::sync::OnceLock<::durare::DurableEngine>, }
    } else {
        quote! {}
    };

    quote! {
        pub struct ContextRuntime<'a, P: Ports> {
            env: &'a Env<P>,
            #engine_field
            ctx: &'a #ty,
        }

        impl<'a, P: Ports> ContextRuntime<'a, P> {
            /// Run a command against this borrowed context.
            pub async fn command<C>(
                &self,
                cmd: C,
            ) -> ::core::result::Result<C::Output, C::Error>
            where
                C: Command,
            {
                cmd.run(self).await
            }

            /// Run a query against this borrowed context.
            /// The query environment cannot dispatch commands.
            pub async fn query<Q>(
                &self,
                query: Q,
            ) -> ::core::result::Result<Q::Output, Q::Error>
            where
                Q: Query,
            {
                query.run(self).await
            }
        }

        impl<'a, P: Ports> EnvExt for ContextRuntime<'a, P> {
            type Ports = P;

            fn ctx(&self) -> &#ty {
                self.ctx
            }
        }

        impl<'a, P: Ports> QueryRuntime for ContextRuntime<'a, P> {
            fn query<Q>(
                &self,
                query: Q,
            ) -> impl ::core::future::Future<
                Output = ::core::result::Result<Q::Output, Q::Error>,
            > + ::core::marker::Send
            where
                Q: Query,
            {
                query.run(self)
            }
        }

        impl<'a, P: Ports> CommandRuntime for ContextRuntime<'a, P> {
            fn command<C>(
                &self,
                cmd: C,
            ) -> impl ::core::future::Future<
                Output = ::core::result::Result<C::Output, C::Error>,
            > + ::core::marker::Send
            where
                C: Command,
            {
                cmd.run(self)
            }
        }

        impl<'a, P: Ports> QueryEnv for ContextRuntime<'a, P> {
            #(#query_bodies)*
        }

        impl<'a, P: Ports> CommandEnv for ContextRuntime<'a, P> {
            #(#command_bodies)*
        }
    }
}

fn workflow_provider_impl(ports: &[Port]) -> TokenStream2 {
    let generics: Vec<_> = ports.iter().map(|port| &port.assoc).collect();
    let moves: Vec<_> = ports
        .iter()
        .map(|port| {
            let field = &port.field;
            quote! { #field: self.#field }
        })
        .collect();

    quote! {
        impl<P: Ports, #(#generics),*> ServiceBuilder<P, #(#generics),*, Unset> {
            /// State backend for the workflows this service launches.
            #[must_use]
            pub fn workflow_provider(
                self,
                provider: ::std::sync::Arc<dyn ::durare::StateProvider>,
            ) -> ServiceBuilder<
                P,
                #(#generics),*,
                ::std::sync::Arc<dyn ::durare::StateProvider>,
            > {
                ServiceBuilder {
                    #(#moves,)*
                    provider,
                    _ports: ::core::marker::PhantomData,
                }
            }
        }
    }
}

fn setter_impl(ports: &[Port], index: usize, port: &Port, workflow: bool) -> TokenStream2 {
    let field = &port.field;
    let assoc = &port.assoc;
    let other_generics: Vec<_> = ports
        .iter()
        .enumerate()
        .filter(|(other, _)| *other != index)
        .map(|(_, other)| &other.assoc)
        .collect();
    let before: Vec<_> = ports
        .iter()
        .enumerate()
        .map(|(other_index, other)| {
            if other_index == index {
                quote! { Unset }
            } else {
                let assoc = &other.assoc;
                quote! { #assoc }
            }
        })
        .collect();
    let after: Vec<_> = ports
        .iter()
        .enumerate()
        .map(|(other_index, other)| {
            if other_index == index {
                quote! { <P as Ports>::#assoc }
            } else {
                let assoc = &other.assoc;
                quote! { #assoc }
            }
        })
        .collect();
    let other_moves: Vec<_> = ports
        .iter()
        .enumerate()
        .filter(|(other, _)| *other != index)
        .map(|(_, other)| {
            let field = &other.field;
            quote! { #field: self.#field }
        })
        .collect();

    if !workflow {
        return quote! {
            impl<P: Ports, #(#other_generics),*> ServiceBuilder<P, #(#before),*> {
                #[must_use]
                pub fn #field(
                    self,
                    #field: <P as Ports>::#assoc,
                ) -> ServiceBuilder<P, #(#after),*> {
                    ServiceBuilder {
                        #field,
                        #(#other_moves,)*
                        _ports: ::core::marker::PhantomData,
                    }
                }
            }
        };
    }

    quote! {
        impl<P: Ports, #(#other_generics,)* Provider> ServiceBuilder<P, #(#before),*, Provider> {
            #[must_use]
            pub fn #field(
                self,
                #field: <P as Ports>::#assoc,
            ) -> ServiceBuilder<P, #(#after),*, Provider> {
                ServiceBuilder {
                    #field,
                    #(#other_moves,)*
                    provider: self.provider,
                    _ports: ::core::marker::PhantomData,
                }
            }
        }
    }
}

fn accessor(method: &Ident, view: &Path, assoc: &Ident) -> TokenStream2 {
    quote! {
        fn #method(&self) -> #view<'_, <Self::Ports as Ports>::#assoc>
    }
}

fn accessor_body(method: &Ident, view: &Path, assoc: &Ident) -> TokenStream2 {
    quote! {
        fn #method(&self) -> #view<'_, <Self::Ports as Ports>::#assoc> {
            #view::new(&self.#method)
        }
    }
}

fn context_accessor_body(method: &Ident, view: &Path, assoc: &Ident) -> TokenStream2 {
    quote! {
        fn #method(&self) -> #view<'_, <Self::Ports as Ports>::#assoc> {
            #view::new(&self.env.#method)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn prefixes_mock_onto_the_last_segment() {
        let bound: Path = parse_quote!(crate::port::database::Database);
        let mock = mock_for_bound(&bound).unwrap();
        let expected: Path = parse_quote!(crate::port::database::MockDatabase);
        assert_eq!(mock, expected);
    }

    #[test]
    fn override_replaces_the_prefixed_mock() {
        let input: RuntimeInput = parse_quote! {
            error = crate::Error,
            ports {
                database: DB: crate::port::Database,
                clock: Clock: crate::port::Clock => crate::time::ManualClock,
            }
        };
        let expected_database: Path = parse_quote!(crate::port::MockDatabase);
        let expected_clock: Path = parse_quote!(crate::time::ManualClock);
        assert_eq!(input.ports[0].mock, expected_database);
        assert_eq!(input.ports[1].mock, expected_clock);
        let query_database: Path = parse_quote!(crate::port::DatabaseQuery);
        let command_clock: Path = parse_quote!(crate::port::ClockCommand);
        assert_eq!(input.ports[0].query_view, query_database);
        assert_eq!(input.ports[1].command_view, command_clock);
        assert!(input.context.is_none());
        assert!(!input.workflow);
        assert!(input.workflows.is_empty());
    }

    #[test]
    fn context_parses_a_type_between_error_and_ports() {
        let input: RuntimeInput = parse_quote! {
            error = crate::Error,
            context = crate::Context,
            ports {
                database: DB: crate::port::Database,
            }
        };
        let expected: Type = parse_quote!(crate::Context);
        assert_eq!(input.context, Some(expected));
        assert!(!input.workflow);
        assert!(input.workflows.is_empty());
    }

    #[test]
    fn workflow_requires_context() {
        let parsed = syn::parse2::<RuntimeInput>(quote! {
            error = crate::Error,
            workflow,
            ports {
                database: DB: crate::port::Database,
            }
        });
        let Err(err) = parsed else {
            panic!("workflow without context should fail to parse");
        };
        assert!(err.to_string().contains("workflow requires context"));
    }

    #[test]
    fn workflows_requires_the_workflow_keyword() {
        let parsed = syn::parse2::<RuntimeInput>(quote! {
            error = crate::Error,
            context = crate::Context,
            ports {
                database: DB: crate::port::Database,
            },
            workflows {
                crate::cqrs::Copy,
            }
        });
        let Err(err) = parsed else {
            panic!("workflows without workflow should fail to parse");
        };
        assert!(err.to_string().contains("workflows requires workflow"));
    }

    #[test]
    fn omitted_workflows_block_is_empty() {
        let input: RuntimeInput = parse_quote! {
            error = crate::Error,
            context = crate::Context,
            workflow,
            ports {
                database: DB: crate::port::Database,
            }
        };
        assert!(input.workflow);
        assert!(input.workflows.is_empty());
    }

    #[test]
    fn workflows_block_stores_each_path() {
        let input: RuntimeInput = parse_quote! {
            error = crate::Error,
            context = crate::Context,
            workflow,
            ports {
                database: DB: crate::port::Database,
            },
            workflows {
                crate::cqrs::Copy,
                crate::cqrs::Export,
            }
        };
        let expected: Vec<Path> = vec![
            parse_quote!(crate::cqrs::Copy),
            parse_quote!(crate::cqrs::Export),
        ];
        assert_eq!(input.workflows, expected);
    }

    #[test]
    fn without_workflow_the_expansion_does_not_mention_durare() {
        let input: RuntimeInput = parse_quote! {
            error = crate::Error,
            context = crate::Context,
            ports {
                database: DB: crate::port::Database,
            }
        };
        let tokens = expand(input).to_string();
        assert!(!tokens.contains("durare"));
        assert!(!tokens.contains("start_workflow"));
        assert!(!tokens.contains("run_workflow"));
        assert!(!tokens.contains("workflow_provider"));
    }

    #[test]
    fn launch_registers_each_listed_workflow() {
        let input: RuntimeInput = parse_quote! {
            error = crate::Error,
            context = crate::Context,
            workflow,
            ports {
                database: DB: crate::port::Database,
            },
            workflows {
                crate::cqrs::Copy,
                crate::cqrs::Export,
            }
        };
        let tokens = expand(input).to_string();
        assert!(tokens.contains("< crate :: cqrs :: Copy as Workflow > :: name ()"));
        assert!(tokens.contains("< crate :: cqrs :: Export as Workflow > :: name ()"));
        assert!(tokens.contains("fn run_workflow"));
        assert!(tokens.contains("fn attempt"));
        assert!(tokens.contains("struct Attempt"));
        assert!(tokens.contains("fn attempt_defaults"));
        assert!(tokens.contains("struct AttemptDefaults"));
        assert!(tokens.contains("from_millis (100)"));
        assert!(tokens.contains("from_secs (5)"));
        assert!(tokens.contains("fn max_retries"));
        assert!(tokens.contains("fn retry_if"));
        assert_eq!(tokens.matches("as Workflow > :: name").count(), 3);
        let query_env = tokens.split("pub trait QueryEnv").nth(1).unwrap();
        let query_def = query_env.split("pub trait CommandEnv").next().unwrap();
        let command_env = query_env.split("pub trait CommandEnv").nth(1).unwrap();
        assert!(!query_def.contains("start_workflow"));
        assert!(
            command_env.starts_with(" : EnvExt + QueryRuntime + CommandRuntime + WorkflowRuntime")
        );
    }
}
