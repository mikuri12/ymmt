//! Autenticación con Google para YouTube Music.
//!
//! Tres modos, en orden de preferencia:
//! 1. **OAuth device flow** — el usuario abre `google.com/device` en su
//!    navegador, introduce el código, y la app recibe un token que se refresca
//!    solo. Credenciales de cliente embebidas (públicas del ecosistema);
//!    se pueden sobreescribir en la configuración.
//! 2. **Cookie SAPISID** — el usuario pega su cabecera `Cookie:` de music.youtube.com.
//! 3. **Sin login** — solo búsqueda y reproducción (sin playlists propias).

use anyhow::{Context as _, Result, bail};
use std::time::Duration;
use std::sync::OnceLock;
use std::path::{Path, PathBuf};
use ytmapi_rs::auth::{BrowserToken, OAuthToken};
use ytmapi_rs::common::{PlaylistID, VideoID, YoutubeID};
use ytmapi_rs::parse::{LibraryPlaylist, PlaylistItem, SearchResultPlaylist, SearchResultSong, WatchPlaylistTrack};
use ytmapi_rs::query::playlist::{GetPlaylistTracksQuery, GetWatchPlaylistQuery};
use ytmapi_rs::query::recommendations::{GetMoodCategoriesQuery, GetMoodPlaylistsQuery};
use ytmapi_rs::query::search::{CommunityPlaylistsFilter, SongsFilter};
use ytmapi_rs::query::{GetHistoryQuery, GetLibraryPlaylistsQuery, SearchQuery};
use ytmapi_rs::YtMusic;

use crate::player::Track;

/// Client credentials públicas del ecosistema (TV device flow).
pub const DEFAULT_CLIENT_ID: &str =
    "861556708454-d6dlm3lh05idd8npek18k6be8ba3oc68.apps.googleusercontent.com";
pub const DEFAULT_CLIENT_SECRET: &str = "SboVhoG9s0rNafixCSGGKXAT";

pub fn oauth_path(config_dir: &Path) -> PathBuf {
    config_dir.join("oauth.json")
}
pub fn cookie_path(config_dir: &Path) -> PathBuf {
    config_dir.join("cookie.txt")
}

/// El cliente API activo, según el método de autenticación disponible.
///
/// IMPORTANTE (verificado empíricamente): el header `Authorization` de OAuth
/// hace que youtubei rechace con 400 las queries públicas (search, moods...).
/// Esas queries van SIEMPRE por un cliente invitado; la sesión (OAuth/cookie)
/// se usa solo para los endpoints de cuenta: historial, playlists, library.
pub enum Api {
    OAuth(YtMusic<OAuthToken>),
    Browser(YtMusic<BrowserToken>),
    NoAuth(YtMusic<ytmapi_rs::auth::noauth::NoAuthToken>),
}

/// Cliente invitado global (perezoso, cacheado).
static GUEST: Guest = Guest(std::sync::OnceLock::new());

/// Un cliente invitado perezoso para queries públicas (cacheado).
struct Guest(OnceLock<YtMusic<ytmapi_rs::auth::noauth::NoAuthToken>>);

impl Guest {
    async fn get(&self) -> anyhow::Result<YtMusic<ytmapi_rs::auth::noauth::NoAuthToken>> {
        if let Some(yt) = self.0.get() {
            return Ok(yt.clone());
        }
        let yt = ytmapi_rs::YtMusic::new_unauthenticated().await?;
        let _ = self.0.set(yt.clone());
        Ok(yt)
    }
}

