//! The Reelix server: configuration, HTTP app assembly and process lifecycle.

pub mod adapters;
pub mod cli;
pub mod commands;
pub mod config;
pub mod healthcheck;
pub mod integrations;
pub mod lifecycle;
pub mod maintenance;
pub mod modules;
pub mod server;
pub mod service;
pub mod setup_wizard;
pub mod telemetry;
pub mod web;
