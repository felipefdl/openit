//! The PDF text layer: `pdf-inspector`'s positioned runs grouped into lines, a
//! folded stream for search, and ranges for selection and copy.
//!
//! Every rectangle here is a [`RectPt`] in the item frame, so the reader maps
//! it through [`crate::pdf::PageGeometry::to_display`] before painting.

use std::path::Path;
use std::sync::OnceLock;

use pdf_inspector::types::ItemType;
use unicode_normalization::UnicodeNormalization as _;
use unicode_normalization::char::is_combining_mark;

use crate::error::Error;
use crate::pdf::RectPt;

/// One run of text with its box.
#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
  /// The characters the run paints.
  pub text: String,
  /// Where the run sits, in the item frame.
  pub rect: RectPt,
}

impl TextRun {
  /// How many characters the run holds.
  fn char_count(&self) -> usize {
    self.text.chars().count()
  }

  /// The box of the characters in `range`, split proportionally.
  fn slice_rect(&self, start: usize, end: usize) -> RectPt {
    let count = self.char_count();
    if count == 0 {
      return self.rect;
    }
    let per_char = self.rect.width / count_as_f32(count);
    let start = start.min(count);
    let end = end.clamp(start, count);
    RectPt {
      x: per_char.mul_add(count_as_f32(start), self.rect.x),
      y: self.rect.y,
      width: per_char * count_as_f32(end.saturating_sub(start)),
      height: self.rect.height,
    }
  }

  /// The characters in `range` as text.
  fn slice_text(&self, start: usize, end: usize) -> String {
    self.text.chars().skip(start).take(end.saturating_sub(start)).collect()
  }
}

/// Runs sharing one baseline, ordered left to right.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TextLine {
  /// The line's runs.
  pub runs: Vec<TextRun>,
}

impl TextLine {
  /// The line's own text, runs joined where the source leaves a gap.
  fn text(&self) -> String {
    let mut out = String::new();
    let mut previous: Option<&TextRun> = None;
    for run in &self.runs {
      if let Some(previous) = previous {
        let gap = run.rect.x - (previous.rect.x + previous.rect.width);
        let ends_open = previous.text.ends_with(' ') || run.text.starts_with(' ');
        if !ends_open && gap > 0.15 * previous.rect.height.max(1.0) {
          out.push(' ');
        }
      }
      out.push_str(&run.text);
      previous = Some(run);
    }
    out
  }

  /// The vertical band the line covers.
  fn band(&self) -> (f32, f32) {
    let bottom = self.runs.iter().map(|run| run.rect.y).fold(f32::MAX, f32::min);
    let top = self
      .runs
      .iter()
      .map(|run| run.rect.y + run.rect.height)
      .fold(f32::MIN, f32::max);
    (bottom, top)
  }
}

/// One page's lines, ordered top to bottom.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PageText {
  /// The page's lines.
  pub lines: Vec<TextLine>,
}

/// Every page's text.
#[derive(Debug, Clone, Default)]
pub struct TextLayer {
  /// One entry per page, in document order. A page without text is empty.
  pub pages: Vec<PageText>,
  /// Folded haystack, built once on first search.
  haystack: OnceLock<Vec<PageHaystack>>,
}

impl PartialEq for TextLayer {
  fn eq(&self, other: &Self) -> bool {
    self.pages == other.pages
  }
}

impl TextLayer {
  /// Whether no page holds any text.
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.pages.iter().all(|page| page.lines.is_empty())
  }

  fn run(&self, position: TextPos) -> Option<&TextRun> {
    self.pages.get(position.page)?.lines.get(position.line)?.runs.get(position.run)
  }

  fn line(&self, page: usize, line: usize) -> Option<&TextLine> {
    self.pages.get(page)?.lines.get(line)
  }

  fn haystack(&self) -> &[PageHaystack] {
    self.haystack.get_or_init(|| {
      self
        .pages
        .iter()
        .enumerate()
        .map(|(index, page)| page_haystack(index, page))
        .collect()
    })
  }
}

/// A character position inside the layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextPos {
  /// Zero-based page index.
  pub page: usize,
  /// Line index inside the page.
  pub line: usize,
  /// Run index inside the line.
  pub run: usize,
  /// Character index inside the run.
  pub ch: usize,
}

