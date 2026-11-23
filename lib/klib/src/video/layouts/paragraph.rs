use std::collections::HashMap;

use harfbuzz_rs::{GlyphBuffer, UnicodeBuffer};
use klib_macros::EditableConfig;
use serde::{Deserialize, Serialize};
use skia_safe::{GlyphId, Matrix};

use crate::{
  fonts::FontManager,
  objects::{
    event::{Event, EventType},
    track::{Track, TrackType},
  },
  util::{rect::RectBuilder, skfont_to_harfbuzz_font},
  video::{
    context::LyricsTrackContext,
    elements::{paragraph::ParagraphVideoElement, text::TextVideoElement, VideoElement},
  },
  Point, Rect,
};

/// How video elements are combined together to form paragraphs.
#[derive(Serialize, Deserialize, Clone, Copy, EditableConfig, Default, Debug)]
pub enum ParagraphMergerMode {
  /// Each lyric is its own element and is handled independently.
  None,
  /// Each line is its own element.
  Line,
  /// Each `n` lines form one element.
  MultiLine(usize),
  /// Each page of lines form one element.
  #[default]
  Page,
}

pub struct ParagraphLayout {
  merger_mode: ParagraphMergerMode,
}

impl ParagraphLayout {
  pub fn new(merger_mode: ParagraphMergerMode) -> ParagraphLayout {
    ParagraphLayout { merger_mode }
  }

