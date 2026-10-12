//! Render: dibuja la app adaptándose al tamaño de la terminal.
//!
//! La vista NowPlaying es minimalista: portada centrada, título, artista y una
//! sola línea de progreso — como pidió el usuario. El resto de vistas son
//! listas navegables con teclado.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

// ── Paleta ymmt (inspirada en la referencia: fondo azul profundo, selección
//    verde con triángulo, headers verdes, errores rojos) ──────────────────
const FG: Color = Color::White;
const FG_DIM: Color = Color::DarkGray;
const SEL_FG: Color = Color::Black;
const SEL_BG: Color = Color::Green;
const SEL_TITLE: Color = Color::Green;
const ACCENT: Color = Color::Green;
const WARN: Color = Color::Red;
const META: Color = Color::Cyan;

/// Bloque con borde fino y título verde (estilo de la referencia).
fn panel(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(FG_DIM))
        .title(Span::styled(format!(" {title} "), Style::default().fg(SEL_TITLE).add_modifier(Modifier::BOLD)))
}

/// Estilo de fila seleccionada: texto verde + triángulo (como la referencia).
fn sel_style(selected: bool, current: bool) -> (Style, &'static str) {
    if selected {
        (Style::default().fg(SEL_TITLE).add_modifier(Modifier::BOLD), "►")
    } else if current {
        (Style::default().fg(ACCENT), "▶")
    } else {
        (Style::default().fg(FG), " ")
    }
}

use crate::app::App;
use crate::views::{Focus, Popup};

pub type Terminal = ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

pub fn init_terminal() -> anyhow::Result<Terminal> {
    let mut stdout = std::io::stdout();
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    Ok(ratatui::Terminal::new(backend)?)
}

pub fn restore_terminal(terminal: &mut Terminal) -> anyhow::Result<()> {
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Convierte un KeyEvent a la cadena del keybind ("q", "ctrl+s", "esc"…).
pub fn key_event_to_combo(key: crossterm::event::KeyEvent) -> String {
    use crossterm::event::{KeyCode, KeyModifiers};
    let mut parts = Vec::new();
    let m = key.modifiers;
    if m.contains(KeyModifiers::CONTROL) {
        parts.push("ctrl".into());
    }
    if m.contains(KeyModifiers::ALT) {
        parts.push("alt".into());
    }
    let base = match key.code {
        KeyCode::Char(c) => {
            // shift+letra = mayúscula (sin ctrl/alt).
            if m.contains(KeyModifiers::SHIFT)
                && !m.contains(KeyModifiers::CONTROL)
                && !m.contains(KeyModifiers::ALT)
            {
                return c.to_uppercase().to_string();
            }
            c.to_string()
        }
        KeyCode::Enter => "enter".into(),
        KeyCode::Esc => "esc".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::PageUp => "pgup".into(),
        KeyCode::PageDown => "pgdn".into(),
        KeyCode::Tab => "tab".into(),
        KeyCode::BackTab => "backtab".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::Delete => "delete".into(),
        KeyCode::F(n) => format!("f{n}"),
        _ => String::new(),
    };
    parts.push(base);
    parts.join("+")
}

/// Dibuja la app completa.
pub fn draw(app: &mut App, f: &mut Terminal) -> anyhow::Result<()> {
    f.draw(|f| {
        let area = f.area();
        let [body, status_bar] =
            Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(area);

        match app.focus {
            Focus::Home => draw_home(app, f, body),
            Focus::NowPlaying => draw_now_playing(app, f, body),
            Focus::Search => draw_search(app, f, body),
            Focus::Queue => draw_queue(app, f, body),
            Focus::Playlists => draw_playlists(app, f, body),
            Focus::Settings => draw_settings(app, f, body),
        }

        // Barra de estado (una línea, colores de la paleta).
        let status = if let Some(msg) = &app.loading {
            format!(" {msg}")
        } else {
            format!(" {}", app.status)
        };
        let (state_ch, shuffle, repeat, vol, idx, len) = {
            let st = app.player.state.lock().unwrap();
            (
                match st.play_state {
                    crate::player::PlayState::Playing => "▶",
                    crate::player::PlayState::Paused => "⏸",
                    crate::player::PlayState::Stopped => "■",
                },
                if st.shuffle { "⇄" } else { "" },
                match st.repeat {
                    crate::player::RepeatMode::Off => "",
                    crate::player::RepeatMode::All => "⟳",
                    crate::player::RepeatMode::One => "¹",
                },
                st.volume,
                st.index + 1,
                st.queue.len(),
            )
        };
        let session = app.api.kind();
        let is_warn = status.contains('⚠');
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!(" {state_ch} "), Style::default().fg(ACCENT)),
                Span::styled(
                    format!("vol:{vol}% {shuffle}{repeat} [{idx}/{len}] · "),
                    Style::default().fg(FG),
                ),
                Span::styled(
                    status,
                    if is_warn {
                        Style::default().fg(WARN)
                    } else {
                        Style::default().fg(FG_DIM)
                    },
                ),
                Span::styled(format!(" · {session}"), Style::default().fg(META)),
            ])),
            status_bar,
        );

        // Página de letras a pantalla completa (tecla l).
        if app.lyrics_page {
            draw_lyrics_page(app, f, body);
        }

        // Popup encima de todo.
        if let Some(popup) = app.popup.clone() {
            draw_popup(app, &popup, f);
        }

        // Cursor del input de búsqueda.
        if app.focus == Focus::Search && app.popup.is_none() {
            if let Some((x, y)) = app.search_cursor_pos() {
                f.set_cursor_position((x, y));
            }
        }
    })?;
    Ok(())
}



