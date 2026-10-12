//! Motor de reproducción: cola, streaming progresivo y estado.
//!
//! La descarga del audio corre en su propia tarea tokio; el decoder symphonia
//! lee de un buffer spool con Read+Seek bloqueante, así la canción empieza a
//! sonar en cuanto hay unos KB descargados — sin esperar el archivo completo.

use anyhow::Result;
use rodio::source::Source as _;
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Una canción con sus metadatos.
#[derive(Debug, Clone)]
pub struct Track {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub thumbnail_url: String,
    pub duration_secs: u64,
}

impl Track {
    pub fn display(&self) -> String {
        if self.artist.is_empty() {
            self.title.clone()
        } else {
            format!("{} — {}", self.artist, self.title)
        }
    }
}

/// Estado del player visible por la UI.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlayState {
    Stopped,
    Playing,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RepeatMode {
    Off,
    All,
    One,
}

/// Comandos que la UI envía al player.
#[derive(Debug, Clone)]
pub enum PlayerCmd {
    /// Reproducir una cola nueva (reemplaza la actual). (cola, índice inicial)
    PlayQueue(Vec<Track>, usize),
    /// Añadir al final de la cola.
    Enqueue(Vec<Track>),
    /// Insertar como siguiente.
    PlayNext(Vec<Track>),
    TogglePause,
    Next,
    Previous,
    SeekRelative(f64),
    Volume(i8),
    SetVolume(u8),
    Stop,
    Shuffle(bool),
    Repeat(RepeatMode),
    RemoveFromQueue(usize),
    JumpTo(usize),
    Autoplay(bool),
}

/// Eventos que el player publica a la UI.
#[derive(Debug, Clone)]
pub enum PlayerEvent {
    TrackChanged(usize),
    StateChanged(PlayState),
    Progress(Duration, Duration),
    QueueUpdated,
    Error(String),
    /// La cola se agotó con autoplay activo: la UI debe pedir radio.
    NeedsAutofill,
}

pub struct Player {
    pub state: Arc<Mutex<PlayerState>>,
    pub events: tokio::sync::mpsc::UnboundedReceiver<PlayerEvent>,
    pub cmd_tx: tokio::sync::mpsc::UnboundedSender<PlayerCmd>,
    /// Analizador de espectro para el visualizador.
    pub spectrum: Arc<crate::spectrum::Spectrum>,
}

/// Estado interno (mutex).
pub struct PlayerState {
    pub queue: Vec<Track>,
    pub index: usize,
    pub play_state: PlayState,
    pub repeat: RepeatMode,
    pub shuffle: bool,
    pub autoplay: bool,
    pub volume: u8,
    /// Posición de reproducción de la pista actual.
    pub position: Duration,
    /// Duración de la pista actual (ZERO = desconocida aún).
    pub duration: Duration,
}

impl Player {
    pub fn new(volume: u8, shuffle: bool, autoplay: bool) -> Option<Self> {
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (evt_tx, evt_rx) = tokio::sync::mpsc::unbounded_channel();
        let spectrum = crate::spectrum::Spectrum::new();
        let spectrum_task = spectrum.clone();
        let state = Arc::new(Mutex::new(PlayerState {
            queue: Vec::new(),
            index: 0,
            play_state: PlayState::Stopped,
            repeat: RepeatMode::Off,
            shuffle,
            autoplay,
            volume,
            position: Duration::ZERO,
            duration: Duration::ZERO,
        }));
        tokio::spawn(player_task(state.clone(), cmd_rx, evt_tx, spectrum_task));
        Some(Self {
            state,
            events: evt_rx,
            cmd_tx,
            spectrum,
        })
    }

    pub fn cmd(&self, c: PlayerCmd) {
        let _ = self.cmd_tx.send(c);
    }
}

// ---------------------------------------------------------------------------
// Spool: buffer en memoria con Read+Seek bloqueante para el decoder.
// ---------------------------------------------------------------------------

pub struct Spool {
    inner: Mutex<SpoolInner>,
    writer_done: AtomicBool,
}

struct SpoolInner {
    buf: Vec<u8>,
    read_pos: usize,
    eof: bool,
    /// Tamaño total del archivo, conocido desde que la descarga empieza.
    total: Option<u64>,
}

