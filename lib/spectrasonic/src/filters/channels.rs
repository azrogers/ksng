use crate::{
  AudioFilter, Error, PlanarAudioBuffer, buffer::PlanarVecBuffer, chain::AudioChainBuilder,
  chain::AudioInfo,
};

const BLOCK_SIZE: usize = 1024;

struct ChannelRemapperFilter {
  to_channels: usize,
  sample_rate: usize,
  buffer: PlanarVecBuffer,
  /// Frames in `buffer` read from the source but not yet written out.
  remaining_frames: usize,
  /// Where in `buffer` those frames start.
  remaining_offset: usize,
  mapping: Vec<Vec<f32>>,
}

impl ChannelRemapperFilter {
  fn remap_and_write(
    &self,
    buffer: &mut dyn PlanarAudioBuffer,
    num: usize,
    from_offset: usize,
    to_offset: usize,
  ) {
    for i in 0..buffer.num_channels() {
      let mapping = &self.mapping[i];
      for (j, w) in mapping.iter().enumerate() {
        if *w == 0.0 {
          continue;
        }

        for n in 0..num {
          buffer.channel_mut(i)[to_offset + n] += self.buffer.channel(j)[from_offset + n] * *w;
        }
      }
    }
  }
}

impl AudioFilter for ChannelRemapperFilter {
  fn read(
    &mut self,
    buffer: &mut dyn crate::PlanarAudioBuffer,
    mut ancestor: crate::chain::AudioChainWalker,
  ) -> Result<usize, Error> {
    assert!(buffer.num_channels() == self.to_channels);
    buffer.fill(0.0);

    let mut frames_written = 0;
    if self.remaining_frames > 0 {
      let num_to_write = self.remaining_frames.min(buffer.num_frames());
      self.remap_and_write(buffer, num_to_write, self.remaining_offset, 0);
      frames_written += num_to_write;
      self.remaining_frames -= num_to_write;
      self.remaining_offset += num_to_write;
    }

    loop {
      if frames_written >= buffer.num_frames() {
        return Ok(frames_written);
      }

      let num_read = ancestor.read(&mut self.buffer)?;
      if num_read == 0 {
        return Ok(frames_written);
      }
      let num_to_write = (buffer.num_frames() - frames_written).min(num_read);
      self.remap_and_write(buffer, num_to_write, 0, frames_written);
      frames_written += num_to_write;
      self.remaining_frames = num_read - num_to_write;
      self.remaining_offset = num_to_write;
    }
  }

  fn seek(
    &mut self,
    pos: crate::Timecode,
    mut ancestor: crate::chain::AudioChainWalker,
  ) -> Result<(), Error> {
    self.remaining_frames = 0;
    ancestor.seek(pos)
  }

  fn duration(&self, ancestor: crate::chain::AudioChainWalker) -> crate::Timecode {
    ancestor.duration()
  }

  fn info(&self) -> AudioInfo {
    AudioInfo {
      num_channels: self.to_channels,
      sample_rate: self.sample_rate,
    }
  }
}

pub trait WithChannelRemapperFilter {
  /// Adds a `ChannelRemapperFilter` to the chain, remapping the number of
  /// channels to `new_channels`.
  ///
  /// The input channels will be evenly distributed over the output channels.
  fn with_channel_remapper(self, new_channels: usize) -> Result<AudioChainBuilder, Error>;
}

