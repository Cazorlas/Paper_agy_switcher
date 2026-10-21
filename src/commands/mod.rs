#![allow(unused_imports)]

mod add;
mod auto;
pub(crate) use add::add_cmd;
mod login;
mod misc;
pub(crate) mod profile;
mod render;
mod update;

pub(crate) use crate::launch::{launch_cmd, launch_for_tui};
pub(crate) use auto::{AutoOptions, auto_cmd};
pub(crate) use login::login_cmd;
pub(crate) use misc::{doctor_cmd, open_cmd};
pub(crate) use profile::{restore_cmd, delete_cmd, list_cmd, rename_cmd, use_cmd};

pub(crate) use update::self_update_cmd;
