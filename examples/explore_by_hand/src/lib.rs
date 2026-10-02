pub mod durable;

pub use service::{Ports, Service};

mod service {
    pub struct Service<P: Ports> {
        shared: ::std::sync::Arc<Shared<P>>,
    }

    pub struct ContextRuntime<'a, P: Ports> {
        shared: &'a Shared<P>,
        ctx: &'a crate::Context,
    }

    pub trait Ports {
        type DB: crate::port::Database;
    }

    pub trait EnvExt: Send + Sync {
        type Ports: Ports;

        fn ctx(&self) -> &crate::Context;
    }

    #[cfg_attr(test, ::mockall::automock)]
    pub trait QueryRuntime {
        fn query<Q>(&self, query: Q) -> impl Future<Output = Result<Q::Value, Q::Error>> + Send
        where
            Q: Query;
    }

    #[cfg_attr(test, ::mockall::automock)]
    pub trait CommandRuntime {
        fn command<C>(&self, cmd: C) -> impl Future<Output = Result<C::Value, C::Error>> + Send
        where
            C: Command;
    }

    /// Start a workflow already registered on this service.
    ///
    /// Queries do not have this method. The service command env is the
    /// implementation that reaches the launched engine. Other command envs,
    /// including the one inside a running workflow, return an error. A test
    /// replaces that call through `MockEnv::expect_start_workflow`.
    #[cfg_attr(test, ::mockall::automock)]
    pub trait WorkflowRuntime {
        fn start_workflow<W: crate::durable::Workflow>(
            &self,
            id: &str,
            input: W,
        ) -> impl Future<Output = Result<crate::durable::WorkflowRun<W::Output>, crate::Error>> + Send
        {
            let _ = (id, input);
            async {
                Err(crate::Error::General(
                    "workflows are started by a launched service".into(),
                ))
            }
        }
    }

    pub trait QueryEnv: EnvExt + QueryRuntime {
        fn database(&self) -> crate::view::DatabaseQuery<'_, <Self::Ports as Ports>::DB>;
    }

    pub trait CommandEnv: EnvExt + CommandRuntime + QueryRuntime + WorkflowRuntime {
        fn database(&self) -> crate::view::DatabaseCommand<'_, <Self::Ports as Ports>::DB>;
    }

    pub trait Query: Send + Sync + 'static {
        type Value;
        type Error: Into<crate::Error>;

        fn run(
            self,
            env: &impl QueryEnv,
        ) -> impl Future<Output = Result<Self::Value, Self::Error>> + Send;
    }

    pub trait Command: Send + Sync + 'static {
        type Value;
        type Error: Into<crate::Error>;

        fn run(
            self,
            env: &impl CommandEnv,
        ) -> impl Future<Output = Result<Self::Value, Self::Error>> + Send;
    }

    // Service Wiring

    impl<P: Ports> Service<P> {
        pub fn new(database: P::DB) -> Self {
            Self {
                shared: ::std::sync::Arc::new(Shared {
                    env: ::std::sync::Arc::new(Env { database }),
                    engine: ::std::sync::OnceLock::new(),
                }),
            }
        }

        pub async fn command<C: Command>(&self, cmd: C) -> Result<C::Value, C::Error> {
            let ctx = crate::Context::default();
            self.with_context(&ctx).command(cmd).await
        }

        pub async fn query<Q: Query>(&self, cmd: Q) -> Result<Q::Value, Q::Error> {
            let ctx = crate::Context::default();
            self.with_context(&ctx).query(cmd).await
        }

        #[must_use]
        pub fn with_context<'a>(&'a self, ctx: &'a crate::Context) -> ContextRuntime<'a, P> {
            ContextRuntime {
                shared: self.shared.as_ref(),
                ctx,
            }
        }

        /// Register the long-lived workflows this service can start.
        ///
        /// A command reaches them through [`WorkflowRuntime::start_workflow`].
        pub fn workflows(&self) -> crate::durable::WorkflowBuilder<'_, P> {
            crate::durable::WorkflowBuilder::new(
                ::std::sync::Arc::clone(&self.shared.env),
                &self.shared.engine,
            )
        }

        pub async fn shutdown(&self) -> Result<(), crate::Error> {
            crate::durable::shutdown(self.shared.engine.get()).await
        }
    }

    impl<P: Ports> Default for Service<P>
    where
        P::DB: Default,
    {
        fn default() -> Self {
            Self::new(P::DB::default())
        }
    }

    impl<P: Ports> Clone for Service<P> {
        fn clone(&self) -> Self {
            Self {
                shared: self.shared.clone(),
            }
        }
    }

    // Context Runtime Wiring

    pub(crate) struct Shared<P: Ports> {
        env: ::std::sync::Arc<Env<P>>,
        engine: ::std::sync::OnceLock<durare::DurableEngine>,
    }

    pub(crate) struct Env<P: Ports> {
        database: P::DB,
    }

    /// Owned command env for a workflow run.
    ///
    /// The command's borrow ends when the command returns. This value keeps
    /// the same ports and the caller that command recorded.
    pub struct WorkflowCommandEnv<P: Ports> {
        env: ::std::sync::Arc<Env<P>>,
        caller: crate::Context,
    }

    impl<P: Ports> WorkflowCommandEnv<P> {
        pub(crate) fn new(env: ::std::sync::Arc<Env<P>>, caller: crate::Context) -> Self {
            Self { env, caller }
        }
    }

    impl<P: Ports> EnvExt for WorkflowCommandEnv<P> {
        type Ports = P;

        fn ctx(&self) -> &crate::Context {
            &self.caller
        }
    }

    impl<P: Ports> QueryRuntime for WorkflowCommandEnv<P> {
        async fn query<Q>(&self, query: Q) -> Result<Q::Value, Q::Error>
        where
            Q: Query,
        {
            query.run(self).await
        }
    }

    impl<P: Ports> CommandRuntime for WorkflowCommandEnv<P> {
        async fn command<C>(&self, cmd: C) -> Result<C::Value, C::Error>
        where
            C: Command,
        {
            cmd.run(self).await
        }
    }

    impl<P: Ports> QueryEnv for WorkflowCommandEnv<P> {
        fn database(&self) -> crate::view::DatabaseQuery<'_, <Self::Ports as Ports>::DB> {
            crate::view::DatabaseQuery::new(&self.env.database)
        }
    }

    impl<P: Ports> CommandEnv for WorkflowCommandEnv<P> {
        fn database(&self) -> crate::view::DatabaseCommand<'_, <Self::Ports as Ports>::DB> {
            crate::view::DatabaseCommand::new(&self.env.database)
        }
    }

    // The default start_workflow refuses, so a run cannot start another one.
    impl<P: Ports> WorkflowRuntime for WorkflowCommandEnv<P> {}

    impl<'a, P: Ports> ContextRuntime<'a, P> {
        pub async fn command<C: Command>(&self, cmd: C) -> Result<C::Value, C::Error> {
            cmd.run(self).await
        }

        pub async fn query<Q: Query>(&self, cmd: Q) -> Result<Q::Value, Q::Error> {
            cmd.run(self).await
        }
    }

    impl<'a, P: Ports> EnvExt for ContextRuntime<'a, P> {
        type Ports = P;

        fn ctx(&self) -> &crate::Context {
            self.ctx
        }
    }

    impl<'a, P: Ports> QueryRuntime for ContextRuntime<'a, P> {
        async fn query<Q>(&self, query: Q) -> Result<Q::Value, Q::Error>
        where
            Q: Query,
        {
            query.run(self).await
        }
    }

    impl<'a, P: Ports> CommandRuntime for ContextRuntime<'a, P> {
        async fn command<C>(&self, cmd: C) -> Result<C::Value, C::Error>
        where
            C: Command,
        {
            cmd.run(self).await
        }
    }

    impl<'a, P: Ports> QueryEnv for ContextRuntime<'a, P> {
        fn database(&self) -> crate::view::DatabaseQuery<'_, <Self::Ports as Ports>::DB> {
            crate::view::DatabaseQuery::new(&self.shared.env.database)
        }
    }

    impl<'a, P: Ports> CommandEnv for ContextRuntime<'a, P> {
        fn database(&self) -> crate::view::DatabaseCommand<'_, <Self::Ports as Ports>::DB> {
            crate::view::DatabaseCommand::new(&self.shared.env.database)
        }
    }

    impl<'a, P: Ports> WorkflowRuntime for ContextRuntime<'a, P> {
        async fn start_workflow<W: crate::durable::Workflow>(
            &self,
            id: &str,
            input: W,
        ) -> Result<crate::durable::WorkflowRun<W::Output>, crate::Error> {
            let engine = self
                .shared
                .engine
                .get()
                .ok_or_else(|| crate::Error::General("workflows are not launched".into()))?;
            crate::durable::start(engine, id, *self.ctx, input).await
        }
    }
}

