//! Reprodução de `.moflex` (Mobiclip).
//!
//! PROVISÓRIO: ainda não há decodificador Mobiclip em Rust; os quadros vêm do `ffmpeg`
//! (decodificador `mobiclip` + demuxer `moflex`) por pipe, em `bgr0` (= u32 0x00RRGGBB, o formato
//! do framebuffer). A interface (`open`/`next`) é o que fica quando o decodificador nativo existir.
//!
//! Vídeos estéreo (3D) intercalam olho esquerdo/direito quadro a quadro — medido em `yw1_op.moflex`
//! (deslocamento horizontal de ±19 px alternando de sinal; quadro n ≈ quadro n+2). Só o olho
//! esquerdo é mostrado.

use std::io::{self, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::thread;
use std::time::Duration;

pub const WIDTH: usize = 400;
pub const HEIGHT: usize = 240;

pub enum Next {
    Frame(Vec<u32>),
    Pending,
    Ended,
}

pub struct Video {
    rx: Receiver<Vec<u32>>,
    child: Child,
    /// Quadros por segundo exibidos (já dividido por 2 se estéreo).
    pub fps: f64,
}

impl Video {
    pub fn open(ffmpeg: &Path, file: &Path, fps: f64, stereo: bool) -> io::Result<Video> {
        let mut child = Command::new(ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(file)
            .args(["-f", "rawvideo", "-pix_fmt", "bgr0", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut stdout = child.stdout.take().expect("stdout com pipe");
        let (tx, rx) = mpsc::sync_channel(16);
        thread::spawn(move || {
            let mut raw = vec![0u8; WIDTH * HEIGHT * 4];
            let mut n = 0u64;
            while stdout.read_exact(&mut raw).is_ok() {
                n += 1;
                if stereo && n.is_multiple_of(2) {
                    continue;
                }
                let frame = raw
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|p| u32::from_le_bytes(*p))
                    .collect();
                if tx.send(frame).is_err() {
                    break;
                }
            }
        });
        Ok(Video {
            rx,
            child,
            fps: if stereo { fps / 2.0 } else { fps },
        })
    }

    pub fn next(&self) -> Next {
        match self.rx.try_recv() {
            Ok(f) => Next::Frame(f),
            Err(TryRecvError::Empty) => Next::Pending,
            Err(TryRecvError::Disconnected) => Next::Ended,
        }
    }

    pub fn wait(&self, timeout: Duration) -> Next {
        match self.rx.recv_timeout(timeout) {
            Ok(f) => Next::Frame(f),
            Err(RecvTimeoutError::Timeout) => Next::Pending,
            Err(RecvTimeoutError::Disconnected) => Next::Ended,
        }
    }
}

impl Drop for Video {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
