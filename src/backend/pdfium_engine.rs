//! PDFium-specific document loading, metadata extraction, rasterization, and text extraction.
//!
//! This module deliberately contains no worker scheduling or channel code. Its caller owns the
//! single PDFium thread and decides when work is cancelled or published.

use super::{RenderAppearance, RenderColor, TileRequest};
use crate::model::{
    PageSize, PdfLink as DocumentLink, PdfLinkTarget, PixelRect, TextBounds, TextChar, TocEntry,
};
use pdfium_render::prelude::*;
use std::path::{Path, PathBuf};
use url::Url;

pub(super) const MAX_RASTER_DIMENSION: u32 = 65_536;
pub(super) const MAX_TILE_DIMENSION: u32 = 1_088;
pub(super) const MAX_PAGE_TEXT_CHARS: usize = 100_000;

const MAX_PAGE_POINTS: f32 = 1_000_000.0;
const TEXT_CANCEL_INTERVAL: usize = 64;
const MAX_TOC_ENTRIES: usize = 512;
const MAX_TOC_DEPTH: u16 = 32;
pub(super) const MAX_TOC_TITLE_UTF16_BYTES: usize = 2_048;
const MAX_DOCUMENT_LINKS: usize = 20_000;
const MAX_LINK_URI_BYTES: usize = 8_192;

pub(super) enum TextExtraction {
    Complete(Vec<TextChar>),
    Cancelled(Vec<TextChar>),
}

pub(super) fn initialize_pdfium() -> Result<&'static Pdfium, String> {
    let library_name = Pdfium::pdfium_platform_library_name();
    let mut candidates = Vec::new();

    if let Some(configured) = std::env::var_os("PDFIUM_DYNAMIC_LIB_PATH") {
        let configured = PathBuf::from(configured);
        candidates.push(if configured.is_dir() {
            configured.join(&library_name)
        } else {
            configured
        });
    }

    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        candidates.push(directory.join(&library_name));
        candidates.push(directory.join("../Resources").join(&library_name));
    }

    candidates.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("vendor/pdfium/lib")
            .join(&library_name),
    );

    let mut failures = Vec::new();
    for candidate in candidates {
        if candidate.exists() {
            match Pdfium::bind_to_library(&candidate) {
                Ok(bindings) => return Ok(Box::leak(Box::new(Pdfium::new(bindings)))),
                Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => {
                    return Ok(Box::leak(Box::new(Pdfium::default())));
                }
                Err(error) => failures.push(format!("{} ({error})", candidate.display())),
            }
        }
    }

    match Pdfium::bind_to_system_library() {
        Ok(bindings) => Ok(Box::leak(Box::new(Pdfium::new(bindings)))),
        Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => {
            Ok(Box::leak(Box::new(Pdfium::default())))
        }
        Err(error) => {
            let detail = if failures.is_empty() {
                String::new()
            } else {
                format!(" Tried: {}.", failures.join(", "))
            };
            Err(format!(
                "PDFium is not installed. Run scripts/fetch-pdfium.sh, or set \
                 PDFIUM_DYNAMIC_LIB_PATH to {}.{detail} System lookup: {error}",
                library_name.to_string_lossy()
            ))
        }
    }
}

