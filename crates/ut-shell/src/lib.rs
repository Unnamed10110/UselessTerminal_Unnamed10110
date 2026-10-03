//! `ut-shell`: shell detection, shell-integration planning, path quoting and git branches.
//! Everything is synchronous and blocking-IO only where noted; run it off the UI thread.
//!
//! # Public API
//! * [`kind`]: [`ShellKind`] (serde camelCase), [`detect_kind`] (§6.5 rules, in order),
//!   [`split_exe_and_args`] (quote-aware; unquoted spaced paths grow word by word until the
//!   prefix is a file, §9.2), [`file_stem_lower`].
//! * [`detect`]: [`ShellProfile`], [`detect_shells`] (§9.1; registry for WSL distros and Git
//!   for Windows, never spawns `wsl.exe`), [`ShellCache`] (`get(force)`, 60 s TTL),
//!   [`resolve_default`] (`shells.defaultProfile`), [`resolve_exe`] (§9.2),
//!   [`profile_command`].
//! * [`integration`]: [`plan`]`(&`[`IntegrationRequest`]`) -> `[`IntegrationPlan`]
//!   (rewritten command line, extra env, [`PostSpawn`] timed stdin writes, notes);
//!   [`powershell_script`] (Appendix A.1), [`typed_input_escape`] / [`RESET_FG`] (§6.6).
//! * [`quote`]: [`quote_path`] / [`quote_paths`] (§11.6), [`to_posix_path`], [`to_wsl_path`].
//! * [`git`]: [`GitCache`] (`branch_for(cwd)`, `head_file_for(cwd)`), [`find_git_dir`],
//!   [`parse_head`] (§7.6).

pub mod detect;
pub mod git;
pub mod integration;
pub mod kind;
pub mod quote;

pub use detect::{detect_shells, profile_command, resolve_default, resolve_exe, ShellCache, ShellProfile};
pub use git::{find_git_dir, parse_head, GitCache};
pub use integration::{
    plan, powershell_script, typed_input_escape, IntegrationMode, IntegrationPlan, IntegrationRequest, PostSpawn,
    RESET_FG,
};
pub use kind::{detect_kind, file_stem_lower, split_exe_and_args, ShellKind};
pub use quote::{quote_path, quote_paths, to_posix_path, to_wsl_path};
