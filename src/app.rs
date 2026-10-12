//! Estado global de la aplicación y bucle principal de eventos.

use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::auth::{self, Api};
use ytmapi_rs::common::YoutubeID as _;
use crate::config::Config;
use crate::player::{Player, PlayerCmd, PlayerEvent, RepeatMode, Track};
use crate::ui;
use crate::views::{self, Focus, Popup};
use ratatui::widgets::ListState;

pub struct App {
    pub config_dir: PathBuf,
    pub config: Config,
    pub api: Api,
    pub player: Player,
    /// Vista activa.
    pub focus: Focus,
    /// El "para ti": secciones del feed de YTM (tu FYP).
    pub feed: Vec<crate::auth::FeedSection>,
    /// Sección del feed seleccionada y tarjeta dentro de ella.
    pub feed_sel_section: usize,
    pub feed_sel_card: usize,
    /// Canciones escuchadas recientemente (login).
    pub recent: Vec<Track>,
    pub recent_sel: usize,
    /// Panel activo de la Home.
    pub home_panel: views::home::Panel,

    /// Popup activo (help, config…).
    pub popup: Option<Popup>,
    /// Búfer de búsqueda (catálogo).
    pub search_query: String,
    /// Resultados de búsqueda de canciones.
    pub search_results: Vec<Track>,
    /// Playlists del usuario.
    pub playlists: Vec<PlaylistEntry>,
    /// Estado de listas (selección / scroll).
    pub list_state: ListState,
    pub queue_state: ListState,
    pub playlist_state: ListState,
    pub playlist_track_state: ListState,
    /// Panel activo en la vista playlists.
    pub playlist_panel: views::playlist::Panel,
    /// Cargando algo (mensaje en la barra).
    pub loading: Option<String>,
    /// Mensaje de estado (barra inferior).
    pub status: String,
    pub status_at: Instant,
    /// Generación de búsqueda (descarta resultados viejos).
    pub search_gen: u64,
    /// Receptor de resultados de búsqueda en background.
    search_rx: Option<tokio::sync::mpsc::UnboundedReceiver<(u64, Result<Vec<ytmapi_rs::parse::SearchResultSong>, String>)>>,
    lyrics_rx: Option<tokio::sync::mpsc::UnboundedReceiver<anyhow::Result<ytmapi_rs::parse::Lyrics>>>,
    settings_rx: Option<tokio::sync::mpsc::UnboundedReceiver<String>>,
    search_loading_gen: Option<u64>,
    /// Modo del visualizador (None = oculto).
    pub visualizer: Option<u8>,
    /// Letras de la pista actual (None = no cargadas/sin letras).
    pub lyrics: Option<crate::auth::TimedLyrics>,
    /// Última verificación de la cookie en ajustes (None = verificando).
    pub settings_check: Option<String>,
    /// Página de letras abierta (l).
    pub lyrics_page: bool,
    /// Diseño del visualizador (0 barras, 1 espejo, 2 cava-clásico).
    pub vis_style: u8,
    /// Caché de portadas: url -> imagen decodificada.
    pub(crate) covers: HashMap<String, Arc<image::DynamicImage>>,
    /// Canal de portadas descargadas en background.
    cover_tx: tokio::sync::mpsc::UnboundedSender<(String, Arc<image::DynamicImage>)>,
    cover_rx: Option<tokio::sync::mpsc::UnboundedReceiver<(String, Arc<image::DynamicImage>)>>,
    /// URLs ya pedidas (evita duplicar descargas).
    cover_pending: std::collections::HashSet<String>,
    /// Picker de ratatui-image (protocolo de la terminal).
    pub(crate) picker: Option<ratatui_image::picker::Picker>,
    /// Posición del cursor del input de búsqueda.
    search_cursor: Option<(u16, u16)>,
    /// Áreas clickeables del último frame.
    pub click_areas: ClickAreas,
    /// Offset de scroll de la lista de búsqueda.
    pub list_offset: usize,
    /// HTTP client para portadas.
    http: reqwest::Client,
}

/// Áreas clickeables fijadas por ui::draw en el último frame.
#[derive(Debug, Clone, Default)]
pub struct ClickAreas {
    pub search_list: ratatui::layout::Rect,
    pub home_moods: ratatui::layout::Rect,
    pub home_recent: ratatui::layout::Rect,
    pub queue: ratatui::layout::Rect,
}

/// Carga inicial de la Home (desde una tarea background).
struct HomeLoad {
    feed: anyhow::Result<Vec<crate::auth::FeedSection>>,
    history: Vec<Track>,
}

pub struct PlaylistEntry {
    pub id: String,
    pub title: String,
    pub author: String,
    pub count: String,
    pub tracks: Option<Vec<Track>>,
}

