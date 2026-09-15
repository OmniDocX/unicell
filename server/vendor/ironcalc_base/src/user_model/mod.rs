#![deny(missing_docs)]

mod autofill;
mod border;
mod border_utils;
mod clipboard;
mod common;
mod conditional_formatting;
pub(crate) mod history;
mod named_cell_styles;
mod sequence_detector;
mod ui;
mod undo_redo;

pub use common::UserModel;

#[cfg(test)]
pub use ui::SelectedView;

#[cfg(test)]
pub(crate) use clipboard::fail_clipboard_paste_after_writes;
pub use clipboard::ClipboardData;
pub use common::BorderArea;
