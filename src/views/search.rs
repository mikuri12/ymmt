//! Vista de búsqueda: el "catálogo" de YouTube Music en TUI.
//!
//! En esta vista las teclas simples son TEXTO para el input; los atajos de
//! acción usan Ctrl (para no robar letras).

use crate::app::App;
use crate::player::PlayerCmd;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub async fn handle_key(app: &mut App, key: KeyEvent) -> anyhow::Result<()> {
    // Navegación de resultados (flechas no escriben texto).
    match key.code {
        KeyCode::Down => {
            app.move_list_sel(1);
            return Ok(());
        }
        KeyCode::Up => {
            app.move_list_sel(-1);
            return Ok(());
        }
        KeyCode::PageDown => {
            app.move_list_sel(10);
            return Ok(());
        }
        KeyCode::PageUp => {
            app.move_list_sel(-10);
            return Ok(());
        }
        _ => {}
    }

    // Con Ctrl: atajos de acción (no escriben texto).
    if key
        .modifiers
        .contains(KeyModifiers::CONTROL)
    {
        match key.code {
            KeyCode::Char('a') => {
                // Ctrl+a: añadir a la cola.
                if app.list_sel() < app.search_results.len() {
                    let track = app.search_results[app.list_sel()].clone();
                    app.player.cmd(PlayerCmd::Enqueue(vec![track]));
                    app.set_status("añadido a la cola");
                }
                return Ok(());
            }
            KeyCode::Char('n') => {
                // Ctrl+n: reproducir a continuación.
                if app.list_sel() < app.search_results.len() {
                    let track = app.search_results[app.list_sel()].clone();
                    app.player.cmd(PlayerCmd::PlayNext(vec![track]));
                    app.set_status("se reproducirá a continuación");
                }
                return Ok(());
            }
            _ => return Ok(()),
        }
    }

    match key.code {
        KeyCode::Esc => {
            // Esc: limpiar; si ya está vacía, volver a la Home.
            if app.search_query.is_empty() {
                app.focus = crate::views::Focus::Home;
            } else {
                app.search_query.clear();
                app.search_results.clear();
            }
        }
        KeyCode::Backspace => {
            app.search_query.pop();
        }
        KeyCode::Enter => {
            if app.list_sel() < app.search_results.len() {
                let track = app.search_results[app.list_sel()].clone();
                app.player.cmd(PlayerCmd::PlayQueue(vec![track], 0));
                app.focus = crate::views::Focus::NowPlaying;
            } else {
                app.do_search();
            }
        }
        KeyCode::Char(c) if !c.is_control() => {
            // Texto normal al input (incluye espacios).
            app.search_query.push(c);
            // Búsqueda en vivo a partir de 3 caracteres.
            if app.search_query.len() >= 3 {
                app.do_search();
            }
        }
        _ => {}
    }
    Ok(())
}
