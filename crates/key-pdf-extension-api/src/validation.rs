use std::collections::BTreeSet;

use key_extension_api::GenerationId;

use crate::{
    DocumentMetadataSnapshot, GenerationScope, GenerationScoped, InputErrorKind, LinkTarget,
    NavigationRequest, NormalizedPoint, NormalizedRect, OutlineEntryId, OutlineSnapshot,
    OverlayBatch, PageLinksSnapshot, PageMetadataSnapshot, PageTextRequest, PageTextSnapshot,
    PdfExtensionError, PdfExtensionResult, SelectionSnapshot, TextLocation, TextSelectionRange,
    ViewportSnapshot,
};

/// Default contract bounds enforced before retaining or dispatching data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PdfContractLimits {
    /// Maximum bytes in one metadata, label, or hint string.
    pub maximum_string_bytes: usize,
    /// Maximum bytes in one external link URI.
    pub maximum_uri_bytes: usize,
    /// Maximum outline entries in one snapshot.
    pub maximum_outline_entries: usize,
    /// Maximum outline hierarchy depth.
    pub maximum_outline_depth: u16,
    /// Maximum links in one page snapshot.
    pub maximum_links_per_page: usize,
    /// Maximum character count in one text request or response.
    pub maximum_text_characters: usize,
    /// Maximum selected-text bytes in one snapshot.
    pub maximum_selection_text_bytes: usize,
    /// Maximum visible or selected pages in one snapshot.
    pub maximum_snapshot_pages: usize,
    /// Maximum overlay items in one atomic replacement.
    pub maximum_overlays: usize,
    /// Maximum regions in one overlay or link.
    pub maximum_regions_per_item: usize,
    /// Maximum total regions in one overlay batch or selection snapshot.
    pub maximum_total_regions: usize,
}

impl Default for PdfContractLimits {
    fn default() -> Self {
        Self {
            maximum_string_bytes: 16 * 1024,
            maximum_uri_bytes: 16 * 1024,
            maximum_outline_entries: 20_000,
            maximum_outline_depth: 64,
            maximum_links_per_page: 20_000,
            maximum_text_characters: 100_000,
            maximum_selection_text_bytes: 4 * 1024 * 1024,
            maximum_snapshot_pages: 1_024,
            maximum_overlays: 2_048,
            maximum_regions_per_item: 64,
            maximum_total_regions: 8_192,
        }
    }
}

/// Document context used to validate untrusted requests and transport data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PdfValidationContext {
    /// Generation currently owned by the host.
    pub generation: GenerationId,
    /// Number of pages in the current document.
    pub page_count: u32,
    /// Negotiated bounds for the calling extension.
    pub limits: PdfContractLimits,
}

impl PdfValidationContext {
    /// Creates a validation context with default bounds.
    #[must_use]
    pub fn new(generation: GenerationId, page_count: u32) -> Self {
        Self {
            generation,
            page_count,
            limits: PdfContractLimits::default(),
        }
    }

    /// Creates a validation context with host-negotiated bounds.
    #[must_use]
    pub const fn with_limits(
        generation: GenerationId,
        page_count: u32,
        limits: PdfContractLimits,
    ) -> Self {
        Self {
            generation,
            page_count,
            limits,
        }
    }

    /// Checks a generation-scoped handle against the active document.
    ///
    /// # Errors
    ///
    /// Returns `StaleGeneration` when the handle belongs to another generation.
    pub fn validate_handle<T: GenerationScoped>(&self, handle: &T) -> PdfExtensionResult<()> {
        GenerationScope::new(self.generation).validate(handle)
    }

    /// Checks a zero-based page index against the active document.
    ///
    /// # Errors
    ///
    /// Returns `PageOutOfBounds` when the page does not exist.
    pub fn validate_page(&self, _field: &str, page: crate::PageIndex) -> PdfExtensionResult<()> {
        if page.0 < self.page_count {
            Ok(())
        } else {
            Err(PdfExtensionError::PageOutOfBounds {
                page: page.0,
                page_count: self.page_count,
            })
        }
    }
}

/// Validation implemented by bounded PDF request and snapshot types.
pub trait ValidatePdfContract {
    /// Validates generation, page bounds, geometry, strings, and collection limits.
    ///
    /// # Errors
    ///
    /// Returns the first structured contract violation in deterministic order.
    fn validate_contract(&self, context: &PdfValidationContext) -> PdfExtensionResult<()>;
}

