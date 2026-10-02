//! Token streams for the opt-in `workflow` clause of [`crate::runtime`].

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Path, Type};

pub fn runtime_trait(error: &Type) -> TokenStream2 {
    quote! {
        /// Start a workflow registered by `Service::launch`.
        ///
        /// Queries do not have this method. `ContextRuntime` reaches the
        /// launched engine. Other command envs, including the one inside a
        /// running workflow, return an error. A test sets an expectation with
        /// `MockEnv::expect_start_workflow`.
        #[cfg_attr(test, ::mockall::automock)]
        pub trait WorkflowRuntime {
            fn start_workflow<W: Workflow>(
                &self,
                id: &str,
                input: W,
            ) -> impl ::core::future::Future<
                Output = ::core::result::Result<WorkflowRun<W::Output>, #error>,
            > + ::core::marker::Send {
                let _ = (id, input);
                async {
                    Err(app_error(
                        "workflows are started by a launched service",
                    ))
                }
            }
        }
    }
}

pub fn items(
    error: &Type,
    context: &Type,
    query_bodies: &[TokenStream2],
    command_bodies: &[TokenStream2],
) -> TokenStream2 {
    quote! {
        /// A workflow a command can start.
        ///
        /// The implementing value is the input durare stores for the run.
        /// `name` is the engine registration. Set it with `#[workflow("copy")]`
        /// or `fn name() -> &'static str` when runs are persisted. The default
        /// calls `::std::any::type_name`, and that diagnostic string can change
        /// between compilers.
        pub trait Workflow:
            ::serde::Serialize
            + ::serde::de::DeserializeOwned
            + ::core::marker::Send
            + ::core::marker::Sync
            + 'static
        {
            fn name() -> &'static str {
                ::std::any::type_name::<Self>()
            }

            /// Policy for every [`WorkflowContext::attempt`] in this workflow.
            ///
            /// The default is one try: no extra retries, backoff factor `2.0`,
            /// a `100ms` base delay, and a `5s` cap. Set it with
            /// `#[attempt_defaults]` or `fn attempt_defaults`.
            fn attempt_defaults() -> AttemptDefaults {
                AttemptDefaults::default()
            }

            type Output:
                ::serde::Serialize + ::serde::de::DeserializeOwned + ::core::marker::Send + 'static;

            type Error: ::core::convert::Into<#error>;

            fn run(
                self,
                ctx: &WorkflowContext,
                env: &impl CommandEnv,
            ) -> impl ::core::future::Future<
                Output = ::core::result::Result<Self::Output, Self::Error>,
            > + ::core::marker::Send;
        }

        /// Durare's context for one workflow run.
        ///
        /// [`step`](Self::step) records a port call. [`attempt`](Self::attempt)
        /// returns an [`Attempt`] that retries that call before the outcome is
        /// saved. The durare context stays inside this type. A test calls
        /// `run_workflow` to run the body against a mock env. That path calls
        /// the ports and does not journal.
        pub struct WorkflowContext {
            mode: CallMode,
            defaults: AttemptDefaults,
        }

        #[derive(Clone)]
        enum CallMode {
            Durable(::durare::DurableContext),
            #[cfg(test)]
            Direct,
        }

        impl WorkflowContext {
            fn new(durable: ::durare::DurableContext, defaults: AttemptDefaults) -> Self {
                Self {
                    mode: CallMode::Durable(durable),
                    defaults,
                }
            }

            #[cfg(test)]
            fn direct(defaults: AttemptDefaults) -> Self {
                Self {
                    mode: CallMode::Direct,
                    defaults,
                }
            }

            /// Record the result of one port call under `name`.
            ///
            /// The future runs once. Its success or its error is what a later
            /// replay returns.
            pub fn step<'a, T, Fut>(
                &'a self,
                name: &'a str,
                call: Fut,
            ) -> impl ::core::future::Future<Output = ::core::result::Result<T, #error>>
                + ::core::marker::Send
                + 'a
            where
                T: ::serde::Serialize + ::serde::de::DeserializeOwned + ::core::marker::Send,
                Fut: ::core::future::Future<Output = ::core::result::Result<T, #error>>
                    + ::core::marker::Send
                    + 'a,
            {
                let mode = self.mode.clone();
                async move {
                    match mode {
                        CallMode::Durable(durable) => durable
                            .step(name, || async move {
                                call.await.map_err(into_engine)
                            })
                            .await
                            .map_err(from_engine),
                        #[cfg(test)]
                        CallMode::Direct => call.await,
                    }
                }
            }

            /// Start a journaled port call named `name`.
            ///
            /// The policy starts from this workflow's
            /// [`Workflow::attempt_defaults`]. Change one call on the returned
            /// [`Attempt`], then pass the port call to [`Attempt::run`].
            pub fn attempt(&self, name: &str) -> Attempt {
                Attempt {
                    mode: self.mode.clone(),
                    opts: self.defaults.step_options(name),
                }
            }
        }

        /// Run `workflow` on `env` and return its outcome.
        ///
        /// `step` and `attempt` call the ports on `env`. An attempt uses the
        /// workflow's retry policy and does not wait between tries. Nothing is
        /// journaled, so another call runs the body again.
        #[cfg(test)]
        pub async fn run_workflow<W: Workflow>(
            env: &impl CommandEnv,
            workflow: W,
        ) -> ::core::result::Result<W::Output, W::Error> {
            let ctx = WorkflowContext::direct(<W as Workflow>::attempt_defaults());
            Workflow::run(workflow, &ctx, env).await
        }

        /// Retry policy shared by every attempt in one workflow.
        ///
        /// The default is one try: no extra retries, backoff factor `2.0`, a
        /// `100ms` base delay, and a `5s` cap.
        #[must_use]
        #[derive(Clone, Copy)]
        pub struct AttemptDefaults {
            max_retries: u32,
            backoff_factor: f64,
            base_interval: ::std::time::Duration,
            max_interval: ::std::time::Duration,
        }

        impl ::core::default::Default for AttemptDefaults {
            fn default() -> Self {
                Self {
                    max_retries: 0,
                    backoff_factor: 2.0,
                    base_interval: ::std::time::Duration::from_millis(100),
                    max_interval: ::std::time::Duration::from_secs(5),
                }
            }
        }

        impl AttemptDefaults {
            /// How many failures to try again after the first.
            pub fn max_retries(mut self, retries: u32) -> Self {
                self.max_retries = retries;
                self
            }

            /// Exponential backoff multiplier between attempts.
            pub fn backoff_factor(mut self, factor: f64) -> Self {
                self.backoff_factor = factor;
                self
            }

            /// Delay before the first retry.
            pub fn base_interval(mut self, interval: ::std::time::Duration) -> Self {
                self.base_interval = interval;
                self
            }

            /// Upper bound on any single retry delay.
            pub fn max_interval(mut self, interval: ::std::time::Duration) -> Self {
                self.max_interval = interval;
                self
            }

            fn step_options(&self, name: &str) -> ::durare::StepOptions {
                ::durare::StepOptions::new(name)
                    .max_retries(self.max_retries)
                    .backoff_factor(self.backoff_factor)
                    .base_interval(self.base_interval)
                    .max_interval(self.max_interval)
            }
        }

        /// A journaled port call that can retry before its outcome is saved.
        ///
        /// The policy starts from [`Workflow::attempt_defaults`]. `max_retries`
        /// is how many failures to try again after the first.
        /// [`run`](Self::run) invokes its closure once per attempt. Only the
        /// final success or the final error is saved, and a replay returns that
        /// outcome without calling the closure again. With no
        /// [`retry_if`](Self::retry_if) predicate, every error is retried.
        #[must_use]
        pub struct Attempt {
            mode: CallMode,
            opts: ::durare::StepOptions,
        }

        impl Attempt {
            /// How many failures to try again after the first.
            pub fn max_retries(mut self, retries: u32) -> Self {
                self.opts = self.opts.max_retries(retries);
                self
            }

            /// Exponential backoff multiplier between attempts.
            pub fn backoff_factor(mut self, factor: f64) -> Self {
                self.opts = self.opts.backoff_factor(factor);
                self
            }

            /// Delay before the first retry.
            pub fn base_interval(mut self, interval: ::std::time::Duration) -> Self {
                self.opts = self.opts.base_interval(interval);
                self
            }

            /// Upper bound on any single retry delay.
            pub fn max_interval(mut self, interval: ::std::time::Duration) -> Self {
                self.opts = self.opts.max_interval(interval);
                self
            }

            /// Decide whether a failure is tried again.
            ///
            /// The predicate sees the crate error built from the failure's
            /// message. Returning `false` stops at once.
            pub fn retry_if<P>(mut self, predicate: P) -> Self
            where
                P: Fn(&#error) -> bool + ::core::marker::Send + ::core::marker::Sync + 'static,
            {
                self.opts = self.opts.retry_if(move |err| {
                    predicate(&from_engine(::durare::Error::app(err.to_string())))
                });
                self
            }

            /// Run `call` once per attempt and return the journaled outcome.
            #[must_use]
            pub fn run<'a, T, F, Fut>(
                self,
                mut call: F,
            ) -> impl ::core::future::Future<Output = ::core::result::Result<T, #error>>
                + ::core::marker::Send
                + 'a
            where
                T: ::serde::Serialize + ::serde::de::DeserializeOwned + ::core::marker::Send,
                F: ::core::ops::FnMut() -> Fut + ::core::marker::Send + 'a,
                Fut: ::core::future::Future<Output = ::core::result::Result<T, #error>>
                    + ::core::marker::Send
                    + 'a,
            {
                let Attempt { mode, opts } = self;
                async move {
                    match mode {
                        CallMode::Durable(durable) => durable
                            .step_with(opts, || {
                                let attempt = call();
                                async move { attempt.await.map_err(into_engine) }
                            })
                            .await
                            .map_err(from_engine),
                        #[cfg(test)]
                        CallMode::Direct => {
                            let mut tries = 0u32;
                            loop {
                                match call().await {
                                    Ok(value) => return Ok(value),
                                    Err(err) => {
                                        let retryable = opts.retry_if.as_ref().is_none_or(|pred| {
                                            pred(&::durare::Error::app(err.to_string()))
                                        });
                                        if !retryable || tries >= opts.max_retries {
                                            return Err(err);
                                        }
                                        tries += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        #[derive(::serde::Serialize, ::serde::Deserialize)]
        struct Invocation<I> {
            caller: #context,
            input: I,
        }

        pub struct WorkflowRun<O> {
            id: ::std::string::String,
            inner: RunInner<O>,
        }

        enum RunInner<O> {
            Live(::durare::WorkflowHandle<O>),
            #[cfg(test)]
            Ready(O),
        }

        impl<O> WorkflowRun<O> {
            #[must_use]
            pub fn id(&self) -> &str {
                &self.id
            }

            /// A finished run for a test that mocks
            /// [`WorkflowRuntime::start_workflow`].
            #[cfg(test)]
            pub fn ready(id: impl ::core::convert::Into<::std::string::String>, output: O) -> Self {
                Self {
                    id: id.into(),
                    inner: RunInner::Ready(output),
                }
            }
        }

        impl<O: ::serde::de::DeserializeOwned + 'static> WorkflowRun<O> {
            pub async fn result(self) -> ::core::result::Result<O, #error> {
                match self.inner {
                    RunInner::Live(handle) => handle.await.map_err(from_engine),
                    #[cfg(test)]
                    RunInner::Ready(output) => Ok(output),
                }
            }
        }

        /// Command env owned by one workflow run.
        ///
        /// It holds the service's ports and the caller recorded at start.
        /// `start_workflow` on this env refuses, so a run does not start
        /// another workflow.
        pub struct WorkflowCommandEnv<P: Ports> {
            env: ::std::sync::Arc<Env<P>>,
            caller: #context,
        }

        impl<P: Ports> WorkflowCommandEnv<P> {
            fn new(env: ::std::sync::Arc<Env<P>>, caller: #context) -> Self {
                Self { env, caller }
            }
        }

        impl<P: Ports> EnvExt for WorkflowCommandEnv<P> {
            type Ports = P;

            fn ctx(&self) -> &#context {
                &self.caller
            }
        }

        impl<P: Ports> QueryRuntime for WorkflowCommandEnv<P> {
            fn query<Q>(
                &self,
                query: Q,
            ) -> impl ::core::future::Future<
                Output = ::core::result::Result<Q::Output, Q::Error>,
            > + ::core::marker::Send
            where
                Q: Query,
            {
                Query::run(query, self)
            }
        }

        impl<P: Ports> CommandRuntime for WorkflowCommandEnv<P> {
            fn command<C>(
                &self,
                cmd: C,
            ) -> impl ::core::future::Future<
                Output = ::core::result::Result<C::Output, C::Error>,
            > + ::core::marker::Send
            where
                C: Command,
            {
                Command::run(cmd, self)
            }
        }

        impl<P: Ports> QueryEnv for WorkflowCommandEnv<P> {
            #(#query_bodies)*
        }

        impl<P: Ports> CommandEnv for WorkflowCommandEnv<P> {
            #(#command_bodies)*
        }

        impl<P: Ports> WorkflowRuntime for WorkflowCommandEnv<P> {}

        fn into_engine(err: #error) -> ::durare::Error {
            ::durare::Error::app(err.to_string())
        }

        fn from_engine(err: ::durare::Error) -> #error {
            <#error as ::core::convert::From<::durare::Error>>::from(err)
        }

        fn app_error(message: impl ::core::convert::Into<::std::string::String>) -> #error {
            from_engine(::durare::Error::app(message))
        }
    }
}

pub fn service_methods(error: &Type, context: &Type, workflows: &[Path]) -> TokenStream2 {
    let register = workflows.iter().map(|path| {
        quote! {
            {
                let ports = ::std::sync::Arc::clone(&self.shared.env);
                builder.register(
                    <#path as Workflow>::name(),
                    move |durable, invocation: Invocation<#path>| {
                        let env = WorkflowCommandEnv::new(
                            ::std::sync::Arc::clone(&ports),
                            invocation.caller,
                        );
                        let input = invocation.input;
                        async move {
                            let ctx = WorkflowContext::new(
                                durable,
                                <#path as Workflow>::attempt_defaults(),
                            );
                            Workflow::run(input, &ctx, &env).await.map_err(|err| {
                                into_engine(::core::convert::Into::<#error>::into(err))
                            })
                        }
                    },
                );
            }
        }
    });

    quote! {
        /// Register every type in `workflows { ... }` and start the engine.
        ///
        /// A second call fails. The provider stored by the builder is the
        /// state backend.
        pub async fn launch(&self) -> ::core::result::Result<(), #error>
        where
            WorkflowCommandEnv<P>: ::core::marker::Send + ::core::marker::Sync + 'static,
            #context: ::core::clone::Clone
                + ::serde::Serialize
                + ::serde::de::DeserializeOwned
                + ::core::marker::Send
                + ::core::marker::Sync
                + 'static,
            #error: ::core::convert::From<::durare::Error> + ::core::fmt::Display,
        {
            if self.shared.engine.get().is_some() {
                return Err(app_error("workflows already launched"));
            }
            let mut builder = ::durare::DurableEngine::builder(::std::sync::Arc::clone(
                &self.shared.provider,
            ));
            #(#register)*
            let built = builder.build().await.map_err(from_engine)?;
            built.launch().await.map_err(from_engine)?;
            self.shared
                .engine
                .set(built)
                .map_err(|_| app_error("workflows already launched"))?;
            Ok(())
        }

        /// Stop the engine's background tasks and wait up to `timeout` for
        /// workflow tasks. A run that is still going when the wait ends keeps
        /// going. The engine and the ports stay on this service.
        pub async fn shutdown(
            &self,
            timeout: ::std::time::Duration,
        ) -> ::core::result::Result<(), #error>
        where
            #error: ::core::convert::From<::durare::Error>,
        {
            match self.shared.engine.get() {
                Some(engine) => engine.shutdown(timeout).await.map_err(from_engine),
                None => Ok(()),
            }
        }
    }
}

pub fn context_start(error: &Type, context: &Type) -> TokenStream2 {
    quote! {
        impl<'a, P: Ports> WorkflowRuntime for ContextRuntime<'a, P> {
            async fn start_workflow<W: Workflow>(
                &self,
                id: &str,
                input: W,
            ) -> ::core::result::Result<WorkflowRun<W::Output>, #error> {
                let engine = self
                    .engine
                    .get()
                    .ok_or_else(|| app_error("workflows are not launched"))?;
                let handle = engine
                    .start::<_, W::Output>(
                        <W as Workflow>::name(),
                        Invocation {
                            caller: <#context as ::core::clone::Clone>::clone(self.ctx),
                            input,
                        },
                        ::durare::WorkflowOptions::with_id(id),
                    )
                    .await
                    .map_err(from_engine)?;
                Ok(WorkflowRun {
                    id: ::std::string::ToString::to_string(id),
                    inner: RunInner::Live(handle),
                })
            }
        }
    }
}

pub fn mock_pieces() -> (TokenStream2, TokenStream2, TokenStream2) {
    let alias = quote! {
        type ExpectStart<W> = super::__mock_MockWorkflowRuntime_WorkflowRuntime::__start_workflow::Expectation<W>;
    };
    let field = quote! {
        workflows: MockWorkflowRuntime,
    };
    let expect = quote! {
        #[allow(dead_code)]
        pub fn expect_start_workflow<W>(&mut self) -> &mut ExpectStart<W>
        where
            W: Workflow,
        {
            self.workflows.expect_start_workflow::<W>()
        }
    };
    (alias, field, expect)
}

pub fn mock_impl(error: &Type) -> TokenStream2 {
    quote! {
        impl WorkflowRuntime for MockEnv {
            async fn start_workflow<W: Workflow>(
                &self,
                id: &str,
                input: W,
            ) -> ::core::result::Result<WorkflowRun<W::Output>, #error> {
                self.workflows.start_workflow(id, input).await
            }
        }
    }
}
