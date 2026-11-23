use std::{
  collections::HashMap,
  io::{Cursor, Read, Seek, SeekFrom},
  path::{Path, PathBuf},
  sync::{Arc, Mutex, RwLock, mpsc},
  task::Poll,
  thread,
};

use egui::{
  Context,
  load::{Bytes, BytesLoadResult, BytesLoader, BytesPoll, LoadError},
};
use klib::objects::{
  attachment::{AttachmentReader, AttachmentResolver, AttachmentSource},
  audio::AudioFileSource,
  event::{Event, EventValue},
  file::File,
};
use log::error;
use spectrasonic::{PlanarAudioBuffer, buffer::PlanarVecBuffer};
use tar::{Archive, Builder, Header};
use tiny_skia::{Color, Paint, Pixmap, Rect, Transform};
use uuid::Uuid;

use crate::{
  fs::{Cache, KsngAttachmentResolver},
  tabs::timeline,
  util::{error::UiError, logger::Logger},
};

const BASE_BLOCK_SIZE: usize = 8192;
pub const MAX_TEXTURE_SIZE: usize = 2048;

struct WaveformResult {
  levels: Vec<Vec<Vec<u8>>>,
  pixel_to_s: Vec<f32>,
  duration: f32,
}

pub struct WaveformCacheInfo {
  pub levels: usize,
  pub imgs_per_level: Vec<usize>,
  pub pixel_to_s: Vec<f32>,
  pub duration: f32,
}

impl WaveformResult {
  pub fn save_to_archive(&self, out_path: &Path) -> Result<(), UiError> {
    let writer = std::fs::File::create(out_path)?;
    let mut builder = Builder::new(writer);

    assert!(self.levels.len() <= 0xff);

    // first file has info on how many mip levels and how many images per level
    let mut header = Header::new_gnu();
    header.set_path(".shape")?;
    let mut data = Vec::new();
    data.push(self.levels.len() as u8);
    let dur = self.duration.to_le_bytes();
    data.push(dur[0]);
    data.push(dur[1]);
    data.push(dur[2]);
    data.push(dur[3]);
    for (i, level) in self.levels.iter().enumerate() {
      assert!(level.len() <= 0xffff);
      let len = (level.len() as u16).to_le_bytes();
      data.push(len[0]);
      data.push(len[1]);
      let pxs = self.pixel_to_s[i].to_le_bytes();
      data.push(pxs[0]);
      data.push(pxs[1]);
      data.push(pxs[2]);
      data.push(pxs[3]);
    }
    header.set_size(data.len() as u64);
    header.set_cksum();
    builder.append(&header, data.as_slice())?;

    for (l, level) in self.levels.iter().enumerate() {
      for (idx, img) in level.iter().enumerate() {
        let mut header = Header::new_gnu();
        header.set_path(format!("mip-l{l}-i{idx}.png"))?;
        header.set_size(img.len() as u64);
        header.set_cksum();
        builder.append(&header, img.as_slice())?;
      }
    }

    Ok(())
  }
}

/// Generates multiple mipmaps worth of waveforms for a file.
/// Ensures each produced image is no more than MAX_TEXTURE_SIZE pixels in
/// width.
pub struct WaveformGenerator {
  height: usize,
  mip_levels: usize,
}

