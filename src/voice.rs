//! Sesli mesaj: mikrofondan kayıt ve sohbette oynatma.
//!
//! Kayıt tek kanala (mono) indirilip 16 kHz, 16 bit WAV olarak kaydedilir
//! (dakikası yaklaşık 2 MB) ve normal bir dosya gibi gönderilir.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Kaydedilen sesin örnekleme hızı
const RATE: u32 = 16_000;
/// En uzun kayıt
pub const MAX_LEN: Duration = Duration::from_secs(5 * 60);

/// Bu dosya sohbette oynatılabilir mi?
pub fn is_audio(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, e)| e.eq_ignore_ascii_case("wav"))
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Süren bir mikrofon kaydı
pub struct Recorder {
    _stream: cpal::Stream,
    samples: Arc<Mutex<Vec<f32>>>,
    rate: u32,
    started: Instant,
}

impl Recorder {
    pub fn start() -> Result<Recorder, String> {
        let host = cpal::default_host();
        let device = host.default_input_device().ok_or("Mikrofon bulunamadı")?;
        let supported = device
            .default_input_config()
            .map_err(|e| format!("Mikrofon açılamadı: {}", err(e)))?;
        let config = supported.config();
        let channels = config.channels.max(1) as usize;
        let samples = Arc::new(Mutex::new(Vec::<f32>::with_capacity(48_000 * 30)));

        fn build<T>(
            device: &cpal::Device,
            config: cpal::StreamConfig,
            channels: usize,
            out: Arc<Mutex<Vec<f32>>>,
        ) -> Result<cpal::Stream, cpal::Error>
        where
            T: SizedSample,
            f32: FromSample<T>,
        {
            // En uzun kayıt süresinden fazlası tutulmaz (bellek dolmasın)
            let cap = config.sample_rate as usize * (MAX_LEN.as_secs() as usize + 5);
            device.build_input_stream(
                config,
                move |data: &[T], _: &_| {
                    let mut out = out.lock().unwrap();
                    if out.len() >= cap {
                        return;
                    }
                    // Kanalların ortalaması: tek kanal
                    for frame in data.chunks(channels) {
                        let sum: f32 = frame.iter().map(|s| f32::from_sample(*s)).sum();
                        out.push(sum / frame.len() as f32);
                    }
                },
                |_| {},
                None,
            )
        }

        let s = samples.clone();
        let stream = match supported.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, config, channels, s),
            SampleFormat::I16 => build::<i16>(&device, config, channels, s),
            SampleFormat::U16 => build::<u16>(&device, config, channels, s),
            SampleFormat::I32 => build::<i32>(&device, config, channels, s),
            SampleFormat::I8 => build::<i8>(&device, config, channels, s),
            SampleFormat::U8 => build::<u8>(&device, config, channels, s),
            other => return Err(format!("Mikrofonun ses biçimi desteklenmiyor ({other:?})")),
        }
        .map_err(|e| format!("Mikrofon açılamadı: {}", err(e)))?;
        stream
            .play()
            .map_err(|e| format!("Kayıt başlatılamadı: {}", err(e)))?;
        Ok(Recorder {
            _stream: stream,
            samples,
            rate: config.sample_rate,
            started: Instant::now(),
        })
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Son anlardaki ses düzeyi (0..1), göstergede kullanmak için
    pub fn level(&self) -> f32 {
        let s = self.samples.lock().unwrap();
        let n = (self.rate as usize / 10).min(s.len());
        if n == 0 {
            return 0.0;
        }
        let tail = &s[s.len() - n..];
        let rms = (tail.iter().map(|x| x * x).sum::<f32>() / n as f32).sqrt();
        (rms * 4.0).min(1.0)
    }

    /// Kaydı durdurur: (örnekler, örnekleme hızı). Kaydetmek için `save`.
    pub fn stop(self) -> (Vec<f32>, u32) {
        let samples = std::mem::take(&mut *self.samples.lock().unwrap());
        (samples, self.rate)
    }
}

/// Kaydı `dir` içine WAV olarak yazar (uzun sürebilir: arka planda çağırın).
pub fn save(samples: &[f32], rate: u32, dir: &Path) -> io::Result<PathBuf> {
    if samples.len() < (rate as usize / 2) {
        return Err(io::Error::other("Kayıt çok kısa"));
    }
    let max = rate as usize * MAX_LEN.as_secs() as usize;
    let mono = resample(&samples[..samples.len().min(max)], rate, RATE);
    std::fs::create_dir_all(dir)?;
    let stamp = chrono::Local::now().format("%Y-%m-%d %H.%M.%S");
    let path = dir.join(format!("Sesli mesaj {stamp}.wav"));
    write_wav(&path, &mono, RATE)?;
    Ok(path)
}

/// Doğrusal aradeğerlemeyle örnekleme hızını değiştirir.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let n = ((input.len() as f64) / ratio).floor() as usize;
    (0..n)
        .map(|i| {
            let pos = i as f64 * ratio;
            let j = pos.floor() as usize;
            let frac = (pos - j as f64) as f32;
            let a = input[j.min(input.len() - 1)];
            let b = input[(j + 1).min(input.len() - 1)];
            a + (b - a) * frac
        })
        .collect()
}

