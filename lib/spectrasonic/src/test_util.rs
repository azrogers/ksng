//! Helpers shared by unit tests.

use std::{sync::mpsc, time::Duration};

use crate::{
  AudioSource, Error, PlanarAudioBuffer, Timecode,
  chain::{AudioChainBuilder, AudioInfo},
};

/// How long a single test operation may run before it counts as hung.
pub const HANG_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs `f` on a separate thread and returns its result, or panics if it does
/// not finish within `timeout`.
pub fn run_with_timeout<T: Send + 'static>(
  what: &str,
  timeout: Duration,
  f: impl FnOnce() -> T + Send + 'static,
) -> T {
  let (tx, rx) = mpsc::channel();
  std::thread::spawn(move || {
    let _ = tx.send(f());
  });
  match rx.recv_timeout(timeout) {
    Ok(v) => v,
    Err(mpsc::RecvTimeoutError::Timeout) => {
      panic!("{what} did not finish within {timeout:?} (hang)")
    }
    Err(mpsc::RecvTimeoutError::Disconnected) => panic!("{what} panicked"),
  }
}

/// An `AudioSource` that produces `total_frames` frames of a constant value,
/// then reports end of stream by returning short reads.
pub struct FiniteAudioSource {
  pub num_channels: usize,
  pub sample_rate: usize,
  pub total_frames: usize,
  pub value: f32,
  pub ramp: bool,
  pub pos: usize,
}

impl FiniteAudioSource {
  pub fn new(num_channels: usize, sample_rate: usize, total_frames: usize) -> FiniteAudioSource {
    FiniteAudioSource {
      num_channels,
      sample_rate,
      total_frames,
      value: 1.0,
      ramp: false,
      pos: 0,
    }
  }
}

impl AudioSource for FiniteAudioSource {
  fn read(&mut self, buffer: &mut dyn PlanarAudioBuffer) -> Result<usize, Error> {
    let n = (self.total_frames - self.pos).min(buffer.num_frames());
    for ch in 0..buffer.num_channels() {
      let channel = buffer.channel_mut(ch);
      if self.ramp {
        for (i, sample) in channel[..n].iter_mut().enumerate() {
          *sample = (self.pos + i) as f32;
        }
      } else {
        channel[..n].fill(self.value);
      }
      channel[n..].fill(0.0);
    }
    self.pos += n;
    Ok(n)
  }

  fn seek(&mut self, pos: Timecode) -> Result<(), Error> {
    self.pos = pos
      .to_frames_at_rate(self.sample_rate)
      .min(self.total_frames);
    Ok(())
  }

  fn duration(&self) -> Timecode {
    Timecode {
      samples: self.total_frames,
      sample_rate: self.sample_rate,
    }
  }

  fn builder(self: Box<Self>) -> AudioChainBuilder {
    AudioChainBuilder::new(self)
  }

  fn info(&self) -> AudioInfo {
    AudioInfo {
      num_channels: self.num_channels,
      sample_rate: self.sample_rate,
    }
  }
}