impl ValidatePdfContract for DocumentMetadataSnapshot {
    fn validate_contract(&self, context: &PdfValidationContext) -> PdfExtensionResult<()> {
        context.validate_handle(&self.document)?;
        if self.page_count != context.page_count {
            return invalid("document.page_count", InputErrorKind::InvalidIdentifier);
        }
        for (field, value) in [
            ("document.title", self.title.as_deref()),
            ("document.author", self.author.as_deref()),
            ("document.subject", self.subject.as_deref()),
            ("document.keywords", self.keywords.as_deref()),
            ("document.creator", self.creator.as_deref()),
            ("document.producer", self.producer.as_deref()),
            ("document.language", self.language.as_deref()),
            ("document.format_version", self.format_version.as_deref()),
        ] {
            optional_string(field, value, context.limits.maximum_string_bytes)?;
        }
        Ok(())
    }
}

impl ValidatePdfContract for PageMetadataSnapshot {
    fn validate_contract(&self, context: &PdfValidationContext) -> PdfExtensionResult<()> {
        context.validate_handle(&self.document)?;
        context.validate_handle(&self.page)?;
        context.validate_page("page.index", self.index)?;
        optional_string(
            "page.label",
            self.label.as_deref(),
            context.limits.maximum_string_bytes,
        )?;
        positive_finite("page.width_points", self.width_points)?;
        positive_finite("page.height_points", self.height_points)
    }
}

impl ValidatePdfContract for OutlineSnapshot {
    fn validate_contract(&self, context: &PdfValidationContext) -> PdfExtensionResult<()> {
        context.validate_handle(&self.document)?;
        limit(
            "outline.entries",
            self.entries.len(),
            context.limits.maximum_outline_entries,
        )?;
        let mut ids = std::collections::BTreeMap::<OutlineEntryId, u16>::new();
        for (index, entry) in self.entries.iter().enumerate() {
            let base = format!("outline.entries[{index}]");
            if entry.id.0 == 0 || ids.contains_key(&entry.id) {
                return invalid(format!("{base}.id"), InputErrorKind::InvalidIdentifier);
            }
            if entry.depth > context.limits.maximum_outline_depth {
                return Err(PdfExtensionError::LimitExceeded {
                    field: format!("{base}.depth"),
                    limit: u64::from(context.limits.maximum_outline_depth),
                    actual: u64::from(entry.depth),
                });
            }
            let valid_parent = match (entry.depth, entry.parent) {
                (0, None) => true,
                (0, Some(_)) | (_, None) => false,
                (depth, Some(parent)) => ids
                    .get(&parent)
                    .is_some_and(|parent_depth| parent_depth.checked_add(1) == Some(depth)),
            };
            if !valid_parent {
                return invalid(format!("{base}.parent"), InputErrorKind::InvalidIdentifier);
            }
            required_string(
                &format!("{base}.title"),
                &entry.title,
                context.limits.maximum_string_bytes,
            )?;
            validate_destination(&entry.destination, context, &format!("{base}.destination"))?;
            ids.insert(entry.id, entry.depth);
        }
        Ok(())
    }
}

impl ValidatePdfContract for PageLinksSnapshot {
    fn validate_contract(&self, context: &PdfValidationContext) -> PdfExtensionResult<()> {
        context.validate_handle(&self.page)?;
        context.validate_page("links.page", self.index)?;
        limit(
            "links.items",
            self.links.len(),
            context.limits.maximum_links_per_page,
        )?;
        let mut ids = BTreeSet::new();
        for (index, link) in self.links.iter().enumerate() {
            let base = format!("links.items[{index}]");
            if link.id.0 == 0 || !ids.insert(link.id) {
                return invalid(format!("{base}.id"), InputErrorKind::InvalidIdentifier);
            }
            validate_regions(
                &format!("{base}.regions"),
                &link.regions,
                context.limits.maximum_regions_per_item,
                false,
            )?;
            if let Some(range) = link.source_range {
                validate_range(&format!("{base}.source_range"), range, context)?;
            }
            match &link.target {
                LinkTarget::Internal { destination } => {
                    validate_destination(destination, context, &format!("{base}.target"))?;
                }
                LinkTarget::External { uri } => required_string(
                    &format!("{base}.target.uri"),
                    uri,
                    context.limits.maximum_uri_bytes,
                )?,
            }
        }
        Ok(())
    }
}

impl ValidatePdfContract for PageTextRequest {
    fn validate_contract(&self, context: &PdfValidationContext) -> PdfExtensionResult<()> {
        context.validate_handle(&self.page)?;
        if self.maximum_characters == 0 {
            return invalid("text.maximum_characters", InputErrorKind::Empty);
        }
        limit(
            "text.maximum_characters",
            self.maximum_characters as usize,
            context.limits.maximum_text_characters,
        )
    }
}

