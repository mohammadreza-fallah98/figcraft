//! Login against a company portal, from inside the app.
//!
//! The person presses Log in; the app makes a random verifier, opens the portal's login page in the
//! browser with the verifier's hash (the challenge), and the portal sends the browser back to
//! `figcraft://auth?code=…` with a one-time code. The app exchanges the code and the verifier for
//! an access token and a refresh token ([`session::Portal`]), keeps the refresh token in the OS
//! credential store ([`secret`]) and never writes a token to a file. The access token lists the
//! person's projects and asks for the join link of one.
//!
//! The parts that decide things (link parsing, challenge, response parsing, the sign-in state
//! machine) are plain functions and a fake-able [`session::Api`], so they are tested without a
//! network; [`client`] is the HTTPS implementation.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod api;
#[cfg(not(target_arch = "wasm32"))]
pub mod client;
pub mod config;
pub mod deeplink;
pub mod handoff;
pub mod pkce;
pub mod secret;
pub mod session;

pub use api::{Access, Login, OpenedProject, PortalError, ProjectEntry};
pub use deeplink::{AuthLink, LinkError, is_auth_link, parse_auth_link};
pub use session::{Portal, Status, View};

#[cfg(test)]
mod api_tests;
#[cfg(test)]
mod config_tests;
#[cfg(test)]
mod deeplink_tests;
#[cfg(test)]
mod handoff_tests;
#[cfg(test)]
mod pkce_tests;
#[cfg(test)]
mod session_tests;
