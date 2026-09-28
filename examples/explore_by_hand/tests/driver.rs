use ::explore_by_hand::*;
use ::std::collections::HashMap;
use ::std::sync::Arc;
use ::tokio::sync::RwLock;

#[derive(Debug, Clone, Default)]
pub struct MemoryStorage(Arc<RwLock<HashMap<u32, u32>>>);

impl port::Database for MemoryStorage {
    async fn get(&self, id: u32) -> Result<u32, Error> {
        let read = self.0.read().await;
        read.get(&id)
            .cloned()
            .ok_or_else(|| Error::General("missing number".into()))
    }

    async fn put(&self, id: u32, val: u32) -> Result<(), Error> {
        let mut write = self.0.write().await;
        write.insert(id, val);
        Ok(())
    }
}

struct DemoPorts;

impl Ports for DemoPorts {
    type DB = MemoryStorage;
}

type DemoService = Service<DemoPorts>;

#[::tokio::test]
async fn works_as_expected() -> Result<(), Error> {
    let root = &Context::Root;
    let demo = DemoService::default();

    demo.with_context(root)
        .command(cqrs::Put { key: 42, val: 13 })
        .await?;

    let val = demo.query(cqrs::Fetch(42)).await?;
    assert_eq!(val, 13);
    Ok(())
}