impl Api {
    /// Cliente para queries públicas: invitado cacheado (ver doc de Api).
    pub async fn public_client(
        &self,
    ) -> Result<YtMusic<ytmapi_rs::auth::noauth::NoAuthToken>> {
        match self {
            Api::NoAuth(yt) => Ok(yt.clone()),
            _ => GUEST.get().await,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Api::OAuth(_) => "oauth",
            Api::Browser(_) => "cookie",
            Api::NoAuth(_) => "invitado",
        }
    }
    /// Refresca el token OAuth si hace falta y lo persiste.
    pub async fn refresh_if_needed(&mut self, config_dir: &Path) -> Result<()> {
        if let Api::OAuth(yt) = self {
            let token = yt.refresh_token().await?;
            persist_oauth(&token, config_dir)?;
        }
        Ok(())
    }
    pub async fn search_songs(&self, query: &str) -> Result<Vec<SearchResultSong>> {
        let q = SearchQuery::new_filtered(query.to_string(), SongsFilter);
        let yt = self.public_client().await?;
        Ok(yt.query(q).await?)
    }
    /// Playlists públicas de la comunidad que coinciden con la búsqueda.
    pub async fn search_playlists(
        &self,
        query: &str,
    ) -> Result<Vec<SearchResultPlaylist>> {
        let q = SearchQuery::new_filtered(query.to_string(), CommunityPlaylistsFilter);
        let yt = self.public_client().await?;
        Ok(yt.query(q).await?)
    }
    pub async fn library_playlists(&self) -> Result<Vec<LibraryPlaylist>> {
        match self {
            Api::OAuth(yt) => Ok(yt.query(GetLibraryPlaylistsQuery).await?),
            Api::Browser(yt) => Ok(yt.query(GetLibraryPlaylistsQuery).await?),
            Api::NoAuth(_) => bail!("inicia sesión para ver tus playlists"),
        }
    }
    pub async fn playlist_tracks(&self, playlist_id: &str) -> Result<Vec<Track>> {
        let query =
            GetPlaylistTracksQuery::new(PlaylistID::from_raw(playlist_id.to_string()));
        let yt = self.public_client().await?;
        let items: Vec<PlaylistItem> = yt.query(query).await?;
        Ok(items.into_iter().filter_map(track_from_playlist).collect())
    }
    /// El "para ti" REAL: el feed de inicio de YouTube Music (FEmusic_home)
    /// con la sesión del usuario. Cada sección tiene título y tarjetas.
    pub async fn home_feed(&self) -> Result<Vec<FeedSection>> {
        // Se pide CON la sesión (es tu FYP personalizado).
        let body = serde_json::json!({
            "context": {"client": {"clientName": "WEB_REMIX",
                "clientVersion": "1.20240722.01.00", "hl": "es", "gl": "US",
                "user": {"onBehalfOfUser": ""}}},
            "browseId": "FEmusic_home",
        });
        let client = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:88.0) Gecko/20100101 Firefox/88.0")
            .build()?;
        let mut req = client
            .post("https://music.youtube.com/youtubei/v1/browse?alt=json&prettyPrint=false&key=AIzaSyC9XL3ZjWddXya6X74dJoCTL-WEYFDNX30")
            .header("Content-Type", "application/json")
            .body(body.to_string());
        if let Api::Browser(_) = self {
            if let Ok(cookie) = std::fs::read_to_string(cookie_path(&config_dir())) {
                req = req.header("Cookie", cookie.trim());
            }
        }
        let resp = req.timeout(Duration::from_secs(15)).send().await?;
        let data: serde_json::Value =
            serde_json::from_slice(&resp.bytes().await?)?;
        let mut sections = Vec::new();
        let Some(contents) = data
            .pointer("/contents/singleColumnBrowseResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents")
            .and_then(|c| c.as_array())
        else {
            bail!("el feed de inicio no trajo contenido");
        };
        for sec in contents {
            let Some(shelf) = sec.get("musicCarouselShelfRenderer") else { continue };
            // Título de la sección.
            let title = shelf
                .pointer("/header/musicCarouselShelfBasicHeaderRenderer/title/runs/0/text")
                .and_then(|t| t.as_str())
                .unwrap_or("para ti")
                .to_string();
            let mut cards = Vec::new();
            for item in shelf.get("contents").and_then(|c| c.as_array()).unwrap_or(&vec![]) {
                // Dos formas de tarjeta: TwoRow (playlist/álbum) y ResponsiveListItem (canción).
                if let Some(two) = item.get("musicTwoRowItemRenderer") {
                    let t = two
                        .pointer("/title/runs/0/text")
                        .and_then(|x| x.as_str())
                        .unwrap_or("");
                    let sub = two
                        .get("subtitle")
                        .and_then(|s| s.get("runs"))
                        .and_then(|r| r.as_array())
                        .map(|runs| {
                            runs.iter()
                                .map(|x| x.get("text").and_then(|t| t.as_str()).unwrap_or(""))
                                .collect::<String>()
                        })
                        .unwrap_or_default();
                    let bid = two
                        .pointer("/navigationEndpoint/browseEndpoint/browseId")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let thumb = two
                        .pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
                        .and_then(|a| a.as_array())
                        .and_then(|a| a.last())
                        .and_then(|t| t.get("url"))
                        .and_then(|u| u.as_str())
                        .map(hi_res_thumb)
                        .unwrap_or_default();
                    if !t.is_empty() {
                        cards.push(FeedCard {
                            title: t.to_string(),
                            subtitle: sub,
                            video_id: String::new(),
                            playlist_id: if bid.starts_with("VL") { bid } else { String::new() },
                            thumb_url: thumb,
                            kind: CardKind::Playlist,
                        });
                    }
                } else if let Some(list) = item.get("musicResponsiveListItemRenderer") {
                    let t = list
                        .pointer("/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs/0/text")
                        .and_then(|x| x.as_str())
                        .unwrap_or("");
                    let sub = list
                        .pointer("/flexColumns/1/musicResponsiveListItemFlexColumnRenderer/text/runs")
                        .and_then(|r| r.as_array())
                        .map(|runs| {
                            runs.iter()
                                .map(|x| x.get("text").and_then(|t| t.as_str()).unwrap_or(""))
                                .collect::<String>()
                        })
                        .unwrap_or_default();
                    let (vid, pid) = list
                        .pointer("/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint/watchEndpoint")
                        .map(|w| {
                            (
                                w.get("videoId").and_then(|x| x.as_str()).unwrap_or(""),
                                w.get("playlistId").and_then(|x| x.as_str()).unwrap_or(""),
                            )
                        })
                        .unwrap_or(("", ""));
                    let thumb = list
                        .pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
                        .and_then(|a| a.as_array())
                        .and_then(|a| a.last())
                        .and_then(|t| t.get("url"))
                        .and_then(|u| u.as_str())
                        .map(hi_res_thumb)
                        .unwrap_or_default();
                    if !t.is_empty() {
                        cards.push(FeedCard {
                            title: t.to_string(),
                            subtitle: sub,
                            video_id: vid.to_string(),
                            playlist_id: pid.to_string(),
                            thumb_url: thumb,
                            kind: CardKind::Song,
                        });
                    }
                }
            }
            if !cards.is_empty() {
                sections.push(FeedSection { title, cards });
            }
        }
        Ok(sections)
    }

    /// Playlists de ánimo/estado de ánimo ("Éxitos", "Relajación"...).
    /// Funciona incluso sin login: son playlists públicas de YouTube Music.
    ///
    /// NOTA: el endpoint de moods rechaza el header de autorización de OAuth
    /// (400 invalid argument), así que con sesión OAuth se pregunta como
    /// invitado con un cliente efímero.
    pub async fn moods(&self) -> Result<Vec<(String, Vec<MoodShelf>)>> {
        let yt = self.public_client().await?;
        let sections: Vec<ytmapi_rs::parse::MoodCategorySection> = yt.query(GetMoodCategoriesQuery).await?;
        Ok(sections
            .into_iter()
            .map(|s| {
                (
                    s.section_name,
                    s.mood_categories
                        .into_iter()
                        .map(|m| MoodShelf {
                            title: m.title,
                            params: m.params.get_raw().to_string(),
                        })
                        .collect(),
                )
            })
            .collect())
    }

    /// Las playlists de un ánimo concreto.
    pub async fn mood_playlists(&self, params: &str) -> Result<Vec<MoodPlaylistInfo>> {
        use ytmapi_rs::common::MoodCategoryParams;
        let q = GetMoodPlaylistsQuery::new(MoodCategoryParams::from_raw(params.to_string()));
        let yt = self.public_client().await?;
        let cats: Vec<ytmapi_rs::parse::MoodPlaylistCategory> = yt.query(q).await?;
        Ok(cats
            .into_iter()
            .flat_map(|c| c.playlists)
            .map(|p| MoodPlaylistInfo {
                playlist_id: p.playlist_id.get_raw().to_string(),
                title: p.title,
                author: p.author,
                thumbnail_url: best_thumb(&p.thumbnails),
            })
            .collect())
    }

    /// Historial de reproducción (requiere login). None si está vacío o sin
    /// sesión (el endpoint responde sin estructura cuando no hay historial).
    pub async fn history(&self) -> Result<Option<Vec<Track>>> {
        let periods: Vec<ytmapi_rs::parse::HistoryPeriod> = match self {
            Api::OAuth(yt) => yt.query(GetHistoryQuery).await,
            Api::Browser(yt) => yt.query(GetHistoryQuery).await,
            Api::NoAuth(_) => return Ok(None),
        }
        .unwrap_or_default();
        let mut tracks = Vec::new();
        for p in periods {
            for item in p.items {
                match item {
                    ytmapi_rs::parse::HistoryItem::Song(s) => tracks.push(Track {
                        video_id: s.video_id.get_raw().to_string(),
                        title: s.title,
                        artist: s.artists.first().map(|a| a.name.clone()).unwrap_or_default(),
                        album: s.album.name.clone(),
                        thumbnail_url: best_thumb(&s.thumbnails),
                        duration_secs: parse_duration(&s.duration),
                    }),
                    ytmapi_rs::parse::HistoryItem::Video(v) => tracks.push(Track {
                        video_id: v.video_id.get_raw().to_string(),
                        title: v.title,
                        artist: v.channel_name.clone(),
                        album: String::new(),
                        thumbnail_url: best_thumb(&v.thumbnails),
                        duration_secs: parse_duration(&v.duration),
                    }),
                    _ => {}
                }
            }
        }
        if tracks.is_empty() {
            Ok(None)
        } else {
            Ok(Some(tracks))
        }
    }

    /// Letras con timestamps de una pista (Musixmatch vía YTM).
    /// Devuelve (líneas con tiempo, fuente).
    pub async fn lyrics(&self, video_id: &str) -> Result<Option<TimedLyrics>> {
        // El endpoint de letras es público: cliente guest.
        let yt = self.public_client().await?;
        use ytmapi_rs::common::VideoID;
        use ytmapi_rs::query::song::{GetLyricsIDQuery, GetLyricsQuery};
        // 1. lyrics_id para el video.
        let id: ytmapi_rs::common::LyricsID = match yt
            .query(GetLyricsIDQuery::new(VideoID::from_raw(video_id.to_string())))
            .await
        {
            Ok(id) => id,
            Err(_) => return Ok(None), // sin letras disponibles
        };
        // 2. contenido.
        let lyrics: ytmapi_rs::parse::Lyrics = yt.query(GetLyricsQuery::new(id)).await?;
        Ok(parse_timed_lyrics(&lyrics.lyrics))
    }

    /// Radio de una playlist (la cola infinita que YT arma a partir de ella).
    pub async fn watch_playlist_by_playlist(&self, playlist_id: &str) -> Result<Vec<Track>> {
        let q = GetWatchPlaylistQuery::new_from_playlist_id(PlaylistID::from_raw(
            playlist_id.to_string(),
        ));
        let yt = self.public_client().await?;
        let tracks: Vec<WatchPlaylistTrack> = yt.query(q).await?;
        Ok(tracks.into_iter().map(track_from_watch).collect())
    }

    /// Radio / autoplay: la cola "up next" de YouTube para una canción.
    pub async fn watch_playlist(&self, video_id: &str) -> Result<Vec<Track>> {
        let q = GetWatchPlaylistQuery::new_from_video_id(VideoID::from_raw(video_id.to_string()));
        let yt = self.public_client().await?;
        let tracks: Vec<WatchPlaylistTrack> = yt.query(q).await?;
        Ok(tracks.into_iter().map(track_from_watch).collect())
    }
}

