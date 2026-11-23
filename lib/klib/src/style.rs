use klib_macros::EditableConfig;
use serde::{Deserialize, Serialize};

use crate::fonts::Font;

/// A 32-bit RGBA color.
#[derive(Serialize, Deserialize, Copy, Clone, PartialEq, Debug)]
pub struct Color32(u32);

impl Color32 {
  pub fn from_rgb(r: u8, g: u8, b: u8) -> Color32 {
    Self::from_rgba(r, g, b, 0xff)
  }

  pub fn from_rgba(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    Color32((r as u32) << 24 | (g as u32) << 16 | (b as u32) << 8 | (a as u32))
  }

  pub fn from_rgba_f32(r: f32, g: f32, b: f32, a: f32) -> Color32 {
    Self::from_rgba(
      (r * 255.0) as u8,
      (g * 255.0) as u8,
      (b * 255.0) as u8,
      (a * 255.0) as u8,
    )
  }

  pub fn to_bytes(self) -> [u8; 4] {
    let r = self.0 >> 24 & 0xff;
    let g = self.0 >> 16 & 0xff;
    let b = self.0 >> 8 & 0xff;
    let a = self.0 & 0xff;
    [r as u8, g as u8, b as u8, a as u8]
  }

  pub fn to_floats(self) -> [f32; 4] {
    let r = self.0 >> 24 & 0xff;
    let g = self.0 >> 16 & 0xff;
    let b = self.0 >> 8 & 0xff;
    let a = self.0 & 0xff;
    [
      r as f32 / 0xff as f32,
      g as f32 / 0xff as f32,
      b as f32 / 0xff as f32,
      a as f32 / 0xff as f32,
    ]
  }

  pub fn from_floats(f: [f32; 4]) -> Color32 {
    Color32::from_rgba_f32(f[0], f[1], f[2], f[3])
  }
}

impl From<Color32> for skia_safe::Color4f {
  fn from(value: Color32) -> Self {
    let floats = value.to_floats();
    skia_safe::Color4f::new(floats[0], floats[1], floats[2], floats[3])
  }
}

#[derive(Serialize, Deserialize, Clone, EditableConfig, Debug)]
pub struct Colors {
  pub normal: Color32,
  pub highlight: Color32,
  pub background: Color32,
}

impl Default for Colors {
  fn default() -> Self {
    Self {
      normal: Color32::from_rgb(255, 255, 255),
      highlight: Color32::from_rgb(70, 175, 90),
      background: Color32::from_rgb(230, 230, 230),
    }
  }
}

#[derive(Serialize, Deserialize, Clone, EditableConfig, Debug)]
pub struct Stroke {
  pub color: Color32,
  #[float(0.0)]
  pub width: f32,
}

impl Default for Stroke {
  fn default() -> Self {
    Self {
      color: Color32::from_rgb(0, 0, 0),
      width: 3.0,
    }
  }
}

#[derive(Serialize, Deserialize, Clone, EditableConfig, Debug)]
pub struct LyricsTrackStyle {
  pub font: Font,
  pub colors: Colors,
  pub stroke: Stroke,
  pub line_height_multiplier: f32,
}

impl Default for LyricsTrackStyle {
  fn default() -> Self {
    Self {
      font: Default::default(),
      colors: Default::default(),
      stroke: Default::default(),
      line_height_multiplier: 1.0,
    }
  }
}