pub(super) fn open_document(
    pdfium: &'static Pdfium,
    path: &Path,
) -> Result<(PdfDocument<'static>, Vec<PageSize>), String> {
    let document: PdfDocument<'static> = pdfium
        .load_pdf_from_file(path, None)
        .map_err(|error| error.to_string())?;
    let pages = document
        .pages()
        .page_sizes()
        .map_err(|error| error.to_string())?
        .into_iter()
        .enumerate()
        .map(|(index, rect)| {
            let width = rect.width().value;
            let height = rect.height().value;
            if !width.is_finite()
                || !height.is_finite()
                || width <= 0.0
                || height <= 0.0
                || width > MAX_PAGE_POINTS
                || height > MAX_PAGE_POINTS
            {
                return Err(format!("page {} has invalid dimensions", index + 1));
            }
            Ok(PageSize { width, height })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok((document, pages))
}

fn destination_data(
    document: &PdfDocument<'_>,
    destination: &PdfDestination<'_>,
    page_count: usize,
) -> Option<DestinationData> {
    let page = destination
        .page_index()
        .ok()
        .and_then(|page| usize::try_from(page).ok())
        .filter(|page| *page < page_count)?;
    let page_index = i32::try_from(page).ok()?;
    let pdf_page = document.pages().get(page_index).ok()?;
    let boundary = pdf_page
        .boundaries()
        .crop()
        .or_else(|_| pdf_page.boundaries().media())
        .map(|boundary| boundary.bounds)
        .unwrap_or_else(|_| pdf_page.page_size());
    let left = boundary.left();
    let bottom = boundary.bottom();
    let point = match destination.view_settings().ok() {
        Some(PdfDestinationViewSettings::SpecificCoordinatesAndZoom(x, Some(y), _)) => {
            Some((x.unwrap_or(left), y))
        }
        Some(PdfDestinationViewSettings::FitPageHorizontallyToWindow(Some(y)))
        | Some(PdfDestinationViewSettings::FitBoundsHorizontallyToWindow(Some(y))) => {
            Some((left, y))
        }
        Some(PdfDestinationViewSettings::FitPageToRectangle(rect)) => {
            Some((rect.left(), rect.top()))
        }
        Some(PdfDestinationViewSettings::FitPageVerticallyToWindow(Some(x)))
        | Some(PdfDestinationViewSettings::FitBoundsVerticallyToWindow(Some(x))) => {
            Some((x, bottom))
        }
        _ => None,
    };
    let (x_fraction, y_fraction) = point.map_or((None, None), |(x, y)| {
        let (width, height) =
            precision_text_raster(pdf_page.width().value, pdf_page.height().value);
        let config = PdfRenderConfig::new().set_fixed_size(width, height);
        let Ok((device_x, device_y)) = pdf_page.points_to_pixels(x, y, &config) else {
            return (None, None);
        };
        (
            normalized_device_coordinate(device_x, width),
            normalized_device_coordinate(device_y, height),
        )
    });
    Some(DestinationData {
        page,
        x_fraction,
        y_fraction,
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DestinationData {
    page: usize,
    x_fraction: Option<f32>,
    y_fraction: Option<f32>,
}

fn normalized_device_coordinate(value: i32, extent: i32) -> Option<f32> {
    if extent <= 0 {
        return None;
    }
    let normalized = value as f32 / extent as f32;
    normalized.is_finite().then_some(normalized.clamp(0.0, 1.0))
}

pub(super) fn extract_table_of_contents(
    document: &PdfDocument<'_>,
    pages: &[PageSize],
) -> Vec<TocEntry> {
    let Some(root) = document.bookmarks().root() else {
        return Vec::new();
    };
    let mut pending = vec![(root, 0_u16)];
    let mut visited = std::collections::HashSet::new();
    let mut entries = Vec::new();

    while let Some((bookmark, depth)) = pending.pop() {
        if visited.len() >= MAX_TOC_ENTRIES {
            break;
        }
        if !visited.insert(bookmark.clone()) {
            continue;
        }
        if let Some(sibling) = bookmark.next_sibling() {
            pending.push((sibling, depth));
        }
        if depth < MAX_TOC_DEPTH
            && let Some(child) = bookmark.first_child()
        {
            pending.push((child, depth + 1));
        }

        let Some(title) = bookmark.title_with_limit(MAX_TOC_TITLE_UTF16_BYTES) else {
            continue;
        };
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        if title.is_empty() {
            continue;
        }
        let destination = bookmark
            .destination()
            .and_then(|destination| destination_data(document, &destination, pages.len()))
            .or_else(|| {
                let action = bookmark.action()?;
                let destination = action.as_local_destination_action()?.destination().ok()?;
                destination_data(document, &destination, pages.len())
            });
        if let Some(destination) = destination {
            entries.push(TocEntry {
                title,
                page: destination.page,
                depth,
                destination_y: destination.y_fraction,
            });
        }
    }

    entries
}

pub(super) fn extract_document_links(
    document: &PdfDocument<'_>,
    pages: &[PageSize],
) -> Vec<DocumentLink> {
    let mut result = Vec::new();
    for page_index in 0..pages.len() {
        if result.len() >= MAX_DOCUMENT_LINKS {
            break;
        }
        let Ok(pdf_page_index) = i32::try_from(page_index) else {
            break;
        };
        let Ok(page) = document.pages().get(pdf_page_index) else {
            continue;
        };
        let (width, height) = precision_text_raster(page.width().value, page.height().value);
        let config = PdfRenderConfig::new().set_fixed_size(width, height);
        for link in page
            .links()
            .iter()
            .take(MAX_DOCUMENT_LINKS.saturating_sub(result.len()))
        {
            let Some(bounds) = link
                .rect()
                .ok()
                .and_then(|rect| normalized_link_bounds(&page, rect, &config, width, height))
            else {
                continue;
            };
            let destination = link
                .destination()
                .and_then(|destination| destination_data(document, &destination, pages.len()));
            let target = destination
                .or_else(|| {
                    let action = link.action()?;
                    let destination = action.as_local_destination_action()?.destination().ok()?;
                    destination_data(document, &destination, pages.len())
                })
                .map(|destination| PdfLinkTarget::Internal {
                    page: destination.page,
                    x_fraction: destination.x_fraction,
                    y_fraction: destination.y_fraction,
                })
                .or_else(|| {
                    let action = link.action()?;
                    let uri = action.as_uri_action()?.uri().ok()?;
                    validated_link_url(&uri).map(|url| PdfLinkTarget::External { url })
                });
            let Some(target) = target else {
                continue;
            };
            result.push(DocumentLink {
                id: result.len(),
                page: page_index,
                bounds,
                target,
            });
        }
    }
    result
}

fn normalized_link_bounds(
    page: &PdfPage<'_>,
    rect: PdfRect,
    config: &PdfRenderConfig,
    width: i32,
    height: i32,
) -> Option<TextBounds> {
    let points = [
        (rect.left(), rect.top()),
        (rect.right(), rect.top()),
        (rect.right(), rect.bottom()),
        (rect.left(), rect.bottom()),
    ];
    let mut left = f32::INFINITY;
    let mut top = f32::INFINITY;
    let mut right = f32::NEG_INFINITY;
    let mut bottom = f32::NEG_INFINITY;
    for (x, y) in points {
        let (device_x, device_y) = page.points_to_pixels(x, y, config).ok()?;
        let normalized_x = normalized_device_coordinate(device_x, width)?;
        let normalized_y = normalized_device_coordinate(device_y, height)?;
        left = left.min(normalized_x);
        top = top.min(normalized_y);
        right = right.max(normalized_x);
        bottom = bottom.max(normalized_y);
    }
    (right > left && bottom > top).then_some(TextBounds {
        left,
        top,
        right,
        bottom,
    })
}

pub(super) fn validated_link_url(uri: &str) -> Option<String> {
    if uri.len() > MAX_LINK_URI_BYTES {
        return None;
    }
    let parsed = Url::parse(uri).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.to_string())
}

pub(super) type RenderOutput = (u32, u32, Vec<u8>);

fn pdfium_color(color: RenderColor) -> PdfColor {
    // pdfium-render's PdfColor encoder stores colors in Pdfium's native ABGR integer order. Swap
    // the semantic red/blue inputs so PDFium's forced-color and bitmap-clear APIs receive RGB.
    PdfColor::new(color.blue, color.green, color.red, 255)
}

pub(super) fn render_tile(
    document: &PdfDocument<'static>,
    tile: TileRequest,
    appearance: RenderAppearance,
) -> Result<RenderOutput, String> {
    validate_tile_request(tile)?;
    let page_index = i32::try_from(tile.key.page).map_err(|_| "page index is too large")?;
    let page = document
        .pages()
        .get(page_index)
        .map_err(|error| error.to_string())?;

    let full_width =
        i32::try_from(tile.key.raster.width).map_err(|_| "page raster width is too large")?;
    let full_height =
        i32::try_from(tile.key.raster.height).map_err(|_| "page raster height is too large")?;
    let render_left =
        i32::try_from(tile.render_rect.x).map_err(|_| "tile x origin is too large")?;
    let render_top = i32::try_from(tile.render_rect.y).map_err(|_| "tile y origin is too large")?;
    let render_width =
        i32::try_from(tile.render_rect.width).map_err(|_| "tile width is too large")?;
    let render_height =
        i32::try_from(tile.render_rect.height).map_err(|_| "tile height is too large")?;
    let mut config = PdfRenderConfig::new()
        .set_fixed_size(full_width, full_height)
        // GPUI's RenderImage upload path expects BGRA on macOS. Keeping
        // PDFium's native byte order avoids a tile-wide channel conversion.
        .set_reverse_byte_order(false)
        .render_annotations(true)
        .limit_render_image_cache_size(true)
        .render_form_data(true);
    if let RenderAppearance::ForcedColors {
        background,
        foreground,
    } = appearance
    {
        let foreground = pdfium_color(foreground);
        config = config
            .set_clear_color(pdfium_color(background))
            .set_color_scheme(PdfPageRenderColorScheme::new(
                foreground, foreground, foreground, foreground,
            ))
            .render_fills_as_strokes(true);
    }
    let bitmap = page
        .render_tile_with_config(
            &config,
            render_left,
            render_top,
            render_width,
            render_height,
        )
        .map_err(|error| error.to_string())?;
    let rendered_width = u32::try_from(bitmap.width()).map_err(|_| "invalid tile width")?;
    let rendered_height = u32::try_from(bitmap.height()).map_err(|_| "invalid tile height")?;
    if rendered_width != tile.render_rect.width || rendered_height != tile.render_rect.height {
        return Err("PDFium returned an unexpected tile size".into());
    }
    let bgra = bitmap.as_raw_bytes();
    let expected_len = rendered_width
        .checked_mul(rendered_height)
        .and_then(|pixels| pixels.checked_mul(4))
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or("render tile byte count overflows")?;
    if bgra.len() != expected_len {
        return Err("PDFium returned an invalid tile buffer".into());
    }

    Ok((rendered_width, rendered_height, bgra))
}

pub(super) fn precision_text_raster(page_width: f32, page_height: f32) -> (i32, i32) {
    let longest = page_width.max(page_height).max(f32::MIN_POSITIVE);
    let scaled = |dimension: f32| {
        ((dimension / longest) * MAX_RASTER_DIMENSION as f32)
            .round()
            .clamp(1.0, MAX_RASTER_DIMENSION as f32) as i32
    };
    (scaled(page_width), scaled(page_height))
}

pub(super) fn validate_tile_request(tile: TileRequest) -> Result<(), String> {
    let raster = tile.key.raster;
    if raster.width == 0
        || raster.height == 0
        || raster.width > MAX_RASTER_DIMENSION
        || raster.height > MAX_RASTER_DIMENSION
    {
        return Err("page raster dimensions are outside the supported range".into());
    }
    if tile.core_rect.width == 0
        || tile.core_rect.height == 0
        || tile.render_rect.width == 0
        || tile.render_rect.height == 0
        || tile.render_rect.width > MAX_TILE_DIMENSION
        || tile.render_rect.height > MAX_TILE_DIMENSION
    {
        return Err("tile dimensions are outside the supported range".into());
    }

    let core_right = rect_right(tile.core_rect).ok_or("tile core overflows")?;
    let core_bottom = rect_bottom(tile.core_rect).ok_or("tile core overflows")?;
    let render_right = rect_right(tile.render_rect).ok_or("render tile overflows")?;
    let render_bottom = rect_bottom(tile.render_rect).ok_or("render tile overflows")?;
    if core_right > raster.width
        || core_bottom > raster.height
        || render_right > raster.width
        || render_bottom > raster.height
        || tile.render_rect.x > tile.core_rect.x
        || tile.render_rect.y > tile.core_rect.y
        || render_right < core_right
        || render_bottom < core_bottom
    {
        return Err("tile lies outside its page raster".into());
    }
    Ok(())
}

fn rect_right(rect: PixelRect) -> Option<u32> {
    rect.x.checked_add(rect.width)
}

fn rect_bottom(rect: PixelRect) -> Option<u32> {
    rect.y.checked_add(rect.height)
}

pub(super) fn extract_page_text(
    document: &PdfDocument<'static>,
    page: usize,
    mut extracted: Vec<TextChar>,
    mut should_cancel: impl FnMut() -> bool,
) -> Result<TextExtraction, String> {
    let page_index = i32::try_from(page).map_err(|_| "page index is too large")?;
    let page = document
        .pages()
        .get(page_index)
        .map_err(|error| error.to_string())?;
    if should_cancel() {
        return Ok(TextExtraction::Cancelled(extracted));
    }

    // FPDFText_LoadPage is synchronous, but checking immediately before and
    // after it prevents the much longer per-character walk from delaying a
    // replacement viewport.
    let text = page.text().map_err(|error| error.to_string())?;
    if should_cancel() {
        return Ok(TextExtraction::Cancelled(extracted));
    }
    let character_count = validate_text_character_count(text.len())?;
    if extracted.len() > character_count {
        extracted.clear();
    }
    let (text_width, text_height) = precision_text_raster(page.width().value, page.height().value);
    let config = PdfRenderConfig::new().set_fixed_size(text_width, text_height);
    let pixel_width = u32::try_from(text_width).map_err(|_| "invalid text coordinate width")?;
    let pixel_height = u32::try_from(text_height).map_err(|_| "invalid text coordinate height")?;
    extracted.reserve(character_count.saturating_sub(extracted.len()));

    for index in extracted.len()..character_count {
        if index.is_multiple_of(TEXT_CANCEL_INTERVAL) && should_cancel() {
            return Ok(TextExtraction::Cancelled(extracted));
        }
        // SAFETY: `character_count` came from this live PdfPageText, was
        // validated against our 100k cap, and the loop stays strictly below it.
        let character = unsafe { text.char_at_unchecked(index) };
        let bounds = character.loose_bounds().ok().and_then(|bounds| {
            let top_left = page
                .points_to_pixels(bounds.left(), bounds.top(), &config)
                .ok()?;
            let top_right = page
                .points_to_pixels(bounds.right(), bounds.top(), &config)
                .ok()?;
            let bottom_left = page
                .points_to_pixels(bounds.left(), bounds.bottom(), &config)
                .ok()?;
            let bottom_right = page
                .points_to_pixels(bounds.right(), bounds.bottom(), &config)
                .ok()?;
            normalized_text_bounds(
                [top_left, top_right, bottom_left, bottom_right],
                pixel_width,
                pixel_height,
            )
        });
        extracted.push(TextChar {
            value: if character.unicode_value() == 0 {
                '\0'
            } else {
                character.unicode_char().unwrap_or('\0')
            },
            bounds,
        });
    }

    if should_cancel() {
        Ok(TextExtraction::Cancelled(extracted))
    } else {
        Ok(TextExtraction::Complete(extracted))
    }
}

pub(super) fn validate_text_character_count(count: i32) -> Result<usize, String> {
    let count = usize::try_from(count).map_err(|_| "PDFium returned a negative character count")?;
    if count > MAX_PAGE_TEXT_CHARS {
        Err(format!(
            "the page text layer has {count} characters; the safety limit is {MAX_PAGE_TEXT_CHARS}"
        ))
    } else {
        Ok(count)
    }
}

pub(super) fn normalized_text_bounds(
    pixels: [(i32, i32); 4],
    pixel_width: u32,
    pixel_height: u32,
) -> Option<TextBounds> {
    if pixel_width == 0 || pixel_height == 0 {
        return None;
    }
    let mut left = pixels.iter().map(|(x, _)| *x).min()? as f32 / pixel_width as f32;
    let mut right = pixels.iter().map(|(x, _)| *x).max()? as f32 / pixel_width as f32;
    let mut top = pixels.iter().map(|(_, y)| *y).min()? as f32 / pixel_height as f32;
    let mut bottom = pixels.iter().map(|(_, y)| *y).max()? as f32 / pixel_height as f32;
    if ![left, right, top, bottom].into_iter().all(f32::is_finite) {
        return None;
    }
    left = left.clamp(0.0, 1.0);
    right = right.clamp(0.0, 1.0);
    top = top.clamp(0.0, 1.0);
    bottom = bottom.clamp(0.0, 1.0);
    (right > left && bottom > top).then_some(TextBounds {
        left,
        top,
        right,
        bottom,
    })
}
