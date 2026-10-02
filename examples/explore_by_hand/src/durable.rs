//! A command starts a long-lived workflow on the service's command env.
//!
//! The service launches the workflows it knows. [`Copy`] is a command, so a
//! query cannot start one. The command returns a [`WorkflowRun`] and does not
//! wait for the run to finish.
//!
//! The value that implements [`Workflow`] is the input. [`Copy::run`] receives
//! that value, a [`WorkflowContext`], and `&impl CommandEnv`: the same ports
//! and the same caller as the command, owned for the life of the run.
//! [`WorkflowContext::step`] records the port calls that run wants durare to
//! keep. A port call left unwrapped runs against that env.
//!
//! A later start of a finished id attaches to that run. A crash while the
//! run is still pending re-enters the workflow; a recorded step returns its
//! stored result, and an unwrapped call runs again. [`durare::Error`] keeps
//! the message of a failed step. The source error does not survive that
//! checkpoint.

use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use durare::{DurableContext, DurableEngine, InMemoryProvider, WorkflowHandle, WorkflowOptions};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub use crate::service::WorkflowCommandEnv;
use crate::service::{Command, CommandEnv, Env, Ports};
use crate::{Context, Error};

/// A workflow a command can start.
///
/// The implementing value is the input durare stores for the run.
/// [`NAME`](Workflow::NAME) is the engine registration.
/// [`WorkflowBuilder::workflow`] calls [`run`](Workflow::run) with that value,
/// the run's [`WorkflowContext`], and the command env owned by the run.
pub trait Workflow: Serialize + DeserializeOwned + Send + Sync + 'static {
    const NAME: &'static str;

    type Output: Serialize + DeserializeOwned + Send + 'static;

    fn run(
        self,
        ctx: &WorkflowContext,
        env: &impl CommandEnv,
    ) -> impl Future<Output = Result<Self::Output, Error>> + Send;
}

/// Durare's context for one workflow run.
///
/// [`step`](Self::step) records a port call. The durare context stays inside
/// this type.
pub struct WorkflowContext {
    durable: DurableContext,
}

impl WorkflowContext {
    fn new(durable: DurableContext) -> Self {
        Self { durable }
    }

    /// Record the result of one port call under `name`.
    pub fn step<'a, T, Fut>(
        &'a self,
        name: &'a str,
        call: Fut,
    ) -> impl Future<Output = Result<T, Error>> + Send + 'a
    where
        T: Serialize + DeserializeOwned + Send,
        Fut: Future<Output = Result<T, Error>> + Send + 'a,
    {
        let durable = self.durable.clone();
        async move {
            durable
                .step(name, || async move { call.await.map_err(into_engine) })
                .await
                .map_err(from_engine)
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Invocation<I> {
    caller: Context,
    input: I,
}

pub struct WorkflowRun<O> {
    id: String,
    inner: RunInner<O>,
}

enum RunInner<O> {
    Live(WorkflowHandle<O>),
    #[cfg(test)]
    Ready(O),
}

impl<O> WorkflowRun<O> {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// A finished run for a test that mocks the start of a workflow.
    #[cfg(test)]
    pub fn ready(id: impl Into<String>, output: O) -> Self {
        Self {
            id: id.into(),
            inner: RunInner::Ready(output),
        }
    }
}

impl<O: DeserializeOwned + 'static> WorkflowRun<O> {
    pub async fn result(self) -> Result<O, Error> {
        match self.inner {
            RunInner::Live(handle) => handle.await.map_err(from_engine),
            #[cfg(test)]
            RunInner::Ready(output) => Ok(output),
        }
    }
}

impl Command for Copy {
    type Value = WorkflowRun<<Self as Workflow>::Output>;
    type Error = Error;

    async fn run(self, env: &impl CommandEnv) -> Result<Self::Value, Self::Error> {
        let id = format!("{}:{}:{}", Self::NAME, self.key, self.dest);
        env.start_workflow(&id, self).await
    }
}

/// Read the source in a step, then `Put` on the command env.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Copy {
    pub key: u32,
    pub dest: u32,
}

impl Workflow for Copy {
    const NAME: &'static str = "copy";

    type Output = ();

    async fn run(self, ctx: &WorkflowContext, env: &impl CommandEnv) -> Result<(), Error> {
        let key = self.key;
        let dest = self.dest;
        let value = ctx.step("read", env.database().get(key)).await?;
        crate::cqrs::Put {
            key: dest,
            val: value,
        }
        .run(env)
        .await
    }
}

#[must_use = "call launch before starting a workflow from a command"]
pub struct WorkflowBuilder<'a, P: Ports> {
    env: Arc<Env<P>>,
    slot: &'a OnceLock<DurableEngine>,
    engine: durare::DurableEngineBuilder,
}

