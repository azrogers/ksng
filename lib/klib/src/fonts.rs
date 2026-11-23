//! Font management

use std::{collections::BTreeSet, rc::Rc};

use serde::{Deserialize, Serialize};
use skia_safe::{
  font_arguments::{variation_position::Coordinate, VariationPosition},
  font_style::Slant,
  FontArguments, FourByteTag, Typeface,
};

pub const OPEN_SANS_REGULAR: &[u8] = include_bytes!(concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/assets/fonts/OpenSans-VariableFont_wdth,wght.ttf"
));
pub const OPEN_SANS_ITALIC: &[u8] = include_bytes!(concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/assets/fonts/OpenSans-Italic-VariableFont_wdth,wght.ttf"
));

/// We have to copy all the parley font structs so they can be serde'd

#[derive(Serialize, Deserialize, Copy, Clone, PartialEq, Default, Debug)]
pub enum FontStyle {
  /// An upright or "roman" style.
  #[default]
  Normal,
  /// Generally a slanted style, originally based on semi-cursive forms.
  /// This often has a different structure from the normal style.
  Italic,
  Oblique,
}

impl From<FontStyle> for skia_safe::font_style::Slant {
  fn from(value: FontStyle) -> Self {
    match value {
      FontStyle::Normal => skia_safe::font_style::Slant::Upright,
      FontStyle::Italic => skia_safe::font_style::Slant::Italic,
      FontStyle::Oblique => skia_safe::font_style::Slant::Oblique,
    }
  }
}

/// Visual weight class of a font, typically on a scale from 1 to 1000.
#[derive(Serialize, Deserialize, Copy, Clone, PartialEq, PartialOrd, Debug)]
pub struct FontWeight(pub i32);

impl From<FontWeight> for skia_safe::font_style::Weight {
  fn from(value: FontWeight) -> Self {
    skia_safe::font_style::Weight::from(value.0)
  }
}

/// Visual width of a font
#[derive(Serialize, Deserialize, Copy, Clone, PartialEq, PartialOrd, Debug)]
pub struct FontWidth(pub i32);

impl From<FontWidth> for skia_safe::font_style::Width {
  fn from(value: FontWidth) -> Self {
    skia_safe::font_style::Width::from(value.0)
  }
}

/// Family used when a requested family can't be found. It's bundled into the
/// binary, so it's always available.
pub const DEFAULT_FONT_FAMILY: &str = "Open Sans";

/// A font with configuration options.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Font {
  pub style: FontStyle,
  pub weight: FontWeight,
  pub width: FontWidth,
  pub family: String,
  pub size: f32,
}

impl Font {
  fn sk_style(&self) -> skia_safe::FontStyle {
    skia_safe::FontStyle::new(self.weight.into(), self.width.into(), self.style.into())
  }
}

impl Default for Font {
  fn default() -> Self {
    Self {
      style: FontStyle::Normal,
      weight: FontWeight(700),
      width: FontWidth(5),
      family: DEFAULT_FONT_FAMILY.to_string(),
      size: 100.0,
    }
  }
}

#[derive(Debug, Default, PartialEq)]
pub struct FontInfo {
  pub widths: Vec<i32>,
  pub weights: Vec<i32>,
}

// Tags for variable-width typeface axes
const WGHT: FourByteTag = FourByteTag::from_chars('w', 'g', 'h', 't');
const WDTH: FourByteTag = FourByteTag::from_chars('w', 'd', 't', 'h');

/// CSS weights for variable-width fonts
const STANDARD_WEIGHTS: [i32; 9] = [100, 200, 300, 400, 500, 600, 700, 800, 900];

/// Converts a width class (1-9) to a `wdth` axis value (percent of normal).
fn width_class_to_percent(class: i32) -> f32 {
  match class {
    ..=1 => 50.0,
    2 => 62.5,
    3 => 75.0,
    4 => 87.5,
    5 => 100.0,
    6 => 112.5,
    7 => 125.0,
    8 => 150.0,
    _ => 200.0,
  }
}

