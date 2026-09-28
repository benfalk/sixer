//! Optional `context = ...` installs one caller context for a service call.

use std::future::Future;
use std::task::{Context as TaskContext, Poll};

use service::Query;

#[sixer::port]
#[cfg_attr(test, mockall::automock)]
pub trait Clock: Send + Sync {
    #[query]
    fn tick(&self) -> u32;
}

#[derive(Debug, Default)]
enum Caller {
    #[default]
    Nobody,
    User(u32),
}

#[derive(Debug)]
struct ServiceError;

struct Tock;

impl Clock for Tock {
    fn tick(&self) -> u32 {
        1
    }
}

sixer::runtime! {
    error = crate::ServiceError,
    context = crate::Caller,
    ports {
        clock: ClockPort: crate::Clock,
    }
}

struct App;

impl Ports for App {
    type ClockPort = Tock;
}

#[derive(Debug)]
struct Who;

#[sixer::query]
impl Who {
    async fn run(self, env: &impl service::QueryEnv) -> Result<u32, ServiceError> {
        let _ = env.clock().tick();
        Ok(match env.ctx() {
            Caller::User(id) => *id,
            Caller::Nobody => 0,
        })
    }
}

#[derive(Debug)]
struct AsCaller;

#[sixer::command]
impl AsCaller {
    async fn run(self, env: &impl service::CommandEnv) -> Result<u32, ServiceError> {
        env.query(Who).await
    }
}

fn ready<T>(future: impl Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    let waker = std::task::Waker::noop();
    let mut context = TaskContext::from_waker(waker);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("use case future stayed pending"),
    }
}

#[test]
fn mock_env_exposes_its_context() {
    let mut env = service::MockEnv::default();
    env.ctx = Caller::User(9);
    env.clock.expect_tick().returning(|| 1);

    let found = ready(Who.run(&env)).expect("who");
    assert_eq!(found, 9);
}

#[test]
fn service_query_uses_the_default_context() {
    let service = Service::<App>::new(Tock);
    let found = ready(service.query(Who)).expect("who");
    assert_eq!(found, 0);
}

#[test]
fn with_context_reaches_a_nested_query() {
    let service = Service::<App>::new(Tock);
    let ctx = Caller::User(4);
    let found = ready(service.with_context(&ctx).command(AsCaller)).expect("as caller");
    assert_eq!(found, 4);
}