pub mod port {
    #[cfg_attr(test, ::mockall::automock)]
    pub trait Database: Send + Sync {
        fn get(&self, id: u32) -> impl Future<Output = Result<u32, crate::Error>> + Send;
        fn put(&self, id: u32, val: u32) -> impl Future<Output = Result<(), crate::Error>> + Send;
    }
}

mod view {
    use crate::port::Database;

    pub struct DatabaseQuery<'a, T: ?Sized>(&'a T);

    impl<'a, T: Database + ?Sized> DatabaseQuery<'a, T> {
        pub fn new(val: &'a T) -> Self {
            Self(val)
        }

        pub fn get(
            &self,
            id: u32,
        ) -> impl Future<Output = Result<u32, crate::Error>> + Send + use<'a, T> {
            <T as Database>::get(self.0, id)
        }
    }

    pub struct DatabaseCommand<'a, T: ?Sized>(&'a T);

    impl<'a, T: Database + ?Sized> DatabaseCommand<'a, T> {
        pub fn new(val: &'a T) -> Self {
            Self(val)
        }

        pub fn get(
            &self,
            id: u32,
        ) -> impl Future<Output = Result<u32, crate::Error>> + Send + use<'a, T> {
            <T as Database>::get(self.0, id)
        }

        pub fn put(
            &self,
            id: u32,
            val: u32,
        ) -> impl Future<Output = Result<(), crate::Error>> + Send + use<'a, T> {
            <T as Database>::put(self.0, id, val)
        }
    }
}

