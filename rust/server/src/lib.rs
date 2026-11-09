pub mod accounts;
mod accounts_api;
pub mod api;
pub mod application;
pub mod catalog_edit;
mod codex_session;
pub mod config;
mod context;
mod data_api;
mod diagnostics;
pub mod error;
mod guardian_api;
pub mod language;
pub mod launcher;
pub mod metrics;
mod notifications;
pub mod oauth;
pub mod oauth_web;
pub mod provider;
mod service_preview;
pub mod settings_api;
mod speech;
mod speech_text;
pub mod store;
mod update_api;
mod video;
mod voice;

pub use application::Application;
pub use error::{Error, Result};

mod provider_protocol;
pub mod tools;
mod turn_engine;
mod turn_queries;

mod ask_user;

pub mod calendar;
mod calendar_api;
pub mod capability;
pub mod learning;
mod learning_api;
pub mod memory_graph;
pub mod memory_maintenance;
mod solve;
pub mod task_board;
pub mod task_board_api;

mod generation;

mod document_parser;
pub mod knowledge;
mod knowledge_api;
mod knowledge_retrieval;
pub mod ocr;
pub mod reading;
mod reading_api;
mod study;
mod study_api;
mod workspaces;
mod workspaces_api;

pub mod backup;
pub mod browser_testing;
mod browser_testing_api;
pub mod recovery;

mod client_api;

pub mod search;

pub mod documents;
mod documents_api;
pub mod memory_cli;

pub mod flashcards;
mod flashcards_api;

mod web_reading;

pub mod mastery;