impl ValidatePdfContract for PageTextSnapshot {
    fn validate_contract(&self, context: &PdfValidationContext) -> PdfExtensionResult<()> {
        context.validate_handle(&self.page)?;
        context.validate_page("text.page", self.index)?;
        limit(
            "text.characters",
            self.characters.len(),
            context.limits.maximum_text_characters,
        )?;
        let returned =
            u32::try_from(self.characters.len()).map_err(|_| PdfExtensionError::LimitExceeded {
                field: "text.characters".into(),
                limit: u64::from(u32::MAX),
                actual: u64::MAX,
            })?;
        if self.start > self.total_characters
            || self
                .start
                .checked_add(returned)
                .is_none_or(|end| end > self.total_characters)
            || self.complete && self.start + returned != self.total_characters
        {
            return invalid("text.range", InputErrorKind::ReversedRange);
        }
        for (index, character) in self.characters.iter().enumerate() {
            if char::from_u32(character.scalar).is_none() {
                return invalid(
                    format!("text.characters[{index}].scalar"),
                    InputErrorKind::InvalidUnicodeScalar,
                );
            }
            if let Some(bounds) = character.bounds {
                validate_rect(&format!("text.characters[{index}].bounds"), bounds)?;
            }
        }
        Ok(())
    }
}

impl ValidatePdfContract for SelectionSnapshot {
    fn validate_contract(&self, context: &PdfValidationContext) -> PdfExtensionResult<()> {
        context.validate_handle(&self.document)?;
        if let Some(range) = self.range {
            validate_range("selection.range", range, context)?;
        } else if self.text.is_some() || !self.pages.is_empty() {
            return invalid("selection.range", InputErrorKind::Empty);
        }
        optional_string(
            "selection.text",
            self.text.as_deref(),
            context.limits.maximum_selection_text_bytes,
        )?;
        limit(
            "selection.pages",
            self.pages.len(),
            context.limits.maximum_snapshot_pages,
        )?;
        let mut total_regions = 0_usize;
        let mut previous_page = None;
        for (index, page) in self.pages.iter().enumerate() {
            context.validate_handle(&page.page)?;
            context.validate_page(&format!("selection.pages[{index}].index"), page.index)?;
            if previous_page.is_some_and(|previous| page.index <= previous) {
                return invalid(
                    format!("selection.pages[{index}].index"),
                    InputErrorKind::InvalidIdentifier,
                );
            }
            previous_page = Some(page.index);
            validate_regions(
                &format!("selection.pages[{index}].regions"),
                &page.regions,
                context.limits.maximum_regions_per_item,
                true,
            )?;
            total_regions = total_regions.saturating_add(page.regions.len());
        }
        limit(
            "selection.total_regions",
            total_regions,
            context.limits.maximum_total_regions,
        )
    }
}

impl ValidatePdfContract for ViewportSnapshot {
    fn validate_contract(&self, context: &PdfValidationContext) -> PdfExtensionResult<()> {
        context.validate_handle(&self.document)?;
        positive_finite("viewport.zoom_ratio", self.zoom_ratio)?;
        if let Some(anchor) = self.anchor {
            context.validate_page("viewport.anchor.page", anchor.page)?;
            validate_point("viewport.anchor.point", anchor.point)?;
        }
        limit(
            "viewport.visible_pages",
            self.visible_pages.len(),
            context.limits.maximum_snapshot_pages,
        )?;
        let mut previous_page = None;
        for (index, page) in self.visible_pages.iter().enumerate() {
            context.validate_handle(&page.page)?;
            context.validate_page(
                &format!("viewport.visible_pages[{index}].index"),
                page.index,
            )?;
            if previous_page.is_some_and(|previous| page.index <= previous) {
                return invalid(
                    format!("viewport.visible_pages[{index}].index"),
                    InputErrorKind::InvalidIdentifier,
                );
            }
            previous_page = Some(page.index);
            validate_rect(
                &format!("viewport.visible_pages[{index}].visible_area"),
                page.visible_area,
            )?;
        }
        Ok(())
    }
}

impl ValidatePdfContract for NavigationRequest {
    fn validate_contract(&self, context: &PdfValidationContext) -> PdfExtensionResult<()> {
        context.validate_handle(&self.document)?;
        validate_destination(&self.destination, context, "navigation.destination")?;
        normalized(
            "navigation.placement.viewport_anchor_y",
            self.placement.viewport_anchor_y,
        )?;
        if let Some(focus) = &self.focus {
            validate_regions(
                "navigation.focus.regions",
                &focus.regions,
                context.limits.maximum_regions_per_item,
                true,
            )?;
        }
        Ok(())
    }
}