/// Mini-portada de una fila (2 filas de alto, 4 columnas): estilo YTM.
fn draw_row_cover(f: &mut Frame, app: &App, url: &str, x: u16, y: u16) {
    let Some(picker) = &app.picker else { return };
    let area = Rect { x, y, width: 4, height: 2 };
    // Buscar en cache.
    let img = match app.covers.get(url) {
        Some(i) => i.clone(),
        None => return, // aún no descargada; el fondo se ve normal
    };
    if let Ok(proto) = picker.new_protocol(img.as_ref().clone(), area, ratatui_image::Resize::Crop(None)) {
        f.render_widget(ratatui_image::Image::new(&proto), area);
    }
}

/// Home estilo YouTube Music: el feed "para ti" real de tu sesión en
/// estantes con portadas, y abajo lo último que escuchaste.
fn draw_home(app: &mut App, f: &mut Frame, area: Rect) {
    use ratatui::widgets::List;
    let [head_a, body_a] =
        Layout::vertical([Constraint::Length(5), Constraint::Min(1)]).areas(area);

    // ── Cabecera: logo + sesión ──
    let logo = vec![
        Line::from(Span::styled(
            "  ▶ ymmt",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "  para ti — tu YouTube Music en la terminal",
            Style::default().fg(FG_DIM),
        )),
    ];
    f.render_widget(Paragraph::new(logo), head_a);
    let session = format!("sesión: {}", app.api.kind());
    f.render_widget(
        Paragraph::new(Span::styled(session, Style::default().fg(META)))
            .alignment(ratatui::layout::Alignment::Right),
        Rect { y: head_a.y + 1, height: 1, ..head_a },
    );

    let [shelf_a, recent_a] = Layout::vertical([
        Constraint::Length(12),
        Constraint::Min(1),
    ])
    .areas(body_a);

    // ── Estante del feed: la sección seleccionada (↑/↓) con sus tarjetas ──
    let sec_title = app
        .feed
        .get(app.feed_section())
        .map(|s| s.title.clone())
        .unwrap_or_else(|| "para ti".into());
    let cards: Vec<crate::auth::FeedCard> = app
        .feed
        .get(app.feed_section())
        .map(|s| s.cards.clone())
        .unwrap_or_default();
    let sel_card = app.feed_card();
    let panel_active = app.home_panel() == crate::views::home::Panel::Moods;

    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if panel_active { ACCENT } else { FG_DIM }))
            .title(Span::styled(
                format!(" {} ", sec_title),
                Style::default().fg(SEL_TITLE).add_modifier(Modifier::BOLD),
            )),
        shelf_a,
    );
    let inner = Rect {
        x: shelf_a.x + 1,
        y: shelf_a.y + 1,
        width: shelf_a.width.saturating_sub(2),
        height: shelf_a.height.saturating_sub(2),
    };
    // Título de sección arriba de las portadas.
    let cell_w = 18u16;
    let max_cells = (inner.width / cell_w).max(1) as usize;
    let per_row = max_cells;
    let rows = (cards.len() + per_row.saturating_sub(1)) / per_row.max(1);
    let cover_h = 6u16;
    let row_h = 10u16;
    let visible_rows = (inner.height / row_h).max(1) as usize;
    let row_of_sel = sel_card / per_row.max(1);
    let first_visible_row = if row_of_sel >= visible_rows {
        row_of_sel - visible_rows + 1
    } else {
        0
    };
    let mut rendered = std::collections::HashSet::new();
    for (i, card) in cards
        .iter()
        .enumerate()
        .skip(first_visible_row * per_row)
    {
        let row = i / per_row;
        let col = i % per_row;
        if row - first_visible_row >= visible_rows {
            break;
        }
        let x = inner.x + 2 + (col as u16) * cell_w;
        let y = inner.y + 1 + ((row - first_visible_row) as u16) * row_h;
        let cover = Rect { x, y, width: 12, height: 6 };
        let chosen = i == sel_card && panel_active;
        // Marco de selección alrededor de la portada (con margen, sin chocar
        // con el borde del estante).
        if chosen {
            f.render_widget(
                Block::default().borders(Borders::ALL).border_style(Style::default().fg(ACCENT)),
                Rect { x: x.saturating_sub(2), y: y.saturating_sub(1), width: 16, height: 9 },
            );
        }
        // Portada 12x6: real si está en cache, placeholder si no.
        if app.covers.get(&card.thumb_url).is_some() {
            let img = app.covers.get(&card.thumb_url).cloned();
            if let (Some(img), Some(picker)) = (&img, &app.picker) {
                if let Ok(proto) = picker.new_protocol(
                    img.as_ref().clone(),
                    cover,
                    ratatui_image::Resize::Crop(None),
                ) {
                    f.render_widget(ratatui_image::Image::new(&proto), cover);
                    rendered.insert(i);
                }
            }
        } else {
            f.render_widget(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(FG_DIM)),
                cover,
            );
            app.request_cover(&card.thumb_url);
        }
        // Título + subtítulo bajo la portada.
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    truncate_to_width(&card.title, (cell_w - 2) as usize),
                    Style::default().fg(if chosen { ACCENT } else { FG }),
                )),
                Line::from(Span::styled(
                    truncate_to_width(&card.subtitle, (cell_w - 2) as usize),
                    Style::default().fg(FG_DIM),
                )),
            ]),
            Rect { x, y: y + 6, width: cell_w - 2, height: 2 },
        );
        let _ = rendered;
    }

    // ── Recientes ──
    let recents = app.recent_tracks().to_vec();
    let rsel = app.recent_sel();
    let mut items: Vec<ratatui::widgets::ListItem> = Vec::new();
    if recents.is_empty() {
        items.push(ratatui::widgets::ListItem::new(Line::from(Span::styled(
            " aún no escuchas nada — dale enter a una canción de arriba",
            Style::default().fg(FG_DIM),
        ))));
    } else {
    for (i, t) in recents.iter().enumerate() {
        let (style, mark) = sel_style(
            i == rsel && app.home_panel() == crate::views::home::Panel::Recent,
            false,
        );
        let dur = fmt_time(t.duration_secs);
        items.push(ratatui::widgets::ListItem::new(Line::from(vec![
            Span::styled(format!(" {mark} "), style),
            Span::styled(truncate_to_width(&t.title, 34), style),
            Span::styled(
                format!("  {}", truncate_to_width(&t.artist, 22)),
                Style::default().fg(FG_DIM),
            ),
            Span::styled(format!(" {dur:>5}"), Style::default().fg(FG_DIM)),
        ])));
    }
    }
    let list = List::new(items).block(panel("escuchado recientemente — enter: reproducir"));
    f.render_widget(list, recent_a);
    app.click_areas.home_recent = recent_a;
    app.click_areas.home_moods = shelf_a;
}