impl WaveformGenerator {
  fn create_waveform(&self, path: &Path) -> Result<WaveformResult, UiError> {
    let mut source = spectrasonic::sources::source_for_file(path)?;
    let duration = source.duration().to_seconds();

    let mut n_total_blocks: Vec<usize> = Vec::with_capacity(self.mip_levels);
    let mut peaks: Vec<Vec<f32>> = Vec::with_capacity(self.mip_levels);
    let mut block_sizes = Vec::with_capacity(self.mip_levels);
    for i in 0..self.mip_levels {
      peaks.push(Vec::new());
      block_sizes.push(BASE_BLOCK_SIZE / 2_usize.pow((i + 1) as u32));
      n_total_blocks.push(0);
    }

    let mut block = PlanarVecBuffer::new(source.info().num_channels, BASE_BLOCK_SIZE);

    loop {
      let frames = source.read(&mut block)?;
      if frames == 0 {
        break;
      }

      for i in 0..self.mip_levels {
        let n_blocks = frames.div_ceil(block_sizes[i]);
        for b in 0..n_blocks {
          let mut sum = 0.0;
          for j in 0..block_sizes[i] {
            for ch in 0..block.num_channels() {
              sum += block.channel(ch)[b * block_sizes[i] + j].powi(2);
            }
          }
          peaks[i].push(Self::create_peak(
            sum,
            block_sizes[i] * block.num_channels(),
          ));
          n_total_blocks[i] += 1;
        }
      }

      if frames < BASE_BLOCK_SIZE {
        break;
      }
    }

    let mut pixel_to_s = Vec::with_capacity(self.mip_levels);
    for total in n_total_blocks {
      pixel_to_s.push(duration / total as f32);
    }
    self.render_peaks(peaks, pixel_to_s, duration)
  }

  fn create_peak(sum: f32, count: usize) -> f32 {
    assert!(count > 0);
    f32::sqrt(sum / count as f32)
  }

  fn render_peaks(
    &self,
    peaks: Vec<Vec<f32>>,
    pixel_to_s: Vec<f32>,
    duration: f32,
  ) -> Result<WaveformResult, UiError> {
    let mut levels = Vec::with_capacity(peaks.len());
    for level in peaks.iter() {
      let mut images = Vec::new();
      for chunk in level.chunks(MAX_TEXTURE_SIZE) {
        images.push(self.write_peaks_to_image(chunk)?);
      }
      levels.push(images);
    }
    Ok(WaveformResult {
      levels,
      pixel_to_s,
      duration,
    })
  }

  fn write_peaks_to_image(&self, peaks: &[f32]) -> Result<Vec<u8>, UiError> {
    let width: usize = peaks.len();
    let mut pixmap = Pixmap::new(width as u32, self.height as u32).ok_or(UiError::Audio(
      "Can't get pixmap to draw waveform".to_string(),
    ))?;
    pixmap.fill(Color::TRANSPARENT);
    let mut paint = Paint::default();
    paint.set_color_rgba8(0, 0, 0, 255);

    let center_y = (self.height as f32 / 2.0).floor();
    for (i, peak) in peaks.iter().enumerate() {
      assert!(!peak.is_nan());
      let peak = (*peak).clamp(0.05, 1.0);
      let start_y = (center_y - (peak * center_y)).floor();
      let end_y = (center_y + (peak * center_y)).floor();
      pixmap.fill_rect(
        Rect::from_ltrb(i as f32, start_y, (i + 1) as f32, end_y).unwrap(),
        &paint,
        Transform::identity(),
        None,
      );
    }

    pixmap
      .encode_png()
      .map_err(|e| UiError::Audio(e.to_string()))
  }
}

/// Location of one image inside a waveform archive.
#[derive(Clone, Copy)]
struct ArchiveSpan {
  offset: u64,
  size: u64,
}

pub struct WaveformCache {
  id: Uuid,
  info: WaveformCacheInfo,
  /// Indexed by `[level][idx]`.
  index: Vec<Vec<Option<ArchiveSpan>>>,
  file: Mutex<std::fs::File>,
}