impl Spool {
    pub fn new() -> Arc<Self> {
        Arc::new(Spool {
            inner: Mutex::new(SpoolInner {
                buf: Vec::with_capacity(1 << 20),
                read_pos: 0,
                eof: false,
                total: None,
            }),
            writer_done: AtomicBool::new(false),
        })
    }

    /// El tamaño total del archivo, en cuanto la descarga lo conoce.
    pub fn set_total(&self, total: u64) {
        self.inner.lock().unwrap().total = Some(total);
    }

    pub fn total(&self) -> Option<u64> {
        self.inner.lock().unwrap().total
    }

    pub fn write_chunk(&self, chunk: &[u8]) {
        self.inner.lock().unwrap().buf.extend_from_slice(chunk);
    }

    /// Descarga completada con éxito (EOF definitivo).
    pub fn finish(&self) {
        self.writer_done.store(true, Ordering::SeqCst);
        self.inner.lock().unwrap().eof = true;
    }

    /// Descarga falló: EOF inmediato para no colgar al lector.
    pub fn fail(&self) {
        self.finish();
    }

    pub fn downloaded(&self) -> u64 {
        self.inner.lock().unwrap().buf.len() as u64
    }

    pub fn is_done(&self) -> bool {
        self.writer_done.load(Ordering::SeqCst)
    }
}

impl Read for Spool {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        loop {
            {
                let mut inner = self.inner.lock().unwrap();
                if inner.read_pos < inner.buf.len() {
                    let n = out.len().min(inner.buf.len() - inner.read_pos);
                    out[..n].copy_from_slice(&inner.buf[inner.read_pos..inner.read_pos + n]);
                    inner.read_pos += n;
                    return Ok(n);
                }
                if inner.eof {
                    return Ok(0);
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Seek for Spool {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let mut inner = self.inner.lock().unwrap();
        let target: i64 = match pos {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(n) => inner.read_pos as i64 + n,
            SeekFrom::End(n) => inner.buf.len() as i64 + n,
        };
        if target < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek negativo",
            ));
        }
        let target = target as usize;
        loop {
            if target <= inner.buf.len() {
                inner.read_pos = target;
                return Ok(target as u64);
            }
            if inner.eof {
                inner.read_pos = inner.buf.len().min(target);
                return Ok(inner.read_pos as u64);
            }
            drop(inner);
            std::thread::sleep(Duration::from_millis(25));
            inner = self.inner.lock().unwrap();
        }
    }
}


/// Lector del spool para el decoder: delega en el Arc compartido.
/// rodio requiere Read+Seek+Send+Sync+'static sobre el tipo concreto.
pub struct SpoolReader(pub Arc<Spool>);

impl Read for SpoolReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        loop {
            {
                let mut inner = self.0.inner.lock().unwrap();
                if inner.read_pos < inner.buf.len() {
                    let n = out.len().min(inner.buf.len() - inner.read_pos);
                    out[..n].copy_from_slice(&inner.buf[inner.read_pos..inner.read_pos + n]);
                    inner.read_pos += n;
                    return Ok(n);
                }
                if inner.eof {
                    return Ok(0);
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Seek for SpoolReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let mut inner = self.0.inner.lock().unwrap();
        let end = match inner.total {
            Some(t) => t as i64,
            None => inner.buf.len() as i64,
        };
        let target: i64 = match pos {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(n) => inner.read_pos as i64 + n,
            SeekFrom::End(n) => end + n,
        };
        if target < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek negativo",
            ));
        }
        let target = target as usize;
        loop {
            if target <= inner.buf.len() {
                inner.read_pos = target;
                return Ok(target as u64);
            }
            if inner.eof {
                inner.read_pos = inner.buf.len().min(target);
                return Ok(inner.read_pos as u64);
            }
            drop(inner);
            std::thread::sleep(Duration::from_millis(25));
            inner = self.0.inner.lock().unwrap();
        }
    }
}

// ---------------------------------------------------------------------------
// Tarea del player
// ---------------------------------------------------------------------------

const TICK: Duration = Duration::from_millis(100);
/// Se considerará "fin de pista" si el sink está vacío y el decoder ya fue
/// consumido (o si position >= duration conocida).
const STALL_LIMIT: Duration = Duration::from_secs(20);