/// If the font is a variable-width font, applies its weight and width to the
/// typeface via the `wght` and `wdth` axes.
fn apply_variations(typeface: Typeface, font: &Font) -> Option<Typeface> {
  let Some(axes) = typeface
    .variation_design_parameters()
    .filter(|axes| !axes.is_empty())
  else {
    return Some(typeface);
  };

  let coordinates: Vec<Coordinate> = axes
    .iter()
    .filter_map(|axis| {
      let value = if axis.tag == WGHT {
        font.weight.0 as f32
      } else if axis.tag == WDTH {
        width_class_to_percent(font.width.0)
      } else {
        return None;
      };
      Some(Coordinate {
        axis: axis.tag,
        value: value.clamp(axis.min, axis.max),
      })
    })
    .collect();

  let args = FontArguments::new().set_variation_design_position(VariationPosition {
    coordinates: &coordinates,
  });
  typeface.clone_with_arguments(&args)
}

/// Adds the weights and widths `typeface` can produce to the given sets.
fn collect_typeface_info(
  typeface: &Typeface,
  weights: &mut BTreeSet<i32>,
  widths: &mut BTreeSet<i32>,
) {
  let axes = typeface.variation_design_parameters().unwrap_or_default();
  let style = typeface.font_style();

  match axes.iter().find(|a| a.tag == WGHT) {
    Some(axis) => weights.extend(
      STANDARD_WEIGHTS
        .into_iter()
        .filter(|w| (axis.min..=axis.max).contains(&(*w as f32))),
    ),
    None => {
      // No variable weight, use the style's weight instead.
      weights.insert(*style.weight());
    }
  }

  match axes.iter().find(|a| a.tag == WDTH) {
    Some(axis) => widths.extend(
      (1..=9).filter(|class| (axis.min..=axis.max).contains(&width_class_to_percent(*class))),
    ),
    None => {
      // No variable width, use the style's width instead.
      widths.insert(*style.width());
    }
  }
}

/// A typeface embedded in the binary.
struct BundledFace {
  family: String,
  typeface: Typeface,
}

/// Contains system fonts and bundled fonts. Access via the thread-local
/// `current()`
pub struct FontManager {
  system: skia_safe::FontMgr,
  bundled: Vec<BundledFace>,
  family_names: Vec<String>,
}

thread_local! {
  static INSTANCE: Rc<FontManager> = Rc::new(FontManager::new());
}

impl FontManager {
  /// The shared font manager for this thread.
  pub fn current() -> Rc<FontManager> {
    INSTANCE.with(Rc::clone)
  }

  fn new() -> Self {
    let system = skia_safe::FontMgr::new();
    let bundled: Vec<BundledFace> = [OPEN_SANS_REGULAR, OPEN_SANS_ITALIC]
      .into_iter()
      .map(|bytes| BundledFace {
        family: DEFAULT_FONT_FAMILY.to_string(),
        typeface: system
          .new_from_data(bytes, None)
          .expect("Bundled font failed to parse"),
      })
      .collect();

    let family_names: Vec<String> = system
      .family_names()
      .chain(bundled.iter().map(|face| face.family.clone()))
      .collect::<BTreeSet<String>>()
      .into_iter()
      .collect();

    Self {
      system,
      bundled,
      family_names,
    }
  }

  /// All available family names, sorted.
  pub fn font_names(&self) -> &[String] {
    self.family_names.as_slice()
  }

  /// The weights and widths available for a family.
  pub fn font_info(&self, family: &str) -> FontInfo {
    let mut widths = BTreeSet::new();
    let mut weights = BTreeSet::new();

    let mut bundled = self.bundled_faces(family).peekable();
    if bundled.peek().is_some() {
      for typeface in bundled {
        collect_typeface_info(typeface, &mut weights, &mut widths);
      }
    } else {
      let mut style_set = self.system.match_family(family);
      for i in 0..style_set.count() {
        let (style, _) = style_set.style(i);
        widths.insert(*style.width());
        weights.insert(*style.weight());
      }
    }

    FontInfo {
      widths: widths.into_iter().collect(),
      weights: weights.into_iter().collect(),
    }
  }

  /// Finds the typeface for `font`, without falling back to another family.
  pub fn typeface(&self, font: &Font) -> Option<Typeface> {
    // match the requested slant
    let want_upright = font.style == FontStyle::Normal;
    let mut chosen = None;
    for typeface in self.bundled_faces(&font.family) {
      let is_upright = typeface.font_style().slant() == Slant::Upright;
      if is_upright == want_upright {
        chosen = Some(typeface);
        break;
      }
      chosen.get_or_insert(typeface);
    }

    // if we found the right slant, use it and apply the variations. Otherwise
    // just use whatever the system has for that family.
    match chosen {
      Some(typeface) => apply_variations(typeface.clone(), font),
      None => self
        .system
        .match_family_style(&font.family, font.sk_style()),
    }
  }

