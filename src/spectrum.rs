//! Analizador de espectro para el visualizador (estilo cliamp/cava).
//!
//! El player envía muestras al analizador; un hilo hace FFT por ventanas y
//! publica N bandas log-espaciadas (graves→agudos). La UI las dibuja con
//! bloques Unicode fraccionarios.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rtrb::{Consumer, Producer, RingBuffer};
use rustfft::FftPlanner;
use rustfft::num_complex::Complex32;

const N_BANDS: usize = 32;
const FFT_SIZE: usize = 2048;
const RING_CAPACITY: usize = FFT_SIZE * 16;
const MIN_FREQ: f32 = 60.0;
const MAX_FREQ: f32 = 12_000.0;
const GAIN: f32 = 12.0;
const KNEE: f32 = 0.6;
const ATTACK: f32 = 0.8;
const DECAY: f32 = 0.18;

/// Espectro compartido: la UI lee, el player escribe muestras.
pub struct Spectrum {
    bands: Arc<Vec<AtomicU32>>,
    running: Arc<AtomicBool>,
    producer: std::sync::Mutex<Producer<f32>>,
    sample_rate: Arc<AtomicU32>,
}

impl Spectrum {
    pub fn new() -> Arc<Self> {
        let (producer, consumer) = RingBuffer::<f32>::new(RING_CAPACITY);
        let bands: Arc<Vec<AtomicU32>> =
            Arc::new((0..N_BANDS).map(|_| AtomicU32::new(0)).collect());
        let running = Arc::new(AtomicBool::new(true));
        let sample_rate = Arc::new(AtomicU32::new(44_100));

        // Hilo analizador.
        {
            let bands = bands.clone();
            let running = running.clone();
            let rate = sample_rate.clone();
            std::thread::spawn(move || {
                analyzer_thread(consumer, bands, running, rate);
            });
        }

        Arc::new(Self {
            bands,
            running,
            producer: std::sync::Mutex::new(producer),
            sample_rate,
        })
    }

    /// Niveles actuales de las bandas (0.0–~1.0).
    pub fn levels(&self) -> Vec<f32> {
        self.bands
            .iter()
            .map(|b| f32::from_bits(b.load(Ordering::Relaxed)))
            .collect()
    }

    /// El player reporta el sample rate de la pista actual.
    pub fn set_sample_rate(&self, rate: u32) {
        self.sample_rate.store(rate, Ordering::Relaxed);
    }

    /// Enviar muestras (f32, mezcla mono) al analizador.
    pub fn push(&self, samples: &[f32]) {
        if let Ok(mut p) = self.producer.lock() {
            for s in samples {
                let _ = p.push(*s);
            }
        }
    }
}

impl Drop for Spectrum {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

/// Bucle del hilo analizador: ventanas de FFT_SIZE, bandas log, suavizado.
fn analyzer_thread(
    mut consumer: Consumer<f32>,
    bands: Arc<Vec<AtomicU32>>,
    running: Arc<AtomicBool>,
    rate: Arc<AtomicU32>,
) {
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    let hann: Vec<f32> = (0..FFT_SIZE)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / FFT_SIZE as f32).cos())
        .collect();
    let mut buf: Vec<f32> = Vec::with_capacity(FFT_SIZE);
    let mut prev: Vec<f32> = vec![0.0; N_BANDS];

    while running.load(Ordering::Relaxed) {
        // Drenar el ring al buffer local.
        while let Ok(s) = consumer.pop() {
            buf.push(s);
            if buf.len() >= FFT_SIZE * 4 {
                break; // no acumular de más
            }
        }
        if buf.len() < FFT_SIZE {
            std::thread::sleep(Duration::from_millis(16));
            continue;
        }
        // Ventana nueva: FFT sobre las primeras FFT_SIZE muestras.
        let sr = rate.load(Ordering::Relaxed) as f32;
        let edges = band_edges(sr);
        let mut input: Vec<Complex32> = buf[..FFT_SIZE]
            .iter()
            .zip(&hann)
            .map(|(s, h)| Complex32::new(s * h, 0.0))
            .collect();
        fft.process(&mut input);
        // Consumir la ventana usada (con solape del 50% para fluidez).
        buf.drain(..FFT_SIZE / 2);

        for b in 0..N_BANDS {
            let (lo, hi) = edges[b];
            let mut mag = 0.0f32;
            for bin_i in lo..hi.min(FFT_SIZE / 2) {
                mag = mag.max(input[bin_i].norm());
            }
            mag /= FFT_SIZE as f32;
            let level = (mag * GAIN).clamp(0.0, 1.4);
            let prev_v = prev[b];
            let smooth = if level > prev_v {
                prev_v + (level - prev_v) * ATTACK
            } else {
                prev_v + (level - prev_v) * DECAY
            };
            prev[b] = smooth;
            let shown = if smooth > KNEE {
                KNEE + (smooth - KNEE) * 0.5
            } else {
                smooth
            };
            bands[b].store(shown.to_bits(), Ordering::Relaxed);
        }
    }
}

/// Bordes de banda: N_BANDS tramos log-espaciados entre MIN_FREQ y MAX_FREQ,
/// convertidos a bins de FFT.
fn band_edges(sample_rate: f32) -> Vec<(usize, usize)> {
    let n_bins = FFT_SIZE / 2;
    let bin_hz = sample_rate / FFT_SIZE as f32;
    let log_lo = MIN_FREQ.log2();
    let log_hi = MAX_FREQ.log2();
    (0..N_BANDS)
        .map(|b| {
            let f0 = 2f32.powf(log_lo + (log_hi - log_lo) * b as f32 / N_BANDS as f32);
            let f1 = 2f32.powf(log_lo + (log_hi - log_lo) * (b + 1) as f32 / N_BANDS as f32);
            let lo = ((f0 / bin_hz) as usize).min(n_bins.saturating_sub(1));
            let hi = (((f1 / bin_hz) as usize).max(lo + 1)).min(n_bins);
            (lo, hi)
        })
        .collect()
}