impl WaveformCache {
  /// Opens and indexes the archive at `path`
  fn open(id: Uuid, path: &Path) -> Result<WaveformCache, UiError> {
    let data = std::fs::read(path)?;
    let mut ar = Archive::new(Cursor::new(data.as_slice()));
    let mut entries = ar.entries_with_seek()?;

    let Some(Ok(mut shape_entry)) = entries.next() else {
      return Err(UiError::Audio(format!(
        "Failed to load info from archive for {path:?}"
      )));
    };
    let levels = shape_entry.read_le::<u8>()? as usize;
    let duration = shape_entry.read_le::<f32>()?;
    let mut imgs_per_level = Vec::with_capacity(levels);
    let mut pixel_to_s = Vec::with_capacity(levels);
    for _ in 0..levels {
      imgs_per_level.push(shape_entry.read_le::<u16>()? as usize);
      pixel_to_s.push(shape_entry.read_le::<f32>()?);
    }
    drop(shape_entry);

    let mut index: Vec<Vec<Option<ArchiveSpan>>> =
      imgs_per_level.iter().map(|&n| vec![None; n]).collect();
    for entry in entries {
      let entry = entry?;
      let entry_path = entry.path()?;
      let Some((level, idx)) = entry_path
        .file_name()
        .and_then(|s| s.to_str())
        .and_then(parse_image_name)
      else {
        continue;
      };
      if let Some(slot) = index.get_mut(level).and_then(|l| l.get_mut(idx)) {
        *slot = Some(ArchiveSpan {
          offset: entry.raw_file_position(),
          size: entry.size(),
        });
      }
    }

    Ok(WaveformCache {
      id,
      info: WaveformCacheInfo {
        levels,
        imgs_per_level,
        pixel_to_s,
        duration,
      },
      index,
      file: Mutex::new(std::fs::File::open(path)?),
    })
  }

  /// The number of mipmaps and the number of images per mipmap.
  pub fn info(&self) -> &WaveformCacheInfo {
    &self.info
  }

  /// URI for the given image for `WaveformBytesLoader`.
  pub fn image_uri(&self, level: usize, idx: usize) -> String {
    format!("{URI_SCHEME}{}/l{level}-i{idx}.png", self.id)
  }

  fn all_image_uris(&self) -> impl Iterator<Item = String> + '_ {
    self
      .info
      .imgs_per_level
      .iter()
      .enumerate()
      .flat_map(move |(level, &n)| (0..n).map(move |idx| self.image_uri(level, idx)))
  }

  /// Reads one PNG out of the archive.
  fn read_image(&self, level: usize, idx: usize) -> Result<Vec<u8>, UiError> {
    let span = self
      .index
      .get(level)
      .and_then(|l| l.get(idx))
      .copied()
      .flatten()
      .ok_or_else(|| {
        UiError::Audio(format!(
          "Could not find mip level {level} idx {idx} for waveform {}",
          self.id
        ))
      })?;
    let mut buf = vec![0; span.size as usize];
    let mut file = self.file.lock().unwrap();
    file.seek(SeekFrom::Start(span.offset))?;
    file.read_exact(&mut buf)?;
    Ok(buf)
  }
}

/// Parses `mip-l{level}-i{idx}.png`.
fn parse_image_name(name: &str) -> Option<(usize, usize)> {
  let (level, idx) = name
    .strip_prefix("mip-l")?
    .strip_suffix(".png")?
    .split_once("-i")?;
  Some((level.parse().ok()?, idx.parse().ok()?))
}

const URI_SCHEME: &str = "waveform://";

/// Parses `waveform://{uuid}/l{level}-i{idx}.png`.
fn parse_image_uri(uri: &str) -> Option<(Uuid, usize, usize)> {
  let (id, rest) = uri.strip_prefix(URI_SCHEME)?.split_once('/')?;
  let (level, idx) = rest
    .strip_prefix('l')?
    .strip_suffix(".png")?
    .split_once("-i")?;
  Some((
    Uuid::parse_str(id).ok()?,
    level.parse().ok()?,
    idx.parse().ok()?,
  ))
}

enum WaveformState {
  Loading,
  Loaded(Arc<WaveformCache>),
  Failed,
}

type WaveformsMap = Arc<RwLock<HashMap<Uuid, WaveformState>>>;

