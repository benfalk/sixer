//! Two ports, so the builder's order does not match `Service::new`.

#[sixer::port]
#[cfg_attr(test, mockall::automock)]
pub trait Left: Send + Sync + 'static {}

#[sixer::port]
#[cfg_attr(test, mockall::automock)]
pub trait Right: Send + Sync + 'static {}

#[derive(Debug)]
struct Error;

struct LeftVal;
struct RightVal;

impl Left for LeftVal {}
impl Right for RightVal {}

sixer::runtime! {
    error = crate::Error,
    ports {
        left: LeftPort: crate::Left,
        right: RightPort: crate::Right,
    }
}

struct App;

impl Ports for App {
    type LeftPort = LeftVal;
    type RightPort = RightVal;
}

#[test]
fn ports_can_be_set_in_either_order() {
    let first = Service::<App>::builder()
        .left(LeftVal)
        .right(RightVal)
        .build();
    let second = Service::<App>::builder()
        .right(RightVal)
        .left(LeftVal)
        .build();
    let _ = (first, second);
}