  /// Creates a Skia font for `font`. Falls back to `DEFAULT_FONT_FAMILY`.
  pub fn skfont(&self, font: &Font) -> Option<skia_safe::Font> {
    let typeface = self.typeface(font).or_else(|| {
      log::warn!(
        "Font family {:?} not found, falling back to {DEFAULT_FONT_FAMILY}",
        font.family
      );
      self.typeface(&Font {
        family: DEFAULT_FONT_FAMILY.to_string(),
        ..font.clone()
      })
    })?;
    Some(skia_safe::Font::new(typeface, Some(font.size)))
  }

  /// The bundled typefaces in `family`.
  fn bundled_faces<'a>(&'a self, family: &'a str) -> impl Iterator<Item = &'a Typeface> {
    self
      .bundled
      .iter()
      .filter(move |face| face.family.eq_ignore_ascii_case(family))
      .map(|face| &face.typeface)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn open_sans(weight: i32, width: i32, style: FontStyle) -> Font {
    Font {
      style,
      weight: FontWeight(weight),
      width: FontWidth(width),
      family: DEFAULT_FONT_FAMILY.to_string(),
      size: 100.0,
    }
  }

  fn axis_value(typeface: &Typeface, tag: FourByteTag) -> Option<f32> {
    typeface
      .variation_design_position()?
      .into_iter()
      .find(|c| c.axis == tag)
      .map(|c| c.value)
  }

  fn text_width(fonts: &FontManager, font: &Font) -> f32 {
    fonts
      .skfont(font)
      .unwrap()
      .measure_str("Hello world", None)
      .0
  }

  #[test]
  fn bundled_family_is_listed() {
    let fonts = FontManager::new();
    assert!(fonts.font_names().iter().any(|n| n == DEFAULT_FONT_FAMILY));
  }

  #[test]
  fn bundled_info_comes_from_axes() {
    // Open Sans: wght 300-800, wdth 75-100
    let info = FontManager::new().font_info(DEFAULT_FONT_FAMILY);
    assert_eq!(
      info,
      FontInfo {
        weights: vec![300, 400, 500, 600, 700, 800],
        widths: vec![3, 4, 5],
      }
    );
  }

  #[test]
  fn variable_weight_and_width_are_applied() {
    let fonts = FontManager::new();
    let typeface = fonts
      .typeface(&open_sans(700, 3, FontStyle::Normal))
      .unwrap();
    assert_eq!(axis_value(&typeface, WGHT), Some(700.0));
    assert_eq!(axis_value(&typeface, WDTH), Some(75.0));

    let regular = text_width(&fonts, &open_sans(400, 5, FontStyle::Normal));
    let bold = text_width(&fonts, &open_sans(800, 5, FontStyle::Normal));
    let narrow = text_width(&fonts, &open_sans(400, 3, FontStyle::Normal));
    assert!(bold > regular, "bold {bold} <= regular {regular}");
    assert!(narrow < regular, "narrow {narrow} >= regular {regular}");
  }

  #[test]
  fn out_of_range_values_are_clamped() {
    let fonts = FontManager::new();
    let typeface = fonts
      .typeface(&open_sans(900, 9, FontStyle::Normal))
      .unwrap();
    assert_eq!(axis_value(&typeface, WGHT), Some(800.0));
    assert_eq!(axis_value(&typeface, WDTH), Some(100.0));
  }

  #[test]
  fn italic_uses_italic_face() {
    let fonts = FontManager::new();
    let upright = fonts
      .typeface(&open_sans(400, 5, FontStyle::Normal))
      .unwrap();
    let italic = fonts
      .typeface(&open_sans(400, 5, FontStyle::Italic))
      .unwrap();
    assert_eq!(upright.font_style().slant(), Slant::Upright);
    assert_eq!(italic.font_style().slant(), Slant::Italic);
  }

  #[test]
  fn missing_family_falls_back() {
    let fonts = FontManager::new();
    let font = Font {
      family: "This Font Does Not Exist".to_string(),
      ..open_sans(700, 5, FontStyle::Normal)
    };
    assert!(fonts.typeface(&font).is_none());
    assert_eq!(fonts.font_info(&font.family), FontInfo::default());

    let typeface = fonts.skfont(&font).unwrap().typeface();
    assert_eq!(typeface.family_name(), DEFAULT_FONT_FAMILY);
    assert_eq!(axis_value(&typeface, WGHT), Some(700.0));
  }
}
