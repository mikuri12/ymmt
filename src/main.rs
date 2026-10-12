//! ymmt — YouTube Music Minimal TUI.
//!
//! Cliente minimalista de YouTube Music para la terminal: sin anuncios,
//! full keybinds configurables, portadas en terminal.
//!
//! Vistas: NowPlaying (portada + pista de tiempo), Search (catálogo), Queue,
//! Playlists, Config. Navegación 100% por keybinds configurables.

mod app;
mod auth;
mod config;
mod player;
mod spectrum;
mod ui;
mod views;

use clap::Parser;
use std::io;

/// ymmt — YouTube Music Minimal TUI: sin anuncios, full keybinds.
#[derive(Parser, Debug)]
#[command(name = "ymmt", version, about)]
struct Args {
    /// Iniciar el asistente de login (cookie del navegador). Opcionalmente
    /// la ruta de un cookies.txt exportado.
    #[arg(long, value_name = "ARCHIVO", num_args = 0..=1, default_missing_value = "")]
    login: Option<String>,
    /// Desconectar la cuenta (borra el token guardado).
    #[arg(long)]
    logout: bool,
    /// Autenticación por cookie SAPISID en vez de OAuth.
    #[arg(long)]
    cookie: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Directorios de configuración.
    let config_dir = dirs::config_dir()
        .expect("no se pudo resolver el directorio de configuración")
        .join("ymmt");
    std::fs::create_dir_all(&config_dir)?;

    if args.logout {
        auth::logout(&config_dir)?;
        println!("Sesión cerrada. Token eliminado.");
        return Ok(());
    }

    // Login fuera de la TUI: interactivo, o leyendo un cookies.txt dado.
    if let Some(arg) = args.login {
        let file = if arg.is_empty() {
            None
        } else {
            Some(std::path::PathBuf::from(arg))
        };
        return tokio::runtime::Runtime::new()?.block_on(auth::login_cli(&config_dir, file.as_deref()));
    }

    let cookie_arg = args.cookie.clone();
    let rt = tokio::runtime::Runtime::new()?;
    let result = rt.block_on(async move {
        let mut app = app::App::new(&config_dir, cookie_arg).await?;
        app.run().await
    });
    // El runtime de audio de rodio vive fuera del block_on; apagarlo aquí.
    result
}

/// Utilidad: limpiar la terminal al salir (por si un panic rompió el modo raw).
pub fn cleanup_terminal() {
    let _ = io::Write::flush(&mut io::stdout());
    let _ = crossterm::execute!(
        io::stdout(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    );
    let _ = crossterm::terminal::disable_raw_mode();
}
