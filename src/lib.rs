//! A thin, self-owned local gateway that serves the Moshi mobile app's
//! workspace sidebar from a running Herdr server.
//!
//! Scope: `/v1/version`, `/v1/muxes`, `/v1/workspaces` (+ `panes`, `focus`),
//! and the `/events` WebSocket watch. No cloud, no pairing, no agent hooks.

pub mod config;
pub mod gateway;
pub mod herdr;
pub mod mapping;
pub mod model;
pub mod state;
