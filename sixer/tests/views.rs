//! Query views hide command methods. Command views keep both.

use std::future::Future;
use std::task::{Context, Poll};

use service::{Command, Query};

#[sixer::port]
#[cfg_attr(test, mockall::automock)]
pub trait Store: Send + Sync {
    #[query]
    fn get(&self, id: u32) -> u32;

    #[command]
    fn put(&self, id: u32, value: u32);
}

#[derive(Debug)]
struct ServiceError;

sixer::runtime! {
    error = crate::ServiceError,
    ports {
        store: StorePort: crate::Store,
    }
}

#[derive(Debug)]
struct Read(u32);

#[sixer::query]
impl Read {
    async fn run(self, env: &impl service::QueryEnv) -> Result<u32, ServiceError> {
        Ok(env.store().get(self.0))
    }
}

#[derive(Debug)]
struct Write {
    id: u32,
    value: u32,
}

#[sixer::command]
impl Write {
    async fn run(self, env: &impl service::CommandEnv) -> Result<u32, ServiceError> {
        let previous = env.store().get(self.id);
        env.store().put(self.id, self.value);
        Ok(previous)
    }
}

fn ready<T>(future: impl Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    let waker = std::task::Waker::noop();
    let mut context = Context::from_waker(waker);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("use case future stayed pending"),
    }
}

#[test]
fn command_view_can_read_and_query_view_can_read() {
    let mut env = service::MockEnv::default();
    env.store.expect_get().returning(|id| id + 1);
    env.store.expect_put().returning(|_, _| ());

    let previous = ready(Write { id: 1, value: 9 }.run(&env)).expect("write");
    assert_eq!(previous, 2);

    let mut env = service::MockEnv::default();
    env.store.expect_get().returning(|id| id);
    let found = ready(Read(7).run(&env)).expect("read");
    assert_eq!(found, 7);
}
