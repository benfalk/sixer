pub use service::{Ports, Service};

mod service {
    pub struct Service<P: Ports> {
        env: ::std::sync::Arc<Env<P>>,
    }

    pub struct ContextRuntime<'a, P: Ports> {
        env: &'a Env<P>,
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

    pub trait QueryEnv: EnvExt + QueryRuntime {
        fn database(&self) -> crate::view::DatabaseQuery<'_, <Self::Ports as Ports>::DB>;
    }

    pub trait CommandEnv: EnvExt + CommandRuntime + QueryRuntime {
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
                env: ::std::sync::Arc::new(Env { database }),
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
                env: self.env.as_ref(),
                ctx,
            }
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
                env: self.env.clone(),
            }
        }
    }

    // Context Runtime Wiring

    struct Env<P: Ports> {
        database: P::DB,
    }

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
            crate::view::DatabaseQuery::new(&self.env.database)
        }
    }

    impl<'a, P: Ports> CommandEnv for ContextRuntime<'a, P> {
        fn database(&self) -> crate::view::DatabaseCommand<'_, <Self::Ports as Ports>::DB> {
            crate::view::DatabaseCommand::new(&self.env.database)
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

        pub fn get(&self, id: u32) -> impl Future<Output = Result<u32, crate::Error>> + Send {
            <T as Database>::get(self.0, id)
        }
    }

    pub struct DatabaseCommand<'a, T: ?Sized>(&'a T);

    impl<'a, T: Database + ?Sized> DatabaseCommand<'a, T> {
        pub fn new(val: &'a T) -> Self {
            Self(val)
        }

        pub fn get(&self, id: u32) -> impl Future<Output = Result<u32, crate::Error>> + Send {
            <T as Database>::get(self.0, id)
        }

        pub fn put(
            &self,
            id: u32,
            val: u32,
        ) -> impl Future<Output = Result<(), crate::Error>> + Send {
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

    #[derive(Default)]
    pub struct MockEnv {
        pub database: MockDatabase,
        pub ctx: crate::Context,
        query_runtime: MockQueryRuntime,
        command_runtime: MockCommandRuntime,
    }

    // Wiring up MockEnv

    impl MockEnv {
        pub fn expect_query<Q: Query>(&mut self) -> &mut QueryExpectation<Q> {
            self.query_runtime.expect_query::<Q>()
        }

        pub fn expect_command<C: Command>(&mut self) -> &mut CommandExpectation<C> {
            self.command_runtime.expect_command::<C>()
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

#[derive(Debug, Default, PartialEq, Eq)]
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
