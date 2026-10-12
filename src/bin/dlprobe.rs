//! Prueba del downloader de ymmt (mint propio + trozos de 360KB).
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let vid = std::env::args().nth(1).unwrap_or_else(|| "wU26xVT_vBU".into());
    let cookie = std::fs::read_to_string(
        std::env::var("HOME").map(|h| format!("{h}/.config/ymmt/cookie.txt")).unwrap(),
    ).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    println!("cookie: {}", if cookie.is_some() { "SÍ" } else { "NO" });
    let http = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/87.0.4280.101 Safari/537.36")
        .build()?;
    let t0 = Instant::now();
    let (url, clen) = {
        let body = serde_json::json!({
            "context": {"client": {"clientName": "ANDROID", "clientVersion": "20.10.38",
                "androidSdkVersion": 30, "osName": "Android", "osVersion": "11",
                "userAgent": "com.google.android.youtube/20.10.38 (Linux; U; Android 11) gzip"}},
            "videoId": vid, "contentCheckOk": true, "racyCheckOk": true});
        let mut req = http.post("https://www.youtube.com/youtubei/v1/player")
            .header("Content-Type", "application/json")
            .header("User-Agent", "com.google.android.youtube/20.10.38 (Linux; U; Android 11) gzip")
            .body(body.to_string());
        if let Some(c) = &cookie { req = req.header("Cookie", c); }
        let resp = req.timeout(Duration::from_secs(15)).send().await?.error_for_status()?;
        let data: serde_json::Value = serde_json::from_slice(&resp.bytes().await?)?;
        let mut best = None;
        for f in data["streamingData"]["adaptiveFormats"].as_array().unwrap_or(&vec![]) {
            let mt = f["mimeType"].as_str().unwrap_or("");
            let url = f["url"].as_str().unwrap_or("");
            if mt.contains("audio/mp4") && !url.is_empty() {
                let br = f["bitrate"].as_u64().unwrap_or(0);
                let cl = f["contentLength"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0u64);
                if best.as_ref().map(|b: &(String,u64,u64)| br > b.1).unwrap_or(true) {
                    best = Some((url.to_string(), br, cl));
                }
            }
        }
        let (u, _b, cl) = best.ok_or_else(|| anyhow::anyhow!("sin formato"))?;
        (u, cl)
    };
    println!("mint OK: clen={clen} en {:.1}s", t0.elapsed().as_secs_f32());
    // probar 3 rangos con la misma URL
    for &(off, len) in &[(0, 360*1024), (360*1024, 360*1024), (720*1024, 100*1024)] {
        let resp = http.get(&url)
            .header("Range", format!("bytes={}-{}", off, off+len-1))
            .timeout(Duration::from_secs(20))
            .send().await?;
        println!("  rango @{off} ({}B): HTTP {} bytes={}", len, resp.status(), resp.bytes().await?.len());
    }
    Ok(())
}