/// Convierte un item de playlist a Track interno.
fn track_from_playlist(item: PlaylistItem) -> Option<Track> {
    match item {
        PlaylistItem::Song(s) => Some(Track {
            video_id: s.video_id.get_raw().to_string(),
            title: s.title,
            artist: s.artists.first().map(|a| a.name.clone()).unwrap_or_default(),
            album: s.album.name.clone(),
            thumbnail_url: best_thumb(&s.thumbnails),
            duration_secs: parse_duration(&s.duration),
        }),
        PlaylistItem::Video(v) => Some(Track {
            video_id: v.video_id.get_raw().to_string(),
            title: v.title,
            artist: v.channel_name,
            album: String::new(),
            thumbnail_url: best_thumb(&v.thumbnails),
            duration_secs: parse_duration(&v.duration),
        }),
        _ => None,
    }
}

fn track_from_watch(t: WatchPlaylistTrack) -> Track {
    Track {
        video_id: t.video_id.get_raw().to_string(),
        title: t.title,
        artist: t.author,
        album: String::new(),
        thumbnail_url: best_thumb(&t.thumbnails),
        duration_secs: parse_duration(&t.duration),
    }
}

impl Track {
    pub fn from_search(s: SearchResultSong) -> Track {
        Track {
            video_id: s.video_id.get_raw().to_string(),
            title: s.title,
            artist: s.artist,
            album: s.album.map(|a| a.name).unwrap_or_default(),
            thumbnail_url: best_thumb(&s.thumbnails),
            duration_secs: parse_duration(&s.duration),
        }
    }
}

