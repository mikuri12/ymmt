//! Vistas y manejo de teclado por vista.

pub mod home;
pub mod queue;
pub mod playlist;
pub mod search;

use crate::app::App;

/// Vista activa.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Focus {
    /// Pantalla de inicio: recomendaciones (ánimos) y "escuchadas recientemente".
    Home,
    NowPlaying,
    Search,
    Queue,
    Playlists,
    Settings,
}

/// Popups.
#[derive(Debug, Clone, PartialEq)]
pub enum Popup {
    Help,
    Config,
}

/// Tecla dentro de un popup: esc/q/enter cierran.
pub fn handle_popup_key(app: &mut App, key: crossterm::event::KeyEvent) -> anyhow::Result<bool> {
    use crossterm::event::KeyCode;
    match key.code {
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
            app.popup = None;
        }
        _ => {}
    }
    Ok(false)
}