type LoadedBytes = HashMap<String, Poll<Result<Bytes, String>>>;

struct ReadJob {
  uri: String,
  cache: Arc<WaveformCache>,
  level: usize,
  idx: usize,
  ctx: Context,
}

/// egui bytes loader for `waveform://` URIs.
pub struct WaveformBytesLoader {
  waveforms: WaveformsMap,
  loaded: Arc<Mutex<LoadedBytes>>,
  jobs: mpsc::Sender<ReadJob>,
}

impl WaveformBytesLoader {
  pub const ID: &'static str = egui::generate_loader_id!(WaveformBytesLoader);

  fn new(waveforms: WaveformsMap) -> WaveformBytesLoader {
    let loaded: Arc<Mutex<LoadedBytes>> = Default::default();
    let (jobs, rx) = mpsc::channel::<ReadJob>();
    let worker_loaded = loaded.clone();
    thread::Builder::new()
      .name("waveform-reader".to_owned())
      .spawn(move || {
        for job in rx {
          let result = job
            .cache
            .read_image(job.level, job.idx)
            .map(Bytes::from)
            .map_err(|e| format!("{e:?}"));
          // Only store the result if the URI wasn't forgotten in the meantime.
          if let Some(slot) = worker_loaded.lock().unwrap().get_mut(&job.uri) {
            *slot = Poll::Ready(result);
          }
          job.ctx.request_repaint();
        }
      })
      .expect("failed to spawn waveform reader thread");

    WaveformBytesLoader {
      waveforms,
      loaded,
      jobs,
    }
  }
}

impl BytesLoader for WaveformBytesLoader {
  fn id(&self) -> &str {
    Self::ID
  }

  fn load(&self, ctx: &Context, uri: &str) -> BytesLoadResult {
    let Some((id, level, idx)) = parse_image_uri(uri) else {
      return Err(LoadError::NotSupported);
    };

    let mut loaded = self.loaded.lock().unwrap();
    match loaded.get(uri) {
      Some(Poll::Ready(Ok(bytes))) => {
        return Ok(BytesPoll::Ready {
          size: None,
          bytes: bytes.clone(),
          mime: Some("image/png".to_owned()),
        });
      }
      Some(Poll::Ready(Err(e))) => return Err(LoadError::Loading(e.clone())),
      Some(Poll::Pending) => return Ok(BytesPoll::Pending { size: None }),
      None => {}
    }

    let cache = match self.waveforms.read().unwrap().get(&id) {
      Some(WaveformState::Loaded(cache)) => cache.clone(),
      _ => {
        return Err(LoadError::Loading(format!("Waveform {id} is not loaded")));
      }
    };
    loaded.insert(uri.to_owned(), Poll::Pending);
    drop(loaded);

    let job = ReadJob {
      uri: uri.to_owned(),
      cache,
      level,
      idx,
      ctx: ctx.clone(),
    };
    if self.jobs.send(job).is_err() {
      self.loaded.lock().unwrap().remove(uri);
      return Err(LoadError::Loading(
        "Waveform reader thread has stopped".to_owned(),
      ));
    }
    Ok(BytesPoll::Pending { size: None })
  }

  fn forget(&self, uri: &str) {
    self.loaded.lock().unwrap().remove(uri);
  }

  fn forget_all(&self) {
    self.loaded.lock().unwrap().clear();
  }

  fn byte_size(&self) -> usize {
    self
      .loaded
      .lock()
      .unwrap()
      .values()
      .map(|v| match v {
        Poll::Ready(Ok(bytes)) => bytes.len(),
        _ => 0,
      })
      .sum()
  }

  fn has_pending(&self) -> bool {
    self.loaded.lock().unwrap().values().any(Poll::is_pending)
  }
}

pub struct AudioWaveformProvider {
  waveforms: WaveformsMap,
  bytes_loader: Arc<WaveformBytesLoader>,
}

