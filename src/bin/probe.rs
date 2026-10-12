//! Prueba headless del pipeline completo: buscar → stream → decodificar → sonar.
//! Se lanza con: cargo run --release --bin probe

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rodio::source::Source as _;
use ytmapi_rs::common::YoutubeID as _;
use rusty_ytdl::{
    DownloadOptions, RequestOptions, Video, VideoOptions, VideoQuality, VideoSearchOptions,
};
use std::io::{Read, Seek, SeekFrom};
use std::sync::Mutex;

// ---- Spool mínimo (réplica del de player.rs) ----
pub struct Spool {
    inner: Mutex<SpoolInner>,
    done: AtomicBool,
}
pub struct SpoolInner {
    buf: Vec<u8>,
    read_pos: usize,
    eof: bool,
    /// Tamaño total del archivo (content_length), conocido desde el inicio.
    total: Option<u64>,
}
impl Spool {
    fn new() -> Arc<Self> {
        Arc::new(Spool {
            inner: Mutex::new(SpoolInner { buf: Vec::new(), read_pos: 0, eof: false, total: None }),
            done: AtomicBool::new(false),
        })
    }
    fn set_total(&self, total: u64) {
        self.inner.lock().unwrap().total = Some(total);
    }
    fn total(&self) -> Option<u64> {
        self.inner.lock().unwrap().total
    }
    fn write_chunk(&self, c: &[u8]) {
        self.inner.lock().unwrap().buf.extend_from_slice(c);
    }
    fn finish(&self) {
        self.done.store(true, Ordering::SeqCst);
        self.inner.lock().unwrap().eof = true;
    }
    fn fail(&self) {
        self.finish();
    }
    fn downloaded(&self) -> u64 {
        self.inner.lock().unwrap().buf.len() as u64
    }
    fn is_done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }
}
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
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "neg"));
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

// ---- Downloader por rangos con renovación de URL ----

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/87.0.4280.101 Safari/537.36";

/// Obtiene una URL fresca del mejor formato audio/mp4 y su content_length.
async fn fresh_stream_url(video_id: &str) -> anyhow::Result<(String, u64)> {
    let filter = VideoSearchOptions::Custom(Arc::new(|f: &rusty_ytdl::VideoFormat| {
        f.has_audio && !f.has_video && f.mime_type.container != "webm"
    }));
    let options = VideoOptions {
        quality: VideoQuality::HighestAudio,
        filter,
        download_options: DownloadOptions { dl_chunk_size: Some(512 * 1024) },
        request_options: RequestOptions::default(),
    };
    let video = Video::new_with_options(video_id.to_string(), &options)
        .map_err(|e| anyhow::anyhow!("video: {e}"))?;
    let info = video.get_info().await.map_err(|e| anyhow::anyhow!("info: {e}"))?;
    let fmt = info
        .formats
        .iter()
        .filter(|f| f.has_audio && !f.has_video && f.mime_type.container != "webm")
        .max_by_key(|f| f.bitrate)
        .ok_or_else(|| anyhow::anyhow!("sin formatos de audio"))?;
    let clen = fmt
        .content_length
        .as_deref()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    Ok((fmt.url.clone(), clen))
}

