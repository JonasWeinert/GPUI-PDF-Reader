use std::ops::{Range, RangeInclusive};
use std::sync::Arc;

pub const PDF_POINTS_TO_LOGICAL_PIXELS: f32 = 96.0 / 72.0;
pub const PAGE_GAP: f32 = 24.0;
pub const PAGE_MARGIN: f32 = 36.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RasterSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TileKey {
    pub page: usize,
    pub raster: RasterSize,
    pub column: u32,
    pub row: u32,
}

impl Rect {
    pub fn right(self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(self) -> f32 {
        self.y + self.height
    }

    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.right() && y >= self.y && y <= self.bottom()
    }

    pub fn intersects(self, other: Self) -> bool {
        self.x < other.right()
            && self.right() > other.x
            && self.y < other.bottom()
            && self.bottom() > other.y
    }
}

#[derive(Clone, Debug)]
pub struct DocumentLayout {
    page_sizes: Arc<[PageSize]>,
    height_prefix: Arc<[f64]>,
    widest_page: f32,
    scale: f32,
    pub content_width: f32,
    pub content_height: f32,
}

impl DocumentLayout {
    pub fn new(page_sizes: &[PageSize], zoom: f32, viewport_width: f32) -> Self {
        let mut height_prefix = Vec::with_capacity(page_sizes.len() + 1);
        height_prefix.push(0.0);
        let mut total_height = 0.0_f64;
        let mut widest_page = 0.0_f32;
        for page in page_sizes {
            total_height += f64::from(page.height);
            height_prefix.push(total_height);
            widest_page = widest_page.max(page.width);
        }
        Self::from_geometry(
            Arc::from(page_sizes),
            Arc::from(height_prefix),
            widest_page,
            zoom,
            viewport_width,
        )
    }

    /// Returns the same document geometry at a new zoom and viewport width.
    /// The page-size and prefix arrays are shared, so this operation is O(1).
    pub fn rescaled(&self, zoom: f32, viewport_width: f32) -> Self {
        Self::from_geometry(
            self.page_sizes.clone(),
            self.height_prefix.clone(),
            self.widest_page,
            zoom,
            viewport_width,
        )
    }

    fn from_geometry(
        page_sizes: Arc<[PageSize]>,
        height_prefix: Arc<[f64]>,
        widest_page: f32,
        zoom: f32,
        viewport_width: f32,
    ) -> Self {
        let scale = PDF_POINTS_TO_LOGICAL_PIXELS * zoom;
        let content_width = viewport_width.max(widest_page * scale + PAGE_MARGIN * 2.0);
        let content_height = if page_sizes.is_empty() {
            0.0
        } else {
            let last = page_sizes.len() - 1;
            let preceding_height = height_prefix[last] * f64::from(scale);
            let preceding_gaps = last as f64 * f64::from(PAGE_GAP);
            let last_top = (f64::from(PAGE_MARGIN) + preceding_height + preceding_gaps) as f32;
            last_top + page_sizes[last].height * scale + PAGE_MARGIN
        };

        Self {
            page_sizes,
            height_prefix,
            widest_page,
            scale,
            content_width,
            content_height,
        }
    }

    pub fn page_count(&self) -> usize {
        self.page_sizes.len()
    }

    pub fn page_rect(&self, index: usize) -> Option<Rect> {
        let page = *self.page_sizes.get(index)?;
        let width = page.width * self.scale;
        Some(Rect {
            x: (self.content_width - width) * 0.5,
            y: self.page_top(index),
            width,
            height: page.height * self.scale,
        })
    }

    pub fn visible_pages(
        &self,
        scroll_y: f32,
        viewport_height: f32,
        overscan: f32,
    ) -> Range<usize> {
        if self.page_sizes.is_empty() {
            return 0..0;
        }

        let top = (scroll_y - overscan).max(0.0);
        let bottom = scroll_y + viewport_height + overscan;
        let first = self
            .partition_point(|index| self.page_bottom(index) < top)
            .min(self.page_sizes.len());
        let end = self
            .partition_point(|index| self.page_top(index) <= bottom)
            .max(first);
        first..end
    }

    pub fn current_page(&self, scroll_y: f32, viewport_height: f32) -> usize {
        if self.page_sizes.is_empty() {
            return 0;
        }
        let probe = scroll_y + viewport_height * 0.3;
        self.partition_point(|index| self.page_bottom(index) < probe)
            .min(self.page_sizes.len() - 1)
    }

    pub fn page_at_content_point(&self, x: f32, y: f32) -> Option<usize> {
        let candidate = self.partition_point(|index| self.page_bottom(index) < y);
        let rect = self.page_rect(candidate)?;
        rect.contains(x, y).then_some(candidate)
    }

