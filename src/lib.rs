//! Playzoid Server library crate.
//!
//! Exposes all application modules so that integration tests in `tests/` can
//! build a test `App` using the same route configuration and service layer as
//! the production binary.

pub mod api;
pub mod config;
pub mod db;
pub mod entities;
pub mod middleware;
pub mod services;
pub mod sockets;
