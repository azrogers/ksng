use skia_safe::Matrix;
use uuid::Uuid;

use crate::{
  objects::event::Event,
  style::LyricsTrackStyle,
  timecode::Timecode,
  video::elements::{VideoElement, VideoElementRenderContext},
  Point, Rect,
};

struct TextMetrics {
  ascent: f32,
  descent: f32,
}

pub struct TextVideoElement {
  id: Uuid,
  mat: Matrix,
  start: Timecode,
  end: Timecode,
  pos: Point,
  rect: Rect,
  #[allow(dead_code)]
  text: String,
  metrics: TextMetrics,
  blob: skia_safe::TextBlob,
  normal_paint: skia_safe::Paint,
  highlight_paint: skia_safe::Paint,
}

impl TextVideoElement {
  pub fn from_event(
    event: &Event,
    pos: Point,
    text: &str,
    blob: skia_safe::TextBlob,
    rect: Rect,
    font: &skia_safe::Font,
    style: &LyricsTrackStyle,
  ) -> Box<dyn VideoElement> {
    let normal_color: skia_safe::Color4f = style.colors.normal.into();
    let highlight_color: skia_safe::Color4f = style.colors.highlight.into();
    let (_, metrics) = font.metrics();
    Box::new(TextVideoElement {
      id: event.id,
      mat: Matrix::new_identity(),
      start: event.start_timecode,
      end: event.end_timecode,
      pos,
      rect,
      text: text.to_string(),
      metrics: TextMetrics {
        ascent: metrics.ascent,
        descent: metrics.descent,
      },
      blob,
      normal_paint: skia_safe::Paint::new(normal_color, None),
      highlight_paint: skia_safe::Paint::new(highlight_color, None),
    })
  }
}

impl TextVideoElement {
  fn baseline_y(&self) -> f32 {
    self.pos.y - self.metrics.ascent
  }
}

impl VideoElement for TextVideoElement {
  fn id(&self) -> Uuid {
    self.id
  }

  fn start_time(&self) -> Timecode {
    self.start
  }

  fn end_time(&self) -> Timecode {
    self.end
  }

  fn bounds(&self) -> Rect {
    let baseline = self.baseline_y();
    Rect {
      x0: self.pos.x,
      y0: baseline + self.metrics.ascent,
      x1: self.pos.x + self.rect.width(),
      y1: baseline + self.metrics.descent,
    }
  }

  fn transform(&self) -> Matrix {
    self.mat
  }

  fn set_transform(&mut self, mat: Matrix) {
    self.mat = mat;
  }

  fn render<'canvas>(&self, context: &mut VideoElementRenderContext<'canvas>) {
    let canvas = context.canvas;
    let time = context.time;
    canvas.save();
    canvas.concat(&self.mat);
    let normalized_pos =
      ((time - self.start).0 as f32 / (self.end - self.start).0 as f32).clamp(0.0, 1.0);
    let draw_normal = normalized_pos < 1.0;
    let draw_highlight = normalized_pos > 0.0;

    let bounds = self.bounds();

    let pos = Point {
      x: bounds.x0,
      y: self.baseline_y(),
    };

    let pad = bounds.height();
    let clip_top = bounds.y0 - pad;
    let clip_bottom = bounds.y1 + pad;

    if draw_normal && draw_highlight {
      canvas.save();
      canvas.clip_rect(
        skia_safe::Rect {
          left: bounds.x0 + bounds.width() * normalized_pos,
          top: clip_top,
          right: bounds.x1,
          bottom: clip_bottom,
        },
        None,
        true,
      );

      canvas.draw_text_blob(&self.blob, pos, &self.normal_paint);

      canvas.restore();
      canvas.save();

      canvas.clip_rect(
        skia_safe::Rect {
          left: bounds.x0,
          top: clip_top,
          right: bounds.x0 + bounds.width() * normalized_pos,
          bottom: clip_bottom,
        },
        None,
        true,
      );

      canvas.draw_text_blob(self.blob.clone(), pos, &self.highlight_paint);

      canvas.restore();
    } else if draw_normal {
      canvas.draw_text_blob(self.blob.clone(), pos, &self.normal_paint);
    } else if draw_highlight {
      canvas.draw_text_blob(self.blob.clone(), pos, &self.highlight_paint);
    }

    canvas.restore();
  }
}