/// One search hit: `[start, end)` plus the text around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
  /// First character of the hit.
  pub start: TextPos,
  /// One past the last character of the hit.
  pub end: TextPos,
  /// The hit's line (or lines, when it spans a line break), for the results list.
  pub context: String,
}

/// Read every text run of the document.
///
/// An encrypted document is re-read from `path`. pdf-inspector 1.19.0 exposes
/// `extract_text_with_positions_mem` without a password and
/// `extract_text_with_positions_pages_with_password` only on a path; there is no
/// in-memory extract-with-password entry point.
///
/// # Errors
///
/// Returns [`Error::Pdf`] when the document cannot be read.
pub fn text_layer(path: &Path, bytes: &[u8], password: Option<&str>, page_count: usize) -> Result<TextLayer, Error> {
  let items = password
    .map_or_else(
      || pdf_inspector::extract_text_with_positions_mem(bytes),
      |password| pdf_inspector::extract_text_with_positions_pages_with_password(path, None, Some(password)),
    )
    .map_err(|error| Error::Pdf {
      reason: match error {
        pdf_inspector::PdfError::Encrypted => "This PDF is password protected".to_owned(),
        other => other.to_string(),
      },
    })?;

  let mut pages = vec![PageText::default(); page_count];
  let mut by_page: Vec<Vec<TextRun>> = vec![Vec::new(); page_count];
  for item in items {
    if !matches!(item.item_type, ItemType::Text) || item.text.trim().is_empty() {
      continue;
    }
    let index = usize::try_from(item.page).unwrap_or(usize::MAX).saturating_sub(1);
    let Some(slot) = by_page.get_mut(index) else {
      continue;
    };
    slot.push(TextRun {
      text: item.text,
      rect: RectPt {
        x: item.x,
        y: item.y,
        width: item.width.abs(),
        height: item.height.abs().max(1.0),
      },
    });
  }

  for (page, runs) in pages.iter_mut().zip(by_page) {
    page.lines = group_lines(runs);
  }
  Ok(TextLayer { pages, ..TextLayer::default() })
}

/// Group runs into lines by baseline, each line ordered left to right.
fn group_lines(mut runs: Vec<TextRun>) -> Vec<TextLine> {
  runs.sort_by(|a, b| b.rect.y.total_cmp(&a.rect.y).then_with(|| a.rect.x.total_cmp(&b.rect.x)));
  let mut lines: Vec<TextLine> = Vec::new();
  for run in runs {
    let joined = lines.last_mut().is_some_and(|line| {
      line.runs.first().is_some_and(|first| {
        let tolerance = 0.5 * first.rect.height.max(run.rect.height).max(1.0);
        (first.rect.y - run.rect.y).abs() <= tolerance
      })
    });
    if joined {
      if let Some(line) = lines.last_mut() {
        line.runs.push(run);
      }
    } else {
      lines.push(TextLine { runs: vec![run] });
    }
  }
  for line in &mut lines {
    line.runs.sort_by(|a, b| a.rect.x.total_cmp(&b.rect.x));
  }
  lines
}

/// Fold `text` for matching.
///
/// NFKD splits ligatures, combining marks drop out, everything lowercases, and
/// whitespace runs collapse to one space. Every folded character carries the
/// index of the source character it came from.
#[must_use]
pub fn fold(text: &str) -> Vec<(char, usize)> {
  let mut out: Vec<(char, usize)> = Vec::with_capacity(text.len());
  for (index, source) in text.chars().enumerate() {
    if source.is_whitespace() {
      if out.last().is_none_or(|(last, _)| *last != ' ') {
        out.push((' ', index));
      }
      continue;
    }
    for decomposed in source.nfkd() {
      if is_combining_mark(decomposed) {
        continue;
      }
      let lowered = decomposed.to_lowercase().next().unwrap_or(decomposed);
      out.push((lowered, index));
    }
  }
  out
}

/// Folded characters of one page and the source position of each.
#[derive(Debug, Clone)]
struct PageHaystack {
  folded: String,
  at: Vec<Option<TextPos>>,
}

