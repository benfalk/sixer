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

::sixer::runtime! {
    error = crate::Error,
    context = crate::Context,
    ports {
        messages: MSG: crate::port::Messages,
    }
}

pub mod cqrs {
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
}

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