async fn player_task(
    state: Arc<Mutex<PlayerState>>,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<PlayerCmd>,
    evt_tx: tokio::sync::mpsc::UnboundedSender<PlayerEvent>,
    spectrum: Arc<crate::spectrum::Spectrum>,
) {
    let Ok(device_sink) = rodio::DeviceSinkBuilder::open_default_sink() else {
        let _ = evt_tx.send(PlayerEvent::Error(
            "no se pudo abrir el dispositivo de audio (¿PipeWire/ALSA?)".into(),
        ));
        return;
    };
    let core = rodio::Player::connect_new(&device_sink.mixer());
    core.set_volume(vol_to_f32(state.lock().unwrap().volume));

    // Índice de la pista cargada actualmente en el sink (None = silencio).
    let mut loaded: Option<usize> = None;
    // Descarga en curso (para poder cancelar al saltar de pista).
    let mut active_dl: Option<Arc<AtomicBool>> = None;

    loop {
        // 1. Procesar TODOS los comandos pendientes.
        while let Ok(cmd) = cmd_rx.try_recv() {
            handle_cmd(cmd, &state, &core, &evt_tx, &mut loaded, &mut active_dl);
        }

        // 2. Si hay pista pendiente de cargar, cargarla (bloquea el tick el
        //    tiempo de construir el decoder; los comandos se encolan).
        if loaded.is_none() {
            let want: Option<usize> = {
                let st = state.lock().unwrap();
                match st.play_state {
                    PlayState::Playing if !st.queue.is_empty() => Some(st.index),
                    _ => None,
                }
            };
            if let Some(idx) = want {
                let video_id = state.lock().unwrap().queue[idx].video_id.clone();
                let cancel = Arc::new(AtomicBool::new(false));
                active_dl = Some(cancel.clone());
                let _ = evt_tx.send(PlayerEvent::QueueUpdated);
                match load_track(&core, &state, &evt_tx, &video_id, &cancel, &spectrum).await {
                    Ok(()) => loaded = Some(idx),
                    Err(e) => {
                        let _ = evt_tx.send(PlayerEvent::Error(e.to_string()));
                        // Saltar a la siguiente si existe.
                        let mut st = state.lock().unwrap();
                        if st.index + 1 < st.queue.len() {
                            st.index += 1;
                        } else {
                            st.play_state = PlayState::Stopped;
                            let _ = evt_tx
                                .send(PlayerEvent::StateChanged(PlayState::Stopped));
                        }
                        drop(st);
                    }
                }
                active_dl = None;
            }
        }

        // 3. Progreso y fin de pista.
        if let Some(idx) = loaded {
            let pos = core.get_pos();
            let (dur, repeat, autoplay, has_next, queue_len) = {
                let mut st = state.lock().unwrap();
                st.position = pos;
                (
                    st.duration,
                    st.repeat,
                    st.autoplay,
                    st.index + 1 < st.queue.len(),
                    st.queue.len(),
                )
            };
            let _ = evt_tx.send(PlayerEvent::Progress(pos, dur));

            let finished = core.empty()
                || (dur > Duration::ZERO && pos >= dur)
                || pos > STALL_LIMIT && !core.is_paused() && dur == Duration::ZERO && {
                    // Sin duración conocida y el sink se quedó sin datos:
                    // tratar de detectar por bytes descargados... simplificado:
                    false
                };
            if finished && queue_len > 0 {
                match repeat {
                    RepeatMode::One => {
                        let _ = core.try_seek(Duration::ZERO);
                        if core.empty() {
                            // Re-cargar la misma pista.
                            core.stop();
                            loaded = None;
                        }
                    }
                    RepeatMode::All => {
                        let next = (idx + 1) % queue_len;
                        core.stop();
                        loaded = None;
                        state.lock().unwrap().index = next;
                        let _ = evt_tx.send(PlayerEvent::TrackChanged(next));
                    }
                    RepeatMode::Off => {
                        if has_next {
                            core.stop();
                            loaded = None;
                            state.lock().unwrap().index = idx + 1;
                            let _ = evt_tx.send(PlayerEvent::TrackChanged(idx + 1));
                        } else if autoplay {
                            core.stop();
                            loaded = None;
                            let _ = evt_tx.send(PlayerEvent::NeedsAutofill);
                        } else {
                            let mut st = state.lock().unwrap();
                            st.play_state = PlayState::Stopped;
                            st.position = Duration::ZERO;
                            drop(st);
                            loaded = None;
                            let _ = evt_tx.send(PlayerEvent::StateChanged(PlayState::Stopped));
                        }
                    }
                }
            }
        }

        // 4. Esperar al próximo tick o a un comando (lo que antes llegue).
        tokio::select! {
            _ = tokio::time::sleep(TICK) => {}
            Some(cmd) = cmd_rx.recv() => {
                handle_cmd(cmd, &state, &core, &evt_tx, &mut loaded, &mut active_dl);
            }
        }
    }
}