/// Build the folded haystack of one page: runs joined by a space, lines joined
/// by a space unless the previous line ends in a hyphen, which is dropped so
/// the split word reads as one.
fn page_haystack(page_index: usize, page: &PageText) -> PageHaystack {
  let mut folded = String::new();
  let mut at = Vec::new();
  for (line_index, line) in page.lines.iter().enumerate() {
    if !folded.is_empty() {
      let hyphenated = folded.ends_with('-')
        && line
          .runs
          .first()
          .and_then(|run| run.text.chars().next())
          .is_some_and(char::is_alphabetic);
      if hyphenated {
        folded.pop();
        at.pop();
      } else {
        push_separator(&mut folded, &mut at);
      }
    }
    for (run_index, run) in line.runs.iter().enumerate() {
      if run_index > 0 {
        push_separator(&mut folded, &mut at);
      }
      for (ch, source) in fold(&run.text) {
        folded.push(ch);
        at.push(Some(TextPos {
          page: page_index,
          line: line_index,
          run: run_index,
          ch: source,
        }));
      }
    }
  }
  PageHaystack { folded, at }
}

fn push_separator(folded: &mut String, at: &mut Vec<Option<TextPos>>) {
  if !folded.ends_with(' ') {
    folded.push(' ');
    at.push(None);
  }
}

/// Search every page. Matching folds case, ligatures, accents, whitespace, and
/// end-of-line hyphenation. The haystack is folded once per layer; only the
/// needle is folded per query.
#[must_use]
pub fn search(layer: &TextLayer, query: &str) -> Vec<Match> {
  let needle: String = fold(query.trim()).into_iter().map(|(ch, _)| ch).collect();
  if needle.is_empty() {
    return Vec::new();
  }
  let needle_chars = needle.chars().count();
  let mut hits = Vec::new();
  for hay in layer.haystack() {
    if hay.at.len() < needle_chars {
      continue;
    }
    let mut byte = 0_usize;
    let mut char_index = 0_usize;
    while let Some(rest) = hay.folded.get(byte..) {
      let Some(found) = rest.find(&needle) else {
        break;
      };
      let skipped = rest.get(..found).map_or(0, |skipped| skipped.chars().count());
      let start_char = char_index.saturating_add(skipped);
      let end_char = start_char.saturating_add(needle_chars);
      let window = hay.at.get(start_char..end_char).unwrap_or_default();
      let start = window.iter().copied().find_map(|entry| entry);
      let last = window.iter().rev().copied().find_map(|entry| entry);
      if let (Some(start), Some(last)) = (start, last) {
        hits.push(Match {
          start,
          end: after(layer, last),
          context: context_for(layer, start, last),
        });
      }
      byte = byte.saturating_add(found).saturating_add(needle.len());
      char_index = end_char;
    }
  }
  hits
}

/// The position one character past `position`.
fn after(layer: &TextLayer, position: TextPos) -> TextPos {
  let count = layer.run(position).map_or(0, TextRun::char_count);
  TextPos {
    ch: position.ch.saturating_add(1).min(count),
    ..position
  }
}

/// The line text around a hit, trimmed for the results list.
fn context_for(layer: &TextLayer, start: TextPos, end: TextPos) -> String {
  const MAX_CONTEXT: usize = 120;
  let mut context = layer.line(start.page, start.line).map(TextLine::text).unwrap_or_default();
  if end.line != start.line
    && let Some(next) = layer.line(end.page, end.line)
  {
    context.push(' ');
    context.push_str(&next.text());
  }
  let trimmed = context.trim();
  if trimmed.chars().count() <= MAX_CONTEXT {
    return trimmed.to_owned();
  }
  let kept: String = trimmed.chars().take(MAX_CONTEXT).collect();
  format!("{kept}…")
}

/// The box of one character.
#[must_use]
pub fn char_rect(layer: &TextLayer, position: TextPos) -> Option<RectPt> {
  let run = layer.run(position)?;
  Some(run.slice_rect(position.ch, position.ch.saturating_add(1)))
}

/// One box per line segment between two positions, `start` inclusive and `end`
/// exclusive, each with the page it belongs to.
#[must_use]
pub fn rects_between(layer: &TextLayer, start: TextPos, end: TextPos) -> Vec<(usize, RectPt)> {
  let (start, end) = ordered(start, end);
  let mut rects = Vec::new();
  for_each_segment(layer, start, end, &mut |page, _line, segments| {
    let mut combined: Option<RectPt> = None;
    for (run, from, to) in segments {
      let rect = run.slice_rect(from, to);
      if rect.width <= 0.0 {
        continue;
      }
      combined = Some(combined.map_or(rect, |previous| union(previous, rect)));
    }
    if let Some(rect) = combined {
      rects.push((page, rect));
    }
  });
  rects
}

