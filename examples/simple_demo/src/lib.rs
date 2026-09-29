//! Simple Demo

::sixer::runtime! {
    error = crate::Error,
    ports {
        database: DB: crate::port::Database,
        id_generator: ID: crate::port::IdGenerator,
    }
}

/// holds the functionality we expose outside of our domain
pub mod cqrs {
    use crate::{
        entity::Widget,
        service::{CommandEnv, QueryEnv},
    };

    #[derive(Debug)]
    pub struct CreateWidget {
        pub name: String,
    }

    #[::sixer::command]
    impl CreateWidget {
        async fn run(self, env: &impl CommandEnv) -> Result<u32, crate::Error> {
            if self.name.trim().is_empty() {
                return Err(crate::Error::validation(["name cannot be blank"]));
            }

            let id = env.id_generator().next_widget_id();
            let widget = Widget {
                id,
                name: self.name.trim().to_string(),
            };
            env.database().put(widget).await?;
            Ok(id)
        }
    }

    #[derive(Debug)]
    pub struct CreateTwo(pub String, pub String);

    #[::sixer::command]
    impl CreateTwo {
        async fn run(self, env: &impl CommandEnv) -> Result<(u32, u32), crate::Error> {
            let id1 = env.command(CreateWidget { name: self.0 }).await?;
            let id2 = env.command(CreateWidget { name: self.1 }).await?;
            Ok((id1, id2))
        }
    }

    #[derive(Debug)]
    pub struct FetchWidget(pub u32);

    #[::sixer::query]
    impl FetchWidget {
        async fn run(self, env: &impl QueryEnv) -> Result<Option<Widget>, crate::Error> {
            env.database().get(self.0).await
        }
    }
}

/// ports are the contracts you define with outside
pub mod port {
    #[::sixer::port]
    #[cfg_attr(test, ::mockall::automock)]
    pub trait IdGenerator: Send + Sync {
        #[command]
        fn next_widget_id(&self) -> u32;
    }

    #[::sixer::port(async_send)]
    #[cfg_attr(test, ::mockall::automock)]
    pub trait Database: Send + Sync {
        #[query]
        async fn get(&self, id: u32) -> Result<Option<crate::entity::Widget>, crate::Error>;

        #[command]
        async fn put(&self, widget: crate::entity::Widget) -> Result<(), crate::Error>;
    }
}

/// entities are the core data of your domain
pub mod entity {
    #[derive(Debug, Clone)]
    pub struct Widget {
        pub id: u32,
        pub name: String,
    }
}

/// top level error that encompasses all error states
#[derive(Debug, ::thiserror::Error)]
pub enum Error {
    #[error("general error: {0}")]
    General(String),

    #[error("validation errors: {0:#?}")]
    Validation(Vec<String>),
}

impl Error {
    fn validation<S, E>(errors: E) -> Self
    where
        S: Into<String>,
        E: IntoIterator<Item = S>,
    {
        Self::Validation(errors.into_iter().map(Into::into).collect())
    }
}

#[cfg(test)]
mod test {
    use super::cqrs::{CreateTwo, CreateWidget, FetchWidget};
    use super::entity::Widget;
    use super::service::{Command, MockEnv, Query};

    #[::tokio::test]
    async fn create_widget_works() -> Result<(), crate::Error> {
        let mut env = MockEnv::default();

        env.id_generator
            .expect_next_widget_id()
            .once()
            .return_const(42u32);

        env.database
            .expect_put()
            .once()
            .withf(|widget| widget.id == 42 && widget.name == "test")
            .returning(|_| Box::pin(::std::future::ready(Ok(()))));

        let create_widget = CreateWidget {
            name: "test".into(),
        };

        let id = create_widget.run(&env).await?;
        assert_eq!(id, 42);
        Ok(())
    }

    #[::tokio::test]
    async fn create_widget_validation() {
        let env = MockEnv::default();
        let create_widget = CreateWidget { name: "".into() };
        let err = create_widget.run(&env).await.expect_err("validation");
        assert!(matches!(
            err, crate::Error::Validation(msgs) if msgs == ["name cannot be blank"]
        ));
    }

    #[::tokio::test]
    async fn fetch_widget_works() -> Result<(), crate::Error> {
        let mut env = MockEnv::default();

        env.database
            .expect_get()
            .once()
            .withf(|id| *id == 42)
            .returning(|id| {
                Box::pin(::std::future::ready(Ok(Some(Widget {
                    id,
                    name: "test".into(),
                }))))
            });

        let widget = FetchWidget(42).run(&env).await?.expect("a widget");
        assert_eq!(widget.name, "test");
        assert_eq!(widget.id, 42);
        Ok(())
    }

    #[::tokio::test]
    async fn create_two_delegates_to_creat_widget() -> Result<(), crate::Error> {
        let mut env = MockEnv::default();

        env.expect_command::<CreateWidget>()
            .withf(|create| create.name == "thing-one")
            .returning(|_| Box::pin(::std::future::ready(Ok(41))));

        env.expect_command::<CreateWidget>()
            .withf(|create| create.name == "thing-two")
            .returning(|_| Box::pin(::std::future::ready(Ok(42))));

        let create_two = CreateTwo("thing-one".into(), "thing-two".into());
        let (one, two) = create_two.run(&env).await?;
        assert_eq!(one, 41);
        assert_eq!(two, 42);
        Ok(())
    }
}