impl ValidatePdfContract for OverlayBatch {
    fn validate_contract(&self, context: &PdfValidationContext) -> PdfExtensionResult<()> {
        context.validate_handle(&self.document)?;
        limit(
            "overlays",
            self.overlays.len(),
            context.limits.maximum_overlays,
        )?;
        let mut ids = BTreeSet::new();
        let mut total_regions = 0_usize;
        for (index, overlay) in self.overlays.iter().enumerate() {
            let base = format!("overlays[{index}]");
            context.validate_page(&format!("{base}.page"), overlay.page)?;
            if !ids.insert(overlay.id.as_str()) {
                return invalid(format!("{base}.id"), InputErrorKind::InvalidIdentifier);
            }
            validate_regions(
                &format!("{base}.regions"),
                &overlay.regions,
                context.limits.maximum_regions_per_item,
                false,
            )?;
            total_regions = total_regions.saturating_add(overlay.regions.len());
            optional_string(
                &format!("{base}.label"),
                overlay.label.as_deref(),
                context.limits.maximum_string_bytes,
            )?;
        }
        limit(
            "overlays.total_regions",
            total_regions,
            context.limits.maximum_total_regions,
        )
    }
}

fn validate_destination(
    destination: &crate::DocumentDestination,
    context: &PdfValidationContext,
    path: &str,
) -> PdfExtensionResult<()> {
    context.validate_page(&format!("{path}.page"), destination.page)?;
    if let Some(x) = destination.x_fraction {
        normalized(&format!("{path}.x_fraction"), x)?;
    }
    if let Some(y) = destination.y_fraction {
        normalized(&format!("{path}.y_fraction"), y)?;
    }
    if let Some(range) = destination.text_range {
        validate_range(&format!("{path}.text_range"), range, context)?;
    }
    if let Some(hint) = &destination.text_hint {
        required_string(
            &format!("{path}.text_hint.text"),
            &hint.text,
            context.limits.maximum_string_bytes,
        )?;
    }
    Ok(())
}

fn validate_range(
    path: &str,
    range: TextSelectionRange,
    context: &PdfValidationContext,
) -> PdfExtensionResult<()> {
    validate_location(&format!("{path}.anchor"), range.anchor, context)?;
    validate_location(&format!("{path}.focus"), range.focus, context)
}

fn validate_location(
    field: &str,
    location: TextLocation,
    context: &PdfValidationContext,
) -> PdfExtensionResult<()> {
    context.validate_page(field, location.page)
}

fn validate_regions(
    field: &str,
    regions: &[NormalizedRect],
    maximum: usize,
    allow_empty: bool,
) -> PdfExtensionResult<()> {
    if !allow_empty && regions.is_empty() {
        return invalid(field, InputErrorKind::Empty);
    }
    limit(field, regions.len(), maximum)?;
    for (index, region) in regions.iter().copied().enumerate() {
        validate_rect(&format!("{field}[{index}]"), region)?;
    }
    Ok(())
}

fn validate_rect(field: &str, rect: NormalizedRect) -> PdfExtensionResult<()> {
    for (name, value) in [
        ("left", rect.left),
        ("top", rect.top),
        ("right", rect.right),
        ("bottom", rect.bottom),
    ] {
        normalized(&format!("{field}.{name}"), value)?;
    }
    if rect.right <= rect.left || rect.bottom <= rect.top {
        invalid(field, InputErrorKind::EmptyOrReversedRect)
    } else {
        Ok(())
    }
}

fn validate_point(field: &str, point: NormalizedPoint) -> PdfExtensionResult<()> {
    normalized(&format!("{field}.x"), point.x)?;
    normalized(&format!("{field}.y"), point.y)
}

fn normalized(field: &str, value: f32) -> PdfExtensionResult<()> {
    if !value.is_finite() {
        invalid(field, InputErrorKind::NotFinite)
    } else if !(0.0..=1.0).contains(&value) {
        invalid(field, InputErrorKind::OutsideNormalizedRange)
    } else {
        Ok(())
    }
}

fn positive_finite(field: &str, value: f32) -> PdfExtensionResult<()> {
    if !value.is_finite() {
        invalid(field, InputErrorKind::NotFinite)
    } else if value <= 0.0 {
        invalid(field, InputErrorKind::Empty)
    } else {
        Ok(())
    }
}

fn required_string(field: &str, value: &str, maximum: usize) -> PdfExtensionResult<()> {
    if value.trim().is_empty() {
        invalid(field, InputErrorKind::Empty)
    } else {
        limit(field, value.len(), maximum)
    }
}

fn optional_string(field: &str, value: Option<&str>, maximum: usize) -> PdfExtensionResult<()> {
    value.map_or(Ok(()), |value| limit(field, value.len(), maximum))
}

fn limit(field: &str, actual: usize, maximum: usize) -> PdfExtensionResult<()> {
    if actual <= maximum {
        Ok(())
    } else {
        Err(PdfExtensionError::LimitExceeded {
            field: field.into(),
            limit: u64::try_from(maximum).unwrap_or(u64::MAX),
            actual: u64::try_from(actual).unwrap_or(u64::MAX),
        })
    }
}

fn invalid<T>(field: impl Into<String>, reason: InputErrorKind) -> PdfExtensionResult<T> {
    Err(PdfExtensionError::InvalidInput {
        field: field.into(),
        reason,
    })
}