/// Ajustes: cuenta, estado de la sesión y acciones.
fn draw_settings(app: &App, f: &mut Frame, area: Rect) {
    let session_kind = app.api.kind();
    let check = app.settings_check.clone().unwrap_or_else(|| "verificando…".into());
    let cookie_age = std::fs::metadata(
        std::env::var("HOME")
            .map(|h| std::path::PathBuf::from(h).join(".config/ymmt/cookie.txt"))
            .ok()
            .unwrap_or_default(),
    )
    .ok()
    .and_then(|m| m.modified().ok())
    .map(|t| {
        let ago = std::time::SystemTime::now()
            .duration_since(t)
            .map(|d| d.as_secs() / 60)
            .unwrap_or(0);
        if ago < 60 { format!("hace {ago} min") } else { format!("hace {} h", ago / 60) }
    })
    .unwrap_or_else(|| "no guardada".into());

    let (state_ch, vol) = {
        let st = app.player.state.lock().unwrap();
        (st.volume, st.play_state)
    };
    let _ = state_ch;
    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  sesión:      ", Style::default().fg(FG)),
            Span::styled(
                session_kind,
                Style::default().fg(if session_kind == "invitado" { WARN } else { ACCENT }),
            ),
        ]),
        Line::from(vec![
            Span::styled("  cookie:      ", Style::default().fg(FG)),
            Span::styled(cookie_age, Style::default().fg(META)),
        ]),
        Line::from(vec![
            Span::styled("  estado:      ", Style::default().fg(FG)),
            Span::styled(&check, Style::default().fg(if check.starts_with('✓') { ACCENT } else if check.starts_with('✗') { WARN } else { FG_DIM })),
        ]),
        Line::from(""),
        Line::from(Span::styled("  acciones", Style::default().fg(FG_DIM))),
        Line::from("  · ymmt --login        iniciar sesión / actualizar cookie"),
        Line::from("  · ymmt --logout       cerrar sesión"),
        Line::from(""),
        Line::from(Span::styled(
            "  teclas: s ajustes · 1 inicio · 2 buscar · 3 suena · 4 cola · 5 playlists · l letras · c visualizador",
            Style::default().fg(FG_DIM),
        )),
    ];
    f.render_widget(
        Paragraph::new(lines).block(panel("ajustes")),
        centered(area, 80, 70),
    );
}