/// Miniatura de mayor resolución.
pub fn best_thumb(thumbs: &[ytmapi_rs::common::Thumbnail]) -> String {
    thumbs
        .iter()
        .max_by_key(|t| t.width * t.height)
        .map(|t| t.url.clone())
        .unwrap_or_default()
}

/// "3:42" o "1:02:03" -> segundos.
pub fn parse_duration(s: &str) -> u64 {
    let parts: Vec<u64> = s.split(':').filter_map(|p| p.trim().parse().ok()).collect();
    match parts.len() {
        1 => parts[0],
        2 => parts[0] * 60 + parts[1],
        3 => parts[0] * 3600 + parts[1] * 60 + parts[2],
        _ => 0,
    }
}

/// Carga la sesión: OAuth guardado -> cookie guardada -> sin login.
pub async fn load_api(config_dir: &Path, cookie_arg: Option<String>) -> Result<Api> {
    if let Some(cookie) = cookie_arg {
        return Ok(Api::Browser(
            YtMusic::from_cookie(&cookie)
                .await
                .context("cookie SAPISID inválida o expirada")?,
        ));
    }
    let cookie_file = cookie_path(config_dir);
    if cookie_file.exists() {
        let yt = YtMusic::from_cookie_file(&cookie_file)
            .await
            .context("cookie guardada inválida o expirada; haz login de nuevo (ymmt --login)")?;
        return Ok(Api::Browser(yt));
    }
    let oauth_file = oauth_path(config_dir);
    if oauth_file.exists() {
        let data = std::fs::read_to_string(&oauth_file)?;
        let token: OAuthToken = serde_json::from_str(&data)
            .with_context(|| format!("token OAuth corrupto en {}", oauth_file.display()))?;
        return Ok(Api::OAuth(YtMusic::from_auth_token(token)));
    }
    Ok(Api::NoAuth(YtMusic::new_unauthenticated().await?))
}

