pub mod catalog;
pub mod controller;
pub mod outcome;
pub mod session;
#[cfg(test)]
pub(crate) mod test_support;
pub mod view;

pub use catalog::{Catalog, CatalogError};
pub use controller::{ControlError, Controller, MAX_SESSIONS};
pub(crate) use outcome::reject;
pub use outcome::{Candidate, MissDiagnostic, Rejection};
pub use session::{
    DEFAULT_SESSION, RouteOverrideSnapshot, SESSION_HEADER, Selection, SelectionSnapshot,
};
pub use view::SessionView;
