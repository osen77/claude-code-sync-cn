//! Command handler modules
//!
//! This module contains all command handler functions extracted from main.rs,
//! organized by functionality area.

pub mod automate;
pub mod cleanup;
pub mod config;
pub mod history;
pub mod hooks;
pub mod onboarding;
pub mod setup;
pub mod undo;
pub mod update;
pub mod wrapper;

// Re-export all public handler functions for convenient use
pub use automate::{handle_automate_setup, handle_automate_status, handle_automate_uninstall};
pub use cleanup::handle_cleanup_snapshots;
pub use config::{handle_config_interactive, handle_config_wizard, handle_repo_selector};
pub use history::{handle_history_clear, handle_history_last, handle_history_list, handle_history_review};
pub use hooks::{handle_hooks_install, handle_hooks_show, handle_hooks_uninstall, handle_new_project_check, handle_session_start, handle_stop};
pub use onboarding::{is_initialized, run_init_from_config, run_onboarding_flow, try_init_from_config};
pub use setup::handle_setup;
pub use undo::{handle_undo_pull, handle_undo_push};
pub use update::{check_for_update_silent, handle_update, print_update_notification};
pub use wrapper::{handle_wrapper_install, handle_wrapper_show, handle_wrapper_uninstall};