fn persist_oauth(token: &OAuthToken, config_dir: &Path) -> Result<()> {
    let path = oauth_path(config_dir);
    let data = serde_json::to_string_pretty(token)?;
    std::fs::write(&path, data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub fn logout(config_dir: &Path) -> Result<()> {
    let _ = std::fs::remove_file(oauth_path(config_dir));
    let _ = std::fs::remove_file(cookie_path(config_dir));
    Ok(())
}

/// Login. Método recomendado (y único que Google acepta hoy para YTM):
/// cookies del navegador.
///
/// OAuth ya no sirve: el API rechaza esos tokens con 400 en todos los
/// endpoints de cuenta (verificado 2026-10). Las cookies de sesión sí
/// funcionan (playlists, historial, y streams completos).
///
/// `cookie_file`: lee la cookie de un archivo (cabecera Cookie o cookies.txt
/// de Cookie-Editor/Get-cookies.txt) en vez de pedirla interactivamente.
pub async fn login_cli(config_dir: &Path, cookie_file: Option<&Path>) -> Result<()> {
    let raw = match cookie_file {
        Some(path) => {
            println!("→ Login leyendo la cookie de {}", path.display());
            std::fs::read_to_string(path)
                .with_context(|| format!("no se pudo leer {}", path.display()))?
        }
        None => {
            println!("→ Login con Google (cookie del navegador)\n");
            println!("  1. Se abrirá https://music.youtube.com en tu navegador.");
            println!("     Inicia sesión con tu cuenta si no lo estás ya.");
            println!("  2. Abre las herramientas de desarrollador (F12) → pestaña Network.");
            println!("  3. Recarga la página y haz click en la PRIMERA petición.");
            println!("  4. En 'Request Headers' haz click derecho en el valor de la cabecera");
            println!("     'Cookie:' → Copy value.");
            println!("  5. Pégala aquí y presiona ENTER.\n");
            println!("  (También vale el cookies.txt completo de 'Cookie-Editor' o");
            println!("   'Get cookies.txt' — pégalo todo; o pasa el archivo: ymmt --login cookies.txt)\n");
            let _ = open_browser("https://music.youtube.com");
            print!("Pega tu cookie (una sola línea) o todo el cookies.txt: ");
            use std::io::Write as _;
            std::io::stdout().flush()?;
            read_pasted_stdin()
        }
    };
    let cookie = normalize_cookie_input(&raw);
    if !cookie.contains("SAPISID") && !cookie.contains("__Secure-3PAPISID") {
        anyhow::bail!("la cookie no parece de sesión iniciada (falta SAPISID)");
    }
    // Diagnóstico de completitud: una sesión real de Google en youtube.com
    // siempre trae estas cookies. Si faltan, el export vino de un perfil
    // sin sesión (o de incógnito) y YouTube responderá como invitado.
    let names: Vec<String> = cookie
        .split("; ")
        .filter_map(|p| p.split('=').next().map(|s| s.trim().to_string()))
        .collect();
    let criticas = ["SID", "HSID", "APISID", "SAPISID", "SSID", "__Secure-3PSID", "LOGIN_INFO"];
    let faltan: Vec<&str> = criticas
        .iter()
        .filter(|c| !names.iter().any(|n| n == *c))
        .copied()
        .collect();

    // Validar de verdad contra el API antes de guardar: las playlists de la
    // biblioteca solo responden a una sesión real. Una cookie inventada,
    // incompleta o de invitado falla aquí en vez de guardar basura.
    let yt = match YtMusic::from_cookie(cookie.as_str()).await {
        Ok(yt) => yt,
        Err(_) => {
            if !faltan.is_empty() {
                anyhow::bail!(
                    "la cookie está incompleta: faltan {} (y quizá más).\n\
                     Exporta de nuevo DESDE EL PC, con music.youtube.com abierto y tu \
                     foto de perfil visible. Recibí {} cookies: {}",
                    faltan.join(", "),
                    names.len(),
                    names.join(", ")
                );
            }
            anyhow::bail!("cookie rechazada por YouTube");
        }
    };
    let n = match yt.query(GetLibraryPlaylistsQuery).await {
        Ok(list) => list.len(),
        Err(_) => {
            if !faltan.is_empty() {
                anyhow::bail!(
                    "YouTube respondió como INVITADO: la cookie está incompleta, faltan {}.\n\
                     Exporta de nuevo desde el PC con la sesión abierta. Recibí {}: {}",
                    faltan.join(", "),
                    names.len(),
                    names.join(", ")
                );
            }
            anyhow::bail!(
                "YouTube no reconoció la sesión (0 playlists). Suele pasar con exports \
                 hechos desde el teléfono o una ventana privada. Hazlo desde el \
                 navegador del PC con music.youtube.com y tu sesión visible."
            );
        }
    };
    if n == 0 {
        anyhow::bail!("la cookie es válida pero YouTube reportó 0 playlists; ¿seguro que es la sesión correcta?");
    }
    std::fs::write(cookie_path(config_dir), cookie)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(
            cookie_path(config_dir),
            std::fs::Permissions::from_mode(0o600),
        );
    }
    println!("\n✓ Cookie guardada ({n} playlists visibles)");
    println!("  Sesión en: {}", cookie_path(config_dir).display());
    Ok(())
}

/// Flujo OAuth completo usando las utilidades públicas de ytmapi-rs.
pub async fn login_flow(client_id: Option<String>, client_secret: Option<String>) -> Result<OAuthToken> {
    let id = client_id.unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string());
    let secret = client_secret.unwrap_or_else(|| DEFAULT_CLIENT_SECRET.to_string());
    let client = ytmapi_rs::Client::new_rustls_tls()?;
    let (code, url) = ytmapi_rs::generate_oauth_code_and_url(&client, &id).await?;
    println!("  1. Abre:  {url}");
    println!("  2. Introduce el código mostrado y autoriza con tu cuenta Google");
    let _ = open_browser(&url);
    print!("  3. Pulsa ENTER aquí cuando hayas terminado... ");
    use std::io::Write as _;
    std::io::stdout().flush()?;
    let mut buf = String::new();
    let _ = std::io::stdin().read_line(&mut buf);
    Ok(ytmapi_rs::generate_oauth_token(&client, code, &id, &secret).await?)
}

