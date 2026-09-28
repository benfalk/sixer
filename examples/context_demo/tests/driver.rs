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