  pub fn layout_track<'a>(
    &self,
    context: &mut LyricsTrackContext<'a>,
    track: &Track,
  ) -> Vec<Box<dyn VideoElement>> {
    if track.track_type != TrackType::Lyrics {
      return vec![];
    }

    let Some(font) = FontManager::current().skfont(&context.style.font) else {
      log::warn!(
        "Couldn't create font {:?}, skipping layout",
        context.style.font
      );
      return vec![];
    };

    let hbfont = skfont_to_harfbuzz_font(&font);

    let events: Vec<&Event> = track.events.iter().collect();
    let mut elements: Vec<Box<dyn VideoElement>> = Vec::new();

    let mut next_index = 0;
    while next_index < events.len() {
      let mut page_elements = Vec::new();
      next_index = Self::fill_paragraph(
        context,
        &font,
        &hbfont,
        &events,
        next_index,
        &mut page_elements,
      );

      // Centrist
      let mut page_rect_builder = RectBuilder::default();
      let mut line_extents: HashMap<usize, (f32, f32)> = HashMap::new();
      for (line, elem) in &page_elements {
        let rect = elem.bounds();
        page_rect_builder.add_rect(rect);
        line_extents
          .entry(*line)
          .and_modify(|(x0, x1)| {
            *x0 = x0.min(rect.x0);
            *x1 = x1.max(rect.x1);
          })
          .or_insert((rect.x0, rect.x1));
      }

      let page_rect = page_rect_builder.to_rect().unwrap_or_default();
      let area_center = context.area.center();
      let dy = area_center.y - page_rect.center().y;
      for (line, e) in &mut page_elements {
        let Some((x0, x1)) = line_extents.get(line) else {
          continue;
        };

        e.set_transform(Matrix::translate(Point {
          x: area_center.x - (x0 + x1) / 2.0,
          y: dy,
        }));
      }

      match self.merger_mode {
        ParagraphMergerMode::None => {
          elements.extend(page_elements.into_iter().map(|(_, e)| e));
        }
        ParagraphMergerMode::Line => {
          let mut line_index = 0;
          let mut line_elements = Vec::new();
          for (line, elem) in page_elements {
            if line_index != line {
              if !line_elements.is_empty() {
                elements.push(Box::new(ParagraphVideoElement::from_elements(
                  line_elements.into_iter(),
                )));
                line_elements = Vec::new();
              }

              line_index = line;
            }

            line_elements.push(elem);
          }

          if !line_elements.is_empty() {
            elements.push(Box::new(ParagraphVideoElement::from_elements(
              line_elements.into_iter(),
            )));
          }
        }
        ParagraphMergerMode::MultiLine(n) => {
          let mut num_lines_pending = 0;
          let mut line_index = 0;
          let mut line_elements = Vec::new();
          for (line, elem) in page_elements {
            if line_index != line {
              num_lines_pending += 1;
              if num_lines_pending >= n {
                elements.push(Box::new(ParagraphVideoElement::from_elements(
                  line_elements.into_iter(),
                )));

                line_elements = Vec::new();
                num_lines_pending = 0;
              }

              line_index = line;
            }

            line_elements.push(elem);
          }

          if !line_elements.is_empty() {
            elements.push(Box::new(ParagraphVideoElement::from_elements(
              line_elements.into_iter(),
            )));
          }
        }
        ParagraphMergerMode::Page => {
          elements.push(Box::new(ParagraphVideoElement::from_elements(
            page_elements.into_iter().map(|(_, e)| e),
          )));
        }
      }
    }

    elements
  }

  fn fill_paragraph<'a>(
    context: &mut LyricsTrackContext<'a>,
    font: &skia_safe::Font,
    hbfont: &harfbuzz_rs::Owned<harfbuzz_rs::Font<'_>>,
    events: &[&Event],
    start_index: usize,
    out_elements: &mut Vec<(usize, Box<dyn VideoElement>)>,
  ) -> usize {
    let rect = context.area;

    let (space_width, _) = font.measure_str(" ", None);

    let upem = hbfont.face().upem() as f64;
    let size = font.size() as f64;

    let (_, metrics) = font.metrics();
    let line_box_height = metrics.descent - metrics.ascent;
    let line_pitch = font.spacing() * context.style.line_height_multiplier;

    let mut char_positions = Vec::new();
    let mut glyphs = Vec::new();

    let mut idx = start_index;
    let mut line_num = 0;
    let mut first_on_line = true;
    let mut next_x = rect.x0;
    let mut line_y = rect.y0;
    let mut next_y = rect.y0;
    while idx < events.len() {
      let event = events[idx];
      idx += 1;

      match event.event_type {
        EventType::Lyric if event.text().is_some() => {
          let text = event.text().unwrap();
          if text.is_empty() {
            continue;
          }
          let mut text_buffer = UnicodeBuffer::new().add_str(text);
          let mut shape = harfbuzz_rs::shape(hbfont, text_buffer, &[]);
          let layout_rect = Self::compute_rect_from_glyphs(font, hbfont, &shape);

          let width = layout_rect.width();
          let x = if first_on_line || event.linked_id.is_some() {
            next_x
          } else {
            next_x + space_width
          };

          if (x + width) > rect.x1 {
            // Too large, new line
            next_x = rect.x0;
            next_y = line_y;
            first_on_line = false;
            line_num += 1;
          } else {
            next_x = x;
            first_on_line = false;
          }

          if (next_y + line_box_height) > rect.y1 && !out_elements.is_empty() {
            // Paragraph done
            idx -= 1;
            break;
          }

          // Check if the next event is linked and will overflow, and if so
          // hyphenate this one. This is pretty inefficient but should
          // be fine since it's happening once per layout gen...
          // right?
          if idx < events.len() && events[idx].linked_id.is_some() {
            let next_text = events[idx].text().unwrap();
            let next_text_buffer = UnicodeBuffer::new().add_str(next_text);
            let next_shape = harfbuzz_rs::shape(hbfont, next_text_buffer, &[]);
            let next_layout_rect = Self::compute_rect_from_glyphs(font, hbfont, &next_shape);

            if next_x + width + next_layout_rect.width() > rect.x1 {
              let new_text = text.to_string() + "-";
              text_buffer = UnicodeBuffer::new().add_str(&new_text);
              shape = harfbuzz_rs::shape(hbfont, text_buffer, &[]);
            }
          }

          line_y = line_y.max(next_y + line_pitch);

          char_positions.clear();
          glyphs.clear();

          let mut glyph_x = 0.0;
          let mut glyph_y = 0.0_f64;
          for (pos, info) in shape
            .get_glyph_positions()
            .iter()
            .zip(shape.get_glyph_infos())
          {
            let x_advance = (pos.x_advance as f64 * size) / upem;
            let x_offset = (pos.x_offset as f64 * size) / upem;
            let y_offset = (pos.y_offset as f64 * size) / upem;
            glyphs.push(info.codepoint as GlyphId);
            char_positions.push(skia_safe::Point {
              x: (glyph_x + x_offset) as f32,
              y: y_offset as f32,
            });

            glyph_x += x_advance;
            if let Some(extents) = hbfont.get_glyph_extents(info.codepoint) {
              let h = (extents.height as f64 * size) / upem;
              glyph_y = glyph_y.max(h.abs());
            }
          }

          let Some(blob) =
            skia_safe::TextBlob::from_pos_text(glyphs.as_slice(), &char_positions, font)
          else {
            log::warn!("failed to create text blob for string {text}");
            return events.len();
          };

          let ofs = blob.bounds().height() - glyph_y as f32;

          let computed_rect = Rect {
            x0: blob.bounds().left,
            y0: blob.bounds().top + ofs,
            x1: blob.bounds().left + glyph_x as f32,
            y1: blob.bounds().bottom,
          };

          out_elements.push((
            line_num,
            TextVideoElement::from_event(
              event,
              Point {
                x: next_x,
                y: next_y,
              },
              text,
              blob,
              computed_rect,
              font,
              context.style,
            ),
          ));

          next_x += width;
        }
        EventType::LineBreak => {
          // Forced new line
          next_x = rect.x0;
          next_y = line_y;
          first_on_line = true;
          line_num += 1;
        }
        EventType::ParagraphBreak => break,
        _ => continue,
      }
    }

    idx
  }

  fn compute_rect_from_glyphs(
    skfont: &skia_safe::Font,
    hbfont: &harfbuzz_rs::Font,
    buffer: &GlyphBuffer,
  ) -> Rect {
    let upem = hbfont.face().upem() as f64;
    let size = skfont.size() as f64;

    let mut builder = RectBuilder::default();
    let mut x = 0.0;
    for (pos, info) in buffer
      .get_glyph_positions()
      .iter()
      .zip(buffer.get_glyph_infos())
    {
      let glyph_v = hbfont.get_glyph_v_advance(info.codepoint);
      let x_advance = (pos.x_advance as f64 * size) / upem;
      let y_advance = (glyph_v as f64 * size) / upem;
      let x_offset = (pos.x_offset as f64 * size) / upem;
      let y_offset = (pos.y_offset as f64 * size) / upem;

      builder.add_point(Point {
        x: (x + x_offset) as f32,
        y: y_offset as f32,
      });
      builder.add_point(Point {
        x: (x + x_advance) as f32,
        y: y_advance as f32,
      });

      x += x_advance;
    }

    builder.to_rect().unwrap_or_default()
  }
}