/// Lee lo pegado en stdin: la primera línea siempre; y durante los 300ms
/// siguientes todo lo que llegue de más (pegados multilínea). Nunca se queda
/// esperando para siempre: si el flujo se calla, devuelve lo que hay.
fn read_pasted_stdin() -> String {
    use std::io::BufRead as _;
    let mut raw = String::new();
    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    // Primera línea (bloqueante: el usuario aún no pegó).
    if lock.read_line(&mut raw).is_err() {
        return raw;
    }
    // El resto de un pegado multilínea: llega de golpe tras la primera línea.
    // poll de stdin con timeout: si no hay nada en 400ms, fin.
    let fd = 0;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(400);
    loop {
        if std::time::Instant::now() >= deadline || raw.len() > 200_000 {
            break;
        }
        let mut fds = pollfd { fd, events: 1 /*POLLIN*/, revents: 0 };
        let r = unsafe { poll(&mut fds, 1, 50) };
        if r <= 0 {
            continue;
        }
        let mut more = String::new();
        // read_line devuelve 0 en EOF
        match lock.read_line(&mut more) {
            Ok(0) => break,
            Ok(_) => {
                raw.push_str(&more);
                deadline_has_more();
            }
            Err(_) => break,
        }
    }
    raw
}

#[allow(clippy::too_many_arguments)]
fn deadline_has_more() {}

#[repr(C)]
struct pollfd {
    fd: i32,
    events: i16,
    revents: i16,
}
extern "C" {
    fn poll(fds: *mut pollfd, nfds: u64, timeout: i32) -> i32;
}