/// Vista de reproducción estilo kew: portada centrada, metadata apilada
/// (título/artista/álbum), línea de letra sincronizada, visualizador y una
/// barra de progreso con línea de tiempo completa.
fn draw_now_playing(app: &mut App, f: &mut Frame, area: Rect) {
    let st = app.player.state.lock().unwrap();
    let track = st.queue.get(st.index).cloned();
    let pos = st.position;
    let dur = st.duration;
    let vol = st.volume;
    let (idx, len) = (st.index + 1, st.queue.len());
    let playing = st.play_state == crate::player::PlayState::Playing;
    drop(st);

    let Some(track) = track else {
        let msg = Paragraph::new(Line::from(Span::styled(
            "nada suena — 1 inicio · 2 buscar · s ajustes · ? ayuda",
            Style::default().fg(FG_DIM),
        )))
        .alignment(ratatui::layout::Alignment::Center);
        f.render_widget(msg, centered(area, 60, 10));
        return;
    };

    let [art_area, meta_area] =
        Layout::vertical([Constraint::Min(8), Constraint::Length(7)]).areas(area);

    // ── Portada centrada (el visualizador se dibuja encima si está activo) ──
    let img_area = centered_square(art_area);
    if let Some(proto) = app.cover_proto(&track.thumbnail_url, img_area) {
        f.render_widget(ratatui_image::Image::new(&proto), img_area);
    } else {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(FG_DIM))
            .title(Span::styled(" ♪ ", Style::default().fg(FG_DIM)));
        f.render_widget(block, img_area);
    }
    if app.visualizer.is_some() {
        draw_visualizer(app, f, art_area);
    }

    // ── Metadata apilada (estilo kew): título / artista / álbum ──
    let [title_a, artist_a, _extra_a, lyric_a, time_a, bar_area, _pad_a] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(meta_area);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_to_width(&track.title, meta_area.width as usize),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        )))
        .alignment(ratatui::layout::Alignment::Center),
        title_a,
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_to_width(&track.artist, meta_area.width as usize),
            Style::default().fg(META),
        )))
        .alignment(ratatui::layout::Alignment::Center),
        artist_a,
    );

    // ── Línea de letra sincronizada (karaoke) ──
    let lyric_span = if let Some(ly) = &app.lyrics {
        let secs = pos.as_secs_f64();
        let cur = ly
            .lines
            .iter()
            .rev()
            .find(|l| l.time >= 0.0 && l.time <= secs)
            .map(|l| l.text.clone())
            .unwrap_or_default();
        Span::styled(
            truncate_to_width(&cur, meta_area.width as usize),
            Style::default().fg(Color::Yellow),
        )
    } else {
        Span::styled("", Style::default())
    };
    f.render_widget(
        Paragraph::new(Line::from(lyric_span)).alignment(ratatui::layout::Alignment::Center),
        lyric_a,
    );

    // ── Línea de tiempo estilo kew ──
    let total = if dur > std::time::Duration::ZERO {
        dur.as_secs()
    } else {
        track.duration_secs
    };
    let pct = if total > 0 {
        (pos.as_secs_f64() / total as f64 * 100.0) as u64
    } else {
        0
    };
    let state_ch = if playing { "" } else { "⏸ " };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(
                "{}{} / {} ({}%) Vol:{}% ({}/{})",
                state_ch,
                fmt_time(pos.as_secs()),
                fmt_time(total),
                pct,
                vol,
                idx,
                len
            ),
            Style::default().fg(FG_DIM),
        ))),
        time_a,
    );

    // ── Barra de progreso ──
    let ratio = if total > 0 {
        (pos.as_secs_f64() / total as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let w = bar_area.width as usize;
    let bar_len = w.saturating_sub(2);
    let filled = ((bar_len as f64) * ratio).round() as usize;
    let mut spans = vec![Span::raw(" ")];
    for i in 0..bar_len {
        let is_head = i == filled.saturating_sub(1).min(bar_len - 1) && filled > 0;
        spans.push(if is_head {
            Span::styled("●", Style::default().fg(ACCENT))
        } else if i < filled {
            Span::styled("━", Style::default().fg(ACCENT))
        } else {
            Span::styled("━", Style::default().fg(FG_DIM))
        });
    }
    f.render_widget(Paragraph::new(Line::from(spans)), bar_area);
}

/// Página de letras completa (tecla l): la línea actual resaltada al centro.
fn draw_lyrics_page(app: &App, f: &mut Frame, area: Rect) {
    let st = app.player.state.lock().unwrap();
    let pos = st.position;
    drop(st);
    let Some(ly) = &app.lyrics else {
        f.render_widget(
            Paragraph::new(" sin letras para esta canción (pulsa l para volver)")
                .alignment(ratatui::layout::Alignment::Center),
            area,
        );
        return;
    };
    if ly.lines.is_empty() {
        return;
    }
    // Índice de la línea actual.
    let secs = pos.as_secs_f64();
    let cur_idx = ly
        .lines
        .iter()
        .rposition(|l| l.time >= 0.0 && l.time <= secs)
        .unwrap_or(0);
    // Ventana centrada en la línea actual.
    let visible = area.height.saturating_sub(2) as usize;
    let start = cur_idx.saturating_sub(visible / 2);
    let items: Vec<Line> = ly
        .lines
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(i, l)| {
            let is_cur = i == cur_idx;
            Line::from(Span::styled(
                truncate_to_width(&l.text, area.width as usize),
                if is_cur {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(FG_DIM)
                },
            ))
        })
        .collect();
    let title = format!("letras — {} · l para volver", ly.source);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(items).block(panel(&title)).alignment(ratatui::layout::Alignment::Center),
        area,
    );
}