#[cfg(test)]
mod tests {
  use std::{sync::mpsc, time::Duration};

  use super::{ParagraphLayout, ParagraphMergerMode};
  use crate::{
    fonts::DEFAULT_FONT_FAMILY,
    objects::{event::Event, track::Track},
    style::LyricsTrackStyle,
    timecode::Timecode,
    video::context::LyricsTrackContext,
    Rect,
  };

  const HANG_TIMEOUT: Duration = Duration::from_secs(10);

  /// Lays out a lyrics track with one lyric per word in `area`. Panics if
  /// layout doesn't finish in `HANG_TIMEOUT`.
  fn layout_words(area: Rect, font_size: f32, words: &'static [&'static str]) -> usize {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
      let mut style = LyricsTrackStyle::default();
      style.font.family = DEFAULT_FONT_FAMILY.to_string();
      style.font.size = font_size;

      let mut track = Track::new_lyrics(0);
      for (i, word) in words.iter().enumerate() {
        let start = Timecode(i as u32 * 1000);
        let end = Timecode((i as u32 + 1) * 1000);
        track
          .events
          .insert(Event::new_lyric(start, end, word.to_string()));
      }

      let mut context = LyricsTrackContext::new(area, &style);
      let layout = ParagraphLayout::new(ParagraphMergerMode::Page);
      let _ = tx.send(layout.layout_track(&mut context, &track).len());
    });

    match rx.recv_timeout(HANG_TIMEOUT) {
      Ok(n) => n,
      Err(mpsc::RecvTimeoutError::Timeout) => {
        panic!("layout_track did not finish within {HANG_TIMEOUT:?} (hang)")
      }
      Err(mpsc::RecvTimeoutError::Disconnected) => panic!("layout_track panicked"),
    }
  }

  // Control test
  #[test]
  fn layout_fits_in_large_area() {
    let area = Rect {
      x0: 0.0,
      y0: 0.0,
      x1: 1920.0,
      y1: 1080.0,
    };
    let n = layout_words(area, 100.0, &["hello", "world"]);
    assert!(n > 0, "expected at least one paragraph element");
  }

  // Test for bug where layout hangs if no text can be laid out within the area.
  #[test]
  fn layout_terminates_when_line_taller_than_area() {
    let area = Rect {
      x0: 0.0,
      y0: 0.0,
      x1: 1920.0,
      y1: 20.0,
    };
    layout_words(area, 100.0, &["hello", "world"]);
  }
}
