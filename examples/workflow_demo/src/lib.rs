//! A command starts a durable copy.
//!
//! Queries cannot start one.
//!
//! ```compile_fail,E0599
//! fn start(env: &impl workflow_demo::QueryEnv) {
//!     let _ = env.start_workflow(
//!         "copy:1:2",
//!         workflow_demo::cqrs::Copy { key: 1, dest: 2 },
//!     );
//! }
//! ```

#[derive(Debug, ::thiserror::Error)]
pub enum Error {
    #[error("general error: {0}")]
    General(String),
}

impl From<durare::Error> for Error {
    fn from(err: durare::Error) -> Self {
        Self::General(err.to_string())
    }
}

impl From<port::GetError> for Error {
    fn from(err: port::GetError) -> Self {
        Self::General(err.to_string())
    }
}

impl From<port::PutError> for Error {
    fn from(err: port::PutError) -> Self {
        Self::General(err.to_string())
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Context {
    #[default]
    Nobody,
    User(u32),
    Root,
}

impl Context {
    #[must_use]
    pub fn is_nobody(&self) -> bool {
        *self == Self::Nobody
    }
}

pub mod port {
    #[::sixer::port(async_send)]
    #[cfg_attr(test, ::mockall::automock)]
    pub trait Database: Send + Sync {
        #[query]
        async fn get(&self, id: u32) -> Result<u32, GetError>;

        #[command]
        async fn put(&self, id: u32, val: u32) -> Result<(), PutError>;
    }

    #[derive(Debug, ::thiserror::Error)]
    #[error("getting from database: {0}")]
    pub struct GetError(Box<dyn ::std::error::Error + Send + Sync>);

    impl GetError {
        /// Wrap a failure from the database.
        #[must_use]
        pub fn new(error: impl ::std::error::Error + Send + Sync + 'static) -> Self {
            Self(Box::new(error))
        }

        /// Wrap a failure that is only a message.
        #[must_use]
        pub fn message(message: impl Into<::std::string::String>) -> Self {
            Self::new(Message(message.into()))
        }
    }

    #[derive(Debug, ::thiserror::Error)]
    #[error("putting into database: {0}")]
    pub struct PutError(Box<dyn ::std::error::Error + Send + Sync>);

    impl PutError {
        /// Wrap a failure from the database.
        #[must_use]
        pub fn new(error: impl ::std::error::Error + Send + Sync + 'static) -> Self {
            Self(Box::new(error))
        }

        /// Wrap a failure that is only a message.
        #[must_use]
        pub fn message(message: impl Into<::std::string::String>) -> Self {
            Self::new(Message(message.into()))
        }
    }

    #[derive(Debug, ::thiserror::Error)]
    #[error("{0}")]
    struct Message(String);
}

::sixer::runtime! {
    error = crate::Error,
    context = crate::Context,
    workflow,
    ports {
        database: DB: crate::port::Database,
    },
    workflows {
        crate::cqrs::Copy,
        crate::cqrs::FlakyRead,
        crate::cqrs::GuardedRead,
        crate::cqrs::OnceRead,
    },
}

pub use service::QueryEnv;

pub mod cqrs {
    use crate::Error;
    use crate::port::GetError;
    use crate::port::PutError as PortPut;
    use crate::service::{CallError, Command, CommandEnv, Workflow, WorkflowContext, WorkflowRun};

    /// Failure of [`Put`]. A store failure holds the port error.
    #[derive(Debug, ::thiserror::Error)]
    pub enum PutError {
        #[error("lacks privilege")]
        Privilege,
        #[error(transparent)]
        Store(#[from] PortPut),
    }

    impl From<PutError> for Error {
        fn from(err: PutError) -> Self {
            match err {
                PutError::Privilege => Self::General("lacks privilege".into()),
                PutError::Store(err) => err.into(),
            }
        }
    }

    /// Failure of the [`Copy`] workflow.
    ///
    /// A read or a write holds the port error when that call ran. `Recorded`
    /// is the journal message when a replay did not run the read. The
    /// privilege check is this workflow's own failure. The type converts
    /// into [`Error`].
    #[derive(Debug, ::thiserror::Error)]
    pub enum CopyError {
        #[error("lacks privilege")]
        Privilege,
        #[error(transparent)]
        Read(#[from] GetError),
        #[error(transparent)]
        Write(#[from] PortPut),
        #[error("{0}")]
        Recorded(String),
    }

    impl From<CallError<GetError>> for CopyError {
        fn from(err: CallError<GetError>) -> Self {
            match err {
                CallError::Failed(err) => Self::Read(err),
                CallError::Recorded(message) => Self::Recorded(message),
            }
        }
    }

    impl From<PutError> for CopyError {
        fn from(err: PutError) -> Self {
            match err {
                PutError::Privilege => Self::Privilege,
                PutError::Store(err) => Self::Write(err),
            }
        }
    }

    impl From<CopyError> for Error {
        fn from(err: CopyError) -> Self {
            match err {
                CopyError::Privilege => Self::General("lacks privilege".into()),
                CopyError::Read(err) => err.into(),
                CopyError::Write(err) => err.into(),
                CopyError::Recorded(message) => Self::General(message),
            }
        }
    }

    /// Failure of a read workflow.
    ///
    /// `Read` holds the port error when the read ran. `Recorded` is the
    /// journal message when a replay did not run the read.
    #[derive(Debug, ::thiserror::Error)]
    pub enum ReadError {
        #[error(transparent)]
        Read(#[from] GetError),
        #[error("{0}")]
        Recorded(String),
    }

    impl From<CallError<GetError>> for ReadError {
        fn from(err: CallError<GetError>) -> Self {
            match err {
                CallError::Failed(err) => Self::Read(err),
                CallError::Recorded(message) => Self::Recorded(message),
            }
        }
    }

    impl From<ReadError> for Error {
        fn from(err: ReadError) -> Self {
            match err {
                ReadError::Read(err) => err.into(),
                ReadError::Recorded(message) => Self::General(message),
            }
        }
    }

    #[derive(Debug)]
    pub struct Put {
        pub key: u32,
        pub val: u32,
    }

    #[::sixer::command]
    impl Put {
        async fn run(self, env: &impl CommandEnv) -> Result<(), PutError> {
            if env.ctx().is_nobody() {
                return Err(PutError::Privilege);
            }

            env.database().put(self.key, self.val).await?;
            Ok(())
        }
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct Copy {
        pub key: u32,
        pub dest: u32,
    }

    #[::sixer::command]
    impl Copy {
        async fn run(self, env: &impl CommandEnv) -> Result<WorkflowRun<()>, Error> {
            let id = format!("{}:{}:{}", Self::name(), self.key, self.dest);
            env.start_workflow(&id, self).await
        }
    }

    #[::sixer::workflow("copy")]
    impl Copy {
        async fn run(self, ctx: &WorkflowContext, env: &impl CommandEnv) -> Result<(), CopyError> {
            let key = self.key;
            let dest = self.dest;
            let value = ctx.step("read", env.database().get(key)).await?;
            Put {
                key: dest,
                val: value,
            }
            .run(env)
            .await?;
            Ok(())
        }
    }

    /// Read a key, retrying a downed upstream before the step is saved.
    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct FlakyRead {
        pub key: u32,
    }

    #[::sixer::command]
    impl FlakyRead {
        async fn run(self, env: &impl CommandEnv) -> Result<WorkflowRun<u32>, Error> {
            let id = format!("flaky:{}", self.key);
            env.start_workflow(&id, self).await
        }
    }

    #[::sixer::workflow("flaky_read")]
    #[::sixer::attempt_defaults(max_retries = 3, base_interval = 1ms)]
    impl FlakyRead {
        async fn run(self, ctx: &WorkflowContext, env: &impl CommandEnv) -> Result<u32, ReadError> {
            let key = self.key;
            Ok(ctx.attempt("read").run(|| env.database().get(key)).await?)
        }
    }

    /// Read a key, and refuse to retry a known upstream failure.
    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct GuardedRead {
        pub key: u32,
    }

    #[::sixer::command]
    impl GuardedRead {
        async fn run(self, env: &impl CommandEnv) -> Result<WorkflowRun<u32>, Error> {
            let id = format!("guarded:{}", self.key);
            env.start_workflow(&id, self).await
        }
    }

    #[::sixer::workflow("guarded_read")]
    #[::sixer::attempt_defaults(max_retries = 3, base_interval = 1ms)]
    impl GuardedRead {
        async fn run(self, ctx: &WorkflowContext, env: &impl CommandEnv) -> Result<u32, ReadError> {
            let key = self.key;
            Ok(ctx
                .attempt("read")
                .retry_if(|err| !err.to_string().contains("upstream down"))
                .run(|| env.database().get(key))
                .await?)
        }
    }

    /// Read a key once. The workflow keeps the trait's attempt defaults.
    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub struct OnceRead {
        pub key: u32,
    }

    #[::sixer::command]
    impl OnceRead {
        async fn run(self, env: &impl CommandEnv) -> Result<WorkflowRun<u32>, Error> {
            let id = format!("once:{}", self.key);
            env.start_workflow(&id, self).await
        }
    }

    #[::sixer::workflow("once_read")]
    impl OnceRead {
        async fn run(self, ctx: &WorkflowContext, env: &impl CommandEnv) -> Result<u32, ReadError> {
            let key = self.key;
            Ok(ctx.attempt("read").run(|| env.database().get(key)).await?)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use durare::InMemoryProvider;
    use tokio::sync::RwLock;

    use crate::cqrs::{Copy, CopyError, FlakyRead, GuardedRead, OnceRead, ReadError};
    use crate::port::{Database, GetError, PutError};
    use crate::service::{CallError, Command, MockEnv, Ports, run_workflow};
    use crate::{Context, Error, Service, WorkflowRun};

    #[derive(Clone, Default)]
    struct CountingDb {
        rows: Arc<RwLock<HashMap<u32, u32>>>,
        reads: Arc<AtomicUsize>,
        writes: Arc<AtomicUsize>,
        fail_gets: Arc<AtomicUsize>,
    }

    impl CountingDb {
        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }

        fn writes(&self) -> usize {
            self.writes.load(Ordering::SeqCst)
        }

        fn fail_next_gets(&self, n: usize) {
            self.fail_gets.store(n, Ordering::SeqCst);
        }

        async fn row(&self, key: u32) -> Option<u32> {
            self.rows.read().await.get(&key).copied()
        }
    }

    impl Database for CountingDb {
        async fn get(&self, id: u32) -> Result<u32, GetError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            let failing = self
                .fail_gets
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1));
            if failing.is_ok() {
                return Err(GetError::message("upstream down"));
            }
            let rows = self.rows.read().await;
            rows.get(&id)
                .copied()
                .ok_or_else(|| GetError::message("missing number"))
        }

        async fn put(&self, id: u32, val: u32) -> Result<(), PutError> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.rows.write().await.insert(id, val);
            Ok(())
        }
    }

    struct CountingPorts;

    impl Ports for CountingPorts {
        type DB = CountingDb;
    }

    /// `thiserror`'s `#[from]` accepts `CallError` because it implements
    /// `std::error::Error`.
    #[derive(Debug, thiserror::Error)]
    enum WrappedCall {
        #[error("port call failed")]
        Port(#[from] CallError<GetError>),
    }

    #[test]
    fn call_error_implements_error() {
        let failed = CallError::Failed(GetError::message("upstream down"));
        let wrapped = WrappedCall::from(failed);
        let call = std::error::Error::source(&wrapped)
            .expect("the wrapper exposes the call error")
            .downcast_ref::<CallError<GetError>>()
            .expect("source is the call error");
        let port = std::error::Error::source(call).expect("Failed exposes the port error");
        assert_eq!(port.to_string(), "getting from database: upstream down");

        let recorded = WrappedCall::from(CallError::<GetError>::Recorded("saved".to_string()));
        let source = std::error::Error::source(&recorded)
            .expect("the wrapper exposes the call error")
            .downcast_ref::<CallError<GetError>>()
            .expect("source is the call error");
        assert!(std::error::Error::source(source).is_none());
        assert_eq!(source.to_string(), "saved");
    }

    async fn launched(db: CountingDb) -> Result<Service<CountingPorts>, Error> {
        let service = Service::<CountingPorts>::builder()
            .database(db)
            .workflow_provider(Arc::new(InMemoryProvider::new()))
            .build();
        service.launch().await?;
        Ok(service)
    }

    #[tokio::test]
    async fn copy_command_uses_the_mocked_start() -> Result<(), Error> {
        let mut env = MockEnv::default();
        env.expect_start_workflow::<Copy>()
            .withf(|id, copy| id == "copy:7:8" && copy.key == 7 && copy.dest == 8)
            .returning(|id, _| {
                let id = (*id).to_owned();
                Box::pin(std::future::ready(Ok(WorkflowRun::ready(id, ()))))
            });

        let run = Copy { key: 7, dest: 8 }.run(&env).await?;
        assert_eq!(run.id(), "copy:7:8");
        run.result().await?;
        Ok(())
    }

    #[tokio::test]
    async fn copy_workflow_reads_and_puts_through_the_mock() -> Result<(), Error> {
        let mut env = MockEnv::default();
        env.ctx = Context::Root;
        env.database
            .expect_get()
            .once()
            .withf(|id| *id == 7)
            .returning(|_| Box::pin(std::future::ready(Ok(11))));
        env.database
            .expect_put()
            .once()
            .withf(|id, val| *id == 8 && *val == 11)
            .returning(|_, _| Box::pin(std::future::ready(Ok(()))));

        run_workflow(&env, Copy { key: 7, dest: 8 }).await?;
        Ok(())
    }

    #[tokio::test]
    async fn nobody_workflow_stops_before_the_write() {
        let mut env = MockEnv::default();
        env.ctx = Context::Nobody;
        env.database
            .expect_get()
            .once()
            .returning(|_| Box::pin(std::future::ready(Ok(11))));

        let err: CopyError = run_workflow(&env, Copy { key: 7, dest: 8 })
            .await
            .expect_err("nobody");
        assert!(err.to_string().contains("lacks privilege"), "{err}");
    }

    #[tokio::test]
    async fn flaky_workflow_retries_against_the_mock() -> Result<(), Error> {
        let mut env = MockEnv::default();
        let remaining = Arc::new(AtomicUsize::new(2));
        env.database.expect_get().times(3).returning(move |_| {
            let fail = remaining
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok();
            Box::pin(std::future::ready(if fail {
                Err(GetError::message("upstream down"))
            } else {
                Ok(11)
            }))
        });

        let value = run_workflow(&env, FlakyRead { key: 7 }).await?;
        assert_eq!(value, 11);
        Ok(())
    }

    #[tokio::test]
    async fn guarded_workflow_does_not_retry_a_known_failure() {
        let mut env = MockEnv::default();
        env.database
            .expect_get()
            .once()
            .returning(|_| Box::pin(std::future::ready(Err(GetError::message("upstream down")))));

        let err: ReadError = run_workflow(&env, GuardedRead { key: 7 })
            .await
            .expect_err("stopped");
        assert!(err.to_string().contains("getting from database"), "{err}");
        assert!(err.to_string().contains("upstream down"), "{err}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn root_copies_once_and_a_second_start_reuses_the_run() -> Result<(), Error> {
        let db = CountingDb::default();
        db.put(7, 11).await?;
        let service = launched(db.clone()).await?;
        let ctx = Context::Root;

        let first = service
            .with_context(&ctx)
            .command(Copy { key: 7, dest: 8 })
            .await?;
        assert_eq!(first.id(), "copy:7:8");
        first.result().await?;

        let second = service
            .with_context(&ctx)
            .command(Copy { key: 7, dest: 8 })
            .await?;
        second.result().await?;

        assert_eq!(db.row(8).await, Some(11));
        assert_eq!(db.reads(), 1);
        assert_eq!(db.writes(), 2);
        service.shutdown(Duration::from_secs(1)).await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nobody_is_refused_by_put_before_the_write() -> Result<(), Error> {
        let db = CountingDb::default();
        db.put(7, 11).await?;
        let service = launched(db.clone()).await?;
        let ctx = Context::Nobody;

        let run = service
            .with_context(&ctx)
            .command(Copy { key: 7, dest: 8 })
            .await?;
        let err = run.result().await.expect_err("nobody cannot put");
        assert!(err.to_string().contains("lacks privilege"), "{err}");

        let again = service
            .with_context(&ctx)
            .command(Copy { key: 7, dest: 8 })
            .await?;
        let err = again.result().await.expect_err("replayed refusal");
        assert!(err.to_string().contains("lacks privilege"), "{err}");

        assert_eq!(db.row(8).await, None);
        assert_eq!(db.reads(), 1);
        assert_eq!(db.writes(), 1);
        service.shutdown(Duration::from_secs(1)).await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_flaky_read_retries_until_it_succeeds() -> Result<(), Error> {
        let db = CountingDb::default();
        db.put(7, 11).await?;
        db.fail_next_gets(2);
        let service = launched(db.clone()).await?;

        let run = service
            .with_context(&Context::Root)
            .command(FlakyRead { key: 7 })
            .await?;
        assert_eq!(run.result().await?, 11);
        assert_eq!(db.reads(), 3);

        let again = service
            .with_context(&Context::Root)
            .command(FlakyRead { key: 7 })
            .await?;
        assert_eq!(again.result().await?, 11);
        assert_eq!(db.reads(), 3);
        service.shutdown(Duration::from_secs(1)).await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_flaky_read_keeps_the_error_after_retries_are_spent() -> Result<(), Error> {
        let db = CountingDb::default();
        db.put(7, 11).await?;
        db.fail_next_gets(10);
        let service = launched(db.clone()).await?;

        let run = service
            .with_context(&Context::Root)
            .command(FlakyRead { key: 7 })
            .await?;
        let err = run.result().await.expect_err("retries exhausted");
        assert!(err.to_string().contains("upstream down"), "{err}");
        assert_eq!(db.reads(), 4);

        let again = service
            .with_context(&Context::Root)
            .command(FlakyRead { key: 7 })
            .await?;
        let err = again.result().await.expect_err("stored failure");
        assert!(err.to_string().contains("upstream down"), "{err}");
        assert_eq!(db.reads(), 4);
        service.shutdown(Duration::from_secs(1)).await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_predicate_stops_retries_for_a_known_failure() -> Result<(), Error> {
        let db = CountingDb::default();
        db.put(7, 11).await?;
        db.fail_next_gets(5);
        let service = launched(db.clone()).await?;

        let run = service
            .with_context(&Context::Root)
            .command(GuardedRead { key: 7 })
            .await?;
        let err = run.result().await.expect_err("predicate rejects");
        assert!(err.to_string().contains("upstream down"), "{err}");
        assert_eq!(db.reads(), 1);
        service.shutdown(Duration::from_secs(1)).await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_attempt_without_overrides_runs_once() -> Result<(), Error> {
        let db = CountingDb::default();
        db.put(7, 11).await?;
        db.fail_next_gets(3);
        let service = launched(db.clone()).await?;

        let run = service
            .with_context(&Context::Root)
            .command(OnceRead { key: 7 })
            .await?;
        let err = run.result().await.expect_err("no retries");
        assert!(err.to_string().contains("upstream down"), "{err}");
        assert_eq!(db.reads(), 1);
        service.shutdown(Duration::from_secs(1)).await
    }
}