pub fn write_wav(path: &Path, samples: &[f32], rate: u32) -> io::Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(io::Error::other)?;
    for s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .map_err(io::Error::other)?;
    }
    w.finalize().map_err(io::Error::other)
}

/// WAV dosyasını tek kanallı örneklere çevirir: (örnekler, örnekleme hızı)
pub fn read_wav_mono(path: &Path) -> io::Result<(Vec<f32>, u32)> {
    let mut r = hound::WavReader::open(path).map_err(io::Error::other)?;
    let spec = r.spec();
    let ch = spec.channels.max(1) as usize;
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().filter_map(Result::ok).collect(),
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f32;
            r.samples::<i32>()
                .filter_map(Result::ok)
                .map(|s| s as f32 / max)
                .collect()
        }
    };
    let mono = raw
        .chunks(ch)
        .map(|f| f.iter().sum::<f32>() / f.len() as f32)
        .collect();
    Ok((mono, spec.sample_rate))
}

/// WAV dosyasının süresi (yalnızca başlığa bakar)
pub fn wav_duration(path: &Path) -> Option<Duration> {
    let r = hound::WavReader::open(path).ok()?;
    let spec = r.spec();
    Some(Duration::from_secs_f64(
        r.duration() as f64 / spec.sample_rate.max(1) as f64,
    ))
}

/// Çalınan ses
pub struct Player {
    _stream: cpal::Stream,
    pos: Arc<AtomicUsize>,
    len: usize,
    pub msg_id: String,
}

impl Player {
    pub fn play(path: &Path, msg_id: &str) -> Result<Player, String> {
        let (mono, rate) =
            read_wav_mono(path).map_err(|e| format!("Ses dosyası okunamadı: {e}"))?;
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("Hoparlör bulunamadı")?;
        let supported = device
            .default_output_config()
            .map_err(|e| format!("Hoparlör açılamadı: {}", err(e)))?;
        let config = supported.config();
        let channels = config.channels.max(1) as usize;
        let data = Arc::new(resample(&mono, rate, config.sample_rate));
        let len = data.len();
        let pos = Arc::new(AtomicUsize::new(0));

        fn build<T>(
            device: &cpal::Device,
            config: cpal::StreamConfig,
            channels: usize,
            data: Arc<Vec<f32>>,
            pos: Arc<AtomicUsize>,
        ) -> Result<cpal::Stream, cpal::Error>
        where
            T: SizedSample + FromSample<f32>,
        {
            device.build_output_stream(
                config,
                move |out: &mut [T], _: &_| {
                    let mut i = pos.load(Ordering::Relaxed);
                    for frame in out.chunks_mut(channels) {
                        let v = data.get(i).copied().unwrap_or(0.0);
                        for s in frame {
                            *s = T::from_sample(v);
                        }
                        i += 1;
                    }
                    pos.store(i.min(data.len()), Ordering::Relaxed);
                },
                |_| {},
                None,
            )
        }

        let (d, p) = (data.clone(), pos.clone());
        let stream = match supported.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, config, channels, d, p),
            SampleFormat::I16 => build::<i16>(&device, config, channels, d, p),
            SampleFormat::U16 => build::<u16>(&device, config, channels, d, p),
            SampleFormat::I32 => build::<i32>(&device, config, channels, d, p),
            other => return Err(format!("Hoparlörün ses biçimi desteklenmiyor ({other:?})")),
        }
        .map_err(|e| format!("Hoparlör açılamadı: {}", err(e)))?;
        stream
            .play()
            .map_err(|e| format!("Çalınamadı: {}", err(e)))?;
        Ok(Player {
            _stream: stream,
            pos,
            len,
            msg_id: msg_id.to_string(),
        })
    }

    /// Çalınan kısım (0..1)
    pub fn progress(&self) -> f32 {
        if self.len == 0 {
            return 1.0;
        }
        self.pos.load(Ordering::Relaxed) as f32 / self.len as f32
    }

    pub fn finished(&self) -> bool {
        self.pos.load(Ordering::Relaxed) >= self.len
    }
}

/// "1:05" gibi
pub fn format_len(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_roundtrip_and_resample() {
        let dir = std::env::temp_dir().join(format!("lanchat-ses-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // 48 kHz'de 0,5 sn 440 Hz → 16 kHz
        let src: Vec<f32> = (0..24_000)
            .map(|i| (i as f32 / 48_000.0 * 440.0 * std::f32::consts::TAU).sin() * 0.5)
            .collect();
        let out = resample(&src, 48_000, RATE);
        assert_eq!(out.len(), 8_000);
        let path = dir.join("Sesli mesaj.wav");
        write_wav(&path, &out, RATE).unwrap();
        assert!(is_audio("Sesli mesaj.wav") && is_audio("A.WAV") && !is_audio("wav"));
        let d = wav_duration(&path).unwrap();
        assert!((d.as_secs_f32() - 0.5).abs() < 0.01, "{d:?}");
        let (back, rate) = read_wav_mono(&path).unwrap();
        assert_eq!((back.len(), rate), (8_000, RATE));
        let peak = back.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!((peak - 0.5).abs() < 0.02, "{peak}");
        assert_eq!(format_len(Duration::from_secs(65)), "1:05");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