impl WithChannelRemapperFilter for AudioChainBuilder {
  fn with_channel_remapper(mut self, new_channels: usize) -> Result<AudioChainBuilder, Error> {
    let info = self.info();
    let mut mapping = Vec::with_capacity(new_channels);

    // treat each destination channel as a window over the source channels
    // for example, for 6 input channels and 2 output channels:
    // in:  [0][1][2][3][4][5]
    // out: [   0   ][   1   ]
    // with divisions that aren't perfect we'll see fractional windows, in which
    // case we need to calculate the exact contribution.
    // for example, with 6 input channels and 4 output channels:
    // in:  [0 ][1 ][2 ][3 ][4 ][5 ]
    // out: [0   ][1   ][2   ][3   ]
    // each destination channel covers 1.5 input channels. here, in channel 0
    // contributes 100% to out channel 0, and in channel 1 contributes 50% to
    // out channel 0 and out channel 1.
    // we can say the window for out channel 0 is (0, 1.5) and the window for in
    // channel 1 is (1, 2), so the overlap is (1, 1.5) and the contribution is
    // (1.5 - 1) or 0.5.
    let step = info.num_channels as f32 / new_channels as f32;
    for i in 0..new_channels {
      // the range of source channels that this channel covers
      let dest_start = i as f32 * step;
      let dest_end = (i + 1) as f32 * step;
      let mut row = Vec::with_capacity(info.num_channels);
      for j in 0..info.num_channels {
        let overlap_start = dest_start.max(j as f32);
        let overlap_end = dest_end.min((j + 1) as f32);
        let overlap = (overlap_end - overlap_start).max(0.0);
        row.push(overlap);
      }
      mapping.push(row);
    }

    let buffer = PlanarVecBuffer::new(info.num_channels, BLOCK_SIZE);

    let filter = ChannelRemapperFilter {
      sample_rate: info.sample_rate,
      to_channels: new_channels,
      mapping,
      buffer,
      remaining_frames: 0,
      remaining_offset: 0,
    };

    self.add_filter(filter);

    Ok(self)
  }
}

#[cfg(test)]
mod tests {
  use super::WithChannelRemapperFilter;
  use crate::{
    AudioSource, PlanarAudioBuffer,
    buffer::PlanarVecBuffer,
    test_util::{FiniteAudioSource, HANG_TIMEOUT, run_with_timeout},
  };

  /// Reads `reads` times from a mono source of `source_frames` frames remapped
  /// to stereo, returning the frame count of each read.
  fn remap_mono_to_stereo_reads(
    what: &str,
    source_frames: usize,
    reads: usize,
  ) -> Result<Vec<usize>, String> {
    run_with_timeout(what, HANG_TIMEOUT, move || {
      let source = Box::new(FiniteAudioSource::new(1, 44100, source_frames));
      let chain = source
        .builder()
        .with_channel_remapper(2)
        .map_err(|e| format!("{e:?}"))?
        .commit();
      let mut buf = PlanarVecBuffer::new(2, 1024);
      (0..reads)
        .map(|_| chain.read(&mut buf).map_err(|e| format!("{e:?}")))
        .collect()
    })
  }

  // A source that ends partway through a read must produce a short read.
  #[test]
  fn remapper_returns_short_read_when_source_ends_mid_read() {
    let result = remap_mono_to_stereo_reads("remapper read across end of stream", 1500, 2);
    assert_eq!(result, Ok(vec![1024, 476]));
  }

  // Reading a source that has already ended should return a 0.
  #[test]
  fn remapper_returns_zero_after_end_of_stream() {
    let result = remap_mono_to_stereo_reads("remapper read after end of stream", 1024, 2);
    assert_eq!(result, Ok(vec![1024, 0]));
  }

  // When the source returns fewer frames than the remapper's internal buffer
  // holds, frames left over for the next read must still come out in order.
  #[test]
  fn remapper_preserves_leftover_frames_after_short_source_read() {
    let result = run_with_timeout("remapper leftover after short read", HANG_TIMEOUT, || {
      // 1000 frames is less than the remapper's 1024-frame block, so the
      // source's first read is short.
      let mut source = FiniteAudioSource::new(1, 44100, 1000);
      source.ramp = true;
      let chain = Box::new(source)
        .builder()
        .with_channel_remapper(1)
        .map_err(|e| format!("{e:?}"))?
        .commit();

      // Reading in 600-frame chunks leaves 400 frames over after the first
      // read.
      let mut buf = PlanarVecBuffer::new(1, 600);
      let mut out = Vec::new();
      loop {
        let n = chain.read(&mut buf).map_err(|e| format!("{e:?}"))?;
        if n == 0 {
          break;
        }
        out.extend_from_slice(&buf.channel(0)[..n]);
      }
      Ok::<_, String>(out)
    });

    let expected: Vec<f32> = (0..1000).map(|i| i as f32).collect();
    assert_eq!(result, Ok(expected));
  }
}