impl AudioWaveformProvider {
  pub fn new() -> AudioWaveformProvider {
    let waveforms: WaveformsMap = Default::default();
    AudioWaveformProvider {
      bytes_loader: Arc::new(WaveformBytesLoader::new(waveforms.clone())),
      waveforms,
    }
  }

  /// Loader for `waveform://` URIs.
  pub fn bytes_loader(&self) -> Arc<WaveformBytesLoader> {
    self.bytes_loader.clone()
  }

  pub fn clear(&mut self, ctx: &Context) {
    for v in self.waveforms.read().unwrap().values() {
      if let WaveformState::Loaded(cache) = &v {
        for uri in cache.all_image_uris() {
          ctx.forget_image(&uri);
        }
      }
    }

    self.waveforms.write().unwrap().clear();
  }

  pub fn get_waveform(
    &self,
    ctx: &Context,
    project_file: &File,
    event: &Event,
  ) -> Option<Arc<WaveformCache>> {
    if let Some(state) = self.waveforms.read().ok()?.get(&event.id) {
      return match state {
        WaveformState::Loading | WaveformState::Failed => None,
        WaveformState::Loaded(cache) => Some(cache.clone()),
      };
    }

    let cache_path = Cache::get_file_path(event.id, "tar").ok()?;
    if std::fs::exists(&cache_path).unwrap_or(false) {
      self.load_waveform(ctx, event.id, None);
      return None;
    }

    let resolver = KsngAttachmentResolver {};

    let audio_path = event.value.as_ref().and_then(|v| match v {
      EventValue::AudioClip { file, .. } => match &file.source {
        AudioFileSource::Path(path_buf) => Some(path_buf.clone()),
        AudioFileSource::Attachment(id) => {
          let attachment = project_file.attachments.iter().find(|a| a.id == *id)?;
          match &attachment.source {
            AttachmentSource::File(path_buf) => Some(path_buf.clone()),
            AttachmentSource::Managed => match resolver.read(attachment) {
              AttachmentReader::Path(path_buf) => Some(path_buf),
              AttachmentReader::Stream(_read) => todo!(),
            },
          }
        }
      },
      _ => None,
    });

    if let Some(path) = audio_path {
      self.load_waveform(ctx, event.id, Some(path));
    } else {
      error!("Failed to load audio file ID {}, no path found", event.id);
      self
        .waveforms
        .write()
        .ok()?
        .insert(event.id, WaveformState::Failed);
    }
    None
  }

  /// Opens the waveform archive for `id` on a background thread, loading it
  /// from `audio_path` if supplied and no cache present.
  fn load_waveform(&self, ctx: &Context, id: Uuid, audio_path: Option<PathBuf>) {
    if let Ok(mut waveforms) = self.waveforms.write() {
      waveforms.insert(id, WaveformState::Loading);
    }
    let waveforms = self.waveforms.clone();
    let ctx = ctx.clone();
    thread::spawn(move || {
      let cache = Logger::wrap(Self::load_waveform_impl(id, audio_path));
      if let Ok(mut waveforms) = waveforms.write() {
        let state = match cache {
          Some(cache) => WaveformState::Loaded(Arc::new(cache)),
          None => WaveformState::Failed,
        };
        waveforms.insert(id, state);
      }
      ctx.request_repaint();
    });
  }

  fn load_waveform_impl(id: Uuid, audio_path: Option<PathBuf>) -> Result<WaveformCache, UiError> {
    let cache_file = Cache::get_file_path(id, "tar")?;
    if let Some(audio_path) = audio_path {
      let generator = WaveformGenerator {
        height: timeline::TRACK_HEIGHT as usize,
        mip_levels: 8,
      };
      let buffer = generator.create_waveform(audio_path.as_ref())?;
      buffer.save_to_archive(&cache_file)?;
    }
    WaveformCache::open(id, &cache_file)
  }
}