impl App {
    pub async fn new(config_dir: &std::path::Path, cookie_arg: Option<String>) -> Result<Self> {
        let config = Config::load(config_dir)?;
        let api = auth::load_api(config_dir, cookie_arg).await?;
        let player = Player::new(
            config.settings.volume,
            config.settings.shuffle,
            config.settings.autoplay,
        )
        .expect("no se pudo abrir el audio");
        // Picker de portadas: halfblocks con el tamaño de celda REAL de la
        // terminal cuando se puede pedir en <300ms; si no, 8x16 típico.
        //
        // (El from_query_stdio de ratatui-image bloquea leyendo stdin y se
        // come las primeras teclas; este query propio tiene timeout corto.)
        // El picker de portadas se crea tras entrar a la alternate screen
        // (from_query_stdio detecta sixel/kitty del terminal real: portadas
        // NÍTIDAS en vez de halfblocks pixelados). Ver run().
        let picker = None;
        let session_kind = api.kind();
        let (cover_tx, cover_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = Self {
            feed: Vec::new(),
            feed_sel_section: 0,
            feed_sel_card: 0,
            recent: Vec::new(),
            recent_sel: 0,
            home_panel: views::home::Panel::Moods,

            config_dir: config_dir.to_path_buf(),
            config,
            api,
            player,
            focus: Focus::Home,
            popup: None,
            search_query: String::new(),
            search_results: Vec::new(),
            playlists: Vec::new(),
            list_state: ListState::default(),
            queue_state: ListState::default(),
            playlist_state: ListState::default(),
            playlist_track_state: ListState::default(),
            playlist_panel: views::playlist::Panel::List,
            loading: None,
            status: format!("sesión: {session_kind} — ? para ayuda"),
            status_at: Instant::now(),
            search_gen: 0,
            search_rx: None,
            search_loading_gen: None,
            lyrics_rx: None,
            settings_rx: None,
            visualizer: None,
            vis_style: 0,
            lyrics: None,
            lyrics_page: false,
            settings_check: None,
            covers: HashMap::new(),
            cover_tx,
            cover_rx: Some(cover_rx),
            cover_pending: std::collections::HashSet::new(),
            picker,
            search_cursor: None,
            click_areas: ClickAreas::default(),
            list_offset: 0,
            http: reqwest::Client::new(),
        };
        app.list_state.select(Some(0));
        app.queue_state.select(Some(0));
        app.playlist_state.select(Some(0));
        app.playlist_track_state.select(Some(0));

        // Nota: los datos de la Home (ánimos + recientes) se cargan en run()
        // una vez abierta la terminal, para no bloquear el arranque.
        Ok(app)
    }

    /// Carga los datos de la Home (feed "para ti" + recientes).
    /// Se llama al arrancar y tras un login.
    pub async fn load_home(&mut self) {
        self.loading = Some("cargando tu feed…".into());
        // 1. El feed "para ti" real de YouTube Music (tu FYP).
        match self.api.home_feed().await {
            Ok(sections) => {
                self.feed = sections;
                self.loading = None;
                let n: usize = self.feed.iter().map(|s| s.cards.len()).sum();
                self.set_status(format!("{n} recomendaciones para ti"));
                self.prime_feed_covers();
            }
            Err(e) => {
                self.loading = None;
                self.set_status(format!("⚠ feed: {e}"));
            }
        }
        // 2. Recientes (solo con login). El historial vacío devuelve None.
        if let Ok(Some(tracks)) = self.api.history().await {
            self.recent = tracks;
        }
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
        self.status_at = Instant::now();
    }

    // ----- helpers de selección (usados por ui.rs y views/) -----

    pub fn list_sel(&self) -> usize {
        self.list_state.selected().unwrap_or(0)
    }
    pub fn move_list_sel(&mut self, delta: i32) {
        let max = self.search_results.len().saturating_sub(1);
        let cur = self.list_sel() as i32;
        let new = (cur + delta).clamp(0, max as i32) as usize;
        self.list_state.select(Some(new));
    }
    pub fn list_state(&mut self) -> ListState {
        self.list_state.clone()
    }
    pub fn set_list_state(&mut self, s: ListState) {
        self.list_offset = s.offset();
        self.list_state = s;
    }

    pub fn queue_sel(&self) -> usize {
        self.queue_state.selected().unwrap_or(0)
    }
    pub fn move_queue_sel(&mut self, delta: i32) {
        let max = self.player.state.lock().unwrap().queue.len().saturating_sub(1);
        let new = (self.queue_sel() as i32 + delta).clamp(0, max as i32) as usize;
        self.queue_state.select(Some(new));
    }
    pub fn queue_state(&mut self) -> ListState {
        self.queue_state.clone()
    }
    pub fn set_queue_state(&mut self, s: ListState) {
        self.queue_state = s;
    }

    pub fn playlist_sel(&self) -> usize {
        self.playlist_state.selected().unwrap_or(0)
    }
    pub fn move_playlist_sel(&mut self, delta: i32) {
        let max = self.playlists.len().saturating_sub(1);
        let new = (self.playlist_sel() as i32 + delta).clamp(0, max as i32) as usize;
        self.playlist_state.select(Some(new));
    }
    pub fn playlist_state(&mut self) -> ListState {
        self.playlist_state.clone()
    }
    pub fn set_playlist_state(&mut self, s: ListState) {
        self.playlist_state = s;
    }

    pub fn playlist_track_sel(&self) -> usize {
        self.playlist_track_state.selected().unwrap_or(0)
    }
    pub fn move_playlist_track_sel(&mut self, delta: i32) {
        let max = self
            .playlists
            .get(self.playlist_sel())
            .and_then(|p| p.tracks.as_ref())
            .map(|t| t.len().saturating_sub(1))
            .unwrap_or(0);
        let new = (self.playlist_track_sel() as i32 + delta).clamp(0, max as i32) as usize;
        self.playlist_track_state.select(Some(new));
    }
    pub fn playlist_track_state(&mut self) -> ListState {
        self.playlist_track_state.clone()
    }
    pub fn set_playlist_track_state(&mut self, s: ListState) {
        self.playlist_track_state = s;
    }

    pub fn home_panel(&self) -> views::home::Panel {
        self.home_panel
    }
    pub fn home_panel_toggle(&mut self) {
        self.home_panel = match self.home_panel {
            views::home::Panel::Moods => views::home::Panel::Recent,
            views::home::Panel::Recent => views::home::Panel::Moods,
        };
    }
    /// Portadas visibles de los estantes (primera sección).
    pub fn prime_feed_covers(&mut self) {
        let urls: Vec<String> = self
            .feed
            .get(self.feed_sel_section)
            .map(|s| s.cards.iter().map(|c| c.thumb_url.clone()).collect())
            .unwrap_or_default();
        for u in urls.into_iter().take(12) {
            self.request_cover(&u);
        }
    }

    /// Sección del feed seleccionada.
    pub fn feed_section(&self) -> usize {
        self.feed_sel_section
    }

    pub fn move_feed_section(&mut self, dir: i32) {
        let max = self.feed.len().saturating_sub(1);
        self.feed_sel_section =
            (self.feed_sel_section as i32 + dir).clamp(0, max as i32) as usize;
        self.prime_feed_covers();
    }

    /// Fija la tarjeta seleccionada directamente (clicks del ratón).
    pub fn move_feed_card_raw(&mut self, idx: usize) {
        let max = self
            .feed
            .get(self.feed_sel_section)
            .map(|s| s.cards.len().saturating_sub(1))
            .unwrap_or(0);
        self.feed_sel_card = idx.min(max);
        self.prime_feed_covers();
    }

    /// Tarjeta seleccionada dentro de la sección.
    pub fn feed_card(&self) -> usize {
        self.feed_sel_card
    }

    pub fn move_feed_card(&mut self, dir: i32) {
        let max = self
            .feed
            .get(self.feed_sel_section)
            .map(|s| s.cards.len().saturating_sub(1))
            .unwrap_or(0);
        self.feed_sel_card =
            (self.feed_sel_card as i32 + dir).clamp(0, max as i32) as usize;
    }

    /// Ejecuta la tarjeta seleccionada: reproduce canción o abre playlist.
    pub async fn activate_feed_card(&mut self) {
        let Some(card) = self
            .feed
            .get(self.feed_sel_section)
            .and_then(|s| s.cards.get(self.feed_sel_card))
            .cloned()
        else {
            return;
        };
        match card.kind {
            crate::auth::CardKind::Song => {
                // Canción: watch playlist de YouTube (la cola que YTM arma).
                if !card.video_id.is_empty() {
                    match self.api.watch_playlist(&card.video_id).await {
                        Ok(tracks) if !tracks.is_empty() => {
                            let first_is = tracks[0].video_id == card.video_id;
                            let (queue, start) = if first_is {
                                (tracks, 0)
                            } else {
                                let mut q = vec![Track {
                                    video_id: card.video_id.clone(),
                                    title: card.title.clone(),
                                    artist: card.subtitle.clone(),
                                    album: String::new(),
                                    thumbnail_url: card.thumb_url.clone(),
                                    duration_secs: 0,
                                }];
                                q.extend(tracks);
                                (q, 0)
                            };
                            self.player.cmd(PlayerCmd::PlayQueue(queue, start));
                            self.focus = Focus::NowPlaying;
                        }
                        _ => {
                            // fallback: solo la canción.
                            self.player.cmd(PlayerCmd::PlayQueue(
                                vec![Track {
                                    video_id: card.video_id.clone(),
                                    title: card.title.clone(),
                                    artist: card.subtitle.clone(),
                                    album: String::new(),
                                    thumbnail_url: card.thumb_url.clone(),
                                    duration_secs: 0,
                                }],
                                0,
                            ));
                            self.focus = Focus::NowPlaying;
                        }
                    }
                }
            }
            crate::auth::CardKind::Playlist => {
                // Playlist del feed: reproducir sus pistas. Los IDs "RD..."
                // (mixes/radio de YTM) solo responden al endpoint watch;
                // los normales (VL.../OL...) al de pistas.
                if !card.playlist_id.is_empty() {
                    let pid = card.playlist_id.trim_start_matches("VL").to_string();
                    let res = if pid.starts_with("RD") {
                        self.api.watch_playlist_by_playlist(&pid).await
                    } else {
                        self.api.playlist_tracks(&pid).await
                    };
                    match res {
                        Ok(tracks) if !tracks.is_empty() => {
                            self.set_status(format!("▶ {}", card.title));
                            self.player.cmd(PlayerCmd::PlayQueue(tracks, 0));
                            self.focus = Focus::NowPlaying;
                        }
                        _ => self.set_status("no se pudieron cargar las pistas"),
                    }
                }
            }
        }
    }

    pub fn recent_tracks(&self) -> &[Track] {
        &self.recent
    }
    pub fn recent_sel(&self) -> usize {
        self.recent_sel
    }
    pub fn set_recent_sel(&mut self, s: usize) {
        self.recent_sel = s;
    }

    /// Reproduce la radio de una playlist de ánimo (shuffle de YT).
    pub async fn play_playlist_radio(&mut self, playlist_id: String) {
        self.loading = Some("preparando radio…".into());
        match self.api.watch_playlist_by_playlist(&playlist_id).await {
            Ok(tracks) if !tracks.is_empty() => {
                self.player.cmd(crate::player::PlayerCmd::PlayQueue(tracks, 0));
                self.loading = None;
                self.focus = Focus::NowPlaying;
            }
            Ok(_) => {
                self.loading = None;
                self.set_status("la playlist no tiene pistas");
            }
            Err(e) => {
                self.loading = None;
                self.set_status(format!("⚠ radio: {e}"));
            }
        }
    }

    /// Añade las pistas de una playlist a la cola.
    pub async fn enqueue_playlist_tracks(&mut self, playlist_id: String) {
        self.loading = Some("cargando pistas…".into());
        match self.api.playlist_tracks(&playlist_id).await {
            Ok(tracks) if !tracks.is_empty() => {
                self.player.cmd(crate::player::PlayerCmd::Enqueue(tracks));
                self.loading = None;
                self.set_status("playlist añadida a la cola");
            }
            Ok(_) => {
                self.loading = None;
                self.set_status("la playlist no tiene pistas");
            }
            Err(e) => {
                self.loading = None;
                self.set_status(format!("⚠ pistas: {e}"));
            }
        }
    }

    pub fn playlist_panel(&self) -> views::playlist::Panel {
        self.playlist_panel
    }
    pub fn playlist_panel_set(&mut self, p: views::playlist::Panel) {
        self.playlist_panel = p;
    }
    pub fn playlist_panel_toggle(&mut self) {
        self.playlist_panel = match self.playlist_panel {
            views::playlist::Panel::List => views::playlist::Panel::Tracks,
            views::playlist::Panel::Tracks => views::playlist::Panel::List,
        };
    }

    pub fn search_cursor_pos(&self) -> Option<(u16, u16)> {
        self.search_cursor
    }
    pub fn set_search_cursor(&mut self, pos: Option<(u16, u16)>) {
        self.search_cursor = pos;
    }

    // ----- portadas -----

    /// Pide una portada en background (mini-covers de las listas).
    pub fn request_cover(&mut self, url: &str) {
        if url.is_empty() {
            return;
        }
        // Siempre pedir la versión hi-res (544px): la clave del cache es la
        // URL original para que todas las vistas compartan la portada.
        let hi = crate::auth::hi_res_thumb(url);
        if self.covers.contains_key(url) || self.cover_pending.contains(url) {
            return;
        }
        if self.covers.contains_key(&hi) {
            // ya está en cache bajo la clave hi-res: reinsertar bajo la original
            if let Some(img) = self.covers.get(&hi).cloned() {
                self.covers.insert(url.to_string(), img);
            }
            return;
        }
        self.cover_pending.insert(url.to_string());
        let tx = self.cover_tx.clone();
        let url_owned = hi;
        let key = url.to_string();
        let http = self.http.clone();
        tokio::spawn(async move {
            let res = async {
                let resp = http
                    .get(&url_owned)
                    .timeout(Duration::from_secs(5))
                    .send()
                    .await?;
                resp.bytes().await
            }
            .await;
            if let Ok(bytes) = res {
                if let Ok(img) = image::load_from_memory(&bytes) {
                    let _ = tx.send((key, Arc::new(img)));
                }
            }
        });
    }

    /// Aplica la verificación de sesión cuando llega (llamado en el loop).
    fn poll_settings(&mut self) {
        let Some(rx) = self.settings_rx.as_mut() else { return };
        while let Ok(msg) = rx.try_recv() {
            self.settings_check = Some(msg);
        }
    }

    /// Mete al cache las portadas que llegaron (llamado en el loop).
    pub fn poll_covers(&mut self) {
        let Some(rx) = self.cover_rx.as_mut() else { return };
        while let Ok((url, img)) = rx.try_recv() {
            self.cover_pending.remove(&url);
            self.covers.insert(url, img);
        }
    }

    /// Obtiene el protocolo de imagen para la portada (descarga + cache).
    pub fn cover_proto(
        &mut self,
        url: &str,
        area: ratatui::layout::Rect,
    ) -> Option<ratatui_image::protocol::Protocol> {
        if url.is_empty() {
            return None;
        }
        let picker = self.picker.as_ref()?;
        let img = match self.covers.get(url) {
            Some(img) => img.clone(),
            None => {
                // Descarga bloqueante (cacheada: solo la primera vez por portada).
                let url_owned = url.to_string();
                let http = self.http.clone();
                let bytes = tokio::task::block_in_place(|| {
                    futures::executor::block_on(async {
                        let resp = http
                            .get(&url_owned)
                            .timeout(Duration::from_secs(5))
                            .send()
                            .await?;
                        resp.bytes().await
                    })
                })
                .ok()?;
                let img = Arc::new(image::load_from_memory(&bytes).ok()?);
                self.covers.insert(url.to_string(), img.clone());
                img
            }
        };
        // Escalado suave (Triangle): sin pixelado al reducir la portada.
        picker
            .new_protocol(
                img.as_ref().clone(),
                area,
                ratatui_image::Resize::Fit(Some(ratatui_image::FilterType::Triangle)),
            )
            .ok()
    }

    // ----- bucle principal -----

    pub async fn run(&mut self) -> Result<()> {
        let mut terminal = ui::init_terminal()?;
        // Detectar gráficos del terminal (sixel/kitty) ya en alternate screen:
        // portadas nítidas si el terminal lo soporta; halfblocks si no.
        self.picker = ratatui_image::picker::Picker::from_query_stdio().ok();
        // La Home se carga en background: la TUI responde a teclas desde el
        // primer instante y los datos aparecen al llegar.
        let (home_tx, mut home_rx) = tokio::sync::mpsc::unbounded_channel();
        {
            // Clonamos lo justo para las queries (api no es clonable: usamos
            // un canal de comandos hacia una tarea propietaria).
            // Simplificación: cargo solo los ánimos (público) en una tarea
            // independiente con su propio cliente API, y el historial cuando
            // el usuario navega.
            let config_dir = self.config_dir.clone();
            let tx = home_tx;
            tokio::spawn(async move {
                // Cliente efímero para la carga inicial de la Home.
                match auth::load_api(&config_dir, None).await {
                    Ok(api) => {
                        let feed = api.home_feed().await;
                        let history = api.history().await.ok().flatten().unwrap_or_default();
                        let _ = tx.send(HomeLoad { feed, history });
                    }
                    Err(e) => {
                        eprintln!("ymmt: carga inicial: {e:#}");
                    }
                }
            });
        }
        // Drenar el resultado cuando llegue (en el loop).
        let mut home_loaded = false;
        let result = self.event_loop_with_home(&mut terminal, &mut home_rx, &mut home_loaded).await;
        ui::restore_terminal(&mut terminal)?;
        result
    }

    async fn event_loop_with_home(
        &mut self,
        terminal: &mut ui::Terminal,
        home_rx: &mut tokio::sync::mpsc::UnboundedReceiver<HomeLoad>,
        home_loaded: &mut bool,
    ) -> Result<()> {
        loop {
            // 1. Carga de la Home en background: aplicar al llegar.
            if !*home_loaded {
                if let Ok(load) = home_rx.try_recv() {
                    match load.feed {
                        Ok(sections) => {
                            self.feed = sections;
                            let n: usize = self.feed.iter().map(|s| s.cards.len()).sum();
                            self.set_status(format!("{n} recomendaciones para ti"));
                            // Pedir las portadas del primer estante en background.
                            self.prime_feed_covers();
                        }
                        Err(e) => self.set_status(format!("⚠ feed: {e}")),
                    }
                    self.recent = load.history;
                    *home_loaded = true;
                }
            }

            // 2. Resultados de búsqueda y letras en background.
            self.poll_search_results();
            self.poll_lyrics();
            self.poll_covers();
            self.poll_settings();

            // 3. Eventos del player.
            while let Ok(evt) = self.player.events.try_recv() {
                self.on_player_event(evt).await;
            }

            // 3. Teclado.
            if crossterm::event::poll(Duration::from_millis(100))? {
                let ev = crossterm::event::read()?;
                match ev {
                    crossterm::event::Event::Key(k) => {
                        if k.kind == crossterm::event::KeyEventKind::Press {
                            let quit = self.on_key(k).await?;
                            if quit {
                                self.player.cmd(PlayerCmd::Stop);
                                return Ok(());
                            }
                            // Redibujar INMEDIATAMENTE tras la tecla: el cursor
                            // del input queda a la par del texto, no retrasado.
                            ui::draw(self, terminal)?;
                        }
                    }
                    crossterm::event::Event::Mouse(m) => {
                        self.on_mouse(m).await;
                        ui::draw(self, terminal)?;
                    }
                    _ => {}
                }
            }

            // 4. Dibujar (también en ticks: progreso y visualizador).
            ui::draw(self, terminal)?;
        }
    }

    /// Clicks y scroll del mouse.
    async fn on_mouse(&mut self, m: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};
        let (mx, my) = (m.column, m.row);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Detectar sobre qué elemento cayó el click y actuar.
                self.handle_click(mx, my).await;
            }
            MouseEventKind::ScrollDown => {
                self.handle_scroll(1).await;
            }
            MouseEventKind::ScrollUp => {
                self.handle_scroll(-1).await;
            }
            _ => {}
        }
    }

    /// Click en (x,y): reproduce la fila clickeada según la vista activa.
    async fn handle_click(&mut self, x: u16, y: u16) {
        // Áreas de listas del último render (las fija ui::draw).
        let areas = self.click_areas.clone();
        match self.focus {
            Focus::Search => {
                // Fila clickeada en la lista de resultados.
                if x >= areas.search_list.x
                    && x < areas.search_list.x + areas.search_list.width
                    && y >= areas.search_list.y
                    && y < areas.search_list.y + areas.search_list.height
                {
                    let row = (y - areas.search_list.y - 1) as usize; // -1: borde
                    let idx = row + self.search_scroll_offset();
                    if idx < self.search_results.len() {
                        let track = self.search_results[idx].clone();
                        self.player.cmd(PlayerCmd::PlayQueue(vec![track], 0));
                        self.focus = Focus::NowPlaying;
                    }
                }
            }
            Focus::Home => {
                // Click en ánimos (columna izquierda).
                if x >= areas.home_moods.x
                    && x < areas.home_moods.x + areas.home_moods.width
                    && y >= areas.home_moods.y
                    && y < areas.home_moods.y + areas.home_moods.height
                {
                    // Click en una tarjeta del estante: seleccionar y reproducir.
                    let cell_w = 18usize;
                    let col = ((x - areas.home_moods.x).saturating_sub(1)) / cell_w as u16;
                    let cards_in_sec = self
                        .feed
                        .get(self.feed_section())
                        .map(|s| s.cards.len())
                        .unwrap_or(0);
                    let idx = col as usize;
                    if idx < cards_in_sec {
                        self.move_feed_card_raw(idx);
                        self.activate_feed_card().await;
                    }
                }
                // Click en recientes (columna derecha).
                if x >= areas.home_recent.x
                    && x < areas.home_recent.x + areas.home_recent.width
                    && y >= areas.home_recent.y
                    && y < areas.home_recent.y + areas.home_recent.height
                {
                    let row = (y - areas.home_recent.y - 1) as usize;
                    if row < self.recent.len() {
                        self.set_recent_sel(row);
                        let tracks = self.recent.clone();
                        self.player.cmd(PlayerCmd::PlayQueue(tracks, row));
                        self.focus = Focus::NowPlaying;
                    }
                }
            }
            Focus::Queue => {
                if x >= areas.queue.x
                    && x < areas.queue.x + areas.queue.width
                    && y >= areas.queue.y
                    && y < areas.queue.y + areas.queue.height
                {
                    let row = (y - areas.queue.y - 1) as usize;
                    if row < self.player.state.lock().unwrap().queue.len() {
                        self.player.cmd(PlayerCmd::JumpTo(row));
                    }
                }
            }
            _ => {}
        }
    }

    /// Scroll: mueve la selección de la lista activa.
    async fn handle_scroll(&mut self, dir: i32) {
        match self.focus {
            Focus::Search => self.move_list_sel(dir),
            Focus::Home => {
                let _ = dir;
                // Scroll sobre el estante: mover la tarjeta seleccionada.
                self.move_feed_card(dir);
            }
            Focus::Queue => self.move_queue_sel(dir),
            Focus::Playlists => self.move_playlist_sel(dir),
            _ => {}
        }
    }

    /// Offset de scroll de la lista de búsqueda (primera fila visible).
    fn search_scroll_offset(&self) -> usize {
        self.list_offset
    }



    async fn on_player_event(&mut self, evt: PlayerEvent) {
        match evt {
            PlayerEvent::TrackChanged(idx) => {
                let (label, vid) = {
                    let st = self.player.state.lock().unwrap();
                    (
                        st.queue.get(idx).map(|t| format!("▶ {}", t.display())),
                        st.queue.get(idx).map(|t| t.video_id.clone()),
                    )
                };
                if let Some(label) = label {
                    self.set_status(label);
                }
                // Letras de la pista nueva (en background vía canal).
                if let Some(vid) = vid {
                    self.fetch_lyrics(vid);
                }
            }
            PlayerEvent::StateChanged(s) => match s {
                crate::player::PlayState::Stopped => {
                    // No sobreescribir un error con "detenido": el error explica
                    // el porqué (p.ej. cookie faltante).
                    if !self.status.starts_with('⚠') {
                        self.set_status("detenido");
                    }
                }
                _ => {}
            },
            PlayerEvent::Error(e) => self.set_status(format!("⚠ {e}")),
            PlayerEvent::NeedsAutofill => {
                let vid = {
                    let st = self.player.state.lock().unwrap();
                    st.queue.last().map(|t| t.video_id.clone()).unwrap_or_default()
                };
                if !vid.is_empty() {
                    self.autofill_radio(vid).await;
                }
            }
            _ => {}
        }
    }

    async fn autofill_radio(&mut self, video_id: String) {
        self.set_status("… autoplay: pidiendo radio");
        match self.api.watch_playlist(&video_id).await {
            Ok(tracks) => {
                if !tracks.is_empty() {
                    let tail = if tracks[0].video_id == video_id {
                        tracks[1..].to_vec()
                    } else {
                        tracks
                    };
                    self.player.cmd(PlayerCmd::Enqueue(tail));
                    self.set_status("▶ radio encolada");
                }
            }
            Err(e) => self.set_status(format!("⚠ autoplay: {e}")),
        }
    }

    /// Maneja una tecla; devuelve true para salir.
    async fn on_key(&mut self, key: crossterm::event::KeyEvent) -> Result<bool> {
        // Popup activo: solo cierre.
        if self.popup.is_some() {
            return views::handle_popup_key(self, key);
        }

        let combo = ui::key_event_to_combo(key);
        let code = key.code;
        // ¿La tecla lleva Ctrl/Alt? En búsqueda, las teclas simples son TEXTO;
        // los keybinds solo responden con modificador.
        let with_mods = key
            .modifiers
            .intersects(crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT);

        // ── Vista de búsqueda: input de texto ─────────────────────────────
        if self.focus == Focus::Search {
            if combo == "ctrl+c" {
                return Ok(true);
            }
            if with_mods {
                // Con Ctrl: los keybinds de reproducción/vista funcionan.
                if let Some(cmd) = self.playback_cmd_for(&combo) {
                    self.player.cmd(cmd);
                    return Ok(false);
                }
                if let Some(focus) = self.view_for_combo(&combo) {
                    self.focus = focus;
                    if focus == Focus::Playlists && self.playlists.is_empty() {
                        self.load_playlists().await;
                    }
                    return Ok(false);
                }
                if combo == self.keybind("config") {
                    self.popup = Some(Popup::Config);
                    return Ok(false);
                }
                if combo == self.keybind("help") {
                    self.popup = Some(Popup::Help);
                    return Ok(false);
                }
                if combo == self.keybind("quit") {
                    return Ok(true);
                }
                // Otro Ctrl+tecla: ignorar.
                return Ok(false);
            }
            // Sin modificadores: la tecla va al input (texto/navegación).
            views::search::handle_key(self, key).await?;
            return Ok(false);
        }

        // 1. Salida global (ctrl+q también, por seguridad).
        if combo == "ctrl+c" || combo == self.keybind("quit") {
            return Ok(true);
        }

        // Ajustes con 's' (literal): siempre disponible fuera de búsqueda.
        if self.focus != Focus::Search && combo == "s" && self.popup.is_none() {
            self.focus = Focus::Settings;
            if self.settings_check.is_none() {
                self.run_cookie_check();
            }
            return Ok(false);
        }

        // 2. Control de reproducción global (disponible en cualquier vista).
        if combo == self.keybind("toggle_play") {
            self.player.cmd(PlayerCmd::TogglePause);
            return Ok(false);
        }
        if combo == self.keybind("next") {
            self.player.cmd(PlayerCmd::Next);
            return Ok(false);
        }
        if combo == self.keybind("previous") {
            self.player.cmd(PlayerCmd::Previous);
            return Ok(false);
        }
        // En el estante de la Home, left/right navegan tarjetas (no seek).
        let home_shelf = self.focus == Focus::Home
            && self.home_panel == views::home::Panel::Moods;
        if home_shelf && (combo == self.keybind("seek_forward") || combo == self.keybind("seek_back")) {
            let dir = if combo == self.keybind("seek_forward") { 1 } else { -1 };
            self.move_feed_card(dir);
            return Ok(false);
        }
        if combo == self.keybind("seek_forward") {
            self.player.cmd(PlayerCmd::SeekRelative(10.0));
            return Ok(false);
        }
        if combo == self.keybind("seek_back") {
            self.player.cmd(PlayerCmd::SeekRelative(-10.0));
            return Ok(false);
        }
        if combo == self.keybind("volume_up") {
            self.player.cmd(PlayerCmd::Volume(5));
            return Ok(false);
        }
        if combo == self.keybind("volume_down") {
            self.player.cmd(PlayerCmd::Volume(-5));
            return Ok(false);
        }
        if combo == self.keybind("shuffle") && combo != "s" {
            let on = !self.player.state.lock().unwrap().shuffle;
            self.player.cmd(PlayerCmd::Shuffle(on));
            self.set_status(if on { "shuffle on" } else { "shuffle off" });
            return Ok(false);
        }
        if combo == self.keybind("repeat") {
            let next = match self.player.state.lock().unwrap().repeat {
                RepeatMode::Off => RepeatMode::All,
                RepeatMode::All => RepeatMode::One,
                RepeatMode::One => RepeatMode::Off,
            };
            self.player.cmd(PlayerCmd::Repeat(next));
            let label = match next {
                RepeatMode::Off => "repeat off",
                RepeatMode::All => "repeat all",
                RepeatMode::One => "repeat one",
            };
            self.set_status(label);
            return Ok(false);
        }

        // 3. Cambio de vista.
        if combo == self.keybind("home") {
            self.focus = Focus::Home;
            return Ok(false);
        }
        if combo == self.keybind("now_playing") {
            self.focus = Focus::NowPlaying;
            return Ok(false);
        }
        if combo == self.keybind("search") {
            self.focus = Focus::Search;
            return Ok(false);
        }
        if combo == self.keybind("queue") {
            // Sincronizar selección de la cola con la pista actual.
            let cur = self.player.state.lock().unwrap().index;
            self.queue_state.select(Some(cur));
            self.focus = Focus::Queue;
            return Ok(false);
        }
        if combo == self.keybind("playlists") {
            self.focus = Focus::Playlists;
            if self.playlists.is_empty() {
                self.load_playlists().await;
            }
            return Ok(false);
        }
        if combo == self.keybind("config") {
            self.popup = Some(Popup::Config);
            return Ok(false);
        }
        if combo == self.keybind("help") {
            self.popup = Some(Popup::Help);
            return Ok(false);
        }

        // Visualizador: 'c' alterna on/off (solo fuera de la búsqueda, donde
        // 'c' es texto). Shift+C cambia de diseño.
        if self.focus != Focus::Search {
            if combo == "l" {
                self.lyrics_page = !self.lyrics_page;
                return Ok(false);
            }
            if combo == "C" {
                self.vis_style = (self.vis_style + 1) % 3;
                let names = ["barras", "espejo", "clásico"];
                self.set_status(format!("visualizador: {}", names[self.vis_style as usize]));
                return Ok(false);
            }
            if combo == self.keybind("visualizer") {
                self.visualizer = match self.visualizer {
                    None => Some(0),
                    Some(_) => None,
                };
                let on = self.visualizer.is_some();
                self.set_status(if on { "visualizador on" } else { "visualizador off" });
                return Ok(false);
            }
        }

        // 4. Teclas propias de la vista activa.
        match self.focus {
            Focus::Home => views::home::handle_key(self, key).await?,
            Focus::Search => views::search::handle_key(self, key).await?,
            Focus::Queue => views::queue::handle_key(self, key).await?,
            Focus::Playlists => views::playlist::handle_key(self, key).await?,
            Focus::Settings | Focus::NowPlaying => {}
        }

        Ok(false)
    }

    /// Un combo de teclas con Ctrl -> comando de reproducción (o None).
    fn playback_cmd_for(&self, combo: &str) -> Option<PlayerCmd> {
        if combo == &self.keybind("toggle_play") {
            return Some(PlayerCmd::TogglePause);
        }
        if combo == &self.keybind("next") {
            return Some(PlayerCmd::Next);
        }
        if combo == &self.keybind("previous") {
            return Some(PlayerCmd::Previous);
        }
        if combo == &self.keybind("seek_forward") {
            return Some(PlayerCmd::SeekRelative(10.0));
        }
        if combo == &self.keybind("seek_back") {
            return Some(PlayerCmd::SeekRelative(-10.0));
        }
        if combo == &self.keybind("volume_up") {
            return Some(PlayerCmd::Volume(5));
        }
        if combo == &self.keybind("volume_down") {
            return Some(PlayerCmd::Volume(-5));
        }
        if combo == &self.keybind("shuffle") {
            let on = !self.player.state.lock().unwrap().shuffle;
            return Some(PlayerCmd::Shuffle(on));
        }
        if combo == &self.keybind("repeat") {
            let next = match self.player.state.lock().unwrap().repeat {
                RepeatMode::Off => RepeatMode::All,
                RepeatMode::All => RepeatMode::One,
                RepeatMode::One => RepeatMode::Off,
            };
            return Some(PlayerCmd::Repeat(next));
        }
        None
    }

    /// Un combo de teclas con Ctrl -> vista destino (o None).
    fn view_for_combo(&self, combo: &str) -> Option<Focus> {
        if combo == &self.keybind("home") {
            Some(Focus::Home)
        } else if combo == &self.keybind("search") {
            Some(Focus::Search)
        } else if combo == &self.keybind("now_playing") {
            Some(Focus::NowPlaying)
        } else if combo == &self.keybind("queue") {
            Some(Focus::Queue)
        } else if combo == &self.keybind("playlists") {
            Some(Focus::Playlists)
        } else {
            None
        }
    }

    pub fn keybind(&self, action: &str) -> String {
        self.config.keybinds.get(action).cloned().unwrap_or_default()
    }

    /// Lanza la búsqueda en background: la UI nunca se bloquea escribiendo.
    /// Los resultados viejos se descartan por generación.
    pub fn do_search(&mut self) {
        let q = self.search_query.trim().to_string();
        if q.len() < 2 {
            return;
        }
        self.search_gen += 1;
        let gen = self.search_gen;
        self.loading = Some("buscando…".into());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let cookie_path = self.config_dir.join("cookie.txt");
        std::fs::write(&self.search_req_path(), format!("{gen}\n{q}")).ok();
        tokio::spawn(async move {
            // API guest (las búsquedas son públicas; el header de OAuth rompe).
            let api = match ytmapi_rs::YtMusic::new_unauthenticated().await {
                Ok(a) => a,
                Err(e) => {
                    let _ = tx.send((gen, Err(format!("{e}"))));
                    return;
                }
            };
            let q2 = q.clone();
            let res = api
                .query(ytmapi_rs::query::SearchQuery::new_filtered(
                    q2,
                    ytmapi_rs::query::search::SongsFilter,
                ))
                .await;
            let out = match res {
                Ok(list) => Ok(list),
                Err(e) => Err(format!("{e}")),
            };
            let _ = tx.send((gen, out));
            let _ = cookie_path;
        });
        self.search_rx = Some(rx);
        self.search_loading_gen = Some(gen);
    }

    /// Ruta del archivo señal de búsqueda (coordinación interna).
    fn search_req_path(&self) -> std::path::PathBuf {
        std::env::temp_dir().join("ymmt-search-req")
    }

    /// Verifica la sesión (cookie) para la vista de ajustes.
    fn run_cookie_check(&mut self) {
        self.settings_check = Some("verificando…".into());
        let config_dir = self.config_dir.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            let msg = match auth::load_api(&config_dir, None).await {
                Ok(api) => {
                    if let auth::Api::NoAuth(_) = api {
                        "sin sesión: ejecuta ymmt --login".to_string()
                    } else {
                        // La query a veces responde vacía por rate del host:
                        // reintentar hasta 3 veces antes de declarar el estado.
                        let mut last = String::new();
                        for attempt in 0..3 {
                            match api.library_playlists().await {
                                Ok(list) => {
                                    if !list.is_empty() || attempt == 2 {
                                        last = if list.is_empty() {
                                            "✓ sesión activa (tu biblioteca de playlists está vacía)".into()
                                        } else {
                                            format!("✓ sesión activa — {} playlist(s) en tu biblioteca", list.len())
                                        };
                                        break;
                                    }
                                }
                                Err(e) => {
                                    last = format!("✗ la sesión no responde: {e}");
                                }
                            }
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        }
                        last
                    }
                }
                Err(e) => format!("✗ cookie inválida o expirada: {e}"),
            };
            let _ = tx.send(msg);
        });
        self.settings_rx = Some(rx);
    }

    /// Pide las letras de una pista en background.
    fn fetch_lyrics(&mut self, vid: String) {
        self.lyrics = None;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            // El endpoint de letras es público: cliente guest efímero.
            let res = async {
                let yt = ytmapi_rs::YtMusic::new_unauthenticated().await?;
                use ytmapi_rs::common::VideoID;
                use ytmapi_rs::query::song::{GetLyricsIDQuery, GetLyricsQuery};
                let id: ytmapi_rs::common::LyricsID = yt
                    .query(GetLyricsIDQuery::new(VideoID::from_raw(vid)))
                    .await?;
                let lyrics: ytmapi_rs::parse::Lyrics = yt.query(GetLyricsQuery::new(id)).await?;
                Ok::<_, anyhow::Error>(lyrics)
            }
            .await;
            let _ = tx.send(res);
        });
        self.lyrics_rx = Some(rx);
    }

    /// Aplica letras que hayan llegado (llamado en el event loop).
    fn poll_lyrics(&mut self) {
        let Some(rx) = self.lyrics_rx.as_mut() else { return };
        let mut got = false;
        while let Ok(res) = rx.try_recv() {
            got = true;
            self.lyrics = res.ok().and_then(|l| crate::auth::timed_lyrics_from(l));
        }
        if got {
            self.lyrics_rx = None;
        }
    }

    /// Aplica resultados de búsqueda pendientes (llamado en el event loop).
    pub fn poll_search_results(&mut self) {
        // Drenar todos los resultados pendientes sin retener el borrow.
        let pending: Vec<(u64, Result<Vec<ytmapi_rs::parse::SearchResultSong>, String>)> =
            match self.search_rx.as_mut() {
                Some(rx) => {
                    let mut out = Vec::new();
                    while let Ok(item) = rx.try_recv() {
                        out.push(item);
                    }
                    out
                }
                None => return,
            };
        for (gen, res) in pending {
            if gen != self.search_gen {
                continue; // resultado obsoleto
            }
            match res {
                Ok(list) => {
                    self.search_results = list.into_iter().map(Track::from_search).collect();
                    self.list_state.select(Some(0));
                    self.set_status(format!("{} resultados", self.search_results.len()));
                }
                Err(e) => self.set_status(format!("⚠ búsqueda: {e}")),
            }
            self.loading = None;
            self.search_loading_gen = None;
        }
    }

    pub async fn load_playlists(&mut self) {
        self.loading = Some("cargando playlists…".into());
        match self.api.library_playlists().await {
            Ok(list) => {
                self.playlists = list
                    .into_iter()
                    .map(|p| PlaylistEntry {
                        id: p.playlist_id.get_raw().to_string(),
                        title: p.title,
                        author: p.author,
                        count: p.tracks.clone(),
                        tracks: None,
                    })
                    .collect();
                self.playlist_state.select(Some(0));
                self.loading = None;
                self.set_status(format!("{} playlists", self.playlists.len()));
            }
            Err(e) => {
                self.loading = None;
                self.set_status(format!("⚠ playlists: {e}"));
            }
        }
    }

    pub async fn load_playlist_tracks(&mut self, id: String) {
        self.loading = Some("cargando pistas…".into());
        match self.api.playlist_tracks(&id).await {
            Ok(tracks) => {
                let n = tracks.len();
                if let Some(p) = self.playlists.iter_mut().find(|p| p.id == id) {
                    p.tracks = Some(tracks);
                }
                self.playlist_track_state.select(Some(0));
                self.loading = None;
                self.set_status(format!("{n} pistas"));
            }
            Err(e) => {
                self.loading = None;
                self.set_status(format!("⚠ pistas: {e}"));
            }
        }
    }
}


