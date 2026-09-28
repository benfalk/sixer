# sixer

An opinionated helper for the [`hexagonal design pattern`].

> The hexagonal architecture, or ports and adapters architecture, is an
> architectural style used in software design. It aims at creating loosely
> coupled application components that can be easily connected to their
> software environment by means of ports and adapters. This makes components
> exchangeable at any level and facilitates test automation.

## Runtime Tutorial

`sixer` generates service boilerplate traits and structures inside of your
crate that allows you to focus on the core of your domain logic.  Let's
follow the working example to see how it works.

### Error and Context

```rust
#[derive(Debug, ::thiserror::Error)]
pub enum Error {
    #[error("general error: {0}")]
    General(String),

    #[error("user is required")]
    UserRequired,
}

#[derive(Debug, PartialEq, Eq, Default)]
pub enum Context {
    #[default]
    Nobody,
    User(u32),
    Root,
}

impl Context {
    pub fn user_id(&self) -> Option<u32> {
        match self {
            Self::User(id) => Some(*id),
            _ => None,
        }
    }
}
```

1. Here we have a crate level `Error`; `sixer` requires all queries and commands
   errors can be converted into this structure. This enforcement is more than
   just good hygiene; future drivers will rely on this.

2. A context is not required for `sixer`; however, in this tutorial we're going
   to be using one.  Your context **MUST** implement default.  The default is
   provided when a context is not provided for queries and commands.

### Defining Ports

Without ports your hexagonal design won't get far; this is how to define them:

```rust
pub mod port {
    #[::sixer::port]
    #[cfg_attr(test, ::mockall::automock)]
    pub trait Messages: Send + Sync {
        #[query]
        fn get(&self, id: u32)
        -> impl Future<Output = Result<Option<String>, crate::Error>> + Send;

        #[command]
        fn put(
            &self,
            id: u32,
            message: String,
        ) -> impl Future<Output = Result<(), crate::Error>> + Send;
    }
}
```

`sixer::port` will require each method be tagged with either `#[query]`
or `[#command]`.  Methods tagged with query can only be used from
queries while commands can call methods tagged with either.  We'll
demonstrate that more in the [cqrs] section.

Some things to note:

- ports require `Send` + `Sync` so they are async compatible
- `#[cfg_attr(test, ::mockall::automock)]` is required, testing is
  a first class citizen and the test setup is expecting a `MockMessages`
- If you want your async methods to work on popular runtimes like `tokio`
  right now we're stuck defining them like this for traits.  I don't care
  for `async_trait`; but that is also an option.

### Wiring Up the Service Runtime

We have everything we need to create a service runtime:

```rust
::sixer::runtime! {
    error = crate::Error,
    context = crate::Context,
    ports {
        messages: MSG: crate::port::Messages,
    }
}
```

This macro writes the bulk of the code that makes everything work. It does
this by creating a `service` module; and is required for this to be at the
root of your crate.  It also exports a `Service` struct and a `Ports` trait.
`Ports` is a trait bag which allows us to define all of the concrete types
used at runtime for `Service`.  The `context = crate::StructName` is optional
and if left out will not create any of the context plumbing for a service.

Each port line is can be read as the following:

```text
<port_identifier>: <port_type_alias>: <full_path_to_port_trait>
```

It is important for each port's path to be a full path so that the macro
system can correctly find the helper types created by `sixer::port`.

### Creating Commands and Queries

Now that we have the service runtime created we're ready to start building
command and queries for our domain.

```rust
// in mod cqrs
use crate::Error;
use crate::service::{CommandEnv, QueryEnv};

#[derive(Debug)]
pub struct FetchMyMessage;

#[::sixer::query]
impl FetchMyMessage {
    async fn run(self, env: &impl QueryEnv) -> Result<Option<String>, Error> {
        let Some(user_id) = env.ctx().user_id() else {
            return Err(crate::Error::UserRequired);
        };

        env.messages().get(user_id).await
    }
}

#[derive(Debug)]
pub struct StoreMyMessage(pub String);

#[::sixer::command]
impl StoreMyMessage {
    async fn run(self, env: &impl CommandEnv) -> Result<(), Error> {
        let Some(user_id) = env.ctx().user_id() else {
            return Err(crate::Error::UserRequired);
        };

        env.messages().put(user_id, self.0).await
    }
}
```

