//! Configuración: keybinds y preferencias en `~/.config/ymmt/config.toml`.

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Keybinds: acción -> combinación ("s", "ctrl+s", "C-s", "goto 5").
    pub keybinds: BTreeMap<String, String>,
    pub settings: Settings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Volumen inicial 0-100.
    pub volume: u8,
    /// Reproducción aleatoria al añadir a la cola.
    pub shuffle: bool,
    /// Reproducir automáticamente la cola "up next" (radio) al terminar.
    pub autoplay: bool,
    /// Mostrar la letra de la canción (si existe).
    pub show_lyrics: bool,
    /// Cliente OAuth propio (opcional).
    pub oauth_client_id: Option<String>,
    pub oauth_client_secret: Option<String>,
    /// Filtro de calidades aceptables, de mejor a peor (bitrate).
    pub audio_quality: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 70,
            shuffle: false,
            autoplay: true,
            show_lyrics: false,
            oauth_client_id: None,
            oauth_client_secret: None,
            audio_quality: "high".into(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        let mut keybinds = BTreeMap::new();
        // Navegación global
        keybinds.insert("quit".into(), "q".into());
        keybinds.insert("home".into(), "1".into());
        keybinds.insert("search".into(), "2".into());
        keybinds.insert("now_playing".into(), "3".into());
        keybinds.insert("queue".into(), "4".into());
        keybinds.insert("playlists".into(), "5".into());
        keybinds.insert("config".into(), "6".into());
        keybinds.insert("help".into(), "?".into());
        // Now playing
        keybinds.insert("toggle_play".into(), "space".into());
        keybinds.insert("next".into(), "n".into());
        keybinds.insert("previous".into(), "p".into());
        keybinds.insert("seek_forward".into(), "right".into());
        keybinds.insert("seek_back".into(), "left".into());
        keybinds.insert("volume_up".into(), "+".into());
        keybinds.insert("volume_down".into(), "-".into());
        keybinds.insert("shuffle".into(), "x".into());
        keybinds.insert("settings".into(), "s".into());
        keybinds.insert("visualizer".into(), "c".into());
        keybinds.insert("repeat".into(), "r".into());
        // Búsqueda / listas
        keybinds.insert("up".into(), "up".into());
        keybinds.insert("down".into(), "down".into());
        keybinds.insert("left".into(), "left".into());
        keybinds.insert("right".into(), "right".into());
        keybinds.insert("page_up".into(), "pgup".into());
        keybinds.insert("page_down".into(), "pgdn".into());
        keybinds.insert("confirm".into(), "enter".into());
        keybinds.insert("escape".into(), "esc".into());
        keybinds.insert("play_now".into(), "enter".into());
        keybinds.insert("play_next".into(), "shift+n".into());
        keybinds.insert("add_queue".into(), "a".into());
        keybinds.insert("add_radio".into(), "R".into());
        keybinds.insert("clear_filters".into(), "ctrl+l".into());
        Self { keybinds, settings: Settings::default() }
    }
}

impl Config {
    pub fn load(config_dir: &Path) -> Result<Self> {
        let path = config_dir.join("config.toml");
        if !path.exists() {
            let cfg = Config::default();
            let _ = cfg.save(config_dir);
            return Ok(cfg);
        }
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("no se pudo leer {}", path.display()))?;
        let cfg: Config = toml::from_str(&data)
            .with_context(|| format!("config.toml inválido en {}", path.display()))?;
        Ok(cfg)
    }
    pub fn save(&self, config_dir: &Path) -> Result<()> {
        let path = config_dir.join("config.toml");
        let data = toml::to_string_pretty(self)?;
        std::fs::write(&path, data)?;
        Ok(())
    }
}
