use klib_macros::EditableConfig;
use serde::{Deserialize, Serialize};

use crate::{
  timecode::Timecode,
  util::easing::EasingFunction,
  video::{
    elements::{VideoElement, VideoElementRenderContext},
    vacancy::VacancyChecker,
  },
};

#[derive(Clone, Serialize, Deserialize, EditableConfig, Debug)]
pub struct SlideTransitionConfig {
  /// The maximum amount of time an event will be displayed for before it
  /// happens.
  pub lead_time: Timecode,
  /// The maximum amount of time an event will be displayed for after it
  /// happens.
  pub trail_time: Timecode,
  /// The maximum amount of time that the slide transition itself will play at
  /// the start and end.
  pub transition_time: Timecode,
  /// The easing function used for the slide in.
  pub easing_in: EasingFunction,
  /// The easing function used for the slide out.
  pub easing_out: EasingFunction,
}

impl Default for SlideTransitionConfig {
  fn default() -> Self {
    Self {
      lead_time: Timecode(2000),
      trail_time: Timecode(3000),
      transition_time: Timecode(500),
      easing_in: EasingFunction::QuadIn,
      easing_out: EasingFunction::QuadOut,
    }
  }
}

pub struct SlideTransitionElement {
  element: Box<dyn VideoElement>,
  start_time: Timecode,
  end_time: Timecode,
  transition_time: Timecode,
  easing_in: EasingFunction,
  easing_out: EasingFunction,
}

impl SlideTransitionElement {
  pub fn wrap_elements(
    config: &SlideTransitionConfig,
    elements: Vec<Box<dyn VideoElement>>,
  ) -> Vec<Box<dyn VideoElement>> {
    let vacancy = VacancyChecker::new(&elements);
    elements
      .into_iter()
      .map(|elem| {
        let (start_time, end_time) =
          vacancy.calc_start_end_midpoints(elem.as_ref(), config.lead_time, config.trail_time);

        Box::new(SlideTransitionElement {
          start_time,
          end_time,
          transition_time: config.transition_time,
          element: elem,
          easing_in: config.easing_in,
          easing_out: config.easing_out,
        }) as Box<dyn VideoElement>
      })
      .collect()
  }
}

impl VideoElement for SlideTransitionElement {
  fn id(&self) -> uuid::Uuid {
    self.element.id()
  }

  fn start_time(&self) -> Timecode {
    self.start_time
  }

  fn end_time(&self) -> Timecode {
    self.end_time
  }

  fn bounds(&self) -> crate::Rect {
    self.element.bounds()
  }

  fn transform(&self) -> skia_safe::Matrix {
    self.element.transform()
  }

  fn set_transform(&mut self, mat: skia_safe::Matrix) {
    self.element.set_transform(mat);
  }

  fn render<'canvas>(&self, context: &mut VideoElementRenderContext<'canvas>) {
    let p = context.time;
    let ts = self.element.start_time();
    let te = self.element.end_time();
    let tl = self.start_time;
    let tt = self.end_time;
    if p < tl || p >= tt {
      return;
    }

    let slide_in = self.transition_time.min(ts - tl);
    let slide_out = self.transition_time.min(tt - te);

    let bounds: skia_safe::Rect = self.element.bounds().into();
    let (bounds, _) = self.element.transform().map_rect(bounds);
    let size = context.canvas.base_layer_size();
    let clip = |left: f32, right: f32| skia_safe::Rect {
      left,
      top: 0.0,
      right,
      bottom: size.height as f32,
    };

    let clip_rect = if p < tl + slide_in {
      let t = self
        .easing_in
        .evaluate((p - tl).to_seconds() / slide_in.to_seconds());
      Some(clip(bounds.left, bounds.left + bounds.width() * t))
    } else if p >= tt - slide_out {
      let t = self
        .easing_out
        .evaluate((p - (tt - slide_out)).to_seconds() / slide_out.to_seconds());
      Some(clip(bounds.left + bounds.width() * t, bounds.right))
    } else {
      None
    };

    context.canvas.save();
    if let Some(clip_rect) = clip_rect {
      context
        .canvas
        .clip_rect(clip_rect, skia_safe::ClipOp::Intersect, Some(true));
    }

    self.element.render(context);

    context.canvas.restore();
  }
}