/// The text between two positions: one line break per line, one blank line per
/// page break.
#[must_use]
pub fn text_between(layer: &TextLayer, start: TextPos, end: TextPos) -> String {
  let (start, end) = ordered(start, end);
  let mut out = String::new();
  let mut last_page: Option<usize> = None;
  for_each_segment(layer, start, end, &mut |page, _line, segments| {
    match last_page {
      None => {},
      Some(previous) if previous == page => out.push('\n'),
      Some(_) => out.push_str("\n\n"),
    }
    last_page = Some(page);
    let mut previous: Option<&TextRun> = None;
    for (run, from, to) in segments {
      if let Some(previous) = previous {
        let gap = run.rect.x - (previous.rect.x + previous.rect.width);
        if gap > 0.15 * previous.rect.height.max(1.0) && !out.ends_with(' ') {
          out.push(' ');
        }
      }
      out.push_str(&run.slice_text(from, to));
      previous = Some(run);
    }
  });
  out
}

/// One line's selected runs: the run and the `[from, to)` character range.
type Segments<'layer> = Vec<(&'layer TextRun, usize, usize)>;

/// The smallest box covering both.
fn union(first: RectPt, second: RectPt) -> RectPt {
  let left = first.x.min(second.x);
  let right = (first.x + first.width).max(second.x + second.width);
  let bottom = first.y.min(second.y);
  let top = (first.y + first.height).max(second.y + second.height);
  RectPt {
    x: left,
    y: bottom,
    width: right - left,
    height: top - bottom,
  }
}

/// Walk the selected segments line by line, handing each line's runs and
/// character ranges to `visit`.
fn for_each_segment<'layer>(
  layer: &'layer TextLayer,
  start: TextPos,
  end: TextPos,
  visit: &mut dyn FnMut(usize, usize, Segments<'layer>),
) {
  for (page_index, page) in layer.pages.iter().enumerate() {
    if page_index < start.page || page_index > end.page {
      continue;
    }
    for (line_index, line) in page.lines.iter().enumerate() {
      let before_start = page_index == start.page && line_index < start.line;
      let after_end = page_index == end.page && line_index > end.line;
      if before_start || after_end {
        continue;
      }
      let mut segments = Vec::new();
      for (run_index, run) in line.runs.iter().enumerate() {
        let count = run.char_count();
        let here = TextPos {
          page: page_index,
          line: line_index,
          run: run_index,
          ch: 0,
        };
        let from = if here.page == start.page && here.line == start.line && run_index == start.run {
          start.ch.min(count)
        } else if (here.page, here.line, run_index) < (start.page, start.line, start.run) {
          count
        } else {
          0
        };
        let to = if here.page == end.page && here.line == end.line && run_index == end.run {
          end.ch.min(count)
        } else if (here.page, here.line, run_index) > (end.page, end.line, end.run) {
          0
        } else {
          count
        };
        if to > from {
          segments.push((run, from, to));
        }
      }
      if !segments.is_empty() {
        visit(page_index, line_index, segments);
      }
    }
  }
}

/// The position nearest to a point in the item frame on `page`.
#[must_use]
pub fn position_at(layer: &TextLayer, page: usize, x: f32, y: f32) -> Option<TextPos> {
  let text = layer.pages.get(page)?;
  let mut best: Option<(f32, usize)> = None;
  for (line_index, line) in text.lines.iter().enumerate() {
    let (bottom, top) = line.band();
    let distance = if y < bottom {
      bottom - y
    } else if y > top {
      y - top
    } else {
      0.0
    };
    if best.is_none_or(|(previous, _)| distance < previous) {
      best = Some((distance, line_index));
    }
  }
  let (_, line_index) = best?;
  let line = text.lines.get(line_index)?;
  let mut run_index = 0;
  let mut best_run: Option<f32> = None;
  for (index, run) in line.runs.iter().enumerate() {
    let distance = if x < run.rect.x {
      run.rect.x - x
    } else if x > run.rect.x + run.rect.width {
      x - (run.rect.x + run.rect.width)
    } else {
      0.0
    };
    if best_run.is_none_or(|previous| distance < previous) {
      best_run = Some(distance);
      run_index = index;
    }
  }
  let run = line.runs.get(run_index)?;
  let count = run.char_count();
  let offset = if run.rect.width <= 0.0 || count == 0 {
    0
  } else {
    let ratio = ((x - run.rect.x) / run.rect.width).clamp(0.0, 1.0);
    round_to_usize(ratio * count_as_f32(count)).min(count)
  };
  Some(TextPos {
    page,
    line: line_index,
    run: run_index,
    ch: offset,
  })
}

