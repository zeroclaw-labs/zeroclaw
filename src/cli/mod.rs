mod auth;
mod completions;
mod config;
mod doctor;
mod memory;
mod models;
mod team;

pub use auth::{
    clear_pending_oauth_login, extract_openai_account_id_for_profile, format_expiry,
    handle_auth_command, load_pending_oauth_login, pending_oauth_login_path,
    pending_oauth_secret_store, read_auth_input, read_plain_input, save_pending_oauth_login,
    set_owner_only_permissions, AuthCommands, PendingOAuthLogin, PendingOAuthLoginFile,
};
pub use completions::{write_shell_completion, CompletionShell};
pub use config::ConfigCommands;
pub use doctor::DoctorCommands;
pub use memory::MemoryCommands;
pub use models::ModelCommands;
pub use team::handle_command as handle_team_command;
