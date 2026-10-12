# ymmt — YouTube Music Minimal TUI

Cliente minimalista de YouTube Music para la terminal, en Rust (ratatui).
Sin anuncios, full keybinds.

## Qué hace

- **Para ti**: el feed REAL de tu YouTube Music (las mismas secciones que ves
  en music.youtube.com) en estantes con portadas.
- **Reproducción por streaming** con cola "up next" de YouTube, sin descargas.
- **Letras sincronizadas** (karaoke) con `l`.
- **Visualizador de espectro** con `c` (cambia de diseño con `Shift+C`).
- **Cola, playlists de tu biblioteca, búsqueda** — todo con teclado o mouse.
- Login por **cookie** (copia la cabecera Cookie de music.youtube.com o exporta
  un cookies.txt con la extensión "Get cookies.txt").

## Uso

```
ymmt --login          # login interactivo (pega tu cookie o cookies.txt)
ymmt --login cookies.txt
ymmt                  # abre el TUI
```

## Teclas principales

| Tecla | Acción |
|---|---|
| 1 | inicio (para ti) |
| 2 | búsqueda |
| 3 | reproducción |
| 4 | cola |
| 5 | tus playlists |
| s | ajustes (sesión, estado de la cookie, verificación) |
| espacio | play/pausa |
| n / p | siguiente / anterior |
| ← → | seek ±10s (±1 tarjeta en el estante del inicio) |
| ↑ ↓ | navegar / cambiar de sección |
| enter | reproducir |
| l | letras (karaoke) |
| c / Shift+C | visualizador on/off / cambiar diseño |
| x | shuffle |
| r | repeat (off → all → one) |
| q | salir |

Todas configurables en `~/.config/ymmt/config.toml`.

## Build

```
cargo build --release
```

Requiere Rust 1.80+ y una terminal con audio (el playback usa rodio/CPAL).

## Notas

- Las sesiones por cookie viven en `~/.config/ymmt/cookie.txt`, nunca se suben
  a ningún lado. `cookies.txt` está en `.gitignore`.
- Tu historial puede estar vacío si no has usado YouTube Music en la web.
