//! Shared logic for the `bk` CLI and the Clibooker desktop app.

pub mod doctor;
pub mod env;
pub mod history;
pub mod model;
pub mod paths;
pub mod plan;
pub mod proc;
pub mod projects;
pub mod recent;
pub mod remote;
pub mod render;
pub mod render_pwsh;
pub mod script;
pub mod scripts;
pub mod servers;
pub mod shell;
pub mod store;
pub mod template;
pub mod trust;

pub use model::{Bookmark, EnvSpec, Server, Settings, Step};
pub use paths::Paths;
pub use store::{Entry, Library, Scope};
