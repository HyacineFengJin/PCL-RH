//! Native desktop ports used below official plugins. This crate knows about
//! windows, clipboard formats and webview engines, not projects, Modrinth links,
//! plugin grants or account credentials. Callers retain their existing admission
//! checks; a platform port is not a second permission/configuration plane.
pub mod clipboard;
pub mod editing;
pub mod opener;
