use ::simple_demo::cqrs::{CreateTwo, CreateWidget, FetchWidget};
use ::simple_demo::entity::Widget;
use ::simple_demo::{Error, Ports, Service};

mod adapter {
    use super::*;
    use ::std::collections::HashMap;
    use ::std::sync::Arc;
    use ::std::sync::atomic::{AtomicU32, Ordering};
    use ::tokio::sync::RwLock;

    type SharableHashMap<K, V> = Arc<RwLock<HashMap<K, V>>>;

    #[derive(Debug, Clone, Default)]
    pub struct MemoryDb(SharableHashMap<u32, Widget>);

    impl ::simple_demo::port::Database for MemoryDb {
        async fn get(&self, id: u32) -> Result<Option<Widget>, Error> {
            let read = self.0.read().await;
            Ok(read.get(&id).cloned())
        }

        async fn put(&self, widget: Widget) -> Result<(), Error> {
            let mut write = self.0.write().await;
            write.insert(widget.id, widget);
            Ok(())
        }
    }

    #[derive(Debug, Clone, Default)]
    pub struct MemoryWidgetIds(Arc<AtomicU32>);

    impl ::simple_demo::port::IdGenerator for MemoryWidgetIds {
        fn next_widget_id(&self) -> u32 {
            self.0.fetch_add(1, Ordering::Relaxed)
        }
    }
}

struct TestPorts;

impl Ports for TestPorts {
    type DB = adapter::MemoryDb;
    type ID = adapter::MemoryWidgetIds;
}

#[tokio::test]
async fn service_works_as_expected() -> Result<(), Error> {
    let widgets = Service::<TestPorts>::builder()
        .database(adapter::MemoryDb::default())
        .id_generator(adapter::MemoryWidgetIds::default())
        .build();

    let nothing = widgets.query(FetchWidget(42)).await?;
    assert!(nothing.is_none());

    let id1 = widgets
        .command(CreateWidget {
            name: "thing-one".into(),
        })
        .await?;

    let (id2, id3) = widgets
        .command(CreateTwo("thing-two".into(), "thing-three".into()))
        .await?;

    assert_eq!(id1, 0);
    assert_eq!(id2, 1);
    assert_eq!(id3, 2);

    let one = widgets.query(FetchWidget(id1)).await?.expect("widget 1");
    let two = widgets.query(FetchWidget(id2)).await?.expect("widget 2");
    let three = widgets.query(FetchWidget(id3)).await?.expect("widget 3");

    assert_eq!(one.name, "thing-one");
    assert_eq!(two.name, "thing-two");
    assert_eq!(three.name, "thing-three");

    Ok(())
}