    pub fn anchor_at_content_point(&self, x: f32, y: f32) -> Option<PageAnchor> {
        let page = self.page_at_content_point(x, y).or_else(|| {
            let next = self.partition_point(|index| self.page_bottom(index) < y);
            let previous = next.checked_sub(1);
            previous
                .into_iter()
                .chain((next < self.page_sizes.len()).then_some(next))
                .min_by(|a, b| {
                    let a_distance = distance_to_rect_y(self.page_rect(*a).unwrap(), y);
                    let b_distance = distance_to_rect_y(self.page_rect(*b).unwrap(), y);
                    a_distance.total_cmp(&b_distance).then_with(|| a.cmp(b))
                })
        })?;
        let rect = self.page_rect(page)?;
        Some(PageAnchor {
            page,
            x_fraction: ((x - rect.x) / rect.width).clamp(0.0, 1.0),
            y_fraction: ((y - rect.y) / rect.height).clamp(0.0, 1.0),
        })
    }

    pub fn content_point_for_anchor(&self, anchor: PageAnchor) -> Option<(f32, f32)> {
        let rect = self.page_rect(anchor.page)?;
        Some((
            rect.x + rect.width * anchor.x_fraction,
            rect.y + rect.height * anchor.y_fraction,
        ))
    }

    fn page_top(&self, index: usize) -> f32 {
        let preceding_height = self.height_prefix[index] * f64::from(self.scale);
        let preceding_gaps = index as f64 * f64::from(PAGE_GAP);
        (f64::from(PAGE_MARGIN) + preceding_height + preceding_gaps) as f32
    }

    fn page_bottom(&self, index: usize) -> f32 {
        self.page_top(index) + self.page_sizes[index].height * self.scale
    }

    fn partition_point(&self, mut predicate: impl FnMut(usize) -> bool) -> usize {
        let mut left = 0;
        let mut right = self.page_sizes.len();
        while left < right {
            let middle = left + (right - left) / 2;
            if predicate(middle) {
                left = middle + 1;
            } else {
                right = middle;
            }
        }
        left
    }
}