/// Descarga por rangos de 1 MiB; renueva la URL en cada 403/429.
async fn download_ranged(
    http: &reqwest::Client,
    video_id: &str,
    spool: &Arc<Spool>,
    cancel: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    const CHUNK: u64 = 1024 * 1024;
    let (mut url, clen) = fresh_stream_url(video_id).await?;
    if clen == 0 {
        anyhow::bail!("el stream no reporta content_length");
    }
    spool.set_total(clen);

    let mut offset: u64 = 0;
    let mut refusals = 0u32;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Ok(());
        }
        let end = (offset + CHUNK - 1).min(clen - 1);
        let resp = http
            .get(&url)
            .header(reqwest::header::USER_AGENT, UA)
            .header(reqwest::header::RANGE, format!("bytes={offset}-{end}"))
            .timeout(Duration::from_secs(20))
            .send()
            .await?;
        let status = resp.status();
        if status.as_u16() == 403 || status.as_u16() == 429 {
            refusals += 1;
            if refusals > 8 {
                anyhow::bail!("el host rechazó {refusals} URLs seguidas en el byte {offset}");
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
            let (new_url, new_clen) = fresh_stream_url(video_id).await?;
            if new_clen != clen {
                anyhow::bail!("el contenido cambió de tamaño al renovar");
            }
            url = new_url;
            continue;
        }
        if !status.is_success() {
            anyhow::bail!("HTTP {status} en el rango {offset}-{end}");
        }
        let bytes = resp.bytes().await?;
        if bytes.is_empty() {
            anyhow::bail!("rango {offset}-{end} devuelto vacío");
        }
        spool.write_chunk(&bytes);
        offset += bytes.len() as u64;
        refusals = 0;
        if offset >= clen {
            break;
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Modo dumpurl: obtener URL del stream y salir (para probar con curl).
    if std::env::args().nth(1).as_deref() == Some("dumpurl") {
        return dump_url().await;
    }
    // 1. Búsqueda sin login.
    let api = ytmapi_rs::YtMusic::new_unauthenticated().await?;
    println!("→ buscando…");
    let results = api
        .query(ytmapi_rs::query::SearchQuery::new_filtered(
            "daft punk one more time",
            ytmapi_rs::query::search::SongsFilter,
        ))
        .await?;
    let first = &results[0];
    println!(
        "  ✓ {} — {} [{}]",
        first.artist,
        first.title,
        first.video_id.get_raw()
    );

    // 2. Descarga por rangos con renovación de URL (comportamiento verificado:
    //    sin PoToken el host solo sirve ~1 MiB por URL minteada).
    let video_id = first.video_id.get_raw().to_string();
    let spool = Spool::new();
    let cancel = Arc::new(AtomicBool::new(false));
    let http = reqwest::Client::builder().user_agent(UA).build()?;
    let t0 = Instant::now();
    let spool_w = spool.clone();
    let cancel_w = cancel.clone();
    let http_w = http.clone();
    let vid = video_id.clone();
    let dl = tokio::spawn(async move {
        if let Err(e) = download_ranged(&http_w, &vid, &spool_w, &cancel_w).await {
            spool_w.fail();
            eprintln!("✗ descarga: {e:#}");
            Err::<(), anyhow::Error>(e)
        } else {
            spool_w.finish();
            Ok(())
        }
    });

    // 3. Esperar cabeza (y conocer el tamaño total) y decodificar.
    while (spool.downloaded() < 96 * 1024 || spool.total().is_none()) && !spool.is_done() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    println!(
        "  ✓ cabeza: {} KB en {:.2}s",
        spool.downloaded() / 1024,
        t0.elapsed().as_secs_f64()
    );

    let t1 = Instant::now();
    let spool_r = spool.clone();
    let total = spool.total().unwrap_or(0);
    let decoder = tokio::task::spawn_blocking(move || {
        rodio::Decoder::builder()
            .with_data(SpoolReader(spool_r))
            .with_byte_len(total)
            .with_seekable(true)
            .build()
    })
    .await??;
    println!(
        "  ✓ decoder en {:?} — duración: {:?}",
        t1.elapsed(),
        decoder.total_duration()
    );

    // 4. Reproducir 6 segundos.
    let sink = rodio::DeviceSinkBuilder::open_default_sink()?;
    let player = rodio::Player::connect_new(&sink.mixer());
    player.append(decoder);
    println!("  ▶ sonando 6s…");
    for i in 0..6 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        println!("    {}s pos={:?}", i + 1, player.get_pos());
    }
    player.pause();
    cancel.store(true, Ordering::SeqCst);
    println!("  ✓ PIPELINE OK (descargado: {} KB)", spool.downloaded() / 1024);
    Ok(())
}

/// Obtiene la URL del formato audio/mp4 y la guarda en /tmp/ytm-url.txt.
async fn dump_url() -> anyhow::Result<()> {
    let video_id = std::env::args().nth(2).expect("uso: probe dumpurl <video_id>");
    let (url, clen) = fresh_stream_url(&video_id).await?;
    println!("clen={clen}");
    std::fs::write("/tmp/ytm-url.txt", format!("{url}\n{clen}"))?;
    println!("→ URL guardada en /tmp/ytm-url.txt");
    Ok(())
}