/// The word around `position`, as a `[start, end)` pair.
#[must_use]
pub fn word_at(layer: &TextLayer, position: TextPos) -> (TextPos, TextPos) {
  let Some(run) = layer.run(position) else {
    return (position, position);
  };
  let chars: Vec<char> = run.text.chars().collect();
  let count = chars.len();
  if count == 0 {
    return (position, position);
  }
  let cursor = position.ch.min(count.saturating_sub(1));
  if !chars.get(cursor).is_some_and(|ch| ch.is_alphanumeric()) {
    return (
      position,
      TextPos {
        ch: cursor.saturating_add(1).min(count),
        ..position
      },
    );
  }
  let mut start = cursor;
  while start > 0 && chars.get(start.saturating_sub(1)).is_some_and(|ch| ch.is_alphanumeric()) {
    start = start.saturating_sub(1);
  }
  let mut end = cursor.saturating_add(1);
  while end < count && chars.get(end).is_some_and(|ch| ch.is_alphanumeric()) {
    end = end.saturating_add(1);
  }
  (TextPos { ch: start, ..position }, TextPos { ch: end, ..position })
}

/// The first and last positions of the whole document.
#[must_use]
pub fn document_range(layer: &TextLayer) -> Option<(TextPos, TextPos)> {
  let mut first: Option<TextPos> = None;
  let mut last: Option<TextPos> = None;
  for (page_index, page) in layer.pages.iter().enumerate() {
    for (line_index, line) in page.lines.iter().enumerate() {
      for (run_index, run) in line.runs.iter().enumerate() {
        let count = run.char_count();
        if count == 0 {
          continue;
        }
        let start = TextPos {
          page: page_index,
          line: line_index,
          run: run_index,
          ch: 0,
        };
        if first.is_none() {
          first = Some(start);
        }
        last = Some(TextPos { ch: count, ..start });
      }
    }
  }
  Some((first?, last?))
}

/// Put two positions in document order.
#[must_use]
pub fn ordered(first: TextPos, second: TextPos) -> (TextPos, TextPos) {
  if first <= second {
    (first, second)
  } else {
    (second, first)
  }
}

fn count_as_f32(count: usize) -> f32 {
  f32::from(u16::try_from(count).unwrap_or(u16::MAX))
}

/// Round a finite, non-negative `f32` to the nearest `usize`, saturating.
#[expect(
  clippy::as_conversions,
  clippy::cast_possible_truncation,
  clippy::cast_sign_loss,
  reason = "no checked float-to-integer conversion exists; the value is clamped into range first"
)]
fn round_to_usize(value: f32) -> usize {
  if !value.is_finite() || value <= 0.0 {
    return 0;
  }
  (value.round().min(f32::from(u16::MAX)) as u32) as usize
}

#[cfg(test)]
mod tests {
  use super::{
    Match, PageText, TextLayer, TextLine, TextPos, TextRun, after, context_for, document_range, fold, page_haystack,
    position_at, rects_between, search, text_between, text_layer, word_at,
  };
  use crate::pdf::RectPt;
  use crate::pdf::test_support::tiny_pdf_pages;