/// Acepta dos formatos de entrada y devuelve SIEMPRE la cabecera Cookie:
/// 1. La cabecera Cookie cruda (una línea "a=1; b=2; ...").
/// 2. Un cookies.txt Netscape exportado por extensiones: líneas
///    "dominio\tFLAG\tpath\tsecure\texpiry\tnombre\tvalor" (con tabuladores).
fn normalize_cookie_input(raw: &str) -> String {
    let raw = raw.trim();
    // ¿Formato Netscape? Líneas con 6+ campos separados por tabuladores.
    // OJO: las líneas "#HttpOnly_dominio" son cookies REALES (ahí viven
    // __Secure-3PSID y SID), no comentarios: solo se saltan las líneas
    // "#..." sin tabuladores.
    let netscape_lines: Vec<&str> = raw
        .lines()
        .map(|l| l.strip_prefix("#HttpOnly_").unwrap_or(l))
        .filter(|l| l.matches('\t').count() >= 6)
        .collect();
    if !netscape_lines.is_empty() {
        let mut pairs: Vec<String> = Vec::new();
        for line in netscape_lines {
            // Los exportadores a veces meten tabuladores de más: colapsar.
            let parts: Vec<&str> = line.split('\t').filter(|p| !p.is_empty()).collect();
            if parts.len() >= 7 {
                let (name, value) = (parts[parts.len() - 2], parts[parts.len() - 1]);
                if !name.is_empty() && !value.is_empty() {
                    pairs.push(format!("{name}={value}"));
                }
            }
        }
        return pairs.join("; ");
    }
    // Formato cabecera: ya es "a=1; b=2".
    raw.to_string()
}

/// Información de la cuenta de la sesión (nombre, email) desde el
/// account_menu de innertube con la cookie dada. None si no hay sesión.
pub async fn account_info(cookie: &str) -> Option<(String, String)> {
    let body = serde_json::json!({
        "context": {"client": {"clientName": "WEB_REMIX", "clientVersion": "1.20240722.01.00",
            "hl": "es", "gl": "US", "user": {}}},
    });
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:88.0) Gecko/20100101 Firefox/88.0")
        .build()
        .ok()?;
    let resp = client
        .post("https://music.youtube.com/youtubei/v1/account/account_menu?alt=json&prettyPrint=false&key=AIzaSyC9XL3ZjWddXya6X74dJoCTL-WEYFDNX30")
        .header("Content-Type", "application/json")
        .header("Cookie", cookie)
        .body(body.to_string())
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    let text = resp.bytes().await.ok()?;
    let data: serde_json::Value = serde_json::from_slice(&text).ok()?;
    // logged_in?
    let s = data.to_string();
    if !s.contains("\"yt_li\":\"1\"") && !s.contains("\"yt_li\": 1") {
        return None; // invitado
    }
    let (name, email) = find_account_fields(&data);
    Some((name?, email.unwrap_or_default()))
}

/// Búsqueda recursiva de accountName/email en el JSON del account_menu.
fn find_account_fields(v: &serde_json::Value) -> (Option<String>, Option<String>) {
    match v {
        serde_json::Value::Object(map) => {
            let mut name = v.get("accountName").and_then(json_text);
            let mut email = v
                .get("email")
                .and_then(|e| e.as_str())
                .map(|s| s.to_string());
            if name.is_some() && email.is_some() {
                return (name, email);
            }
            for (_, val) in map {
                let (n2, e2) = find_account_fields(val);
                name = name.or(n2);
                email = email.or(e2);
                if name.is_some() && email.is_some() {
                    return (name, email);
                }
            }
            (name, email)
        }
        serde_json::Value::Array(arr) => {
            let mut name = None;
            let mut email = None;
            for val in arr {
                let (n2, e2) = find_account_fields(val);
                name = name.or(n2);
                email = email.or(e2);
                if name.is_some() && email.is_some() {
                    return (name, email);
                }
            }
            (name, email)
        }
        _ => (None, None),
    }
}

fn json_text(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(a) => a
            .as_slice()
            .first()
            .and_then(|x| x.get("text"))
            .and_then(|t| t.as_str())
            .map(|s| s.to_string()),
        serde_json::Value::Object(_) => v
            .get("runs")
            .and_then(|r| r.as_array())
            .and_then(|a| a.first())
            .and_then(|x| x.get("text"))
            .and_then(|t| t.as_str())
            .map(|s| s.to_string()),
        _ => None,
    }
}

