use rubato::Resampler;

use crate::{
  AudioFilter, Error, PlanarAudioBuffer,
  buffer::PlanarVecBuffer,
  chain::{AudioChainBuilder, AudioChainWalker, AudioInfo},
};

struct ResamplerFilter {
  resampler: rubato::Fft<f32>,
  in_buffer: PlanarVecBuffer,
  out_buffer: PlanarVecBuffer,
  remaining_out_frames: usize,
  out_pos: usize,
  delay_frames: usize,
  frames_in: usize,
  frames_out: usize,
  in_sample_rate: usize,
  out_sample_rate: usize,
  num_channels: usize,
}

impl AudioFilter for ResamplerFilter {
  fn read(
    &mut self,
    buffer: &mut dyn crate::PlanarAudioBuffer,
    mut ancestor: AudioChainWalker,
  ) -> Result<usize, Error> {
    let mut frames_written = 0;
    if self.remaining_out_frames > 0 {
      let num = self.remaining_out_frames.min(buffer.num_frames());
      buffer.copy_from_buffer(&self.out_buffer, self.out_pos, 0, num);
      frames_written += num;
      self.remaining_out_frames -= num;
      self.out_pos += num;
    }

    loop {
      if frames_written >= buffer.num_frames() {
        return Ok(frames_written);
      }

      self.in_buffer.fill(0.0_f32);
      let num_read = ancestor.read(&mut self.in_buffer)?;
      if num_read == 0 {
        break;
      }
      self.frames_in += num_read;
      let in_adapter = self.in_buffer.into_adapter()?;
      let mut out_adapter = self.out_buffer.into_adapter_mut()?;
      let (in_frames, out_frames) =
        self
          .resampler
          .process_into_buffer(&in_adapter, &mut out_adapter, None)?;
      assert!(in_frames == self.in_buffer.num_frames()); // should be true if we did everything right

      let skip = self.delay_frames.min(out_frames);
      self.delay_frames -= skip;

      // Drop output produced from the zero padding of a partial final block.
      let max_out = (self.frames_in * self.out_sample_rate).div_ceil(self.in_sample_rate);
      let out_frames = (out_frames - skip).min(max_out - self.frames_out);
      self.frames_out += out_frames;
      let num_to_write = (buffer.num_frames() - frames_written).min(out_frames);
      buffer.copy_from_buffer(&self.out_buffer, skip, frames_written, num_to_write);
      self.remaining_out_frames = out_frames - num_to_write;
      self.out_pos = skip + num_to_write;
      frames_written += num_to_write;
    }
    Ok(frames_written)
  }

  fn seek(&mut self, pos: crate::Timecode, mut ancestor: AudioChainWalker) -> Result<(), Error> {
    self.remaining_out_frames = 0;
    self.resampler.reset();
    self.delay_frames = self.resampler.output_delay();
    self.frames_in = 0;
    self.frames_out = 0;
    ancestor.seek(pos)
  }

  fn duration(&self, ancestor: AudioChainWalker) -> crate::Timecode {
    ancestor.duration()
  }

  fn info(&self) -> AudioInfo {
    AudioInfo {
      num_channels: self.num_channels,
      sample_rate: self.out_sample_rate,
    }
  }
}

pub trait WithResamplerFilter {
  /// Adds a `ResamplerFilter` to the chain, changing the sample rate.
  fn with_resampler(self, target_sample_rate: usize) -> Result<AudioChainBuilder, Error>;
}

impl WithResamplerFilter for AudioChainBuilder {
  fn with_resampler(mut self, target_sample_rate: usize) -> Result<AudioChainBuilder, Error> {
    let info = self.info();

    let resampler = rubato::Fft::<f32>::new(
      info.sample_rate,
      target_sample_rate,
      1024,
      info.num_channels,
      rubato::FixedSync::Both,
    )?;

    let in_buffer_size = resampler.input_frames_max();
    let in_buffer = PlanarVecBuffer::new(info.num_channels, in_buffer_size);

    let out_buffer_size = resampler.output_frames_max();
    let out_buffer = PlanarVecBuffer::new(info.num_channels, out_buffer_size);

    let delay_frames = resampler.output_delay();
    let filter = ResamplerFilter {
      resampler,
      in_buffer,
      out_buffer,
      remaining_out_frames: 0,
      out_pos: 0,
      delay_frames,
      frames_in: 0,
      frames_out: 0,
      in_sample_rate: info.sample_rate,
      out_sample_rate: target_sample_rate,
      num_channels: info.num_channels,
    };

    self.add_filter(filter);

    Ok(self)
  }
}

