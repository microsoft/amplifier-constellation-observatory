pub mod platform;
pub mod storage;

pub mod app;
pub mod client;

pub mod installation;
#[path = "../../src/instance_package.rs"]
pub mod instance_package;
#[path = "../../src/paired_protocol.rs"]
pub mod paired_protocol;

#[cfg(windows)]
#[path = "../../src/process_tree.rs"]
mod process_tree;