/// Área de portada centrada, tan grande como el área permite.
/// Las celdas de terminal son ~2x más altas que anchas: un cuadrado visual
/// de N columnas ocupa N/2 filas.
fn centered_square(area: Rect) -> Rect {
    let side_w = area.width.min(area.height.saturating_mul(2)).max(10);
    let side_h = (side_w / 2).min(area.height);
    let side_w = side_h * 2; // recomponer para proporción exacta
    Rect {
        x: area.x + (area.width.saturating_sub(side_w)) / 2,
        y: area.y + (area.height.saturating_sub(side_h)) / 2,
        width: side_w,
        height: side_h,
    }
}

/// Recorta un texto al ancho disponible.
fn truncate_to_width(s: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let width = width.max(1);
    if s.width() <= width {
        return s.to_string();
    }
    use unicode_width::UnicodeWidthChar as _;
    let mut out = String::new();
    for ch in s.chars() {
        if out.width() + ch.width().unwrap_or(0) >= width.saturating_sub(1) {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

pub fn fmt_time(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn draw_search(app: &mut App, f: &mut Frame, area: Rect) {
    use ratatui::widgets::List;
    let [input_area, list_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(area);

    // Input con estilo del resto de paneles.
    let input = Paragraph::new(Line::from(vec![
        Span::raw(" "),
        Span::styled(app.search_query.clone(), Style::default().fg(Color::Yellow)),
    ]))
    .block(panel("buscar — escribe y enter (esc: volver)"));
    f.render_widget(input, input_area);
    use unicode_width::UnicodeWidthStr;
    let cursor_x = input_area.x + 1 + app.search_query.width() as u16;
    app.set_search_cursor(Some((cursor_x, input_area.y + 1)));

    // Resultados: número, título, artista, duración a la derecha.
    app.click_areas.search_list = list_area;
    let sel = app.list_sel();
    let width = list_area.width as usize;
    let items: Vec<ratatui::widgets::ListItem> = app
        .search_results
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let selected = i == sel;
            let (style, mark) = sel_style(selected, false);
            let dur = fmt_time(t.duration_secs);
            let title_w = width.saturating_sub(dur.len() + 34);
            ratatui::widgets::ListItem::new(Line::from(vec![
                Span::styled(format!(" {mark} "), style),
                Span::styled(format!("{:<2}", i + 1), Style::default().fg(FG_DIM)),
                Span::raw(" "),
                Span::styled(truncate_to_width(&t.title, title_w), style),
                Span::styled(
                    format!("  {}", truncate_to_width(&t.artist, 20)),
                    Style::default().fg(FG_DIM),
                ),
                Span::styled(format!(" {dur:>5}"), Style::default().fg(FG_DIM)),
            ]))
        })
        .collect();
    let title = format!("resultados ({})", app.search_results.len());
    let list = List::new(items).block(panel(&title));
    let mut state = app.list_state();
    f.render_stateful_widget(list, list_area, &mut state);
    app.set_list_state(state);

    // Mini-portadas de las filas visibles (estilo YTM): pedir en background
    // las que falten y dibujar las ya cacheadas.
    let first = state.offset();
    let visible = list_area.height.saturating_sub(2) as usize;
    for row in 0..visible {
        let idx = first + row;
        let Some(t) = app.search_results.get(idx) else { break };
        let url = t.thumbnail_url.clone();
        let (x, y) = (list_area.x + 2, list_area.y + 1 + row as u16);
        draw_row_cover(f, app, &url, x, y);
        app.request_cover(&url);
    }
}

fn draw_queue(app: &mut App, f: &mut Frame, area: Rect) {
    use ratatui::widgets::List;
    let st = app.player.state.lock().unwrap();
    let sel = app.queue_sel();
    let cur = st.index;
    let items: Vec<ratatui::widgets::ListItem> = st
        .queue
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let (style, mark) = sel_style(i == sel, i == cur);
            let dur = fmt_time(t.duration_secs);
            ratatui::widgets::ListItem::new(Line::from(vec![
                Span::styled(format!(" {mark} "), style),
                Span::styled(
                    truncate_to_width(&t.title, (area.width as usize).saturating_sub(dur.len() + 28)),
                    style,
                ),
                Span::styled(
                    format!("  {}", truncate_to_width(&t.artist, 18)),
                    Style::default().fg(FG_DIM),
                ),
                Span::styled(format!(" {dur:>5}"), Style::default().fg(FG_DIM)),
            ]))
        })
        .collect();
    let title = format!("cola ({}) — enter: saltar · d: quitar", st.queue.len());
    drop(st);
    app.click_areas.queue = area;
    let list = List::new(items).block(panel(&title));
    let mut state = app.queue_state();
    f.render_stateful_widget(list, area, &mut state);
    app.set_queue_state(state);
}

fn draw_playlists(app: &mut App, f: &mut Frame, area: Rect) {
    use ratatui::widgets::List;
    let [list_area, detail_area] = Layout::horizontal([
        Constraint::Percentage(40),
        Constraint::Percentage(60),
    ])
    .areas(area);

    // Lista de playlists.
    let sel = app.playlist_sel();
    let items: Vec<ratatui::widgets::ListItem> = app
        .playlists
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let (style, mark) = sel_style(i == sel, false);
            ratatui::widgets::ListItem::new(Line::from(vec![
                Span::styled(format!(" {mark} "), style),
                Span::styled(
                    truncate_to_width(&p.title, (list_area.width as usize).saturating_sub(12)),
                    style,
                ),
                Span::styled(format!("  {}", p.count), Style::default().fg(FG_DIM)),
            ]))
        })
        .collect();
    let list = List::new(items).block(panel("playlists — enter: cargar"));
    let mut state = app.playlist_state();
    f.render_stateful_widget(list, list_area, &mut state);
    app.set_playlist_state(state);

    // Pistas de la playlist seleccionada.
    let tracks = app
        .playlists
        .get(sel)
        .and_then(|p| p.tracks.clone())
        .unwrap_or_default();
    let tsel = app.playlist_track_sel();
    let items: Vec<ratatui::widgets::ListItem> = tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let (style, mark) = sel_style(i == tsel, false);
            let dur = fmt_time(t.duration_secs);
            ratatui::widgets::ListItem::new(Line::from(vec![
                Span::styled(format!(" {mark} {:<2}", i + 1), style),
                Span::styled(
                    truncate_to_width(&t.title, (detail_area.width as usize).saturating_sub(dur.len() + 30)),
                    style,
                ),
                Span::styled(
                    format!("  {}", truncate_to_width(&t.artist, 18)),
                    Style::default().fg(FG_DIM),
                ),
                Span::styled(format!(" {dur:>5}"), Style::default().fg(FG_DIM)),
            ]))
        })
        .collect();
    let title = app
        .playlists
        .get(sel)
        .map(|p| truncate_to_width(&p.title, 30))
        .unwrap_or_else(|| "pistas".into());
    let list = List::new(items).block(panel(&title));
    let mut state = app.playlist_track_state();
    f.render_stateful_widget(list, detail_area, &mut state);
    app.set_playlist_track_state(state);
}