#[cfg(test)]
mod tests {
  use super::WithResamplerFilter;
  use crate::{
    AudioSource, PlanarAudioBuffer,
    buffer::PlanarVecBuffer,
    sources::NullAudioSource,
    test_util::{FiniteAudioSource, HANG_TIMEOUT, run_with_timeout},
  };

  /// Basic resampler test. Does it return?
  #[test]
  fn resampler_read_returns() {
    let result = run_with_timeout("resampler read", HANG_TIMEOUT, || {
      let source = Box::new(NullAudioSource {
        channels: 2,
        sample_rate: 44100,
      });
      let chain = source
        .builder()
        .with_resampler(48000)
        .map_err(|e| format!("{e:?}"))?
        .commit();
      let mut buf = PlanarVecBuffer::new(2, 1024);
      chain.read(&mut buf).map_err(|e| format!("{e:?}"))
    });
    assert_eq!(result, Ok(1024));
  }

  /// Make sure the resampler actually ends instead of padding with silence.
  #[test]
  fn resampler_returns_short_read_at_end_of_stream() {
    const MAX_READS: usize = 100;
    let result: Result<Option<usize>, String> =
      run_with_timeout("resampler read to end of stream", HANG_TIMEOUT, || {
        let source = Box::new(FiniteAudioSource::new(1, 44100, 4410));
        let chain = source
          .builder()
          .with_resampler(48000)
          .map_err(|e| format!("{e:?}"))?
          .commit();
        let mut buf = PlanarVecBuffer::new(1, 1024);
        for i in 0..MAX_READS {
          let n = chain.read(&mut buf).map_err(|e| format!("{e:?}"))?;
          if n < 1024 {
            return Ok(Some(i));
          }
        }
        Ok(None)
      });
    let reads = result.expect("resampler read failed");
    assert!(
      reads.is_some(),
      "resampler never returned a short read after {MAX_READS} reads of a 4,410-frame source"
    );
  }

  const SOURCE_RATE: usize = 44100;
  const TARGET_RATE: usize = 48000;
  const SOURCE_FRAMES: usize = 4410;
  // How many frames we expect after resampling `SOURCE_FRAMES` from
  // `SOURCE_RATE` to `TARGET_RATE`.
  const EXPECTED_FRAMES: usize = (SOURCE_FRAMES * TARGET_RATE).div_ceil(SOURCE_RATE);

  /// Resamples a mono `FiniteAudioSource` of constant 1.0 and reads it until
  /// the first short read.
  fn resample_finite_source_to_end() -> Vec<f32> {
    const MAX_READS: usize = 100;
    let result: Result<Vec<f32>, String> =
      run_with_timeout("resampler read to end of stream", HANG_TIMEOUT, || {
        let source = Box::new(FiniteAudioSource::new(1, SOURCE_RATE, SOURCE_FRAMES));
        let chain = source
          .builder()
          .with_resampler(TARGET_RATE)
          .map_err(|e| format!("{e:?}"))?
          .commit();
        let mut buf = PlanarVecBuffer::new(1, 1024);
        let mut out = Vec::new();
        for _ in 0..MAX_READS {
          let n = chain.read(&mut buf).map_err(|e| format!("{e:?}"))?;
          out.extend_from_slice(&buf.channel(0)[..n]);
          if n < buf.num_frames() {
            return Ok(out);
          }
        }
        Err(format!(
          "stream did not end within {MAX_READS} reads ({} frames)",
          out.len()
        ))
      });
    result.expect("resampler failed to read to end of stream")
  }

  // Currently we don't handle resampler delay, so we drop frames at the end.
  // This is a test to make sure we fix that.
  #[test]
  fn resampler_flushes_tail_at_end_of_stream() {
    let out = resample_finite_source_to_end();
    let sum: f32 = out.iter().take(EXPECTED_FRAMES).sum();
    let expected = EXPECTED_FRAMES as f32;
    assert!(
      (sum - expected).abs() <= expected * 0.01,
      "output sum {sum} is not within 1% of {expected} - the end of the stream was probably truncated (got {} frames)",
      out.len()
    );
  }

  // When returning from a partially-filled input block, the resampler will read
  // the filled zeroes as audio data. We should cut that so no silence is
  // appended.
  #[test]
  fn resampler_output_length_matches_source_duration() {
    let out = resample_finite_source_to_end();
    assert!(
      out.len().abs_diff(EXPECTED_FRAMES) <= 1,
      "expected {EXPECTED_FRAMES} frames (±1), got {}",
      out.len()
    );
  }
}