#[cfg(test)]
pub mod support {
    #![allow(dead_code)]
    use crate::port::MockDatabase;
    use crate::service::*;

    type CommandExpectation<C> =
        crate::service::__mock_MockCommandRuntime_CommandRuntime::__command::Expectation<C>;
    type QueryExpectation<Q> =
        crate::service::__mock_MockQueryRuntime_QueryRuntime::__query::Expectation<Q>;
    type StartExpectation<W> =
        crate::service::__mock_MockWorkflowRuntime_WorkflowRuntime::__start_workflow::Expectation<
            W,
        >;

    #[derive(Default)]
    pub struct MockEnv {
        pub database: MockDatabase,
        pub ctx: crate::Context,
        query_runtime: MockQueryRuntime,
        command_runtime: MockCommandRuntime,
        workflow_runtime: MockWorkflowRuntime,
    }

    // Wiring up MockEnv

    impl MockEnv {
        pub fn expect_query<Q: Query>(&mut self) -> &mut QueryExpectation<Q> {
            self.query_runtime.expect_query::<Q>()
        }

        pub fn expect_command<C: Command>(&mut self) -> &mut CommandExpectation<C> {
            self.command_runtime.expect_command::<C>()
        }

        pub fn expect_start_workflow<W: crate::durable::Workflow>(
            &mut self,
        ) -> &mut StartExpectation<W> {
            self.workflow_runtime.expect_start_workflow::<W>()
        }
    }

    impl Ports for MockEnv {
        type DB = MockDatabase;
    }

    impl EnvExt for MockEnv {
        type Ports = Self;

        fn ctx(&self) -> &crate::Context {
            &self.ctx
        }
    }

    impl QueryRuntime for MockEnv {
        async fn query<Q>(&self, query: Q) -> Result<Q::Value, Q::Error>
        where
            Q: Query,
        {
            self.query_runtime.query(query).await
        }
    }

    impl CommandRuntime for MockEnv {
        async fn command<C>(&self, cmd: C) -> Result<C::Value, C::Error>
        where
            C: Command,
        {
            self.command_runtime.command(cmd).await
        }
    }

    impl WorkflowRuntime for MockEnv {
        async fn start_workflow<W: crate::durable::Workflow>(
            &self,
            id: &str,
            input: W,
        ) -> Result<crate::durable::WorkflowRun<W::Output>, crate::Error> {
            self.workflow_runtime.start_workflow(id, input).await
        }
    }

    impl QueryEnv for MockEnv {
        fn database(&self) -> crate::view::DatabaseQuery<'_, <Self::Ports as Ports>::DB> {
            crate::view::DatabaseQuery::new(&self.database)
        }
    }

    impl CommandEnv for MockEnv {
        fn database(&self) -> crate::view::DatabaseCommand<'_, <Self::Ports as Ports>::DB> {
            crate::view::DatabaseCommand::new(&self.database)
        }
    }
}

#[derive(Debug, ::thiserror::Error)]
pub enum Error {
    #[error("general error: {0}")]
    General(String),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Context {
    #[default]
    Nobody,
    User(u32),
    Root,
}

impl Context {
    pub fn is_nobody(&self) -> bool {
        *self == Self::Nobody
    }
}

pub mod cqrs {
    use crate::service::{Command, CommandEnv, Query, QueryEnv};

    pub struct Fetch(pub u32);

    impl Query for Fetch {
        type Value = u32;
        type Error = crate::Error;

        async fn run(self, env: &impl QueryEnv) -> Result<Self::Value, Self::Error> {
            env.database().get(self.0).await
        }
    }

    pub struct Put {
        pub key: u32,
        pub val: u32,
    }

    impl Command for Put {
        type Value = ();
        type Error = crate::Error;

        async fn run(self, env: &impl CommandEnv) -> Result<Self::Value, Self::Error> {
            if env.ctx().is_nobody() {
                return Err(crate::Error::General("lacks privledge".into()));
            }

            env.database().put(self.key, self.val).await
        }
    }
}

#[cfg(test)]
mod test {
    use crate::cqrs::*;
    use crate::service::{Command, Query};
    use crate::support::MockEnv;

    #[::tokio::test]
    async fn fetch_works() {
        let mut env = MockEnv::default();
        env.database
            .expect_get()
            .withf(|id| *id == 42)
            .returning(|_| Box::pin(::std::future::ready(Ok(13))));

        let fetch = Fetch(42);
        let val = fetch.run(&env).await.expect("a value");
        assert_eq!(val, 13);
    }

    #[::tokio::test]
    async fn put_works() {
        let mut env = MockEnv::default();
        env.ctx = crate::Context::Root;
        env.database
            .expect_put()
            .withf(|key, val| *key == 13 && *val == 42)
            .returning(|_, _| Box::pin(::std::future::ready(Ok(()))));

        let put = Put { key: 13, val: 42 };
        put.run(&env).await.expect("put to succeed");
    }
}
