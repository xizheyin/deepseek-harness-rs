//! Provider-neutral core for the `dsh` terminal agent.

#![deny(unsafe_code)]

mod entropy;
mod goal;
mod json_value;
mod plan_mode;
mod resident_credit;
mod tui;
mod user_question;
mod workspace_authority;
mod workspace_instructions;

pub mod agent;
pub mod cli;
pub mod model;
pub mod provider;
pub mod session;
pub mod tools;