fn distance_to_rect_y(rect: Rect, y: f32) -> f32 {
    if y < rect.y {
        rect.y - y
    } else if y > rect.bottom() {
        y - rect.bottom()
    } else {
        0.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageAnchor {
    pub page: usize,
    pub x_fraction: f32,
    pub y_fraction: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextBounds {
    /// Normalized device coordinates with a top-left origin.
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextChar {
    pub value: char,
    pub bounds: Option<TextBounds>,
}

const TARGET_TEXT_CHARACTERS_PER_CELL: usize = 64;
const MAX_TEXT_GRID_AXIS: usize = 16;
const MAX_TEXT_GRID_CELLS: usize = MAX_TEXT_GRID_AXIS * MAX_TEXT_GRID_AXIS;

/// An immutable page text layer. Character order is exactly PDFium's order;
/// the spatial index only stores original offsets into `characters`.
#[derive(Clone, Debug, Default)]
pub struct TextLayer {
    characters: Vec<TextChar>,
    spatial_index: TextSpatialIndex,
}

impl TextLayer {
    pub fn new(characters: Vec<TextChar>) -> Self {
        let spatial_index = TextSpatialIndex::new(&characters);
        Self {
            characters,
            spatial_index,
        }
    }

    pub fn empty() -> Self {
        Self::default()
    }

    pub fn as_slice(&self) -> &[TextChar] {
        &self.characters
    }

    #[cfg(test)]
    fn spatial_index(&self) -> &TextSpatialIndex {
        &self.spatial_index
    }

    /// Returns the original PDFium character offset at a logical page point.
    /// Exact padded hits take precedence over the nearest-character fallback.
    pub fn hit_test(&self, page_rect: Rect, x: f32, y: f32, nearest: bool) -> Option<usize> {
        self.spatial_index
            .hit_test(&self.characters, page_rect, x, y, nearest)
    }

    /// Visits selected character highlights that intersect the logical
    /// viewport. Visit order is spatial and must not be used as text order.
    pub fn for_each_visible_in_range(
        &self,
        page_rect: Rect,
        viewport: Rect,
        range: RangeInclusive<usize>,
        visit: impl FnMut(usize, Rect),
    ) {
        self.spatial_index.for_each_visible_in_range(
            &self.characters,
            page_rect,
            viewport,
            range,
            visit,
        );
    }
}

impl From<Vec<TextChar>> for TextLayer {
    fn from(characters: Vec<TextChar>) -> Self {
        Self::new(characters)
    }
}

impl std::ops::Deref for TextLayer {
    type Target = [TextChar];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

/// A small, dependency-free broad-phase index for one page's text bounds.
/// Each bounded character appears in exactly one cell, keeping adversarial
/// large glyph bounds from multiplying memory use.
#[derive(Clone, Debug, Default)]
pub struct TextSpatialIndex {
    cells: Vec<TextCell>,
    members: Vec<usize>,
    #[cfg(test)]
    columns: usize,
    #[cfg(test)]
    rows: usize,
}

#[derive(Clone, Debug)]
struct TextCell {
    bounds: TextBounds,
    members: Range<usize>,
    min_index: usize,
    max_index: usize,
}

#[derive(Default)]
struct TextCellBuilder {
    bounds: Option<TextBounds>,
    members: Vec<usize>,
}

impl TextSpatialIndex {
    fn new(characters: &[TextChar]) -> Self {
        let located = characters
            .iter()
            .filter(|character| character.bounds.and_then(canonical_text_bounds).is_some())
            .count();
        if located == 0 {
            return Self::default();
        }

        let target_cells = located
            .div_ceil(TARGET_TEXT_CHARACTERS_PER_CELL)
            .clamp(1, MAX_TEXT_GRID_CELLS);
        let columns = (target_cells as f64).sqrt().ceil() as usize;
        let rows = target_cells.div_ceil(columns);
        debug_assert!(columns <= MAX_TEXT_GRID_AXIS && rows <= MAX_TEXT_GRID_AXIS);
        let mut builders = (0..columns * rows)
            .map(|_| TextCellBuilder::default())
            .collect::<Vec<_>>();

        for (index, character) in characters.iter().enumerate() {
            let Some(bounds) = character.bounds.and_then(canonical_text_bounds) else {
                continue;
            };
            let center_x =
                ((f64::from(bounds.left) + f64::from(bounds.right)) * 0.5).clamp(0.0, 1.0);
            let center_y =
                ((f64::from(bounds.top) + f64::from(bounds.bottom)) * 0.5).clamp(0.0, 1.0);
            let column = ((center_x * columns as f64).floor() as usize).min(columns - 1);
            let row = ((center_y * rows as f64).floor() as usize).min(rows - 1);
            let builder = &mut builders[row * columns + column];
            builder.bounds = Some(match builder.bounds {
                Some(current) => union_text_bounds(current, bounds),
                None => bounds,
            });
            builder.members.push(index);
        }

        let mut cells = Vec::with_capacity(builders.len());
        let mut members = Vec::with_capacity(located);
        for builder in builders {
            let Some(bounds) = builder.bounds else {
                continue;
            };
            let start = members.len();
            let min_index = builder.members[0];
            let max_index = *builder.members.last().unwrap();
            members.extend(builder.members);
            cells.push(TextCell {
                bounds,
                members: start..members.len(),
                min_index,
                max_index,
            });
        }

        Self {
            cells,
            members,
            #[cfg(test)]
            columns,
            #[cfg(test)]
            rows,
        }
    }

    #[cfg(test)]
    fn cell_count(&self) -> usize {
        self.cells.len()
    }

    #[cfg(test)]
    fn grid_size(&self) -> (usize, usize) {
        (self.columns, self.rows)
    }

    fn hit_test(
        &self,
        characters: &[TextChar],
        page_rect: Rect,
        x: f32,
        y: f32,
        nearest: bool,
    ) -> Option<usize> {
        if !valid_page_rect(page_rect) || !x.is_finite() || !y.is_finite() {
            return None;
        }

        let mut exact_hit = None;
        for cell in &self.cells {
            let cell_rect = text_rect_in_page(page_rect, cell.bounds);
            if !conservative_hit_bounds(cell_rect).contains(x, y) {
                continue;
            }
            for &index in &self.members[cell.members.clone()] {
                if exact_hit.is_some_and(|current| index >= current) {
                    break;
                }
                let Some(rect) = character_rect(characters, index, page_rect) else {
                    continue;
                };
                if text_hit_bounds(rect).contains(x, y) {
                    exact_hit = Some(index);
                    break;
                }
            }
        }
        if exact_hit.is_some() || !nearest {
            return exact_hit;
        }

        let mut seed = None;
        for (cell_index, cell) in self.cells.iter().enumerate() {
            let distance =
                weighted_distance_squared(text_rect_in_page(page_rect, cell.bounds), x, y);
            if !distance.is_finite() {
                continue;
            }
            if seed.is_none_or(|(_, current_distance, current_min)| {
                distance < current_distance
                    || (distance == current_distance && cell.min_index < current_min)
            }) {
                seed = Some((cell_index, distance, cell.min_index));
            }
        }
        let (seed_index, _, _) = seed?;
        let mut best = None;
        self.scan_nearest_cell(seed_index, characters, page_rect, x, y, &mut best);

        for (cell_index, cell) in self.cells.iter().enumerate() {
            if cell_index == seed_index {
                continue;
            }
            let lower_bound =
                weighted_distance_squared(text_rect_in_page(page_rect, cell.bounds), x, y);
            let should_scan = best.is_none_or(|(best_index, best_distance)| {
                lower_bound < best_distance
                    || (lower_bound == best_distance && cell.min_index < best_index)
            });
            if should_scan {
                self.scan_nearest_cell(cell_index, characters, page_rect, x, y, &mut best);
            }
        }
        best.map(|(index, _)| index)
    }

    fn scan_nearest_cell(
        &self,
        cell_index: usize,
        characters: &[TextChar],
        page_rect: Rect,
        x: f32,
        y: f32,
        best: &mut Option<(usize, f32)>,
    ) {
        let cell = &self.cells[cell_index];
        for &index in &self.members[cell.members.clone()] {
            let Some(rect) = character_rect(characters, index, page_rect) else {
                continue;
            };
            let distance = weighted_distance_squared(rect, x, y);
            if !distance.is_finite() {
                continue;
            }
            if best.is_none_or(|(current_index, current_distance)| {
                distance < current_distance
                    || (distance == current_distance && index < current_index)
            }) {
                *best = Some((index, distance));
            }
        }
    }

    fn for_each_visible_in_range(
        &self,
        characters: &[TextChar],
        page_rect: Rect,
        viewport: Rect,
        range: RangeInclusive<usize>,
        mut visit: impl FnMut(usize, Rect),
    ) {
        if characters.is_empty() || !valid_page_rect(page_rect) || !valid_query_rect(viewport) {
            return;
        }
        let first = *range.start();
        let last = (*range.end()).min(characters.len() - 1);
        if first > last {
            return;
        }

        for cell in &self.cells {
            if cell.max_index < first || cell.min_index > last {
                continue;
            }
            let cell_rect = text_rect_in_page(page_rect, cell.bounds);
            if !inflate_rect(cell_rect, 3.0).intersects(viewport) {
                continue;
            }
            let members = &self.members[cell.members.clone()];
            let member_start = members.partition_point(|index| *index < first);
            for &index in &members[member_start..] {
                if index > last {
                    break;
                }
                let Some(rect) = character_rect(characters, index, page_rect) else {
                    continue;
                };
                let highlight = text_highlight_bounds(rect);
                if highlight.intersects(viewport) {
                    visit(index, highlight);
                }
            }
        }
    }
}

fn canonical_text_bounds(bounds: TextBounds) -> Option<TextBounds> {
    if !bounds.left.is_finite()
        || !bounds.top.is_finite()
        || !bounds.right.is_finite()
        || !bounds.bottom.is_finite()
    {
        return None;
    }
    Some(TextBounds {
        left: bounds.left,
        top: bounds.top,
        right: bounds.right.max(bounds.left),
        bottom: bounds.bottom.max(bounds.top),
    })
}

fn union_text_bounds(a: TextBounds, b: TextBounds) -> TextBounds {
    TextBounds {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    }
}

fn text_rect_in_page(page_rect: Rect, text: TextBounds) -> Rect {
    Rect {
        x: page_rect.x + text.left * page_rect.width,
        y: page_rect.y + text.top * page_rect.height,
        width: (text.right - text.left).max(0.0) * page_rect.width,
        height: (text.bottom - text.top).max(0.0) * page_rect.height,
    }
}

fn character_rect(characters: &[TextChar], index: usize, page_rect: Rect) -> Option<Rect> {
    let bounds = characters.get(index)?.bounds?;
    canonical_text_bounds(bounds).map(|bounds| text_rect_in_page(page_rect, bounds))
}

fn text_hit_bounds(rect: Rect) -> Rect {
    Rect {
        x: rect.x - 2.0,
        y: rect.y - 3.0,
        width: rect.width.max(2.0) + 4.0,
        height: rect.height.max(4.0) + 6.0,
    }
}

fn conservative_hit_bounds(rect: Rect) -> Rect {
    // The minimum glyph size in `text_hit_bounds()` can extend four logical
    // pixels to the right and seven to the bottom. Seven on every edge is a
    // deliberately simple conservative bound for a whole cell.
    inflate_rect(rect, 7.0)
}

fn text_highlight_bounds(rect: Rect) -> Rect {
    Rect {
        width: rect.width.max(1.5),
        height: rect.height.max(3.0),
        ..rect
    }
}

fn inflate_rect(rect: Rect, amount: f32) -> Rect {
    Rect {
        x: rect.x - amount,
        y: rect.y - amount,
        width: rect.width + amount * 2.0,
        height: rect.height + amount * 2.0,
    }
}

fn weighted_distance_squared(rect: Rect, x: f32, y: f32) -> f32 {
    let dx = if x < rect.x {
        rect.x - x
    } else if x > rect.right() {
        x - rect.right()
    } else {
        0.0
    };
    let dy = if y < rect.y {
        rect.y - y
    } else if y > rect.bottom() {
        y - rect.bottom()
    } else {
        0.0
    };
    dx * dx + dy * dy * 2.0
}

fn valid_page_rect(rect: Rect) -> bool {
    rect.x.is_finite()
        && rect.y.is_finite()
        && rect.width.is_finite()
        && rect.height.is_finite()
        && rect.width > 0.0
        && rect.height > 0.0
}

fn valid_query_rect(rect: Rect) -> bool {
    rect.x.is_finite()
        && rect.y.is_finite()
        && rect.width.is_finite()
        && rect.height.is_finite()
        && rect.width > 0.0
        && rect.height > 0.0
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TextPosition {
    pub page: usize,
    pub index: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextSelection {
    pub anchor: TextPosition,
    pub focus: TextPosition,
}

impl TextSelection {
    pub fn ordered(self) -> (TextPosition, TextPosition) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    /// Returns the inclusive original-character range selected on `page`.
    pub fn indices_on_page(
        self,
        page: usize,
        character_count: usize,
    ) -> Option<RangeInclusive<usize>> {
        if character_count == 0 {
            return None;
        }
        let (start, end) = self.ordered();
        if page < start.page || page > end.page {
            return None;
        }
        let first = if page == start.page { start.index } else { 0 };
        let last = if page == end.page {
            end.index.min(character_count - 1)
        } else {
            character_count - 1
        };
        (first <= last && first < character_count).then_some(first..=last)
    }
}

#[cfg(test)]
pub fn selected_text(selection: TextSelection, page_text: &[Option<&[TextChar]>]) -> String {
    let (start, end) = selection.ordered();
    let mut result = String::new();

    for page in start.page..=end.page {
        let Some(chars) = page_text.get(page).and_then(|chars| *chars) else {
            continue;
        };
        append_selected_page_text(&mut result, selection, page, chars);
    }

    result
}

/// Appends one available page of a selection using the same newline and page
/// separator rules as [`selected_text`]. This lets callers stream a large copy
/// one page at a time without retaining every page text layer simultaneously.
pub fn append_selected_page_text(
    result: &mut String,
    selection: TextSelection,
    page: usize,
    chars: &[TextChar],
) {
    if let Some(range) = selection.indices_on_page(page, chars.len()) {
        let first = *range.start();
        let last = *range.end();
        let mut previous_was_cr = false;
        for text_char in &chars[first..=last] {
            match text_char.value {
                '\0' => {}
                '\r' => {
                    result.push('\n');
                    previous_was_cr = true;
                }
                '\n' if previous_was_cr => {
                    previous_was_cr = false;
                }
                '\n' => {
                    result.push('\n');
                }
                value => {
                    result.push(value);
                    previous_was_cr = false;
                }
            }
        }
    }

    let (_, end) = selection.ordered();
    if page != end.page && !result.ends_with("\n\n") {
        if !result.ends_with('\n') {
            result.push('\n');
        }
        result.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn letter_pages(count: usize) -> Vec<PageSize> {
        vec![
            PageSize {
                width: 612.0,
                height: 792.0,
            };
            count
        ]
    }

    #[test]
    fn layout_centers_pages_and_virtualizes_with_overscan() {
        let layout = DocumentLayout::new(&letter_pages(20), 1.0, 1200.0);
        let first = layout.page_rect(0).unwrap();
        assert_eq!(first.width, 816.0);
        assert_eq!(first.x, 192.0);
        assert_eq!(layout.visible_pages(0.0, 700.0, 300.0), 0..1);

        let second = layout.page_rect(1).unwrap();
        assert_eq!(layout.visible_pages(second.y, 700.0, 10.0), 1..2);
    }

    #[test]
    fn rescaling_reuses_large_document_geometry_and_matches_a_fresh_layout() {
        let pages = (0..50_000)
            .map(|index| PageSize {
                width: 480.0 + (index % 13) as f32 * 11.25,
                height: 620.0 + (index % 17) as f32 * 19.5,
            })
            .collect::<Vec<_>>();
        let original = DocumentLayout::new(&pages, 0.72, 940.0);
        let rescaled = original.rescaled(2.35, 1_280.0);
        let fresh = DocumentLayout::new(&pages, 2.35, 1_280.0);

        assert!(Arc::ptr_eq(&original.page_sizes, &rescaled.page_sizes));
        assert!(Arc::ptr_eq(
            &original.height_prefix,
            &rescaled.height_prefix
        ));
        assert!(!Arc::ptr_eq(&rescaled.page_sizes, &fresh.page_sizes));
        assert_eq!(rescaled.page_count(), pages.len());
        assert_eq!(rescaled.content_width, fresh.content_width);
        assert_eq!(rescaled.content_height, fresh.content_height);
        assert_eq!(
            rescaled.content_height,
            rescaled.page_rect(pages.len() - 1).unwrap().bottom() + PAGE_MARGIN
        );

        for page in [0, 1, 127, 9_999, 24_817, pages.len() - 1] {
            assert_eq!(rescaled.page_rect(page), fresh.page_rect(page));
            let rect = rescaled.page_rect(page).unwrap();
            assert_eq!(
                rescaled.page_at_content_point(rect.x + rect.width * 0.5, rect.y + 1.0),
                Some(page)
            );
            assert_eq!(
                rescaled.visible_pages(rect.y + 5.0, 700.0, 80.0),
                fresh.visible_pages(rect.y + 5.0, 700.0, 80.0)
            );
        }
    }

    #[test]
    fn lazy_geometry_preserves_the_eager_layout_equations() {
        let pages = (0..512)
            .map(|index| PageSize {
                width: 510.0 + (index % 9) as f32 * 7.0,
                height: 680.0 + (index % 11) as f32 * 13.0,
            })
            .collect::<Vec<_>>();
        let zoom = 1.37;
        let viewport_width = 1_140.0;
        let layout = DocumentLayout::new(&pages, zoom, viewport_width);
        let (eager, content_width, content_height) = eager_layout(&pages, zoom, viewport_width);

        assert_eq!(layout.content_width, content_width);
        assert!(
            (layout.content_height - content_height).abs() < 2.0,
            "lazy content height={} eager content height={content_height}",
            layout.content_height
        );
        for (index, expected) in eager.iter().enumerate() {
            let actual = layout.page_rect(index).unwrap();
            assert_eq!(actual.x, expected.x);
            assert_eq!(actual.width, expected.width);
            assert_eq!(actual.height, expected.height);
            assert!(
                (actual.y - expected.y).abs() < 2.0,
                "page {index}: lazy y={} eager y={}",
                actual.y,
                expected.y
            );
        }

        for index in [0, 1, 87, 255, 511] {
            let rect = layout.page_rect(index).unwrap();
            let scroll = rect.y + rect.height * 0.25;
            assert_eq!(
                layout.visible_pages(scroll, 750.0, 120.0),
                eager_visible_pages(&eager, scroll, 750.0, 120.0)
            );
            assert_eq!(
                layout.current_page(scroll, 750.0),
                eager_current_page(&eager, scroll, 750.0)
            );
        }
    }

    #[test]
    fn gap_anchors_choose_the_nearest_adjacent_page() {
        let layout = DocumentLayout::new(&letter_pages(3), 1.0, 1_000.0);
        let first = layout.page_rect(0).unwrap();
        let second = layout.page_rect(1).unwrap();
        let midpoint = (first.bottom() + second.y) * 0.5;

        assert_eq!(
            layout
                .anchor_at_content_point(-100.0, midpoint)
                .unwrap()
                .page,
            0
        );
        assert_eq!(
            layout
                .anchor_at_content_point(-100.0, midpoint + 0.01)
                .unwrap()
                .page,
            1
        );
        assert_eq!(
            layout.anchor_at_content_point(-100.0, -500.0).unwrap().page,
            0
        );
        assert_eq!(
            layout
                .anchor_at_content_point(-100.0, layout.content_height + 500.0)
                .unwrap()
                .page,
            2
        );
    }

    #[test]
    fn zoom_anchor_round_trips() {
        let pages = letter_pages(2);
        let before = DocumentLayout::new(&pages, 1.0, 1000.0);
        let rect = before.page_rect(1).unwrap();
        let anchor = before
            .anchor_at_content_point(rect.x + 100.0, rect.y + 220.0)
            .unwrap();
        let after = DocumentLayout::new(&pages, 2.0, 1000.0);
        let (x, y) = after.content_point_for_anchor(anchor).unwrap();
        let after_rect = after.page_rect(1).unwrap();
        assert!((x - (after_rect.x + 200.0)).abs() < 0.01);
        assert!((y - (after_rect.y + 440.0)).abs() < 0.01);
    }

    #[test]
    fn selected_text_orders_reverse_selections_and_separates_pages() {
        let first: Vec<_> = "Hello".chars().map(text_char).collect();
        let second: Vec<_> = "world".chars().map(text_char).collect();
        let pages = [Some(first.as_slice()), Some(second.as_slice())];
        let selection = TextSelection {
            anchor: TextPosition { page: 1, index: 2 },
            focus: TextPosition { page: 0, index: 1 },
        };
        assert_eq!(selected_text(selection, &pages), "ello\n\nwor");
    }

    #[test]
    fn selected_text_normalizes_crlf_without_losing_blank_lines() {
        let text: Vec<_> = "a\r\nb\n\nc\rd".chars().map(text_char).collect();
        let pages = [Some(text.as_slice())];
        let selection = TextSelection {
            anchor: TextPosition { page: 0, index: 0 },
            focus: TextPosition {
                page: 0,
                index: text.len() - 1,
            },
        };
        assert_eq!(selected_text(selection, &pages), "a\nb\n\nc\nd");
    }

    #[test]
    fn selection_ranges_match_forward_and_reverse_multi_page_selection() {
        let forward = TextSelection {
            anchor: TextPosition { page: 1, index: 4 },
            focus: TextPosition { page: 3, index: 2 },
        };
        let reverse = TextSelection {
            anchor: forward.focus,
            focus: forward.anchor,
        };

        for selection in [forward, reverse] {
            assert_eq!(selection.indices_on_page(0, 10), None);
            assert_eq!(selection.indices_on_page(1, 10), Some(4..=9));
            assert_eq!(selection.indices_on_page(2, 10), Some(0..=9));
            assert_eq!(selection.indices_on_page(3, 10), Some(0..=2));
            assert_eq!(selection.indices_on_page(4, 10), None);
            assert_eq!(selection.indices_on_page(2, 0), None);
            assert_eq!(selection.indices_on_page(1, 4), None);
        }
    }

    #[test]
    fn text_layer_preserves_unlocated_character_offsets() {
        let layer = TextLayer::new(vec![
            bounded_char('a', 0.10, 0.10, 0.20, 0.20),
            text_char('\n'),
            bounded_char('b', 0.70, 0.70, 0.80, 0.80),
        ]);
        let page = Rect {
            x: 25.0,
            y: 40.0,
            width: 500.0,
            height: 700.0,
        };

        assert_eq!(layer.len(), 3);
        assert_eq!(layer[1].value, '\n');
        assert_eq!(
            layer.hit_test(
                page,
                page.x + page.width * 0.75,
                page.y + page.height * 0.75,
                false
            ),
            Some(2)
        );

        let invalid = TextLayer::new(vec![TextChar {
            value: 'x',
            bounds: Some(TextBounds {
                left: f32::NAN,
                top: 0.0,
                right: 1.0,
                bottom: 1.0,
            }),
        }]);
        assert_eq!(invalid.len(), 1);
        assert_eq!(invalid.spatial_index().cell_count(), 0);
        assert_eq!(invalid.hit_test(page, 100.0, 100.0, true), None);
    }

    #[test]
    fn indexed_hit_testing_matches_a_dense_linear_reference() {
        let mut characters = Vec::with_capacity(64 * 64 + 3);
        for row in 0..64 {
            for column in 0..64 {
                let index = row * 64 + column;
                if index % 113 == 0 {
                    characters.push(text_char('\n'));
                    continue;
                }
                let left = (column as f32 + 0.08) / 64.0;
                let top = (row as f32 + 0.10) / 64.0;
                characters.push(bounded_char(
                    char::from(b'a' + (index % 26) as u8),
                    left,
                    top,
                    (column as f32 + 0.72) / 64.0,
                    (row as f32 + 0.68) / 64.0,
                ));
            }
        }
        characters.push(bounded_char('|', -0.08, 0.12, -0.02, 0.88));
        characters.push(bounded_char('_', 0.25, 1.03, 0.75, 1.04));
        characters.push(bounded_char('!', 0.55, 0.50, 0.53, 0.48));

        let layer = TextLayer::new(characters);
        let (columns, rows) = layer.spatial_index().grid_size();
        assert!(columns > 1 && rows > 1);
        assert!(columns <= MAX_TEXT_GRID_AXIS && rows <= MAX_TEXT_GRID_AXIS);
        assert!(layer.spatial_index().cell_count() <= MAX_TEXT_GRID_CELLS);

        for page in [
            Rect {
                x: 30.0,
                y: 50.0,
                width: 816.0,
                height: 1_056.0,
            },
            Rect {
                x: -10.0,
                y: 5.0,
                width: 120.0,
                height: 156.0,
            },
            Rect {
                x: 100.0,
                y: 20.0,
                width: 1_200.0,
                height: 600.0,
            },
        ] {
            for query_y in 0..=20 {
                for query_x in 0..=20 {
                    let fraction_x = -0.06 + query_x as f32 * 1.12 / 20.0;
                    let fraction_y = -0.06 + query_y as f32 * 1.12 / 20.0;
                    let x = page.x + page.width * fraction_x;
                    let y = page.y + page.height * fraction_y;
                    for nearest in [false, true] {
                        assert_eq!(
                            layer.hit_test(page, x, y, nearest),
                            linear_hit_test(layer.as_slice(), page, x, y, nearest),
                            "query ({fraction_x}, {fraction_y}), nearest={nearest}, page={page:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn indexed_hits_preserve_overlap_and_nearest_ties() {
        let page = Rect {
            x: 0.0,
            y: 0.0,
            width: 1_000.0,
            height: 1_000.0,
        };
        let overlapping = TextLayer::new(vec![
            bounded_char('a', 0.10, 0.10, 0.65, 0.65),
            bounded_char('b', 0.35, 0.35, 0.90, 0.90),
        ]);
        assert_eq!(overlapping.hit_test(page, 500.0, 500.0, false), Some(0));

        let mut tied = vec![
            bounded_char('l', 0.20, 0.48, 0.25, 0.52),
            bounded_char('r', 0.75, 0.48, 0.80, 0.52),
        ];
        for index in 0..130 {
            let column = index % 13;
            tied.push(bounded_char(
                '.',
                column as f32 / 13.0,
                0.90,
                (column as f32 + 0.2) / 13.0,
                0.91,
            ));
        }
        let tied = TextLayer::new(tied);
        assert_eq!(tied.hit_test(page, 500.0, 500.0, true), Some(0));
        assert_eq!(
            tied.hit_test(page, 500.0, 500.0, true),
            linear_hit_test(tied.as_slice(), page, 500.0, 500.0, true)
        );
    }

    #[test]
    fn cell_bounds_do_not_prune_a_glyph_far_from_its_center() {
        let mut characters = vec![bounded_char('|', 0.08, -0.20, 0.12, 1.20)];
        for row in 0..10 {
            for column in 0..14 {
                characters.push(bounded_char(
                    '.',
                    0.25 + column as f32 * 0.04,
                    0.20 + row as f32 * 0.04,
                    0.26 + column as f32 * 0.04,
                    0.21 + row as f32 * 0.04,
                ));
            }
        }
        let layer = TextLayer::new(characters);
        let page = Rect {
            x: 20.0,
            y: 30.0,
            width: 900.0,
            height: 1_100.0,
        };
        let x = page.x + page.width * 0.10;
        let y = page.y + page.height * 0.98;
        assert_eq!(layer.hit_test(page, x, y, false), Some(0));
        assert_eq!(
            layer.hit_test(page, x, y, false),
            linear_hit_test(layer.as_slice(), page, x, y, false)
        );
    }

    #[test]
    fn visible_selected_query_matches_a_full_scan() {
        let mut characters = Vec::new();
        for row in 0..40 {
            for column in 0..40 {
                characters.push(bounded_char(
                    'x',
                    (column as f32 + 0.05) / 40.0,
                    (row as f32 + 0.05) / 40.0,
                    (column as f32 + 0.80) / 40.0,
                    (row as f32 + 0.75) / 40.0,
                ));
            }
        }
        let layer = TextLayer::new(characters);
        let page = Rect {
            x: 50.0,
            y: 80.0,
            width: 800.0,
            height: 1_000.0,
        };
        let viewport = Rect {
            x: 260.0,
            y: 310.0,
            width: 280.0,
            height: 330.0,
        };
        let range = 317..=1_402;
        let mut indexed = Vec::new();
        layer.for_each_visible_in_range(page, viewport, range.clone(), |index, _| {
            indexed.push(index);
        });
        indexed.sort_unstable();

        let expected = range
            .filter(|index| {
                let bounds = layer[*index].bounds.unwrap();
                let rect = Rect {
                    x: page.x + bounds.left * page.width,
                    y: page.y + bounds.top * page.height,
                    width: (bounds.right - bounds.left).max(0.0) * page.width,
                    height: (bounds.bottom - bounds.top).max(0.0) * page.height,
                };
                Rect {
                    width: rect.width.max(1.5),
                    height: rect.height.max(3.0),
                    ..rect
                }
                .intersects(viewport)
            })
            .collect::<Vec<_>>();
        assert_eq!(indexed, expected);
    }

    fn linear_hit_test(
        characters: &[TextChar],
        page: Rect,
        x: f32,
        y: f32,
        nearest: bool,
    ) -> Option<usize> {
        let mut best = None;
        for (index, character) in characters.iter().enumerate() {
            let Some(bounds) = character.bounds else {
                continue;
            };
            if !bounds.left.is_finite()
                || !bounds.top.is_finite()
                || !bounds.right.is_finite()
                || !bounds.bottom.is_finite()
            {
                continue;
            }
            let rect = Rect {
                x: page.x + bounds.left * page.width,
                y: page.y + bounds.top * page.height,
                width: (bounds.right - bounds.left).max(0.0) * page.width,
                height: (bounds.bottom - bounds.top).max(0.0) * page.height,
            };
            let padded = Rect {
                x: rect.x - 2.0,
                y: rect.y - 3.0,
                width: rect.width.max(2.0) + 4.0,
                height: rect.height.max(4.0) + 6.0,
            };
            if padded.contains(x, y) {
                return Some(index);
            }
            if nearest {
                let dx = if x < rect.x {
                    rect.x - x
                } else if x > rect.right() {
                    x - rect.right()
                } else {
                    0.0
                };
                let dy = if y < rect.y {
                    rect.y - y
                } else if y > rect.bottom() {
                    y - rect.bottom()
                } else {
                    0.0
                };
                let distance = dx * dx + dy * dy * 2.0;
                if best.is_none_or(|(_, current)| distance < current) {
                    best = Some((index, distance));
                }
            }
        }
        best.map(|(index, _)| index)
    }

    fn bounded_char(value: char, left: f32, top: f32, right: f32, bottom: f32) -> TextChar {
        TextChar {
            value,
            bounds: Some(TextBounds {
                left,
                top,
                right,
                bottom,
            }),
        }
    }

    fn text_char(value: char) -> TextChar {
        TextChar {
            value,
            bounds: None,
        }
    }

    fn eager_layout(
        page_sizes: &[PageSize],
        zoom: f32,
        viewport_width: f32,
    ) -> (Vec<Rect>, f32, f32) {
        let scale = PDF_POINTS_TO_LOGICAL_PIXELS * zoom;
        let widest_page = page_sizes
            .iter()
            .map(|page| page.width * scale)
            .fold(0.0_f32, f32::max);
        let content_width = viewport_width.max(widest_page + PAGE_MARGIN * 2.0);
        let mut y = PAGE_MARGIN;
        let mut page_rects = Vec::with_capacity(page_sizes.len());
        for page in page_sizes {
            let width = page.width * scale;
            let height = page.height * scale;
            page_rects.push(Rect {
                x: (content_width - width) * 0.5,
                y,
                width,
                height,
            });
            y += height + PAGE_GAP;
        }
        let content_height = if page_sizes.is_empty() {
            0.0
        } else {
            y - PAGE_GAP + PAGE_MARGIN
        };
        (page_rects, content_width, content_height)
    }

    fn eager_visible_pages(
        page_rects: &[Rect],
        scroll_y: f32,
        viewport_height: f32,
        overscan: f32,
    ) -> Range<usize> {
        if page_rects.is_empty() {
            return 0..0;
        }
        let top = (scroll_y - overscan).max(0.0);
        let bottom = scroll_y + viewport_height + overscan;
        let first = page_rects
            .partition_point(|rect| rect.bottom() < top)
            .min(page_rects.len());
        let end = page_rects[first..].partition_point(|rect| rect.y <= bottom) + first;
        first..end
    }

    fn eager_current_page(page_rects: &[Rect], scroll_y: f32, viewport_height: f32) -> usize {
        if page_rects.is_empty() {
            return 0;
        }
        let probe = scroll_y + viewport_height * 0.3;
        page_rects
            .partition_point(|rect| rect.bottom() < probe)
            .min(page_rects.len() - 1)
    }
}