  /// Build a layer from `(text, x, baseline)` triples: six points per
  /// character, twelve points tall.
  fn layer(pages: &[&[(&str, f32, f32)]]) -> TextLayer {
    TextLayer {
      pages: pages
        .iter()
        .map(|lines| {
          let mut page = PageText::default();
          for (text, x, y) in *lines {
            let width = 6.0 * f32::from(u16::try_from(text.chars().count()).unwrap_or(u16::MAX));
            let run = TextRun {
              text: (*text).to_owned(),
              rect: RectPt { x: *x, y: *y, width, height: 12.0 },
            };
            let same_line = page
              .lines
              .last()
              .and_then(|line| line.runs.first())
              .is_some_and(|first| (first.rect.y - y).abs() < 1.0);
            if same_line {
              if let Some(line) = page.lines.last_mut() {
                line.runs.push(run);
              }
            } else {
              page.lines.push(TextLine { runs: vec![run] });
            }
          }
          page
        })
        .collect(),
      ..TextLayer::default()
    }
  }

  #[test]
  fn fold_splits_ligatures_drops_accents_and_lowercases() {
    let folded: String = fold("Confi\u{FB01}guração").iter().map(|(ch, _)| *ch).collect();
    assert_eq!(folded, "confifiguracao");

    let mapped: Vec<usize> = fold("\u{FB01}x").iter().map(|(_, index)| *index).collect();
    assert_eq!(mapped, vec![0, 0, 1], "both halves of the ligature point at one source char");
  }

  #[test]
  fn fold_collapses_whitespace() {
    let folded: String = fold("a \t\n b").iter().map(|(ch, _)| *ch).collect();
    assert_eq!(folded, "a b");
  }

