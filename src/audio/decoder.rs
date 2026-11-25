//! Décodage complet d'un fichier audio en mémoire (`symphonia`) et
//! rééchantillonnage vers la fréquence du moteur (`rubato`).

use std::fs::File;
use std::path::Path;

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use super::engine::TrackData;

/// Extensions de fichiers que l'application sait décoder.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "wav", "ogg", "oga", "m4a", "aac", "aif", "aiff", "caf", "mka",
];

/// Décode `path` en stéréo `f32` à la fréquence `target_rate`.
pub fn decode_file(path: &Path, target_rate: u32) -> Result<TrackData, String> {
    let (samples, source_rate) = decode_stereo(path, None)?;
    if samples.is_empty() {
        return Err("le fichier ne contient pas d'audio".into());
    }
    let samples = if source_rate == target_rate {
        samples
    } else {
        resample(&samples, source_rate, target_rate)?
    };
    Ok(TrackData {
        samples,
        sample_rate: target_rate,
    })
}

/// Décode seulement le début du fichier (au plus `seconds`), en mono et à sa
/// fréquence d'origine : suffisant pour l'analyse du tempo et de la tonalité.
pub fn decode_excerpt_mono(path: &Path, seconds: f32) -> Result<(Vec<f32>, u32), String> {
    let (stereo, rate) = decode_stereo(path, Some(seconds))?;
    let (pairs, _) = stereo.as_chunks::<2>();
    Ok((pairs.iter().map(|[l, r]| 0.5 * (l + r)).collect(), rate))
}

/// Décode le fichier (ou seulement ses `max_seconds` premières secondes) en
/// stéréo entrelacé, à sa fréquence d'origine.
fn decode_stereo(path: &Path, max_seconds: Option<f32>) -> Result<(Vec<f32>, u32), String> {
    let file = File::open(path).map_err(|e| format!("ouverture impossible : {e}"))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(&hint, stream, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| format!("format non reconnu : {e}"))?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| "aucune piste audio".to_string())?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| "paramètres de codec manquants".to_string())?;

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| format!("codec non géré : {e}"))?;

    let mut stereo: Vec<f32> = Vec::new();
    let mut packet_samples: Vec<f32> = Vec::new();
    let mut rate = 0u32;

    // S'arrête à la fin du flux, ou sur un flux tronqué (on garde ce qui a été décodé).
    while let Ok(Some(packet)) = format.next_packet() {
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            // Paquet corrompu : on l'ignore et on continue.
            Err(Error::DecodeError(_)) | Err(Error::IoError(_)) => continue,
            Err(e) => return Err(format!("erreur de décodage : {e}")),
        };

        let channels = decoded.spec().channels().count();
        if channels == 0 {
            continue;
        }
        if rate == 0 {
            rate = decoded.spec().rate();
        }
        decoded.copy_to_vec_interleaved(&mut packet_samples);

        match channels {
            1 => stereo.extend(packet_samples.iter().flat_map(|&s| [s, s])),
            2 => stereo.extend_from_slice(&packet_samples),
            // Multicanal : on ne garde que les canaux avant gauche et droit.
            n => stereo.extend(packet_samples.chunks_exact(n).flat_map(|f| [f[0], f[1]])),
        }
        if max_seconds.is_some_and(|max| stereo.len() as f32 >= max * rate as f32 * 2.0) {
            break;
        }
    }

    if rate == 0 {
        return Err("aucun échantillon décodé".into());
    }
    Ok((stereo, rate))
}

fn resample(samples: &[f32], from: u32, to: u32) -> Result<Vec<f32>, String> {
    let frames = samples.len() / 2;
    let mut resampler = Fft::<f32>::new(from as usize, to as usize, 1024, 2, FixedSync::Input)
        .map_err(|e| format!("rééchantillonnage impossible : {e}"))?;
    let input = InterleavedSlice::new(samples, 2, frames).map_err(|e| format!("rééchantillonnage impossible : {e}"))?;
    let output = resampler
        .process_all(&input, frames, None)
        .map_err(|e| format!("rééchantillonnage impossible : {e}"))?;
    Ok(output.take_data())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Écrit un WAV PCM 16 bits contenant un sinus 440 Hz d'amplitude 0,5.
    fn write_wav(path: &Path, sample_rate: u32, channels: u16, seconds: f32) {
        let frames = (seconds * sample_rate as f32) as u32;
        let data_len = frames * channels as u32 * 2;
        let mut bytes = Vec::new();
        bytes.extend(b"RIFF");
        bytes.extend((36 + data_len).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16u32.to_le_bytes());
        bytes.extend(1u16.to_le_bytes()); // PCM
        bytes.extend(channels.to_le_bytes());
        bytes.extend(sample_rate.to_le_bytes());
        bytes.extend((sample_rate * channels as u32 * 2).to_le_bytes());
        bytes.extend((channels * 2).to_le_bytes());
        bytes.extend(16u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(data_len.to_le_bytes());
        for n in 0..frames {
            let v = 0.5 * (2.0 * std::f32::consts::PI * 440.0 * n as f32 / sample_rate as f32).sin();
            for _ in 0..channels {
                bytes.extend(((v * 32767.0) as i16).to_le_bytes());
            }
        }
        std::fs::write(path, bytes).unwrap();
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    #[test]
    fn decodes_and_resamples_wav() {
        let dir = std::env::temp_dir().join(format!("rustymix-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Stéréo 48 kHz → 48 kHz : aucune conversion.
        let stereo = dir.join("stereo.wav");
        write_wav(&stereo, 48_000, 2, 1.0);
        let track = decode_file(&stereo, 48_000).unwrap();
        assert_eq!(track.frames(), 48_000);
        assert!((peak(&track.samples) - 0.5).abs() < 0.01);

        // Mono 44,1 kHz → stéréo 48 kHz : duplication du canal et rééchantillonnage.
        let mono = dir.join("mono.wav");
        write_wav(&mono, 44_100, 1, 1.0);
        let track = decode_file(&mono, 48_000).unwrap();
        assert_eq!(track.sample_rate, 48_000);
        assert!(
            (track.frames() as i64 - 48_000).abs() < 64,
            "frames = {}",
            track.frames()
        );
        assert!(
            (peak(&track.samples) - 0.5).abs() < 0.03,
            "pic = {}",
            peak(&track.samples)
        );
        assert!(track.samples.as_chunks::<2>().0.iter().all(|[l, r]| l == r));

        // Fichier illisible : erreur, pas de panique.
        let bogus = dir.join("bogus.mp3");
        std::fs::write(&bogus, b"ceci n'est pas un mp3").unwrap();
        assert!(decode_file(&bogus, 48_000).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }
}
