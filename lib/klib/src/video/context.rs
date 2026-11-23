use crate::{style::LyricsTrackStyle, Rect};

#[derive(Clone)]
pub struct LyricsTrackContext<'a> {
  pub area: Rect,
  pub style: &'a LyricsTrackStyle,
}

impl<'a> LyricsTrackContext<'a> {
  pub fn new(area: Rect, style: &'a LyricsTrackStyle) -> Self {
    Self { area, style }
  }
}