  #[test]
  fn search_joins_end_of_line_hyphenation() {
    let layer = layer(&[&[("configu-", 72.0, 700.0), ("ration file", 72.0, 686.0)]]);

    let hits = search(&layer, "Configuration");

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].start, TextPos { page: 0, line: 0, run: 0, ch: 0 });
    assert_eq!(hits[0].end, TextPos { page: 0, line: 1, run: 0, ch: 6 });
    assert!(hits[0].context.contains("configu-"), "{}", hits[0].context);
  }

  #[test]
  fn search_spans_runs_and_collapses_whitespace() {
    let layer = layer(&[&[("Hello", 72.0, 700.0), ("World", 108.0, 700.0)]]);

    assert_eq!(search(&layer, "hello world").len(), 1);
    assert_eq!(search(&layer, "hello   world").len(), 1);
    assert!(search(&layer, "helloworld").is_empty());
  }

  #[test]
  fn search_folds_accents_and_case() {
    let layer = layer(&[&[("Configuração", 72.0, 700.0)]]);

    assert_eq!(search(&layer, "configuracao").len(), 1);
    assert_eq!(search(&layer, "CONFIGURAÇÃO").len(), 1);
  }

  #[test]
  fn search_reports_every_page_in_order() {
    let layer = layer(&[&[("alpha", 72.0, 700.0)], &[("Alpha beta alpha", 72.0, 700.0)]]);

    let hits: Vec<usize> = search(&layer, "alpha").iter().map(|hit: &Match| hit.start.page).collect();

    assert_eq!(hits, vec![0, 1, 1]);
  }

  #[test]
  fn an_empty_query_matches_nothing() {
    let layer = layer(&[&[("alpha", 72.0, 700.0)]]);

    assert!(search(&layer, "   ").is_empty());
  }

  #[test]
  fn text_between_breaks_lines_and_pages() {
    let layer = layer(&[&[("one two", 72.0, 700.0), ("three", 72.0, 686.0)], &[("four", 72.0, 700.0)]]);
    let (start, end) = document_range(&layer).unwrap();

    assert_eq!(text_between(&layer, start, end), "one two\nthree\n\nfour");

    let partial = text_between(
      &layer,
      TextPos { page: 0, line: 0, run: 0, ch: 4 },
      TextPos { page: 0, line: 1, run: 0, ch: 3 },
    );
    assert_eq!(partial, "two\nthr");
  }

  #[test]
  fn rects_between_yields_one_box_per_line_segment() {
    let layer = layer(&[&[("one two", 72.0, 700.0), ("three", 72.0, 686.0)]]);

    let rects = rects_between(
      &layer,
      TextPos { page: 0, line: 0, run: 0, ch: 4 },
      TextPos { page: 0, line: 1, run: 0, ch: 3 },
    );

    assert_eq!(rects.len(), 2);
    assert!(
      (rects[0].1.x - 96.0).abs() < 1e-3 && (rects[0].1.width - 18.0).abs() < 1e-3,
      "{:?}",
      rects[0]
    );
    assert!(
      (rects[1].1.x - 72.0).abs() < 1e-3 && (rects[1].1.width - 18.0).abs() < 1e-3,
      "{:?}",
      rects[1]
    );
  }

  #[test]
  fn position_at_and_word_at_pick_the_nearest_character() {
    let layer = layer(&[&[("one two", 72.0, 700.0)]]);

    let position = position_at(&layer, 0, 96.0, 704.0).unwrap();

    assert_eq!(position, TextPos { page: 0, line: 0, run: 0, ch: 4 });
    assert_eq!(
      word_at(&layer, position),
      (
        TextPos { page: 0, line: 0, run: 0, ch: 4 },
        TextPos { page: 0, line: 0, run: 0, ch: 7 }
      )
    );
  }

  #[test]
  fn position_at_clamps_a_point_above_the_page_to_the_first_line() {
    let layer = layer(&[&[("one", 72.0, 700.0), ("two", 72.0, 600.0)]]);

    let position = position_at(&layer, 0, 0.0, 9999.0).unwrap();

    assert_eq!(position.line, 0);
    assert_eq!(position.ch, 0);
  }

  #[test]
  fn a_layer_reads_the_text_of_a_real_document() {
    let bytes = tiny_pdf_pages(&["Hello World", "second page"]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.pdf");
    std::fs::write(&path, &bytes).unwrap();

    let layer = text_layer(&path, &bytes, None, 2).unwrap();

    assert_eq!(layer.pages.len(), 2);
    assert!(!layer.is_empty());
    let (start, end) = document_range(&layer).unwrap();
    let text = text_between(&layer, start, end);
    assert!(text.contains("Hello World"), "{text}");
    assert!(text.contains("second page"), "{text}");
    assert_eq!(search(&layer, "hello world").len(), 1);
    assert_eq!(search(&layer, "second").len(), 1);
  }

  #[test]
  fn folded_haystack_search_matches_the_scan_on_the_fixture() {
    let bytes = tiny_pdf_pages(&["Hello World", "second page", "Configuracao file"]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.pdf");
    std::fs::write(&path, &bytes).unwrap();
    let layer = text_layer(&path, &bytes, None, 3).unwrap();

    for query in ["hello world", "second", "configuracao", "HELLO", "nope", "  "] {
      assert_eq!(search(&layer, query), search_by_scan(&layer, query), "{query}");
    }
  }

  /// The previous O(n*m) scan, kept to pin haystack search to the same hits.
  fn search_by_scan(layer: &TextLayer, query: &str) -> Vec<Match> {
    let needle: Vec<char> = fold(query.trim()).into_iter().map(|(ch, _)| ch).collect();
    if needle.is_empty() {
      return Vec::new();
    }
    let mut hits = Vec::new();
    for (page_index, page) in layer.pages.iter().enumerate() {
      let hay = page_haystack(page_index, page);
      let chars: Vec<char> = hay.folded.chars().collect();
      if chars.len() < needle.len() {
        continue;
      }
      let last_start = chars.len().saturating_sub(needle.len());
      let mut index = 0;
      while index <= last_start {
        let matched = chars
          .get(index..index.saturating_add(needle.len()))
          .is_some_and(|window| window.iter().copied().eq(needle.iter().copied()));
        if !matched {
          index = index.saturating_add(1);
          continue;
        }
        let window = hay.at.get(index..index.saturating_add(needle.len())).unwrap_or_default();
        let start = window.iter().copied().find_map(|entry| entry);
        let last = window.iter().rev().copied().find_map(|entry| entry);
        if let (Some(start), Some(last)) = (start, last) {
          hits.push(Match {
            start,
            end: after(layer, last),
            context: context_for(layer, start, last),
          });
        }
        index = index.saturating_add(needle.len());
      }
    }
    hits
  }

  #[test]
  fn a_layer_of_an_encrypted_document_needs_the_password() {
    let path = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/protected.pdf"));
    let bytes = std::fs::read(&path).unwrap();

    assert!(matches!(
      text_layer(&path, &bytes, None, 1),
      Err(crate::error::Error::Pdf { .. })
    ));

    let layer = text_layer(&path, &bytes, Some("openit"), 1).unwrap();

    assert_eq!(search(&layer, "locked page").len(), 1);
  }
}
