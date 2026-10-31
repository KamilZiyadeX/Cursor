//! LanChat'in bildirim sesi: ağır, metalik vuruşlarla "dıdıt dıt" ritmi.
//!
//! Ses dosyası taşımamak için ses program içinde sentezlenir (16 bit, mono WAV).
//! Her vuruş üç katmandan oluşur:
//! - gövde: perdesi hızla düşen alçak bir sinüs (göğüse vuran "güm"),
//! - metal: uyumsuz kısmi tonların toplamı (örs/çelik tınısı),
//! - tık: vuruşun başındaki çok kısa gürültü patlaması.
//!
//! Son vuruş daha uzun çınlar.

// Ses yalnızca Windows'ta çalınır; diğer sistemlerde yalnızca testlerde kullanılır.
#![cfg_attr(not(windows), allow(dead_code))]

use std::f32::consts::TAU;

const RATE: u32 = 22_050;

/// Ritim: (başlangıç saniyesi, vuruş gücü, sönme çarpanı). "dı-dıt ... dıt"
/// Son vuruş daha uzun çınlar.
const HITS: [(f32, f32, f32); 3] = [(0.0, 0.75, 1.0), (0.13, 1.0, 1.0), (0.42, 1.0, 2.2)];

/// Metal tınısı için uyumsuz kısmi tonlar (Hz) ve göreli güçleri
const PARTIALS: [(f32, f32); 6] = [
    (196.0, 1.0),
    (311.0, 0.7),
    (467.0, 0.55),
    (702.0, 0.35),
    (1046.0, 0.22),
    (1569.0, 0.12),
];

/// Basit, tekrarlanabilir gürültü üreteci (xorshift)
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

/// Tek bir vuruşun `t` saniyedeki değeri; `ring` sönme süresini uzatır.
fn hit(t: f32, ring: f32, noise: &mut Noise) -> f32 {
    // Gövde: 150 Hz'den 50 Hz'e hızla inen sinüs. Frekans 50 + 100·e^(-t/0.03);
    // faz bunun integrali.
    let phase = TAU * (50.0 * t + 100.0 * 0.03 * (1.0 - (-t / 0.03).exp()));
    let body = phase.sin() * (-t / (0.07 * ring)).exp();

    // Metal: her kısmi ton biraz farklı hızda söner
    let metal: f32 = PARTIALS
        .iter()
        .enumerate()
        .map(|(i, &(f, g))| {
            (TAU * f * t + i as f32).sin() * g * (-t / ((0.05 - i as f32 * 0.005) * ring)).exp()
        })
        .sum::<f32>()
        * 0.3;

    // Tık: ilk birkaç milisaniyedeki gürültü
    let click = noise.next() * (-t / 0.003).exp() * 0.8;

    // Çok kısa yükseliş (1 ms): ani başlangıçtaki çıtlamayı önler
    let attack = (t / 0.001).min(1.0);
    (body + metal + click) * attack
}

/// Örnekleri -1..1 aralığında üretir.
fn samples() -> Vec<f32> {
    let length = 0.42 + 0.5; // son vuruş + sönme süresi
    let n = (length * RATE as f32) as usize;
    let mut out = vec![0.0f32; n];
    let mut noise = Noise(0x1A2B_3C4D);
    for &(start, gain, ring) in &HITS {
        let s0 = (start * RATE as f32) as usize;
        for (i, v) in out[s0..].iter_mut().enumerate() {
            let t = i as f32 / RATE as f32;
            *v += hit(t, ring, &mut noise) * gain;
        }
    }
    // Tepe değeri 0.85'e getir, sondaki çıtlamayı önlemek için yavaşça kapat
    let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-6);
    let fade = (0.08 * RATE as f32) as usize;
    for (i, v) in out.iter_mut().enumerate() {
        *v *= 0.85 / peak;
        if i + fade > n {
            *v *= (n - i) as f32 / fade as f32;
        }
    }
    out
}

/// Bildirim sesini WAV dosyası (bayt dizisi) olarak üretir.
pub fn notification_wav() -> Vec<u8> {
    let pcm: Vec<i16> = samples()
        .into_iter()
        .map(|v| (v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
        .collect();
    let data_len = (pcm.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes()); // fmt bölümü uzunluğu
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&RATE.to_le_bytes());
    w.extend_from_slice(&(RATE * 2).to_le_bytes()); // bayt/sn
    w.extend_from_slice(&2u16.to_le_bytes()); // blok hizası
    w.extend_from_slice(&16u16.to_le_bytes()); // bit
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_is_valid_and_short() {
        let w = notification_wav();
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(&w[8..16], b"WAVEfmt ");
        let data_len = u32::from_le_bytes(w[40..44].try_into().unwrap()) as usize;
        assert_eq!(w.len(), 44 + data_len);
        let secs = data_len as f32 / 2.0 / RATE as f32;
        assert!((0.8..1.2).contains(&secs), "süre {secs}");
        // Sessiz değil ve kırpılmıyor
        let peak = w[44..]
            .chunks(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs())
            .max()
            .unwrap();
        assert!(peak > 20_000 && peak < i16::MAX as u16);
    }

    /// Sesi dinlemek için: `LANCHAT_WAV=ses.wav cargo test -- --ignored sesi_yaz`
    #[test]
    #[ignore]
    fn sesi_yaz() {
        let path = std::env::var("LANCHAT_WAV").unwrap_or_else(|_| "lanchat-bildirim.wav".into());
        std::fs::write(path, notification_wav()).unwrap();
    }
}
