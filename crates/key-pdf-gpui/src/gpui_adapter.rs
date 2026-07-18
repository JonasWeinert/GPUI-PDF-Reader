use crate::{
    InputDisposition, PdfReaderAppearance, PdfReaderConfig, PdfReaderEvent, ViewportController,
    ViewportError,
};
use gpui::{Context, EventEmitter, Rgba};
use key_pdf_core::PageSize;
use key_pdf_runtime::{ColorMode, PixelColor};
use key_ui_gpui::ThemeTokens;
use std::sync::Arc;

/// Thin GPUI entity adapter around [`ViewportController`].
///
/// Pair this state adapter with [`crate::pdf_canvas`] for base rendering. Image
/// resources and feature overlays remain caller-supplied because their
/// lifetimes differ between applications. Mutations made through
/// [`update_controller`](Self::update_controller) emit every typed controller
/// event through GPUI's subscription system.
pub struct PdfViewport {
    controller: ViewportController,
}

impl PdfViewport {
    #[must_use]
    pub fn new(config: PdfReaderConfig) -> Self {
        Self {
            controller: ViewportController::new(config),
        }
    }

    pub fn with_document(
        config: PdfReaderConfig,
        pages: impl Into<Arc<[PageSize]>>,
    ) -> Result<Self, ViewportError> {
        Ok(Self {
            controller: ViewportController::with_document(config, pages)?,
        })
    }

    #[must_use]
    pub const fn controller(&self) -> &ViewportController {
        &self.controller
    }

    /// Applies a mutation and forwards its typed events to GPUI subscribers.
    pub fn update_controller<R>(
        &mut self,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut ViewportController) -> R,
    ) -> R {
        let result = update(&mut self.controller);
        self.emit_pending(cx);
        result
    }

    /// Converts shared semantic theme tokens into PDF canvas appearance and
    /// optionally asks the engine adapter for forced dark paper colors.
    pub fn apply_theme(
        &mut self,
        tokens: &ThemeTokens,
        force_dark_pdf: bool,
        cx: &mut Context<Self>,
    ) -> InputDisposition {
        let appearance = appearance_from_theme(tokens, force_dark_pdf);
        let disposition = self.controller.set_appearance(appearance);
        self.emit_pending(cx);
        disposition
    }

    /// Emits events accumulated before the adapter was inserted into an entity.
    pub fn emit_pending(&mut self, cx: &mut Context<Self>) {
        let events = self.controller.drain_events().collect::<Vec<_>>();
        for event in events {
            cx.emit(event);
        }
        cx.notify();
    }
}

impl EventEmitter<PdfReaderEvent> for PdfViewport {}

/// Maps the common Key semantic theme onto the domain-specific PDF surfaces.
#[must_use]
pub fn appearance_from_theme(tokens: &ThemeTokens, force_dark_pdf: bool) -> PdfReaderAppearance {
    let pane = color(tokens.surface.canvas);
    let mut paper = color(tokens.surface.background);
    if paper == pane {
        let luminance = (u16::from(pane.red) + u16::from(pane.green) + u16::from(pane.blue)) / 3;
        let shift = if luminance < 128 { 14 } else { -14 };
        paper.red = shift_channel(paper.red, shift);
        paper.green = shift_channel(paper.green, shift);
        paper.blue = shift_channel(paper.blue, shift);
    }
    let foreground = color(tokens.content.primary);
    PdfReaderAppearance {
        pane_background: pane,
        paper_background: paper,
        paper_border: color(tokens.surface.border),
        selection: color(tokens.selection),
        render_color_mode: if force_dark_pdf {
            ColorMode::Forced {
                background: PixelColor::from(paper),
                foreground: PixelColor::from(foreground),
            }
        } else {
            ColorMode::Original
        },
    }
}

fn color(value: gpui::Hsla) -> crate::ViewportColor {
    let value = Rgba::from(value);
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    crate::ViewportColor::rgba(
        channel(value.r),
        channel(value.g),
        channel(value.b),
        channel(value.a),
    )
}

fn shift_channel(channel: u8, shift: i16) -> u8 {
    (i16::from(channel) + shift).clamp(0, 255) as u8
}
