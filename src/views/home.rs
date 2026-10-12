//! Vista Home: el feed "para ti" REAL de YouTube Music — las mismas secciones
//! ("Selecciones rápidas", "Clásicos inolvidables"...) que verías al abrir
//! music.youtube.com con tu sesión, en estantes horizontales con portadas.

use crate::app::App;
use crate::player::PlayerCmd;
use crossterm::event::{KeyCode, KeyEvent};

/// Panel activo de la Home: el estante del feed o las recientes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Panel {
    Moods,
    Recent,
}

pub async fn handle_key(app: &mut App, key: KeyEvent) -> anyhow::Result<()> {
    // Tab alterna entre el estante y las recientes.
    match key.code {
        KeyCode::Tab | KeyCode::BackTab => {
            app.home_panel_toggle();
            return Ok(());
        }
        _ => {}
    }
    match app.home_panel() {
        Panel::Moods => match key.code {
            // ↑/↓ cambian de sección del feed; ←/→ mueven la tarjeta.
            KeyCode::Up => app.move_feed_section(-1),
            KeyCode::Down => app.move_feed_section(1),
            KeyCode::Left => app.move_feed_card(-1),
            KeyCode::Right => app.move_feed_card(1),
            KeyCode::Enter => app.activate_feed_card().await,
            KeyCode::Char('a') => {
                // Añadir la canción de la tarjeta a la cola.
                let Some(card) = app
                    .feed
                    .get(app.feed_section())
                    .and_then(|s| s.cards.get(app.feed_card()))
                    .cloned()
                else {
                    return Ok(());
                };
                if card.kind == crate::auth::CardKind::Song && !card.video_id.is_empty() {
                    app.player.cmd(PlayerCmd::Enqueue(vec![crate::player::Track {
                        video_id: card.video_id.clone(),
                        title: card.title.clone(),
                        artist: card.subtitle.clone(),
                        album: String::new(),
                        thumbnail_url: card.thumb_url.clone(),
                        duration_secs: 0,
                    }]));
                    app.set_status(format!("añadida a la cola: {}", card.title));
                }
            }
            _ => {}
        },
        Panel::Recent => {
            let len = app.recent_tracks().len();
            match key.code {
                KeyCode::Down => {
                    let new = (app.recent_sel() as i32 + 1).clamp(0, len as i32 - 1) as usize;
                    app.set_recent_sel(new);
                }
                KeyCode::Up => app.set_recent_sel(app.recent_sel().saturating_sub(1)),
                KeyCode::Enter => {
                    let sel = app.recent_sel();
                    let tracks = app.recent_tracks().to_vec();
                    if sel < tracks.len() {
                        app.player.cmd(PlayerCmd::PlayQueue(tracks, sel));
                        app.focus = crate::views::Focus::NowPlaying;
                    }
                }
                KeyCode::Char('a') => {
                    let sel = app.recent_sel();
                    let tracks = app.recent_tracks().to_vec();
                    if sel < tracks.len() {
                        app.player.cmd(PlayerCmd::Enqueue(vec![tracks[sel].clone()]));
                        app.set_status("añadido a la cola");
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}