impl<'a, P: Ports> WorkflowBuilder<'a, P> {
    pub(crate) fn new(env: Arc<Env<P>>, slot: &'a OnceLock<DurableEngine>) -> Self {
        Self {
            env,
            slot,
            engine: DurableEngine::builder(Arc::new(InMemoryProvider::new())),
        }
    }

    /// Register one workflow the service's commands can start.
    ///
    /// The owned command env moves into the spawned run, so the ports it
    /// holds are `'static`.
    pub fn workflow<W: Workflow>(mut self) -> Self
    where
        WorkflowCommandEnv<P>: 'static,
    {
        let ports = Arc::clone(&self.env);
        self.engine
            .register(W::NAME, move |durable, invocation: Invocation<W>| {
                let env = WorkflowCommandEnv::new(Arc::clone(&ports), invocation.caller);
                let input = invocation.input;
                async move {
                    let ctx = WorkflowContext::new(durable);
                    input.run(&ctx, &env).await.map_err(into_engine)
                }
            });
        self
    }

    pub async fn launch(self) -> Result<(), Error> {
        if self.slot.get().is_some() {
            return Err(Error::General("workflows already launched".into()));
        }
        let engine = self.engine.build().await.map_err(from_engine)?;
        engine.launch().await.map_err(from_engine)?;
        self.slot
            .set(engine)
            .map_err(|_| Error::General("workflows already launched".into()))?;
        Ok(())
    }
}

pub(crate) async fn start<W: Workflow>(
    engine: &DurableEngine,
    id: &str,
    caller: Context,
    input: W,
) -> Result<WorkflowRun<W::Output>, Error> {
    let handle = engine
        .start::<_, W::Output>(
            W::NAME,
            Invocation { caller, input },
            WorkflowOptions::with_id(id),
        )
        .await
        .map_err(from_engine)?;
    Ok(WorkflowRun {
        id: id.to_owned(),
        inner: RunInner::Live(handle),
    })
}

pub(crate) async fn shutdown(engine: Option<&DurableEngine>) -> Result<(), Error> {
    match engine {
        Some(engine) => engine
            .shutdown(Duration::from_secs(1))
            .await
            .map_err(from_engine),
        None => Ok(()),
    }
}

fn into_engine(err: Error) -> durare::Error {
    durare::Error::app(err.to_string())
}

fn from_engine(err: durare::Error) -> Error {
    Error::General(err.to_string())
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::RwLock;

    use super::{Copy, WorkflowRun};
    use crate::port::Database;
    use crate::service::{Command, Ports};
    use crate::support::MockEnv;
    use crate::{Context, Error, Service};

    #[derive(Clone, Default)]
    struct CountingDb {
        rows: Arc<RwLock<HashMap<u32, u32>>>,
        reads: Arc<AtomicUsize>,
        writes: Arc<AtomicUsize>,
    }

    impl CountingDb {
        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }

        fn writes(&self) -> usize {
            self.writes.load(Ordering::SeqCst)
        }

        async fn row(&self, key: u32) -> Option<u32> {
            self.rows.read().await.get(&key).copied()
        }
    }

    impl Database for CountingDb {
        async fn get(&self, id: u32) -> Result<u32, Error> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            let rows = self.rows.read().await;
            rows.get(&id)
                .copied()
                .ok_or_else(|| Error::General("missing number".into()))
        }

        async fn put(&self, id: u32, val: u32) -> Result<(), Error> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.rows.write().await.insert(id, val);
            Ok(())
        }
    }

    struct CountingPorts;

    impl Ports for CountingPorts {
        type DB = CountingDb;
    }

    async fn launched(db: CountingDb) -> Result<Service<CountingPorts>, Error> {
        let service = Service::new(db);
        service.workflows().workflow::<Copy>().launch().await?;
        Ok(service)
    }

    #[tokio::test]
    async fn copy_command_uses_the_mocked_start() -> Result<(), Error> {
        let mut env = MockEnv::default();
        env.expect_start_workflow::<Copy>()
            .withf(|id, copy| id == "copy:7:8" && copy.key == 7 && copy.dest == 8)
            .returning(|id, _| {
                let id = (*id).to_owned();
                Box::pin(::std::future::ready(Ok(WorkflowRun::ready(id, ()))))
            });

        let run = Copy { key: 7, dest: 8 }.run(&env).await?;
        assert_eq!(run.id(), "copy:7:8");
        run.result().await?;
        Ok(())
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
        service.shutdown().await
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
        assert!(err.to_string().contains("lacks privledge"), "{err}");

        let again = service
            .with_context(&ctx)
            .command(Copy { key: 7, dest: 8 })
            .await?;
        let err = again.result().await.expect_err("replayed refusal");
        assert!(err.to_string().contains("lacks privledge"), "{err}");

        assert_eq!(db.row(8).await, None);
        assert_eq!(db.reads(), 1);
        assert_eq!(db.writes(), 1);
        service.shutdown().await
    }
}