fn vol_to_f32(v: u8) -> f32 {
    (v as f32 / 100.0).powi(2)
}

fn handle_cmd(
    cmd: PlayerCmd,
    state: &Arc<Mutex<PlayerState>>,
    core: &rodio::Player,
    evt_tx: &tokio::sync::mpsc::UnboundedSender<PlayerEvent>,
    loaded: &mut Option<usize>,
    active_dl: &mut Option<Arc<AtomicBool>>,
) {
    match cmd {
        PlayerCmd::PlayQueue(tracks, start) => {
            if tracks.is_empty() {
                return;
            }
            cancel_dl(active_dl);
            core.stop();
            *loaded = None;
            let mut st = state.lock().unwrap();
            st.queue = tracks;
            st.index = start.min(st.queue.len() - 1);
            st.play_state = PlayState::Playing;
            st.position = Duration::ZERO;
            st.duration = Duration::ZERO;
            let idx = st.index;
            drop(st);
            let _ = evt_tx.send(PlayerEvent::QueueUpdated);
            let _ = evt_tx.send(PlayerEvent::TrackChanged(idx));
        }
        PlayerCmd::Enqueue(tracks) => {
            let mut st = state.lock().unwrap();
            let was_stopped = st.play_state == PlayState::Stopped || st.queue.is_empty();
            st.queue.extend(tracks);
            drop(st);
            let _ = evt_tx.send(PlayerEvent::QueueUpdated);
            if was_stopped {
                let mut st = state.lock().unwrap();
                st.play_state = PlayState::Playing;
                st.position = Duration::ZERO;
                st.duration = Duration::ZERO;
            }
        }
        PlayerCmd::PlayNext(tracks) => {
            let mut st = state.lock().unwrap();
            if st.queue.is_empty() {
                drop(st);
                return handle_cmd(
                    PlayerCmd::Enqueue(tracks),
                    state,
                    core,
                    evt_tx,
                    loaded,
                    active_dl,
                );
            }
            let at = (st.index + 1).min(st.queue.len());
            for (n, t) in tracks.into_iter().enumerate() {
                st.queue.insert(at + n, t);
            }
            drop(st);
            let _ = evt_tx.send(PlayerEvent::QueueUpdated);
        }
        PlayerCmd::TogglePause => {
            let mut st = state.lock().unwrap();
            match st.play_state {
                PlayState::Playing => {
                    core.pause();
                    st.play_state = PlayState::Paused;
                    drop(st);
                    let _ = evt_tx.send(PlayerEvent::StateChanged(PlayState::Paused));
                }
                PlayState::Paused => {
                    core.play();
                    st.play_state = PlayState::Playing;
                    drop(st);
                    let _ = evt_tx.send(PlayerEvent::StateChanged(PlayState::Playing));
                }
                PlayState::Stopped => {}
            }
        }
        PlayerCmd::Next | PlayerCmd::Previous | PlayerCmd::JumpTo(_) => {
            let mut st = state.lock().unwrap();
            if st.queue.is_empty() {
                return;
            }
            let new_index = match cmd {
                PlayerCmd::Next => (st.index + 1).min(st.queue.len() - 1),
                PlayerCmd::Previous => {
                    // Si pasaron >3s, previous reinicia la pista; si no, va a la anterior.
                    if st.position > Duration::from_secs(3) {
                        st.index
                    } else {
                        st.index.saturating_sub(1)
                    }
                }
                PlayerCmd::JumpTo(i) => i.min(st.queue.len() - 1),
                _ => unreachable!(),
            };
            st.index = new_index;
            st.play_state = PlayState::Playing;
            st.position = Duration::ZERO;
            st.duration = Duration::ZERO;
            drop(st);
            cancel_dl(active_dl);
            core.stop();
            *loaded = None;
            let _ = evt_tx.send(PlayerEvent::TrackChanged(new_index));
        }
        PlayerCmd::SeekRelative(delta) => {
            let st = state.lock().unwrap();
            let max = if st.duration > Duration::ZERO {
                st.duration.as_secs_f64()
            } else {
                st.queue.get(st.index).map(|t| t.duration_secs as f64).unwrap_or(0.0)
            };
            let target = (st.position.as_secs_f64() + delta).clamp(0.0, max.max(0.0));
            if core.try_seek(Duration::from_secs_f64(target)).is_err() {
                drop(st);
                let _ = evt_tx.send(PlayerEvent::Error("seek no disponible (buffer corto)".into()));
            }
        }
        PlayerCmd::Volume(delta) => {
            let mut st = state.lock().unwrap();
            st.volume = ((st.volume as i16) + (delta as i16)).clamp(0, 100) as u8;
            let v = st.volume;
            drop(st);
            core.set_volume(vol_to_f32(v));
            let _ = evt_tx.send(PlayerEvent::QueueUpdated);
        }
        PlayerCmd::SetVolume(v) => {
            let mut st = state.lock().unwrap();
            st.volume = v.min(100);
            let vol = st.volume;
            drop(st);
            core.set_volume(vol_to_f32(vol));
            let _ = evt_tx.send(PlayerEvent::QueueUpdated);
        }
        PlayerCmd::Stop => {
            cancel_dl(active_dl);
            core.stop();
            let mut st = state.lock().unwrap();
            st.play_state = PlayState::Stopped;
            st.position = Duration::ZERO;
            drop(st);
            *loaded = None;
            let _ = evt_tx.send(PlayerEvent::StateChanged(PlayState::Stopped));
        }
        PlayerCmd::Shuffle(on) => {
            state.lock().unwrap().shuffle = on;
            if on {
                // Reordenar la cola restante (a partir de la actual) al azar.
                let mut st = state.lock().unwrap();
                let cur = st.index;
                if cur + 1 < st.queue.len() {
                    let tail = st.queue.split_off(cur + 1);
                    let mut tail = tail;
                    shuffle_vec(&mut tail);
                    st.queue.extend(tail);
                }
            }
            let _ = evt_tx.send(PlayerEvent::QueueUpdated);
        }
        PlayerCmd::Repeat(mode) => {
            state.lock().unwrap().repeat = mode;
            let _ = evt_tx.send(PlayerEvent::QueueUpdated);
        }
        PlayerCmd::RemoveFromQueue(i) => {
            let mut st = state.lock().unwrap();
            if i < st.queue.len() {
                let was_current = i == st.index;
                st.queue.remove(i);
                if i < st.index {
                    st.index -= 1;
                }
                st.index = st.index.min(st.queue.len().saturating_sub(1));
                drop(st);
                if was_current {
                    cancel_dl(active_dl);
                    core.stop();
                    *loaded = None;
                }
                let _ = evt_tx.send(PlayerEvent::QueueUpdated);
            }
        }
        PlayerCmd::Autoplay(on) => {
            state.lock().unwrap().autoplay = on;
        }
    }
}

