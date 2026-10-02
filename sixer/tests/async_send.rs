//! `#[sixer::port(async_send)]` rewrites `async fn` methods to `impl Future + Send`.

use std::future::Future;
use std::task::{Context, Poll};

use service::{Command, Query};

#[sixer::port(async_send)]
#[cfg_attr(test, mockall::automock)]
pub trait Mailbox: Send + Sync {
    #[query]
    async fn get(&self, id: u32) -> Option<u32>;

    #[command]
    fn stamp(&self) -> u32;
}

struct Memory;

impl Mailbox for Memory {
    async fn get(&self, id: u32) -> Option<u32> {
        Some(id)
    }

    fn stamp(&self) -> u32 {
        7
    }
}

#[derive(Debug)]
struct ServiceError;

sixer::runtime! {
    error = crate::ServiceError,
    ports {
        mailbox: Mail: crate::Mailbox,
    }
}

#[derive(Debug)]
struct Read(u32);

#[sixer::query]
impl Read {
    async fn run(self, env: &impl service::QueryEnv) -> Result<Option<u32>, ServiceError> {
        Ok(env.mailbox().get(self.0).await)
    }
}

#[derive(Debug)]
struct Write(u32);

#[sixer::command]
impl Write {
    async fn run(self, env: &impl service::CommandEnv) -> Result<u32, ServiceError> {
        let found = env.mailbox().get(self.0).await;
        let stamp = env.mailbox().stamp();
        Ok(found.unwrap_or(stamp))
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

fn assert_send<T: Send>(value: T) -> T {
    value
}

#[sixer::port(async_send)]
trait Lookup: Send + Sync {
    #[query]
    async fn echo<T: Send>(&self, value: T) -> T;

    #[query]
    async fn find(&self, name: &str) -> usize;

    #[query]
    async fn both(&self, name: &str, n: &u32) -> usize;

    #[query]
    async fn label(&self) -> &str;

    #[query]
    async fn nth<const N: usize>(&self) -> u32;
}

#[derive(Clone, Copy)]
struct Words;

impl Lookup for Words {
    async fn echo<T: Send>(&self, value: T) -> T {
        value
    }

    async fn find(&self, name: &str) -> usize {
        name.len()
    }

    async fn both(&self, name: &str, n: &u32) -> usize {
        name.len() + *n as usize
    }

    async fn label(&self) -> &str {
        "ok"
    }

    async fn nth<const N: usize>(&self) -> u32 {
        N as u32
    }
}

fn retry<F, Fut>(mut call: F) -> Fut
where
    F: FnMut() -> Fut,
{
    call()
}

#[test]
fn rewritten_method_is_send_and_runs() {
    let memory = Memory;
    let view = MailboxQuery::new(&memory);
    let found = assert_send(view.get(4));
    assert_eq!(ready(found), Some(4));
    assert_eq!(MailboxCommand::new(&memory).stamp(), 7);
}

#[test]
fn mock_returns_a_boxed_future() {
    let mut env = service::MockEnv::default();
    env.mailbox
        .expect_get()
        .returning(|id| Box::pin(std::future::ready(Some(id + 1))));

    let found = ready(Read(4).run(&env)).expect("read");
    assert_eq!(found, Some(5));
}

#[test]
fn a_closure_returns_the_port_future() {
    let words = Words;
    let name = "port";
    let n = 3u32;
    assert_eq!(ready(retry(|| LookupQuery::new(&words).echo(1u8))), 1);
    assert_eq!(ready(retry(|| LookupQuery::new(&words).find(name))), 4);
    assert_eq!(ready(retry(|| LookupQuery::new(&words).both(name, &n))), 7);
    assert_eq!(ready(retry(|| LookupQuery::new(&words).label())), "ok");
    assert_eq!(ready(retry(|| LookupQuery::new(&words).nth::<4>())), 4);
}

#[test]
fn command_view_keeps_the_sync_method() {
    let mut env = service::MockEnv::default();
    env.mailbox
        .expect_get()
        .returning(|_| Box::pin(std::future::ready(None)));
    env.mailbox.expect_stamp().returning(|| 3);

    let written = ready(Write(4).run(&env)).expect("write");
    assert_eq!(written, 3);
}
