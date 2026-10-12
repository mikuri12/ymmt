//! Vista de playlists: lista de tus playlists y sus pistas.

use crate::app::App;
use crate::player::PlayerCmd;
use crossterm::event::{KeyCode, KeyEvent};

/// ¿El foco está en la lista de playlists o en las pistas?
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Panel {
    List,
    Tracks,
}

pub async fn handle_key(app: &mut App, key: KeyEvent) -> anyhow::Result<()> {
    // Tab/BackTab alterna entre lista de playlists y pistas.
    match key.code {
        KeyCode::Tab => {
            app.playlist_panel_toggle();
            return Ok(());
        }
        KeyCode::BackTab => {
            app.playlist_panel_toggle();
            return Ok(());
        }
        KeyCode::Esc => {
            if app.playlist_panel() == Panel::Tracks {
                app.playlist_panel_set(Panel::List);
                return Ok(());
            }
            app.focus = crate::views::Focus::NowPlaying;
            return Ok(());
        }
        _ => {}
    }

    match app.playlist_panel() {
        Panel::List => match key.code {
            KeyCode::Down => app.move_playlist_sel(1),
            KeyCode::Up => app.move_playlist_sel(-1),
            KeyCode::Enter | KeyCode::Right => {
                let sel = app.playlist_sel();
                if let Some(p) = app.playlists.get(sel) {
                    let id = p.id.clone();
                    if p.tracks.is_none() {
                        app.load_playlist_tracks(id).await;
                    }
                    app.playlist_panel_set(Panel::Tracks);
                }
            }
            _ => {}
        },
        Panel::Tracks => match key.code {
            KeyCode::Down => app.move_playlist_track_sel(1),
            KeyCode::Up => app.move_playlist_track_sel(-1),
            KeyCode::Left => app.playlist_panel_set(Panel::List),
            KeyCode::Enter => {
                // Reproducir la playlist desde la pista seleccionada.
                let sel_p = app.playlist_sel();
                let sel_t = app.playlist_track_sel();
                let tracks = app
                    .playlists
                    .get(sel_p)
                    .and_then(|p| p.tracks.clone())
                    .unwrap_or_default();
                if sel_t < tracks.len() {
                    app.player
                        .cmd(PlayerCmd::PlayQueue(tracks, sel_t));
                    app.focus = crate::views::Focus::NowPlaying;
                }
            }
            KeyCode::Char('a') => {
                // Añadir toda la playlist a la cola.
                let sel_p = app.playlist_sel();
                let tracks = app
                    .playlists
                    .get(sel_p)
                    .and_then(|p| p.tracks.clone())
                    .unwrap_or_default();
                if !tracks.is_empty() {
                    app.player.cmd(PlayerCmd::Enqueue(tracks));
                    app.set_status("playlist añadida a la cola");
                }
            }
            _ => {}
        },
    }
    Ok(())
}
