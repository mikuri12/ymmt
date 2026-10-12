//! Vista de cola: qué sigue, quitar pistas, saltar a una.

use crate::app::App;
use crate::player::PlayerCmd;
use crossterm::event::{KeyCode, KeyEvent};

pub async fn handle_key(app: &mut App, key: KeyEvent) -> anyhow::Result<()> {
    let len = app.player.state.lock().unwrap().queue.len();
    match key.code {
        KeyCode::Down => app.move_queue_sel(1),
        KeyCode::Up => app.move_queue_sel(-1),
        KeyCode::PageDown => app.move_queue_sel(10),
        KeyCode::PageUp => app.move_queue_sel(-10),
        KeyCode::Enter => {
            let sel = app.queue_sel();
            if sel < len {
                app.player.cmd(PlayerCmd::JumpTo(sel));
            }
        }
        KeyCode::Char('d') | KeyCode::Delete => {
            let sel = app.queue_sel();
            if sel < len {
                app.player.cmd(PlayerCmd::RemoveFromQueue(sel));
                app.set_status("pista quitada de la cola");
            }
        }
        KeyCode::Esc => app.focus = crate::views::Focus::NowPlaying,
        _ => {}
    }
    Ok(())
}
