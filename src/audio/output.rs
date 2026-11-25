//! Sortie audio via `cpal` (WASAPI sous Windows, ALSA/PipeWire/PulseAudio sous Linux).

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};

use super::engine::Engine;

/// Taille du tampon intermédiaire `f32`, en échantillons.
const SCRATCH_SAMPLES: usize = 8192;

/// Périphérique de sortie choisi et sa configuration par défaut.
pub struct OutputDevice {
    device: cpal::Device,
    config: cpal::SupportedStreamConfig,
    pub name: String,
}

impl OutputDevice {
    /// Ouvre le périphérique de sortie par défaut du système.
    pub fn open_default() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "aucun périphérique de sortie audio".to_string())?;
        let config = device
            .default_output_config()
            .map_err(|e| format!("configuration audio indisponible : {e}"))?;
        let name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "Sortie par défaut".to_string());
        Ok(Self { device, config, name })
    }

    pub fn sample_rate(&self) -> u32 {
        self.config.sample_rate()
    }

    /// Démarre le flux : le moteur est déplacé dans le callback audio.
    /// Le flux s'arrête quand la valeur retournée est détruite.
    pub fn start(self, engine: Engine) -> Result<cpal::Stream, String> {
        let format = self.config.sample_format();
        let config: StreamConfig = self.config.into();
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&self.device, config, engine),
            SampleFormat::F64 => build::<f64>(&self.device, config, engine),
            SampleFormat::I16 => build::<i16>(&self.device, config, engine),
            SampleFormat::I32 => build::<i32>(&self.device, config, engine),
            SampleFormat::I8 => build::<i8>(&self.device, config, engine),
            SampleFormat::U8 => build::<u8>(&self.device, config, engine),
            SampleFormat::U16 => build::<u16>(&self.device, config, engine),
            SampleFormat::U32 => build::<u32>(&self.device, config, engine),
            other => return Err(format!("format d'échantillon non géré : {other}")),
        }?;
        stream
            .play()
            .map_err(|e| format!("démarrage du flux audio impossible : {e}"))?;
        Ok(stream)
    }
}

fn build<T>(device: &cpal::Device, config: StreamConfig, mut engine: Engine) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    // Tampon alloué une fois pour toutes : le callback n'alloue jamais.
    let mut scratch = vec![0.0f32; SCRATCH_SAMPLES - SCRATCH_SAMPLES % channels.max(1)];

    device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                for chunk in data.chunks_mut(scratch.len()) {
                    let buffer = &mut scratch[..chunk.len()];
                    engine.process(buffer, channels);
                    for (out, &sample) in chunk.iter_mut().zip(buffer.iter()) {
                        *out = T::from_sample(sample);
                    }
                }
            },
            |error| eprintln!("Erreur du flux audio : {error}"),
            None,
        )
        .map_err(|e| format!("ouverture du flux audio impossible : {e}"))
}
