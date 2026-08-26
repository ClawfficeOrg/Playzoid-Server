//! Business-logic services (auth, players, leaderboards, ...).
//!
//! Each service module owns a slice of behaviour and is consumed by the
//! corresponding `api/*` handler. Populated in Phase 0.2.

pub mod auth;
pub mod cache;
pub mod events;
pub mod game_settings;
pub mod leaderboards;
pub mod players;
pub mod saves;