/// Pregunta el tamaño de celda de texto a la terminal (Text Sizing Protocol
/// de kitty / XTWINOPS) con timeout corto. None si no responde.
fn detect_font_size() -> Option<(u16, u16)> {
    use std::io::Read as _;
    use std::io::Write as _;
    use std::os::fd::AsRawFd as _;
    // Solo si stdin es una tty.
    if !unsafe { libc_isatty() } {
        return None;
    }
    // Guardar estado de termios, enviar query, leer con timeout, restaurar.
    unsafe {
        let fd = 0;
        let mut saved: std::mem::MaybeUninit<libc_termios> = std::mem::MaybeUninit::zeroed();
        if tcgetattr(fd, saved.as_mut_ptr()) != 0 {
            return None;
        }
        let saved = saved.assume_init();
        // raw mode ligero
        let mut raw = saved;
        raw.c_lflag &= !(libc_echo() | libc_icanon());
        raw.c_cc[libc_vmin()] = 0;
        raw.c_cc[libc_vtime()] = 3; // 0.3s
        if tcsetattr(fd, 0, &raw) != 0 {
            return None;
        }
        // Query de tamaño de celda: CSI 16 t (XTWINOPS responde con altura;ancho en px)
        let _ = std::io::stdout().write_all(b"\x1b[16t");
        let _ = std::io::stdout().flush();
        let mut buf = [0u8; 64];
        let n = std::io::stdin().read(&mut buf).unwrap_or(0);
        let _ = tcsetattr(fd, 0, &saved);
        if n == 0 {
            return None;
        }
        let s = String::from_utf8_lossy(&buf[..n]).to_string();
        // Respuesta: ESC [ 6 ; height ; width t
        if let Some(rest) = s.strip_prefix("\x1b[6;") {
            let rest = rest.trim_end_matches('t');
            let mut it = rest.split(';');
            if let (Some(h), Some(w)) = (it.next(), it.next()) {
                let h: u16 = h.trim().parse().ok()?;
                let w: u16 = w.trim().parse().ok()?;
                if h > 0 && w > 0 {
                    return Some((w, h));
                }
            }
        }
        None
    }
}

// Shims mínimos de libc (sin depender de la crate libc).
#[repr(C)]
#[derive(Clone, Copy)]
struct libc_termios {
    c_iflag: u32,
    c_oflag: u32,
    c_cflag: u32,
    c_lflag: u32,
    c_line: u8,
    c_cc: [u8; 32],
    c_ispeed: u32,
    c_ospeed: u32,
}
extern "C" {
    fn isatty(fd: i32) -> i32;
    fn tcgetattr(fd: i32, t: *mut libc_termios) -> i32;
    fn tcsetattr(fd: i32, act: i32, t: *const libc_termios) -> i32;
}
unsafe fn libc_isatty() -> bool {
    isatty(0) == 1
}
unsafe fn libc_echo() -> u32 {
    0o10 // ECHO
}
unsafe fn libc_icanon() -> u32 {
    0o2 // ICANON
}
unsafe fn libc_vmin() -> usize {
    6 // VMIN en Linux
}
unsafe fn libc_vtime() -> usize {
    5 // VTIME en Linux
}
