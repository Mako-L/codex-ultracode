use std::io;
use std::io::Write;

use crossterm::execute;
use crossterm::terminal::LeaveAlternateScreen;
use ratatui::backend::Backend;
use ratatui::layout::Rect;

use super::DisableAlternateScroll;
use crate::custom_terminal::Terminal;

pub(super) fn restore_main_screen<B>(terminal: &mut Terminal<B>, saved_viewport: Option<Rect>)
where
    B: Backend<Error = io::Error> + Write,
{
    let _ = execute!(terminal.backend_mut(), DisableAlternateScroll);
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    if let Some(saved) = saved_viewport {
        terminal.set_viewport_area(saved);
    }
    // The restored main screen is unrelated to the alternate-screen diff buffer.
    terminal.invalidate_viewport();
}

#[cfg(test)]
#[path = "alternate_screen_tests.rs"]
mod tests;