### Creating Tests

Here is some examples of tests for our new query and command:

```rust
#[cfg(test)]
mod tests {
    use crate::cqrs::{FetchMyMessage, StoreMyMessage};
    use crate::service::{Command, MockEnv, Query};
    use crate::{Context, Error};

    #[::tokio::test]
    async fn fetch_requires_user_in_context() {
        let env = MockEnv::default();
        let err = FetchMyMessage.run(&env).await;
        assert!(matches!(err, Err(Error::UserRequired)));
    }

    #[::tokio::test]
    async fn store_requires_user_in_context() {
        let env = MockEnv::default();
        let err = StoreMyMessage("test".into()).run(&env).await;
        assert!(matches!(err, Err(Error::UserRequired)));
    }

    #[::tokio::test]
    async fn fetch_with_user_id_in_context() {
        let mut env = MockEnv::default();
        env.ctx = Context::User(42);
        env.messages
            .expect_get()
            .once()
            .withf(|id| *id == 42)
            .returning(|_| {
                // I recommend making a helper to cut out this boilerplate
                Box::pin(::std::future::ready(Ok(Some("hello test".into()))))
            });

        let msg = FetchMyMessage.run(&env).await.expect("fetch message");
        assert_eq!(msg.as_deref(), Some("hello test"));
    }

    #[::tokio::test]
    async fn store_with_user_id_in_context() {
        let mut env = MockEnv::default();
        env.ctx = Context::User(42);
        env.messages
            .expect_put()
            .once()
            .withf(|id, msg| *id == 42 && msg == "test")
            .returning(|_, _| Box::pin(::std::future::ready(Ok(()))));

        StoreMyMessage("test".into())
            .run(&env)
            .await
            .expect("store to work");
    }
}
```

In a test environment you have access to `MockEnv`; which allows loading the
context and setting up any expectation of ports before running any of the
tests.

### Using Service From Another Crate

The following is an integration test; however, it highlights exactly
the same process you would use from the binary crate that wires up
your service.

```rust
use ::context_demo::{Context, Error, Ports, Service, cqrs};
use ::std::collections::HashMap;
use ::std::sync::Arc;
use ::tokio::sync::RwLock;

/// A simple in-memory store adapter of our messages
#[derive(Clone, Default)]
pub struct MemoryMessageStore(Arc<RwLock<HashMap<u32, String>>>);

// implementing our adapter to the Messages port
impl ::context_demo::port::Messages for MemoryMessageStore {
    async fn get(&self, id: u32) -> Result<Option<String>, Error> {
        let read = self.0.read().await;
        Ok(read.get(&id).cloned())
    }

    async fn put(&self, id: u32, message: String) -> Result<(), Error> {
        let mut write = self.0.write().await;
        write.insert(id, message);
        Ok(())
    }
}

// DemoPorts is the port bag we use to implement our Ports
// bag with all of the types used at runtime.
struct DemoPorts;

// defining concrete types for a port bag
impl Ports for DemoPorts {
    type MSG = MemoryMessageStore;
}

// attaching the port bag to the service via type alias
type Demo = Service<DemoPorts>;

#[::tokio::test]
async fn demo_works() -> Result<(), Error> {
    // service comes with a builder that requires each adapter for
    // our service's ports before it can be built.
    let demo = Demo::builder()
        .messages(MemoryMessageStore::default())
        .build();

    // the service creates run-times with a context that can run
    // both queries and commands we have defined
    let user1 = demo.with_context(&Context::User(1));
    let user2 = demo.with_context(&Context::User(2));

    user1.command(cqrs::StoreMyMessage("test".into())).await?;
    let msg1 = user1.query(cqrs::FetchMyMessage).await?;
    let msg2 = user2.query(cqrs::FetchMyMessage).await?;
    assert_eq!(msg1.as_deref(), Some("test"));
    assert!(msg2.is_none());

    // the service itself can also call queries and commands and
    // are provided a default context for the request
    let err = demo.command(cqrs::StoreMyMessage("!".into())).await;
    assert!(matches!(err, Err(Error::UserRequired)));

    Ok(())
}
```

[`hexagonal design pattern`]: https://en.wikipedia.org/wiki/Hexagonal_architecture_(software)