fn cancel_dl(active_dl: &mut Option<Arc<AtomicBool>>) {
    if let Some(flag) = active_dl.take() {
        flag.store(true, Ordering::SeqCst);
    }
}

/// Fisher-Yates con xorshift (sin depender de la crate rand).
fn shuffle_vec<T>(v: &mut [T]) {
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
        | 1;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for i in (1..v.len()).rev() {
        let j = (next() % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
}

// ---------------------------------------------------------------------------
// Carga de una pista
// ---------------------------------------------------------------------------

/// Lanza la descarga, construye el decoder sobre el spool y lo agrega al sink.
///
/// El decoder bloquea leyendo del spool mientras la descarga avanza, así el
/// audio empieza en cuanto la cabecera del contenedor está disponible.
async fn load_track(
    core: &rodio::Player,
    state: &Arc<Mutex<PlayerState>>,
    evt_tx: &tokio::sync::mpsc::UnboundedSender<PlayerEvent>,
    video_id: &str,
    cancel: &Arc<AtomicBool>,
    spectrum: &Arc<crate::spectrum::Spectrum>,
) -> Result<(), String> {
    let _ = evt_tx;
    let spool = Spool::new();

    // 1. Descarga en background.
    let spool_w = spool.clone();
    let cancel_w = cancel.clone();
    let vid = video_id.to_string();
    let dl_handle = tokio::spawn(async move {
        if let Err(e) = download_to_spool(&vid, &spool_w, &cancel_w).await {
            spool_w.fail();
            return Err(e);
        }
        spool_w.finish();
        Ok(())
    });

    // 2. Esperar a que el spool conozca el total (content_length llega con la
    //    primera respuesta) y tenga la cabecera del contenedor descargada.
    let started = std::time::Instant::now();
    while (spool.total().is_none() || spool.downloaded() < 64 * 1024)
        && !spool.is_done()
        && started.elapsed() < Duration::from_secs(10)
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }


    // 3. Construir el decoder (bloquea leyendo del spool hasta tener la
    //    cabecera; corre en spawn_blocking para no frenar el runtime).
    let spool_r = spool.clone();
    let built_fut = tokio::task::spawn_blocking(move || {
        rodio::Decoder::builder()
            .with_data(SpoolReader(spool_r))
            // NO seekable: con seek, symphonia salta al final del archivo a
            // buscar el índice (mfra) de los m4a fragmentados; si aún no está
            // descargado, bloquea para siempre. Leyendo en secuencial, el
            // primer fragmento (~170KB) ya suena.
            .with_seekable(false)
            .build()
    });
    // Timeout de construcción: sin cookie el host corta el stream tras ~1 MiB
    // y el decoder bloquearía para siempre esperando datos que no llegarán.
    let built = match tokio::time::timeout(Duration::from_secs(10), built_fut).await {
        Ok(r) => r,
        Err(_) => {
            cancel.store(true, Ordering::SeqCst);
            return Err(
                "la descarga no arrancó (YouTube calma las peticiones); reintenta en un momento"
                    .into(),
            );
        }
    };

    if cancel.load(Ordering::SeqCst) {
        // Se canceló mientras decodificábamos (salto de pista): descartar.
        return Err("carga cancelada".into());
    }

    match built {
        Ok(Ok(decoder)) => {
            let total = decoder.total_duration().unwrap_or(Duration::ZERO);
            {
                let mut st = state.lock().unwrap();
                if st.duration == Duration::ZERO {
                    st.duration = total;
                }
                // Si la API dio una duración estimada, úsala como fallback.
                if st.duration == Duration::ZERO && st.queue[st.index].duration_secs > 0 {
                    st.duration =
                        Duration::from_secs(st.queue[st.index].duration_secs);
                }
            }
            // Tap de espectro: el visualizador lee estas muestras.
            let tapped = TapSource::new(decoder, spectrum.clone());
            core.append(tapped);
            core.play();
            Ok(())
        }
        Ok(Err(e)) => {
            let _ = dl_handle.abort();
            Err(format!("no se pudo decodificar el audio: {e}"))
        }
        Err(e) => Err(format!("tarea del decoder: {e}")),
    }
}

/// Descarga el audio de YouTube al spool.
///
/// Reglas del host verificadas empíricamente (2026-10, con y sin cookie):
/// - Cada URL minteada sirve ~384 KB de datos, desde cualquier offset.
/// - El endpoint `player` hace rate-limit tras ~2 mints seguidos: responde
///   403 para cualquier rango. Esperar unos segundos lo levanta.
/// Estrategia: trozos de 360 KB, URL nueva por trozo, y en 403 un backoff
/// progresivo (5s, 10s, 20s...) porque es el endpoint el que nos calma, no
/// la URL.
async fn download_to_spool(
    video_id: &str,
    spool: &Arc<Spool>,
    cancel: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    const CHUNK: u64 = 360 * 1024; // bajo el presupuesto de ~384KB por URL
    const MAX_BACKOFFS: u32 = 10;
    const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/87.0.4280.101 Safari/537.36";

    let (cookie, po_token) = session_extras();
    let mut http_builder = reqwest::Client::builder().user_agent(UA);
    if let Some(cookie) = &cookie {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(v) = reqwest::header::HeaderValue::from_str(cookie) {
            headers.insert(reqwest::header::COOKIE, v);
        }
        http_builder = http_builder.default_headers(headers);
    }
    let http = http_builder.build()?;

    // 1. Mint inicial: URL + tamaño. ESTA URL sirve el primer trozo (evitar
    //    un segundo mint inmediato que dispara el rate-limit del endpoint).
    let (mut url, clen) = fresh_stream_url(video_id, &http, &cookie, &po_token).await?;
    if clen == 0 {
        anyhow::bail!("el stream no reporta content_length");
    }
    spool.set_total(clen);

    // 2. Trozos de 360KB. Cada URL sirve ~2 trozos (720KB); al 403 se mintera
    //    URL nueva. Entre mints, espera corta para no golpear el rate-limit.
    let mut offset: u64 = 0;
    let mut backoffs = 0u32;
    while offset < clen {
        if cancel.load(Ordering::SeqCst) {
            return Ok(());
        }
        let end = (offset + CHUNK - 1).min(clen - 1);
        let resp = http
            .get(&url)
            .header(reqwest::header::RANGE, format!("bytes={offset}-{end}"))
            .timeout(Duration::from_secs(20))
            .send()
            .await?;
        let status = resp.status();
        if status.as_u16() == 403 || status.as_u16() == 429 {
            // URL agotada: mintear una nueva y reintentar el mismo offset.
            backoffs += 1;
            if backoffs > MAX_BACKOFFS {
                anyhow::bail!("el host rechazó el stream (403) reiteradamente");
            }
            tokio::time::sleep(Duration::from_millis(700 * backoffs as u64)).await;
            match fresh_stream_url(video_id, &http, &cookie, &po_token).await {
                Ok((new_url, c)) if c == clen => url = new_url,
                Ok(_) => anyhow::bail!("el tamaño cambió al renovar"),
                Err(_) => continue, // el backoff del siguiente loop reintenta
            }
            continue;
        }
        if !status.is_success() {
            anyhow::bail!("HTTP {status} pidiendo bytes {offset}-{end}");
        }
        let bytes = resp.bytes().await?;
        if bytes.is_empty() {
            anyhow::bail!("el rango {offset}-{end} llegó vacío");
        }
        spool.write_chunk(&bytes);
        offset += bytes.len() as u64;
        backoffs = 0;
        // El endpoint player hace rate-limit tras mints seguidos: dar aire
        // entre trozos para que el siguiente mint (al 403) no salga quemado.
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    Ok(())
}

/// Cookie de sesión y po_token opcionales, desde el config dir.
/// - cookie.txt: cabecera Cookie pegada por el usuario (login por cookies).
/// - po_token.txt: PoToken de yt-dlp (guía yt-dlp/Po-Token-Guide), opcional.
fn session_extras() -> (Option<String>, Option<String>) {
    let dir = std::env::var("XDG_CONFIG_HOME")
        .map(|d| std::path::PathBuf::from(d))
        .or_else(|_| std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .map(|p| p.join("ymmt"))
        .unwrap_or_default();
    let cookie = std::fs::read_to_string(dir.join("cookie.txt"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let po_token = std::fs::read_to_string(dir.join("po_token.txt"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    (cookie, po_token)
}

/// Mintea una URL fresca del mejor formato audio/mp4 vía el player de
/// InnerTube (cliente ANDROID) con la cookie de sesión del usuario.
///
/// Verificado empíricamente: con cookie, cada URL sirve ~384 KB desde
/// cualquier offset; el endpoint hace rate-limit breve tras mints seguidos
/// (403, se levanta esperando unos segundos).
async fn fresh_stream_url(
    video_id: &str,
    http: &reqwest::Client,
    cookie: &Option<String>,
    _po_token: &Option<String>,
) -> anyhow::Result<(String, u64)> {
    // Cuerpo del player ANDROID (el mismo que verificamos con curl).
    let body = serde_json::json!({
        "context": {
            "client": {
                "clientName": "ANDROID",
                "clientVersion": "20.10.38",
                "androidSdkVersion": 30,
                "osName": "Android",
                "osVersion": "11",
                "userAgent": "com.google.android.youtube/20.10.38 (Linux; U; Android 11) gzip",
            }
        },
        "videoId": video_id,
        "contentCheckOk": true,
        "racyCheckOk": true,
    });
    let mut req = http
        .post("https://www.youtube.com/youtubei/v1/player")
        .header("Content-Type", "application/json")
        .header("User-Agent", "com.google.android.youtube/20.10.38 (Linux; U; Android 11) gzip")
        .body(body.to_string());
    if let Some(cookie) = cookie {
        req = req.header("Cookie", cookie);
    }
    let resp = req
        .timeout(Duration::from_secs(15))
        .send()
        .await?
        .error_for_status()?;
    let text = resp.bytes().await?;
    let data: serde_json::Value = serde_json::from_slice(&text)?;

    let status = data["playabilityStatus"]["status"].as_str().unwrap_or("");
    if status != "OK" {
        let reason = data["playabilityStatus"]["reason"].as_str().unwrap_or("?");
        anyhow::bail!("YouTube dice: {status} — {reason}");
    }
    // Mejor audio/mp4 con URL directa.
    let mut best: Option<(String, u64, u64)> = None; // (url, bitrate, clen)
    if let Some(formats) = data["streamingData"]["adaptiveFormats"].as_array() {
        for f in formats {
            let mt = f["mimeType"].as_str().unwrap_or("");
            let url = f["url"].as_str();
            if !mt.contains("audio/mp4") || url.is_none_or(|u| u.is_empty()) {
                continue;
            }
            let bitrate = f["bitrate"].as_u64().unwrap_or(0);
            let clen = f["contentLength"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
            if best.as_ref().is_none_or(|b| bitrate > b.1) {
                best = Some((url.unwrap().to_string(), bitrate, clen));
            }
        }
    }
    let Some((url, _bitrate, clen)) = best else {
        anyhow::bail!("sin formato audio/mp4 con URL directa");
    };
    if clen == 0 {
        anyhow::bail!("el stream no reporta content_length");
    }
    Ok((url, clen))
}

// ---------------------------------------------------------------------------
// Tap de espectro
// ---------------------------------------------------------------------------

/// Envuelve un Source y copia las muestras (mezcla mono f32) al analizador.
pub struct TapSource<S> {
    inner: S,
    spectrum: Arc<crate::spectrum::Spectrum>,
    channels: u16,
    acc: f32,
    ch_i: u16,
    sent_rate: u32,
}

impl<S: rodio::source::Source + Send + Sync + 'static> TapSource<S>
where
    S: Iterator<Item = f32>,
{
    pub fn new(inner: S, spectrum: Arc<crate::spectrum::Spectrum>) -> Self {
        let channels = inner.channels().get();
        Self {
            spectrum,
            channels,
            inner,
            acc: 0.0,
            ch_i: 0,
            sent_rate: 0,
        }
    }
}

impl<S: rodio::source::Source + Send + Sync + 'static> Iterator for TapSource<S>
where
    S: Iterator<Item = f32>,
{
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        let s = self.inner.next()?;
        // Reportar el sample rate una vez por pista (o cuando cambie).
        let rate = self.inner.sample_rate().get();
        if rate != self.sent_rate {
            self.spectrum.set_sample_rate(rate);
            self.sent_rate = rate;
        }
        // Mezcla mono: promedio de canales por frame.
        self.acc += s;
        self.ch_i += 1;
        if self.ch_i >= self.channels.max(1) {
            let mono = self.acc / self.channels.max(1) as f32;
            self.spectrum.push(std::slice::from_ref(&mono));
            self.acc = 0.0;
            self.ch_i = 0;
        }
        Some(s)
    }
}

impl<S: rodio::source::Source + Send + Sync + 'static> rodio::source::Source for TapSource<S>
where
    S: Iterator<Item = f32>,
{
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }
    fn channels(&self) -> rodio::ChannelCount {
        self.inner.channels()
    }
    fn sample_rate(&self) -> rodio::SampleRate {
        self.inner.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
}