/// Abre una URL en el navegador predeterminado del sistema.
pub fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        for cmd in ["xdg-open", "gio", "wslview"] {
            if std::process::Command::new(cmd)
                .arg(url)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .is_ok()
            {
                return Ok(());
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = url;
    }
    Ok(())
}


/// Un ánimo ("Éxitos", "Relajación"...) con sus params para pedir sus playlists.
#[derive(Debug, Clone)]
pub struct MoodShelf {
    pub title: String,
    pub params: String,
}

/// Una playlist de ánimo, lista para reproducir.
#[derive(Debug, Clone)]
pub struct MoodPlaylistInfo {
    pub playlist_id: String,
    pub title: String,
    pub author: String,
    pub thumbnail_url: String,
}


/// Una línea de letra con su timestamp (segundos desde el inicio).
#[derive(Debug, Clone)]
pub struct LyricLine {
    pub time: f64,
    pub text: String,
}

/// Letras sincronizadas.
#[derive(Debug, Clone, Default)]
pub struct TimedLyrics {
    pub lines: Vec<LyricLine>,
    pub source: String,
}

/// Convierte la respuesta cruda de GetLyricsQuery a TimedLyrics (público).
pub fn timed_lyrics_from(l: ytmapi_rs::parse::Lyrics) -> Option<TimedLyrics> {
    parse_timed_lyrics(&l.lyrics)
}

/// Parsea las letras de YTM: traen timestamps embebidos en forma
/// "[mm:ss.xx]" o "\r\n" separando líneas con marcas de tiempo.
fn parse_timed_lyrics(raw: &str) -> Option<TimedLyrics> {
    let mut lines = Vec::new();
    let re = time_regex();
    let mut source = String::new();
    // YTM/Musixmatch: "Source: Musixmatch" al final o metadatos similares.
    for l in raw.lines() {
        let l = l.trim();
        if l.is_empty() {
            continue;
        }
        if let Some(rest) = l.strip_prefix("Source:") {
            source = rest.trim().to_string();
            continue;
        }
        // Buscar UN timestamp [mm:ss.xx] o [mm:ss] al inicio.
        if let Some(m) = re.captures(l) {
            let mm: f64 = m.get(1).and_then(|x| x.as_str().parse().ok()).unwrap_or(0.0);
            let ss: f64 = m.get(2).and_then(|x| x.as_str().parse().ok()).unwrap_or(0.0);
            let frac: f64 = m
                .get(3)
                .and_then(|x| x.as_str().parse::<f64>().ok())
                .unwrap_or(0.0);
            let text = l[m.get(0).map(|x| x.end()).unwrap_or(0)..].trim().to_string();
            lines.push(LyricLine {
                time: mm * 60.0 + ss + frac,
                text,
            });
        }
    }
    // Sin timestamps: letras planas, una línea por línea con tiempo 0.
    if lines.is_empty() {
        for l in raw.split("\r\n").chain(raw.split('\n')) {
            let l = l.trim();
            if !l.is_empty() && !l.starts_with("Source:") {
                lines.push(LyricLine { time: -1.0, text: l.to_string() });
            }
            if lines.len() > 200 {
                break;
            }
        }
    }
    if lines.is_empty() {
        return None;
    }
    lines.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal));
    Some(TimedLyrics { lines, source })
}

static TIME_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
fn time_regex() -> &'static regex::Regex {
    TIME_RE.get_or_init(|| {
        regex::Regex::new(r"^\[(\d+):(\d+)(?:\.(\d+))?\]").unwrap()
    })
}


/// Ruta del directorio de configuración de ymmt.
pub fn config_dir() -> std::path::PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|_| {
            std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
        })
        .map(|p| p.join("ymmt"))
        .unwrap_or_default()
}

/// Una sección del feed de inicio ("Selecciones rápidas", "Clásicos...").
#[derive(Debug, Clone)]
pub struct FeedSection {
    pub title: String,
    pub cards: Vec<FeedCard>,
}

/// Una tarjeta del feed: canción (con radio propia) o playlist.
#[derive(Debug, Clone)]
pub struct FeedCard {
    pub title: String,
    pub subtitle: String,
    pub video_id: String,
    pub playlist_id: String,
    pub thumb_url: String,
    pub kind: CardKind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CardKind {
    Song,
    Playlist,
}


/// Sube la miniatura de YTM a la resolución alta (544px) para portadas
/// nítidas; las URLs de googleusercontent aceptan cambiar el sufijo.
pub fn hi_res_thumb(url: &str) -> String {
    if url.contains("googleusercontent") {
        if let Some(i) = url.rfind("=w") {
            return format!("{}=w544-h544-l90-rj", &url[..i]);
        }
    }
    url.to_string()
}
