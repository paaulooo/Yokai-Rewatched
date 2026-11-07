//! Saída de áudio (cpal). O contador de quadros entregues ao dispositivo é o relógio mestre
//! dos vídeos, para manter imagem e música sincronizadas.

pub mod bcstm;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

pub struct Player {
    _stream: cpal::Stream,
    played: Arc<AtomicU64>,
    volume: Arc<AtomicU32>,
    out_rate: u32,
    duration: f64,
}

impl Player {
    pub fn play(src: Arc<bcstm::Stream>) -> Result<Player, String> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("nenhum dispositivo de saída de áudio")?;
        let supported = device.default_output_config().map_err(|e| e.to_string())?;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let out_rate = config.sample_rate.0;
        let played = Arc::new(AtomicU64::new(0));
        let volume = Arc::new(AtomicU32::new(1f32.to_bits()));
        let duration = src.frames() as f64 / src.sample_rate as f64;
        let shared = (played.clone(), volume.clone());
        let stream = match format {
            cpal::SampleFormat::F32 => build::<f32>(&device, &config, src, shared),
            cpal::SampleFormat::I16 => build::<i16>(&device, &config, src, shared),
            cpal::SampleFormat::U16 => build::<u16>(&device, &config, src, shared),
            other => return Err(format!("formato de amostra {other:?} não suportado")),
        }?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(Player {
            _stream: stream,
            played,
            volume,
            out_rate,
            duration,
        })
    }

    /// Segundos já entregues ao dispositivo.
    pub fn seconds(&self) -> f64 {
        self.played.load(Ordering::Relaxed) as f64 / self.out_rate as f64
    }

    pub fn duration(&self) -> f64 {
        self.duration
    }

    pub fn set_volume(&self, v: f32) {
        self.volume
            .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    src: Arc<bcstm::Stream>,
    (played, volume): (Arc<AtomicU64>, Arc<AtomicU32>),
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let out_ch = config.channels as usize;
    let step = src.sample_rate as f64 / config.sample_rate.0 as f64;
    let frames = src.frames();
    let sample = move |f: usize, c: usize| -> f32 {
        if f >= frames {
            return 0.0;
        }
        src.pcm[f * src.channels + c.min(src.channels - 1)] as f32 / 32768.0
    };
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                let mut n = played.load(Ordering::Relaxed);
                let gain = f32::from_bits(volume.load(Ordering::Relaxed));
                for out in data.chunks_mut(out_ch) {
                    let pos = n as f64 * step;
                    let (i, t) = (pos as usize, (pos.fract()) as f32);
                    for (c, o) in out.iter_mut().enumerate() {
                        let v = sample(i, c) * (1.0 - t) + sample(i + 1, c) * t;
                        *o = T::from_sample(v * gain);
                    }
                    n += 1;
                }
                played.store(n, Ordering::Relaxed);
            },
            |e| eprintln!("[yw1] erro no stream de áudio: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::bcstm;
    use std::path::PathBuf;
    use std::process::Command;

    /// Compara o decodificador nativo com o do ffmpeg (adpcm_thp_le) no stream real da abertura.
    /// Pulado se o RomFS ou o ffmpeg não estiverem disponíveis. O ffmpeg arredonda diferente da
    /// fórmula de referência da Nintendo usada aqui (`(pred >> 11) + nib·escala` em vez de
    /// `(nib·escala·2048 + 1024 + pred) >> 11`); com a fórmula dele a saída bate exatamente (verificado),
    /// então só se tolera esse erro de arredondamento — um erro de estrutura daria milhares.
    #[test]
    fn opening_song_matches_ffmpeg() {
        let Some(romfs) = crate::romfs::locate(None) else {
            return;
        };
        let path: PathBuf = romfs.join("snd/product/stream/yw1_op_en.dspadpcm.bcstm");
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        let ours = bcstm::decode(&bytes).expect("decodificar");
        let Ok(out) = Command::new(crate::ffmpeg_path())
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&path)
            .args(["-f", "s16le", "-acodec", "pcm_s16le", "-"])
            .output()
        else {
            return;
        };
        let theirs: Vec<i16> = out
            .stdout
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        assert_eq!(ours.sample_rate, 32728);
        assert_eq!(ours.channels, 2);
        assert_eq!(ours.pcm.len(), theirs.len(), "número de amostras");
        let max = ours
            .pcm
            .iter()
            .zip(&theirs)
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .max();
        assert!(
            max.unwrap_or(0) <= 64,
            "diferença máxima {max:?} além do arredondamento"
        );
    }
}