fn draw_popup(app: &App, popup: &Popup, f: &mut Frame) {
    let area = centered(f.area(), 70, 70);
    match popup {
        Popup::Help => {
            let mut lines = vec![Line::from(Span::styled(
                " keybinds (configurables en config.toml) ",
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            ))];
            let order = [
                "quit", "now_playing", "search", "queue", "playlists", "config",
                "help", "toggle_play", "next", "previous", "seek_forward",
                "seek_back", "volume_up", "volume_down", "shuffle", "repeat",
                "confirm", "escape",
            ];
            for action in order {
                if let Some(key) = app.config.keybinds.get(action) {
                    lines.push(Line::from(format!("  {key:<14} {action}")));
                }
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                " espacio: pausa · n/p: siguiente/anterior · +/-: volumen ",
                Style::default().fg(Color::DarkGray),
            )));
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(lines).block(Block::default().borders(Borders::ALL)),
                area,
            );
        }
        Popup::Config => {
            let _s = &app.config.settings;
            let st = app.player.state.lock().unwrap();
            let lines = vec![
                Line::from(Span::styled(
                    " configuración ",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(format!("  archivo:  {}", app.config_dir.join("config.toml").display())),
                Line::from(format!("  sesión:   {}", app.api.kind())),
                Line::from(format!("  volumen:  {}%", st.volume)),
                Line::from(format!("  shuffle:  {}", if st.shuffle { "on" } else { "off" })),
                Line::from(format!("  repeat:   {:?}", st.repeat)),
                Line::from(format!("  autoplay: {}", if st.autoplay { "on" } else { "off" })),
                Line::from(""),
                Line::from(Span::styled(
                    " edita config.toml y reinicia; esc cierra ",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            drop(st);
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(lines).block(Block::default().borders(Borders::ALL)),
                area,
            );
        }
    }
}

/// Rect centrado por porcentaje del área total.
fn centered(area: Rect, pct_x: u16, pct_y: u16) -> Rect {
    let v = Layout::vertical([
        Constraint::Percentage((100 - pct_y) / 2),
        Constraint::Percentage(pct_y),
        Constraint::Percentage((100 - pct_y) / 2),
    ])
    .split(area);
    let h = Layout::horizontal([
        Constraint::Percentage((100 - pct_x) / 2),
        Constraint::Percentage(pct_x),
        Constraint::Percentage((100 - pct_x) / 2),
    ])
    .split(v[1]);
    h[1]
}


/// Dibuja el visualizador de espectro (estilo cliamp/cava) en el área dada.
fn draw_visualizer(app: &App, f: &mut Frame, area: Rect) {
    const BLOCKS: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    let levels = app.player.spectrum.levels();
    if levels.is_empty() || area.height < 3 || area.width < 10 {
        return;
    }
    let n = levels.len();
    // Altura útil del visualizador: mitad inferior del área de portada.
    let height = (area.height / 2).max(3) as usize;
    let width = area.width as usize;
    // Cada banda ocupa 2 columnas + 1 de separación, o lo que quepa.
    let band_cols = ((width / n).max(1)).min(3);
    let gap = if band_cols >= 2 { 1 } else { 0 };

    // Colores por frecuencia: graves azules, medios verdes, agudos rojos.
    let band_color = |i: usize| -> Color {
        match i * 3 / n {
            0 => Color::Cyan,
            1 => Color::Green,
            _ => Color::Yellow,
        }
    };

    match app.vis_style {
        // ── Estilo 1: espejo (barras desde el centro hacia arriba y abajo) ──
        1 => {
            let half = (height / 2).max(2);
            for row in 0..half {
                let mut spans = Vec::new();
                for (i, level) in levels.iter().enumerate() {
                    let lv = level.clamp(0.0, 1.0);
                    // fila 0 = borde superior (lejos del centro)
                    let dist = (half - 1 - row) as f32 / half as f32;
                    let frac = (lv - dist).clamp(0.0, 1.0 / half as f32) * half as f32 * 8.0;
                    let block = BLOCKS[(frac as usize).min(7)];
                    let col = band_color(i);
                    for _ in 0..band_cols {
                        spans.push(Span::styled(block, Style::default().fg(col)));
                    }
                    if gap == 1 {
                        spans.push(Span::raw(" "));
                    }
                }
                f.render_widget(
                    Paragraph::new(Line::from(spans)),
                    Rect {
                        x: area.x,
                        y: area.y + row as u16,
                        width: area.width,
                        height: 1,
                    },
                );
            }
        }
        // ── Estilo 2: clásico cava (barras de abajo hacia arriba) ──
        _ => {
            for row in 0..height {
                let from_bottom = height - 1 - row;
                let mut spans = Vec::new();
                for (i, level) in levels.iter().enumerate() {
                    let lv = level.clamp(0.0, 1.0);
                    let rows_filled = lv * height as f32;
                    let frac = (rows_filled - from_bottom as f32).clamp(0.0, 1.0) * 8.0;
                    let block = if frac <= 0.0 {
                        if from_bottom == 0 { "▁" } else { " " }
                    } else {
                        BLOCKS[(frac as usize).min(7)]
                    };
                    let col = band_color(i);
                    for _ in 0..band_cols {
                        spans.push(Span::styled(block, Style::default().fg(col)));
                    }
                    if gap == 1 {
                        spans.push(Span::raw(" "));
                    }
                }
                f.render_widget(
                    Paragraph::new(Line::from(spans)),
                    Rect {
                        x: area.x,
                        y: area.y + row as u16,
                        width: area.width,
                        height: 1,
                    },
                );
            }
        }
    }
}
