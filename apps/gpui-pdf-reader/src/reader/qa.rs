use super::*;

#[cfg(debug_assertions)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct QaReaderResourceSnapshot {
    pub(crate) activity: ActivityLevel,
    pub(crate) allocated_cpu_bytes: u64,
    pub(crate) allocated_gpu_bytes: u64,
    pub(crate) allocated_workers: u64,
    pub(crate) cached_tile_bytes: u64,
    pub(crate) estimated_tile_resident_bytes: u64,
    pub(crate) tile_cache_limit_bytes: u64,
    pub(crate) base_tile_cache_limit_bytes: u64,
    pub(crate) cached_tiles: u64,
    pub(crate) pending_tiles: u64,
    pub(crate) cached_text_pages: u64,
    pub(crate) cache_retention_percent: u64,
    pub(crate) cache_trimmed: bool,
    pub(crate) worker_hibernated: bool,
}

#[cfg(debug_assertions)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum QaFeaturePhase {
    Seed,
    WaitCommentEditor,
    WaitCommentEdited,
    WaitCommentSaved,
    WaitCommentBack,
    WaitCommentList,
    WaitCommentsOpen,
    WaitCommentsClosed,
    WaitSearchOpen,
    WaitSearch,
    WaitNavigation,
    WaitSearchReturn,
    WaitSearchClosed,
    WaitSearchReopened,
    WaitSearchRepopulated,
    WaitFinalNavigation,
    Complete,
}

#[cfg(debug_assertions)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum QaFluidPhase {
    Seed,
    WaitEditor,
    WaitAutosave,
    WaitList,
    WaitReopenedEditor,
    WaitFinalList,
    WaitSearchOpen,
    WaitSearchResults,
    Complete,
}

#[cfg(debug_assertions)]
#[cfg_attr(not(feature = "installable-extensions"), allow(dead_code))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum QaExtensionPhase {
    Seed,
    WaitReference,
    WaitReferencePanel,
    WaitRestore,
    WaitRestorePanel,
    WaitAdversarial,
    Complete,
}

impl PdfReader {
    #[cfg(debug_assertions)]
    pub(crate) fn qa_resource_snapshot(&self) -> QaReaderResourceSnapshot {
        let cached_tile_bytes = self
            .rendered
            .values()
            .map(|tile| tile.byte_len as u64)
            .sum::<u64>();
        let (tile_cache_limit_bytes, _) = self.effective_tile_cache_limits();
        QaReaderResourceSnapshot {
            activity: self.resource_allocation.activity,
            allocated_cpu_bytes: self.resource_allocation.amount.cpu_memory_bytes,
            allocated_gpu_bytes: self.resource_allocation.amount.gpu_memory_bytes,
            allocated_workers: self.resource_allocation.amount.worker_slots,
            cached_tile_bytes,
            estimated_tile_resident_bytes: cached_tile_bytes.saturating_mul(2),
            tile_cache_limit_bytes: tile_cache_limit_bytes as u64,
            base_tile_cache_limit_bytes: self.max_cache_bytes as u64,
            cached_tiles: self.rendered.len() as u64,
            pending_tiles: self.pending.len() as u64,
            cached_text_pages: self.page_text.len() as u64,
            cache_retention_percent: self.idle_cache_retention_percent as u64,
            cache_trimmed: self.idle_cache_trimmed,
            worker_hibernated: self.worker_hibernated,
        }
    }

    #[cfg(debug_assertions)]
    pub fn qa_report(&self) -> String {
        let cached_bytes = self
            .rendered
            .values()
            .map(|tile| tile.byte_len)
            .sum::<usize>();
        let visible_exact: Vec<_> = self
            .render_viewport
            .iter()
            .filter_map(|(key, tier)| (*tier == DemandTier::Visible).then_some(*key))
            .collect();
        let exact_cached = visible_exact
            .iter()
            .filter(|key| self.rendered.contains_key(key))
            .count();
        let visible_pages = visible_exact
            .iter()
            .map(|key| key.page)
            .collect::<HashSet<_>>()
            .len();
        let max_tile_bytes = self
            .rendered
            .values()
            .map(|tile| tile.byte_len)
            .max()
            .unwrap_or(0);
        let mut highlight_colors = HashSet::new();
        let mut highlight_count = 0;
        let mut comment_count = 0;
        if let Some(annotations) = self.annotations.as_ref() {
            for annotation in annotations.iter() {
                if let Some(color) = annotation.highlight() {
                    highlight_count += 1;
                    highlight_colors.insert(color);
                }
                comment_count += usize::from(annotation.comment_markdown().is_some());
            }
        }
        let active_search = self
            .search
            .active
            .and_then(|active| self.search.order.iter().position(|id| *id == active))
            .map_or(0, |index| index + 1);
        let theme_name = self
            .selected_theme
            .as_ref()
            .map(|name| name.as_ref())
            .unwrap_or_else(|| self.theme_preference.name());
        let (link_preview, link_preview_state) = self
            .previewed_link
            .and_then(|id| {
                let link = self
                    .document
                    .as_ref()?
                    .links
                    .iter()
                    .find(|link| link.id == id)?;
                let state = match &link.target {
                    PdfLinkTarget::Internal { .. } => {
                        self.resolved_internal_link(id)
                            .map_or("internal-loading", |resolved| {
                                if resolved.matched_source {
                                    "internal-matched"
                                } else {
                                    "internal-fallback"
                                }
                            })
                    }
                    PdfLinkTarget::External { url } => match self
                        .link_preview_session
                        .as_ref()
                        .and_then(|session| session.website(url))
                    {
                        Some(WebsitePreviewState::Loading) => "external-loading",
                        Some(WebsitePreviewState::Ready(_)) => "external-ready",
                        Some(WebsitePreviewState::Failed(_)) => "external-failed",
                        None => "external-unavailable",
                    },
                };
                Some((id + 1, state))
            })
            .unwrap_or((0, "none"));
        let scholarly_state = self
            .current_reference_text()
            .and_then(|reference| self.scholarly_session.state(&reference))
            .map_or("none", |state| match state {
                ScholarlyMetadataState::Loading => "loading",
                ScholarlyMetadataState::Ready(_) => "ready",
                ScholarlyMetadataState::Failed(_) => "failed",
            });
        let citation_source = self
            .previewed_link
            .and_then(|id| {
                let document = self.document.as_ref()?;
                let link = document.links.iter().find(|link| link.id == id)?;
                let text = self.page_text.get(&link.page)?;
                Some(link_source_text(text, link.bounds))
            })
            .filter(|source| !source.is_empty())
            .map(|source| source.split_whitespace().collect::<Vec<_>>().join("_"))
            .unwrap_or_else(|| "none".to_owned());
        let (extension_packages, extension_active, extension_suspended, extension_failed) =
            self.qa_extension_package_counts();
        let extension_panel = self
            .extension_contribution
            .as_ref()
            .map_or_else(|| "none".to_owned(), |pane| pane.owner.to_string());
        format!(
            "GPUI_PDF_READER_QA view={:?} theme={} pdf_render={} pdf_dark_enabled={} toc={} links={} link_navigations={} link_preview={} reference_preview={} reference_group={} citation_source={} link_preview_state={} scholarly={} scientific={}/{} references={} dois={} bracket_citations={} superscript_citations={} toc_hover={} toc_hover_strength={:.3} toc_text_matches={} toc_callout_holds={} zoom={:.3} cached_tiles={} cached_bytes={} max_tile_bytes={} cached_text_pages={} text_desired={} pending={} desired={} visible_exact={}/{} visible_pages={} debouncing={} scroll=({:.2},{:.2}) sidebar={:.3}/{:.0} reference_panel={:.3}/{:.0} comment_pane={:.3}/{:.0} comment_editor={} comment_dirty={} autosave_pending={} sidebar_transitions={} sidebar_anchor_error={:.6} annotations={} highlights={} highlight_colors={} comments={} annotation_revision={}/{}/{} annotation_loading={} annotation_blocked={} search_results={} search_pages={} search_highlight_runs={} active_search={} search_focuses={} search_complete={} extension_packages={} extension_active={} extension_suspended={} extension_failed={} extension_panel={} extension_checks={} extension_native_rejected={} status={:?}",
            self.view_mode,
            theme_name,
            if matches!(
                self.render_appearance,
                RenderAppearance::ForcedColors { .. }
            ) {
                "forced"
            } else {
                "normal"
            },
            u8::from(self.pdf_dark_mode_enabled),
            self.document
                .as_ref()
                .map_or(0, |document| document.toc.len()),
            self.document
                .as_ref()
                .map_or(0, |document| document.links.len()),
            self.qa_link_navigations,
            link_preview,
            self.previewed_reference.map_or(0, |index| index + 1),
            self.current_reference_texts().len(),
            citation_source,
            link_preview_state,
            scholarly_state,
            u8::from(self.scientific_document),
            u8::from(self.scientific_analysis_complete),
            self.scientific_signals.reference_entries,
            self.scientific_signals.doi_entries,
            self.scientific_signals.bracket_citations,
            self.scientific_signals.superscript_citations,
            self.toc_hovered.map_or(0, |index| index + 1),
            self.toc_hover_strength,
            self.qa_toc_text_matches,
            self.qa_toc_callout_holds,
            self.zoom,
            self.rendered.len(),
            cached_bytes,
            max_tile_bytes,
            self.page_text.len(),
            self.text_viewport.len(),
            self.pending.len(),
            self.render_viewport.len(),
            exact_cached,
            visible_exact.len(),
            visible_pages,
            u8::from(self.render_debounce_until.is_some()),
            self.scroll.x,
            self.scroll.y,
            self.sidebar.progress,
            self.sidebar.target,
            self.reference_panel.value(),
            self.reference_panel.target(),
            self.comment_pane.progress,
            self.comment_pane.target,
            u8::from(self.comment_editor.is_some()),
            u8::from(self.comment_draft_dirty),
            u8::from(self.comment_autosave_task.is_some()),
            self.qa_sidebar_transitions,
            self.qa_max_sidebar_anchor_error,
            self.annotations.as_ref().map_or(0, AnnotationSet::len),
            highlight_count,
            highlight_colors.len(),
            comment_count,
            self.annotations.as_ref().map_or(0, AnnotationSet::revision),
            self.annotation_enqueued_revision,
            self.annotation_saved_revision,
            u8::from(self.annotations_loading),
            u8::from(self.annotation_persistence_blocked),
            self.search.order.len(),
            self.search.searched_pages,
            self.search.total_highlight_runs,
            active_search,
            self.qa_search_focuses,
            u8::from(self.search.complete),
            extension_packages,
            extension_active,
            extension_suspended,
            extension_failed,
            extension_panel,
            self.qa_extension_checks,
            u8::from(self.qa_extension_native_rejected),
            self.status,
        )
    }

    #[cfg(feature = "installable-extensions")]
    fn qa_extension_package_counts(&self) -> (usize, usize, usize, usize) {
        let count = |state| {
            self.extension_packages
                .iter()
                .filter(|package| package.state == state)
                .count()
        };
        (
            self.extension_packages.len(),
            count(LifecycleState::Active),
            count(LifecycleState::Suspended),
            count(LifecycleState::Failed),
        )
    }

    #[cfg(not(feature = "installable-extensions"))]
    fn qa_extension_package_counts(&self) -> (usize, usize, usize, usize) {
        (0, 0, 0, 0)
    }

    #[cfg(debug_assertions)]
    pub fn qa_use_fluid_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_view_mode(ReaderView::Fluid, window, cx);
    }

    #[cfg(debug_assertions)]
    pub fn qa_set_pdf_dark_mode(
        &mut self,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pdf_dark_mode_enabled = enabled;
        self.update_render_appearance(window, cx);
    }

    #[cfg(debug_assertions)]
    pub fn qa_set_toc_hovered(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let count = self
            .document
            .as_ref()
            .map_or(0, |document| document.toc.len());
        if index >= count {
            return Err(format!(
                "TOC hover index {index} is outside {count} entries"
            ));
        }
        self.set_toc_hovered(index, true, window, cx);
        Ok(())
    }

    #[cfg(debug_assertions)]
    pub fn qa_navigate_toc(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let count = self
            .document
            .as_ref()
            .map_or(0, |document| document.toc.len());
        if index >= count {
            return Err(format!(
                "TOC navigation index {index} is outside {count} entries"
            ));
        }
        self.navigate_toc(index, window, cx);
        Ok(())
    }

    #[cfg(debug_assertions)]
    pub fn qa_navigate_link(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some(link) = self
            .document
            .as_ref()
            .and_then(|document| document.links.get(index))
        else {
            return Err(format!("link index {index} is unavailable"));
        };
        if !matches!(link.target, PdfLinkTarget::Internal { .. }) {
            return Err(format!("link index {index} is not an internal destination"));
        }
        self.activate_document_link(link.id, window, cx);
        self.qa_link_navigations += 1;
        Ok(())
    }

    #[cfg(debug_assertions)]
    pub fn qa_hover_link(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some((id, page, bounds)) = self
            .document
            .as_ref()
            .and_then(|document| document.links.get(index))
            .map(|link| (link.id, link.page, link.bounds))
        else {
            return Err(format!("link index {index} is unavailable"));
        };
        if let Some(page_rect) = self.layout().and_then(|layout| layout.page_rect(page)) {
            let bounds = normalized_bounds_in_page(page_rect, bounds);
            self.set_link_card_pointer_immediate(self.canvas_to_window(Offset {
                x: bounds.x + bounds.width * 0.5 - self.scroll.x,
                y: bounds.y + bounds.height * 0.5 - self.scroll.y,
            }));
        }
        self.hovered_link = Some(id);
        self.show_link_preview(id, window, cx);
        self.hovered_link = None;
        self.link_source_hovered = false;
        self.schedule_link_preview_clear(window, cx);
        self.set_link_card_hovered(true, window, cx);
        Ok(())
    }

    #[cfg(debug_assertions)]
    pub fn qa_hover_internal_link(
        &mut self,
        ordinal: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some(id) = self.document.as_ref().and_then(|document| {
            document
                .links
                .iter()
                .filter(|link| matches!(link.target, PdfLinkTarget::Internal { .. }))
                .nth(ordinal)
                .map(|link| link.id)
        }) else {
            return Err(format!("internal link ordinal {ordinal} is unavailable"));
        };
        self.qa_hover_link(id, window, cx)
    }

    #[cfg(debug_assertions)]
    pub fn qa_hover_scientific_reference(
        &mut self,
        ordinal: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self
            .document
            .as_ref()
            .is_none_or(|document| ordinal >= document.scientific_references.len())
        {
            return Err(format!(
                "scientific reference ordinal {ordinal} is unavailable"
            ));
        }
        if let Some((page, bounds)) = self.document.as_ref().and_then(|document| {
            let reference = document.scientific_references.get(ordinal)?;
            Some((
                reference.page,
                reference
                    .text_runs
                    .iter()
                    .copied()
                    .reduce(union_text_bounds)?,
            ))
        }) && let Some(page_rect) = self.layout().and_then(|layout| layout.page_rect(page))
        {
            let bounds = normalized_bounds_in_page(page_rect, bounds);
            self.set_link_card_pointer_immediate(self.canvas_to_window(Offset {
                x: bounds.x + bounds.width * 0.5 - self.scroll.x,
                y: bounds.y + bounds.height * 0.5 - self.scroll.y,
            }));
        }
        self.hovered_reference = Some(ordinal);
        self.show_reference_preview(ordinal, window, cx);
        self.hovered_reference = None;
        self.link_source_hovered = false;
        self.schedule_link_preview_clear(window, cx);
        self.set_link_card_hovered(true, window, cx);
        Ok(())
    }

    #[cfg(debug_assertions)]
    pub fn qa_open_reference_details(
        &mut self,
        ordinal: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<bool, String> {
        let reference = self
            .document
            .as_ref()
            .and_then(|document| document.scientific_references.get(ordinal))
            .map(|reference| reference.text.clone())
            .ok_or_else(|| format!("scientific reference ordinal {ordinal} is unavailable"))?;
        match self.scholarly_session.state(&reference) {
            Some(ScholarlyMetadataState::Ready(_)) => {
                self.open_reference_details(reference, window, cx);
                if std::env::var_os("GPUI_PDF_READER_QA_REFERENCE_DETAILS_EXPANDED").is_some() {
                    self.reference_citation_expansion.set_target(1.0);
                    self.start_animation(window, cx);
                }
                Ok(true)
            }
            Some(ScholarlyMetadataState::Loading) | None => Ok(false),
            Some(ScholarlyMetadataState::Failed(message)) => {
                Err(format!("reference lookup failed: {message}"))
            }
        }
    }

    #[cfg(debug_assertions)]
    pub fn qa_hold_toc_callout(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let index = self
            .toc_hovered
            .ok_or_else(|| "TOC callout hold requires a hovered entry".to_owned())?;
        self.set_toc_hovered(index, false, window, cx);
        self.set_toc_callout_hovered(true, window, cx);
        self.qa_toc_callout_holds += 1;
        Ok(())
    }

    #[cfg(debug_assertions)]
    pub fn qa_select_theme(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if name != "system"
            && !theme::bundled_themes()
                .iter()
                .any(|theme| theme.name == name)
        {
            return false;
        }
        let command = self.extensions.borrow().theme_command().clone();
        let selected = if name == "system" { "" } else { name };
        self.invoke_extension_command(
            &InvokeExtensionCommand {
                command,
                payload: Some(DataValue::String(selected.to_owned())),
            },
            window,
            cx,
        );
        true
    }

    #[cfg(all(debug_assertions, feature = "installable-extensions"))]
    pub fn qa_drive_extension_scenario(
        &mut self,
        scenario: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<bool, String> {
        match scenario {
            "reference" => self.qa_drive_reference_extensions(window, cx),
            "manager" => self.qa_drive_extension_manager(window, cx),
            "restore" => self.qa_drive_restored_extensions(window, cx),
            "adversarial" => self.qa_drive_adversarial_extension(window, cx),
            _ => Err(format!("unknown extension QA scenario {scenario:?}")),
        }
    }

    #[cfg(all(debug_assertions, feature = "installable-extensions"))]
    fn qa_drive_extension_manager(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<bool, String> {
        const THEME: &str = "org.key.reference.theme-pack";
        match self.qa_extension_phase {
            QaExtensionPhase::Seed => {
                self.install_extension_path(
                    qa_extension_package_path("reference-theme-pack"),
                    window,
                    cx,
                );
                self.qa_extension_phase = QaExtensionPhase::WaitReference;
                Ok(false)
            }
            QaExtensionPhase::WaitReference => {
                if !self.qa_viewport_is_settled() {
                    return Ok(false);
                }
                let review_is_open = matches!(
                    self.extension_manager_page,
                    Some(ExtensionManagerPage::InstallReview { ref preview, .. })
                        if preview.extension.as_str() == THEME
                );
                if !review_is_open
                    || self.sidebar.panel != SidePanel::Extensions
                    || self.sidebar.progress < 0.999
                {
                    return Err("extension install review did not open inside the manager".into());
                }
                self.qa_extension_checks += 2;
                self.confirm_extension_install(window, cx);
                self.qa_extension_phase = QaExtensionPhase::WaitReferencePanel;
                Ok(false)
            }
            QaExtensionPhase::WaitReferencePanel => {
                if !self.qa_viewport_is_settled() {
                    return Ok(false);
                }
                self.refresh_extension_manager_state();
                let theme = ExtensionId::parse(THEME).expect("static QA extension ID");
                let details_are_open = matches!(
                    self.extension_manager_page,
                    Some(ExtensionManagerPage::Details(ref extension)) if extension == &theme
                );
                if !details_are_open
                    || !qa_packages_are_active(&self.extension_packages, &[THEME])
                    || !self.extension_setting_inputs.contains_key("display-name")
                    || self.extension_contribution.is_some()
                    || self.extension_ui_panel.value() > 0.001
                {
                    return Err(
                        "reviewed extension did not reach its host-rendered settings detail".into(),
                    );
                }
                self.qa_extension_checks += 4;
                self.qa_extension_phase = QaExtensionPhase::Complete;
                Ok(true)
            }
            QaExtensionPhase::Complete => Ok(true),
            _ => Err("extension manager scenario entered an invalid phase".into()),
        }
    }

    #[cfg(all(debug_assertions, not(feature = "installable-extensions")))]
    pub fn qa_drive_extension_scenario(
        &mut self,
        _scenario: &str,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Result<bool, String> {
        Err("extension QA requires the installable-extensions feature".into())
    }

    #[cfg(all(debug_assertions, feature = "installable-extensions"))]
    fn qa_drive_reference_extensions(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<bool, String> {
        const THEME: &str = "org.key.reference.theme-pack";
        const THEME_COMMAND: &str = "org.key.reference.theme-pack/apply-preset";
        const STATISTICS: &str = "org.key.reference.document-statistics";
        const STATISTICS_COMMAND: &str = "org.key.reference.document-statistics/open";

        match self.qa_extension_phase {
            QaExtensionPhase::Seed => {
                let theme_path = qa_extension_package_path("reference-theme-pack");
                let preview = self
                    .extensions
                    .borrow()
                    .preview_package(&theme_path)
                    .map_err(|error| format!("theme package preview failed: {error}"))?;
                let report = self
                    .extensions
                    .borrow_mut()
                    .install_reviewed_package(&theme_path, &preview)
                    .map_err(|error| format!("theme package install failed: {error}"))?;
                if report.activation != PackageActivation::Active {
                    return Err(format!(
                        "theme package did not activate immediately: {:?}",
                        report.activation
                    ));
                }
                self.qa_extension_checks += 1;
                self.invoke_extension_command(
                    &InvokeExtensionCommand {
                        command: key_extension_api::CommandId::parse(THEME_COMMAND)
                            .expect("static QA command ID"),
                        payload: Some(DataValue::String("graphite".into())),
                    },
                    window,
                    cx,
                );

                let statistics_path = qa_extension_package_path("reference-document-statistics");
                let preview = self
                    .extensions
                    .borrow()
                    .preview_package(&statistics_path)
                    .map_err(|error| format!("statistics package preview failed: {error}"))?;
                let report = self
                    .extensions
                    .borrow_mut()
                    .install_reviewed_package(&statistics_path, &preview)
                    .map_err(|error| format!("statistics package install failed: {error}"))?;
                if !matches!(report.activation, PackageActivation::AwaitingPermissions(_)) {
                    return Err(format!(
                        "statistics package bypassed permission review: {:?}",
                        report.activation
                    ));
                }
                self.qa_extension_checks += 1;
                let statistics = ExtensionId::parse(STATISTICS).expect("static QA extension ID");
                let report = self
                    .extensions
                    .borrow_mut()
                    .approve_package(&statistics)
                    .map_err(|error| format!("statistics package approval failed: {error}"))?;
                if !matches!(
                    report.activation,
                    PackageActivation::Active | PackageActivation::Activating
                ) {
                    return Err(format!(
                        "approved statistics package did not activate: {:?}",
                        report.activation
                    ));
                }
                self.qa_extension_checks += 1;
                self.refresh_extension_manager_state();
                self.schedule_extension_snapshot_sync(window, cx);
                self.qa_extension_phase = QaExtensionPhase::WaitReference;
                Ok(false)
            }
            QaExtensionPhase::WaitReference => {
                self.refresh_extension_manager_state();
                qa_reject_failed_packages(&self.extension_packages)?;
                if !qa_packages_are_active(&self.extension_packages, &[THEME, STATISTICS]) {
                    return Ok(false);
                }

                let theme_view = self
                    .extensions
                    .borrow_mut()
                    .contribution_view(
                        &ContributionId::parse("org.key.reference.theme-pack/settings")
                            .expect("static QA contribution ID"),
                    )
                    .ok_or_else(|| "active theme settings view is missing".to_owned())?;
                if qa_nested_state(&theme_view.state, &["settings", "theme-preset"])
                    != Some(&DataValue::String("graphite".into()))
                {
                    return Ok(false);
                }

                let statistics_view = self.extensions.borrow_mut().contribution_view(
                    &ContributionId::parse("org.key.reference.document-statistics/panel")
                        .expect("static QA contribution ID"),
                );
                let Some(statistics_view) = statistics_view else {
                    return Ok(false);
                };
                let runtime_ready =
                    statistics_view.state.get("runtime-ready") == Some(&DataValue::Boolean(true));
                let pages = qa_nested_integer(
                    &statistics_view.state,
                    &["document", "statistics", "page-count"],
                );
                let known_text_pages = qa_nested_integer(
                    &statistics_view.state,
                    &["document", "statistics", "text-pages-known"],
                );
                if !runtime_ready || pages <= 0 || known_text_pages <= 0 {
                    self.schedule_extension_snapshot_sync(window, cx);
                    return Ok(false);
                }
                if self.extension_commands.len() != 2 {
                    return Err(format!(
                        "expected two external commands, found {}",
                        self.extension_commands.len()
                    ));
                }
                let tool_entries = self.extensions.borrow_mut().extension_tool_entries();
                let theme = ExtensionId::parse(THEME).expect("static QA extension ID");
                let statistics = ExtensionId::parse(STATISTICS).expect("static QA extension ID");
                if tool_entries.len() != 2
                    || tool_entries
                        .iter()
                        .find(|entry| entry.extension == theme)
                        .is_none_or(|entry| entry.action.is_some())
                    || tool_entries
                        .iter()
                        .find(|entry| entry.extension == statistics)
                        .and_then(|entry| entry.action.as_ref())
                        .is_none_or(|action| action.command.as_str() != STATISTICS_COMMAND)
                {
                    return Err(
                        "Tools → Extensions entries did not honor trigger/fallback declarations"
                            .into(),
                    );
                }
                self.extensions
                    .borrow_mut()
                    .set_package_setting(
                        &theme,
                        "display-name",
                        DataValue::String("QA palette".into()),
                    )
                    .map_err(|error| format!("string setting update failed: {error}"))?;
                self.extensions
                    .borrow_mut()
                    .set_package_setting(&theme, "follow-document", DataValue::Boolean(false))
                    .map_err(|error| format!("boolean setting update failed: {error}"))?;
                self.refresh_extension_manager_state();
                let theme_summary = self
                    .extension_packages
                    .iter()
                    .find(|package| package.extension == theme)
                    .ok_or_else(|| "theme summary disappeared after setting update".to_owned())?;
                if qa_nested_state(
                    &match theme_summary.settings.as_ref() {
                        Some(DataValue::Record(settings)) => settings.clone(),
                        _ => return Err("theme settings were not persisted as a record".into()),
                    },
                    &["display-name"],
                ) != Some(&DataValue::String("QA palette".into()))
                {
                    return Err("host-rendered string setting was not persisted".into());
                }
                self.open_extension_details(theme, window, cx);
                if !self.extension_setting_inputs.contains_key("display-name")
                    || !matches!(
                        self.extension_manager_page,
                        Some(ExtensionManagerPage::Details(_))
                    )
                {
                    return Err(
                        "extension details did not prepare its host-rendered settings".into(),
                    );
                }
                self.qa_extension_checks += 7;
                self.invoke_extension_command(
                    &InvokeExtensionCommand {
                        command: key_extension_api::CommandId::parse(STATISTICS_COMMAND)
                            .expect("static QA command ID"),
                        payload: None,
                    },
                    window,
                    cx,
                );
                self.qa_extension_phase = QaExtensionPhase::WaitReferencePanel;
                Ok(false)
            }
            QaExtensionPhase::WaitReferencePanel => {
                if !self.qa_viewport_is_settled() {
                    return Ok(false);
                }
                let expected = ExtensionId::parse(STATISTICS).expect("static QA extension ID");
                if self
                    .extension_contribution
                    .as_ref()
                    .is_none_or(|pane| pane.owner != expected)
                    || self.extension_ui_panel.value() < 0.999
                {
                    return Ok(false);
                }
                self.qa_extension_checks += 1;
                self.qa_extension_phase = QaExtensionPhase::Complete;
                Ok(true)
            }
            QaExtensionPhase::Complete => Ok(true),
            _ => Err("reference extension scenario entered an invalid phase".into()),
        }
    }

    #[cfg(all(debug_assertions, feature = "installable-extensions"))]
    fn qa_drive_restored_extensions(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<bool, String> {
        const THEME: &str = "org.key.reference.theme-pack";
        const STATISTICS: &str = "org.key.reference.document-statistics";
        match self.qa_extension_phase {
            QaExtensionPhase::Seed => {
                self.refresh_extension_manager_state();
                self.schedule_extension_snapshot_sync(window, cx);
                self.qa_extension_phase = QaExtensionPhase::WaitRestore;
                Ok(false)
            }
            QaExtensionPhase::WaitRestore => {
                self.refresh_extension_manager_state();
                qa_reject_failed_packages(&self.extension_packages)?;
                if !qa_packages_are_active(&self.extension_packages, &[THEME, STATISTICS]) {
                    return Ok(false);
                }
                let panel_id = ContributionId::parse("org.key.reference.document-statistics/panel")
                    .expect("static QA contribution ID");
                let Some(view) = self.extensions.borrow_mut().contribution_view(&panel_id) else {
                    return Ok(false);
                };
                if view.state.get("runtime-ready") != Some(&DataValue::Boolean(true))
                    || qa_nested_integer(&view.state, &["document", "statistics", "page-count"])
                        <= 0
                {
                    self.schedule_extension_snapshot_sync(window, cx);
                    return Ok(false);
                }
                self.qa_extension_checks += 3;
                self.invoke_extension_command(
                    &InvokeExtensionCommand {
                        command: key_extension_api::CommandId::parse(
                            "org.key.reference.document-statistics/open",
                        )
                        .expect("static QA command ID"),
                        payload: None,
                    },
                    window,
                    cx,
                );
                self.qa_extension_phase = QaExtensionPhase::WaitRestorePanel;
                Ok(false)
            }
            QaExtensionPhase::WaitRestorePanel => {
                if !self.qa_viewport_is_settled() {
                    return Ok(false);
                }
                let expected = ExtensionId::parse(STATISTICS).expect("static QA extension ID");
                if self
                    .extension_contribution
                    .as_ref()
                    .is_none_or(|pane| pane.owner != expected)
                    || self.extension_ui_panel.value() < 0.999
                {
                    return Ok(false);
                }
                self.qa_extension_checks += 1;
                self.qa_extension_phase = QaExtensionPhase::Complete;
                Ok(true)
            }
            QaExtensionPhase::Complete => Ok(true),
            _ => Err("restored extension scenario entered an invalid phase".into()),
        }
    }

    #[cfg(all(debug_assertions, feature = "installable-extensions"))]
    fn qa_drive_adversarial_extension(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<bool, String> {
        const HOSTILE: &str = "org.key.reference.adversarial-loop";
        match self.qa_extension_phase {
            QaExtensionPhase::Seed => {
                let native_path = qa_extension_package_path("reference-native-escape");
                match self.extensions.borrow().preview_package(&native_path) {
                    Err(
                        crate::extension_packages::ExtensionPackageError::ExternalNativeEntrypoint(
                            extension,
                        ),
                    ) if extension.as_str() == "org.key.reference.native-escape" => {
                        self.qa_extension_native_rejected = true;
                        self.qa_extension_checks += 1;
                    }
                    Err(error) => {
                        return Err(format!(
                            "native escape was rejected for the wrong reason: {error}"
                        ));
                    }
                    Ok(_) => return Err("native escape package passed preview".into()),
                }

                let path = qa_extension_package_path("reference-adversarial-loop");
                let preview = self
                    .extensions
                    .borrow()
                    .preview_package(&path)
                    .map_err(|error| format!("hostile package preview failed: {error}"))?;
                let report = self
                    .extensions
                    .borrow_mut()
                    .install_reviewed_package(&path, &preview)
                    .map_err(|error| format!("hostile package install failed: {error}"))?;
                if !matches!(report.activation, PackageActivation::AwaitingPermissions(_)) {
                    return Err(format!(
                        "hostile package bypassed permission review: {:?}",
                        report.activation
                    ));
                }
                let hostile = ExtensionId::parse(HOSTILE).expect("static QA extension ID");
                let report = self
                    .extensions
                    .borrow_mut()
                    .approve_package(&hostile)
                    .map_err(|error| format!("hostile package approval failed: {error}"))?;
                if !matches!(
                    report.activation,
                    PackageActivation::Active | PackageActivation::Activating
                ) {
                    return Err(format!(
                        "hostile package did not enter the runtime: {:?}",
                        report.activation
                    ));
                }
                self.qa_extension_checks += 2;
                let probe =
                    key_extension_api::CommandId::parse("org.key.reference.adversarial-loop/probe")
                        .expect("static QA command ID");
                for _ in 0..4 {
                    let effects = self
                        .extensions
                        .borrow_mut()
                        .invoke_command(&probe, None)
                        .map_err(|error| format!("hostile probe could not be queued: {error}"))?;
                    if !effects.is_empty() {
                        return Err("hostile probe unexpectedly received host authority".into());
                    }
                    self.qa_extension_checks += 1;
                }
                self.refresh_extension_manager_state();
                self.schedule_extension_snapshot_sync(window, cx);
                self.qa_extension_phase = QaExtensionPhase::WaitAdversarial;
                Ok(false)
            }
            QaExtensionPhase::WaitAdversarial => {
                self.refresh_extension_manager_state();
                let Some(summary) = self
                    .extension_packages
                    .iter()
                    .find(|package| package.extension.as_str() == HOSTILE)
                else {
                    return Err("hostile package disappeared from the manager".into());
                };
                match summary.state {
                    LifecycleState::Suspended => {
                        if self
                            .extension_commands
                            .iter()
                            .any(|command| command.owner == summary.extension)
                            || self
                                .extension_contribution
                                .as_ref()
                                .is_some_and(|pane| pane.owner == summary.extension)
                        {
                            return Err("suspended hostile package retained UI authority".into());
                        }
                        self.qa_extension_checks += 1;
                        self.qa_extension_phase = QaExtensionPhase::Complete;
                        Ok(true)
                    }
                    LifecycleState::Failed => Err(format!(
                        "hostile event loop failed the package instead of suspending it: {}",
                        self.extensions
                            .borrow()
                            .latest_diagnostic_message()
                            .unwrap_or_else(|| "no diagnostic".into())
                    )),
                    _ => {
                        self.schedule_extension_snapshot_sync(window, cx);
                        Ok(false)
                    }
                }
            }
            QaExtensionPhase::Complete => Ok(true),
            _ => Err("adversarial extension scenario entered an invalid phase".into()),
        }
    }

    #[cfg(debug_assertions)]
    pub fn qa_viewport_is_settled(&self) -> bool {
        if !matches!(self.status, ReaderStatus::Ready)
            || self.render_debounce_until.is_some()
            || !self.pending.is_empty()
            || self.annotations_loading
            || (!self.annotation_persistence_blocked
                && self.annotations.as_ref().is_some_and(|annotations| {
                    annotations.revision() > self.annotation_saved_revision
                }))
            || (!self.search.query.is_empty()
                && (!self.search.complete || self.search_debounce_task.is_some()))
            || self.sidebar.is_animating()
            || self.comment_pane.is_animating()
            || self.reference_panel.is_animating()
            || self.reference_details_transition.is_animating()
            || self.reference_citation_expansion.is_animating()
            || self.reference_summary_transition.is_animating()
            || self.extension_ui_panel.is_animating()
            || {
                #[cfg(feature = "installable-extensions")]
                {
                    self.extension_manager_transition.is_animating()
                }
                #[cfg(not(feature = "installable-extensions"))]
                {
                    false
                }
            }
            || self.doi_copy_started.is_some()
            || self.link_card_expansion.is_animating()
            || self.link_card_pointer_is_animating()
            || self.toc_hover_is_animating()
            || self.pending_toc_navigation.is_some()
            || self.pending_link_navigation.is_some()
            || !self.scientific_analysis_complete
            || self.previewed_link.is_some_and(|id| {
                self.document
                    .as_ref()
                    .and_then(|document| document.links.iter().find(|link| link.id == id))
                    .is_some_and(|link| match &link.target {
                        PdfLinkTarget::Internal { .. } => self.resolved_internal_link(id).is_none(),
                        PdfLinkTarget::External { url } => self
                            .link_preview_session
                            .as_ref()
                            .and_then(|session| session.website(url))
                            .is_some_and(|state| matches!(state, WebsitePreviewState::Loading)),
                    })
            })
            || self
                .current_reference_text()
                .and_then(|reference| self.scholarly_session.state(&reference))
                .is_some_and(|state| matches!(state, ScholarlyMetadataState::Loading))
            || self.navigation_focus.is_busy(Instant::now())
            || self.comment_autosave_task.is_some()
            || self.extension_snapshot_task.is_some()
            || self.extensions.borrow().has_pending_service_work()
            || self.extensions.borrow().has_pending_extension_work()
        {
            return false;
        }
        let mut visible = self
            .render_viewport
            .iter()
            .filter_map(|(key, tier)| (*tier == DemandTier::Visible).then_some(key));
        let Some(first) = visible.next() else {
            return false;
        };
        self.rendered.contains_key(first) && visible.all(|key| self.rendered.contains_key(key))
    }

    #[cfg(debug_assertions)]
    pub fn qa_resource_is_settled(&self) -> bool {
        match self.resource_allocation.activity {
            ActivityLevel::BackgroundCold => {
                self.pending.is_empty()
                    && self.render_viewport.is_empty()
                    && self.rendered.is_empty()
            }
            ActivityLevel::Suspended => {
                self.pending.is_empty()
                    && self.render_viewport.is_empty()
                    && self.rendered.is_empty()
                    && self.worker_hibernated
            }
            ActivityLevel::BackgroundWarm
            | ActivityLevel::ForegroundVisible
            | ActivityLevel::ForegroundIdle
            | ActivityLevel::ForegroundInteractive => self.qa_viewport_is_settled(),
        }
    }

    #[cfg(debug_assertions)]
    pub fn qa_command_wheel(
        &mut self,
        delta_y: f32,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.qa_wheel(0.0, delta_y, true, position, window, cx);
    }

    #[cfg(debug_assertions)]
    pub fn qa_wheel(
        &mut self,
        delta_x: f32,
        delta_y: f32,
        zoom: bool,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.on_scroll_wheel(
            &ScrollWheelEvent {
                position,
                delta: ScrollDelta::Pixels(Point::new(px(delta_x), px(delta_y))),
                modifiers: Modifiers {
                    platform: zoom,
                    ..Default::default()
                },
                touch_phase: TouchPhase::Moved,
            },
            window,
            cx,
        );
    }

    #[cfg(debug_assertions)]
    fn qa_text_range(&self, page: usize, needle: &str) -> Option<TextRange> {
        let characters = self.page_text.get(&page)?.as_slice();
        let needle: Vec<_> = needle.chars().collect();
        if needle.is_empty() || needle.len() > characters.len() {
            return None;
        }
        let start = characters.windows(needle.len()).position(|window| {
            window
                .iter()
                .map(|character| character.value)
                .eq(needle.iter().copied())
        })?;
        Some(TextRange::new(
            TextPosition { page, index: start },
            TextPosition {
                page,
                index: start + needle.len() - 1,
            },
        ))
    }

    #[cfg(debug_assertions)]
    fn qa_defer_keystrokes(
        keys: &[&str],
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let keystrokes = keys
            .iter()
            .map(|key| {
                Keystroke::parse(key)
                    .map(|keystroke| ((*key).to_owned(), keystroke))
                    .map_err(|error| format!("invalid QA key {key:?}: {error}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        window.defer(cx, move |window, cx| {
            for (name, keystroke) in keystrokes {
                if !window.dispatch_keystroke(keystroke, cx) {
                    eprintln!("GPUI_PDF_READER_QA_ERROR key {name:?} was not handled");
                    cx.quit();
                    return;
                }
            }
        });
        Ok(())
    }

    #[cfg(debug_assertions)]
    fn qa_seed_annotations_and_comment(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let annotations = self
            .annotations
            .as_ref()
            .ok_or_else(|| "annotations have not loaded".to_owned())?;
        if annotations.iter().next().is_some() {
            return Err("feature scenario requires a PDF with no existing sidecar".to_owned());
        }
        if self.annotation_persistence_blocked {
            return Err("annotation persistence is blocked".to_owned());
        }

        let needles = ["GPUI", "PDF", "Reader", "integration", "fixture"];
        let ranges = needles
            .iter()
            .map(|needle| {
                self.qa_text_range(0, needle)
                    .ok_or_else(|| format!("fixture text {needle:?} was not extracted"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        for (range, color) in ranges.into_iter().zip(HighlightColor::ALL) {
            self.selection = Some(range.as_selection());
            self.add_highlight(color, cx);
        }

        let comment_range = self
            .qa_text_range(0, "Select this sentence")
            .ok_or_else(|| "fixture comment text was not extracted".to_owned())?;
        self.selection = Some(comment_range.as_selection());
        self.open_comment_editor(comment_range, None, String::new(), window, cx);

        let annotations = self
            .annotations
            .as_ref()
            .ok_or_else(|| "annotations disappeared while seeding".to_owned())?;
        if annotations.len() != 5 || annotations.revision() != 5 {
            return Err(format!(
                "feature seeding produced {} annotations at revision {}, expected 5/5 before the comment save",
                annotations.len(),
                annotations.revision()
            ));
        }
        Ok(())
    }

    #[cfg(debug_assertions)]
    fn qa_validate_feature_scenario(&self) -> Result<(), String> {
        let annotations = self
            .annotations
            .as_ref()
            .ok_or_else(|| "annotations disappeared".to_owned())?;
        let colors: HashSet<_> = annotations
            .iter()
            .filter_map(|annotation| annotation.highlight())
            .collect();
        let highlights = annotations
            .iter()
            .filter(|annotation| annotation.highlight().is_some())
            .count();
        let comments = annotations
            .iter()
            .filter(|annotation| annotation.comment_markdown().is_some())
            .count();
        if annotations.len() != 6
            || highlights != 5
            || colors.len() != HighlightColor::ALL.len()
            || comments != 1
            || self.annotation_saved_revision != annotations.revision()
        {
            return Err(format!(
                "unexpected persisted feature state: annotations={}, highlights={highlights}, colors={}, comments={comments}, revision={}/{},",
                annotations.len(),
                colors.len(),
                annotations.revision(),
                self.annotation_saved_revision
            ));
        }
        if self.search.order.len() < 3
            || !self.search.complete
            || self.search.total_highlight_runs == 0
            || self.qa_search_focuses != 5
            || self
                .search
                .active
                .and_then(|active| self.search.order.iter().position(|id| *id == active))
                != Some(1)
        {
            return Err(format!(
                "unexpected search state: results={}, runs={}, active={:?}, focuses={}, complete={}",
                self.search.order.len(),
                self.search.total_highlight_runs,
                self.search.active,
                self.qa_search_focuses,
                self.search.complete
            ));
        }
        if self.sidebar.panel != SidePanel::Search
            || self.sidebar.progress != 1.0
            || self.sidebar.target != 1.0
            || self.qa_sidebar_transitions < 4
            || self.qa_max_sidebar_anchor_error > 0.002
        {
            return Err(format!(
                "unexpected sidebar state: panel={:?}, progress={}, target={}, transitions={}, anchor_error={}",
                self.sidebar.panel,
                self.sidebar.progress,
                self.sidebar.target,
                self.qa_sidebar_transitions,
                self.qa_max_sidebar_anchor_error
            ));
        }
        Ok(())
    }

    /// Advances one deterministic native feature scenario without bypassing
    /// production annotation persistence, sidebar animation, or PDFium search.
    #[cfg(debug_assertions)]
    pub fn qa_drive_feature_scenario(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<bool, String> {
        match self.qa_feature_phase {
            QaFeaturePhase::Seed => {
                if !self.qa_viewport_is_settled()
                    || self.annotations_loading
                    || !self.page_text.contains_key(&0)
                {
                    return Ok(false);
                }
                self.qa_seed_annotations_and_comment(window, cx)?;
                self.qa_feature_phase = QaFeaturePhase::WaitCommentEditor;
            }
            QaFeaturePhase::WaitCommentEditor => {
                let Some(editor) = self.comment_editor.clone() else {
                    return Err("comment editor disappeared before its native frame".to_owned());
                };
                if editor.read(cx).qa_has_painted() {
                    // Repeating Add Comment used to silently replace the open
                    // editor and lose its draft. Exercise the production
                    // command path and require identity/range preservation.
                    let pending_range = self.pending_comment_range;
                    self.add_comment(&AddComment, window, cx);
                    if self.comment_editor.as_ref() != Some(&editor)
                        || self.pending_comment_range != pending_range
                    {
                        return Err("repeated Add Comment replaced the open draft".to_owned());
                    }
                    self.warning = None;
                    if std::env::var_os("GPUI_PDF_READER_QA_MARKDOWN_MENU_VISUAL").is_some() {
                        Self::qa_defer_keystrokes(&["/", "h"], window, cx)?;
                        self.qa_feature_phase = QaFeaturePhase::Complete;
                        return Ok(true);
                    }
                    // Exercise the same native key/input path a person uses:
                    // a slash command, multiword text, and Return-driven list
                    // continuation. The shared debounce must persist it
                    // without an explicit save command.
                    Self::qa_defer_keystrokes(
                        &[
                            "/", "b", "u", "l", "l", "e", "t", "enter", "i", "m", "p", "o", "r",
                            "t", "a", "n", "t", "space", "c", "o", "p", "y", "enter", "c", "h",
                            "e", "c", "k",
                        ],
                        window,
                        cx,
                    )?;
                    self.qa_feature_phase = QaFeaturePhase::WaitCommentEdited;
                }
            }
            QaFeaturePhase::WaitCommentEdited => {
                let Some(editor) = self.comment_editor.as_ref() else {
                    return Err("comment editor disappeared after native editing".to_owned());
                };
                if !self.comment_draft_dirty {
                    return Err("native comment edits did not mark the draft dirty".to_owned());
                }
                if editor.read(cx).markdown() != "- important copy\n- check" {
                    return Ok(false);
                }
                self.qa_feature_phase = QaFeaturePhase::WaitCommentSaved;
            }
            QaFeaturePhase::WaitCommentSaved => {
                let annotations = self
                    .annotations
                    .as_ref()
                    .ok_or_else(|| "annotations disappeared after comment save".to_owned())?;
                if annotations.len() != 6 || annotations.revision() != 6 {
                    return Ok(false);
                }
                if self.comment_draft_dirty {
                    return Ok(false);
                }
                if self.comment_editor.is_none() {
                    return Err("Classic autosave closed the comment editor".to_owned());
                }
                let comment = annotations
                    .iter()
                    .find_map(|annotation| annotation.comment_markdown().map(ToOwned::to_owned))
                    .ok_or_else(|| "native comment save produced no Markdown".to_owned())?;
                if comment != "- important copy\n- check" {
                    return Err(format!(
                        "native comment input/formatting produced unexpected Markdown: {comment:?}"
                    ));
                }
                self.qa_feature_phase = QaFeaturePhase::WaitCommentBack;
            }
            QaFeaturePhase::WaitCommentBack => {
                let Some(editor) = self.comment_editor.clone() else {
                    return Err("comment editor disappeared before Back QA".to_owned());
                };
                if editor.read(cx).qa_has_painted() {
                    Self::qa_defer_keystrokes(&["escape"], window, cx)?;
                    self.qa_feature_phase = QaFeaturePhase::WaitCommentList;
                }
            }
            QaFeaturePhase::WaitCommentList => {
                if self.comment_editor.is_some() {
                    return Ok(false);
                }
                let annotations = self
                    .annotations
                    .as_ref()
                    .ok_or_else(|| "annotations disappeared after comment cancel".to_owned())?;
                let comment = annotations
                    .iter()
                    .find_map(|annotation| annotation.comment_markdown());
                if annotations.revision() != 6 || comment != Some("- important copy\n- check") {
                    return Err("Back changed the persisted comment".to_owned());
                }
                let annotation = annotations
                    .iter()
                    .find(|annotation| annotation.comment_markdown().is_some())
                    .ok_or_else(|| "Classic comment vanished before parity checks".to_owned())?;
                let id = annotation.id();
                let start = annotation.range().start();
                let page = self
                    .layout()
                    .and_then(|layout| layout.page_rect(start.page))
                    .ok_or_else(|| "Classic hit-test page is not laid out".to_owned())?;
                let bounds = self
                    .page_text
                    .get(&start.page)
                    .and_then(|text| text.get(start.index))
                    .and_then(|character| character.bounds)
                    .ok_or_else(|| "Classic hit-test character has no bounds".to_owned())?;
                let pointer = self.canvas_to_window(Offset {
                    x: page.x + (bounds.left + bounds.right) * page.width * 0.5 - self.scroll.x,
                    y: page.y + (bounds.top + bounds.bottom) * page.height * 0.5 - self.scroll.y,
                });
                self.active_annotation = None;
                self.on_mouse_down(
                    &MouseDownEvent {
                        button: MouseButton::Left,
                        position: pointer,
                        click_count: 1,
                        ..Default::default()
                    },
                    window,
                    cx,
                );
                self.on_mouse_up(
                    &MouseUpEvent {
                        button: MouseButton::Left,
                        position: pointer,
                        ..Default::default()
                    },
                    window,
                    cx,
                );
                if self.active_annotation != Some(id)
                    || self.selection.is_some()
                    || !self.context_has_comment()
                {
                    return Err(
                        "clicking a Classic highlight did not expose its toolbar context".into(),
                    );
                }
                self.comment_on_context(window, cx);
                if self.comment_editor.is_none() || self.editing_annotation != Some(id) {
                    return Err("Classic toolbar Edit Comment did not open the annotation".into());
                }
                self.return_to_comment_list(window, cx);
                self.open_comment_from_list(id, window, cx);
                if self.comment_editor.is_none() || self.editing_annotation != Some(id) {
                    return Err("Classic comment-list row did not open the editor".into());
                }
                self.return_to_comment_list(window, cx);
                self.qa_feature_phase = QaFeaturePhase::WaitCommentsOpen;
            }
            QaFeaturePhase::WaitCommentsOpen => {
                if self.qa_viewport_is_settled()
                    && self.sidebar.panel == SidePanel::Comments
                    && self.sidebar.progress == 1.0
                {
                    self.toggle_sidebar(SidePanel::Comments, window, cx);
                    // A precise trackpad packet during the slide must move the
                    // document without cancelling the sidebar's frame chain.
                    self.scroll_by(0.0, 12.0, true, window, cx);
                    // Middle-button panning used to cancel the same frame
                    // chain through a separate input branch. Press/release is
                    // enough to prove the slide remains scheduled.
                    let pointer = self.canvas_to_window(Offset { x: 100.0, y: 100.0 });
                    self.on_mouse_down(
                        &MouseDownEvent {
                            button: MouseButton::Middle,
                            position: pointer,
                            ..Default::default()
                        },
                        window,
                        cx,
                    );
                    self.on_mouse_up(
                        &MouseUpEvent {
                            button: MouseButton::Middle,
                            position: pointer,
                            ..Default::default()
                        },
                        window,
                        cx,
                    );
                    self.qa_feature_phase = QaFeaturePhase::WaitCommentsClosed;
                }
            }
            QaFeaturePhase::WaitCommentsClosed => {
                if self.qa_viewport_is_settled() && self.sidebar.progress == 0.0 {
                    if !self.focus_handle.is_focused(window) {
                        return Err(
                            "closing Comments left a hidden comment input focused".to_owned()
                        );
                    }
                    self.show_sidebar(SidePanel::Search, window, cx);
                    self.qa_feature_phase = QaFeaturePhase::WaitSearchOpen;
                }
            }
            QaFeaturePhase::WaitSearchOpen => {
                if self.qa_viewport_is_settled()
                    && self.sidebar.panel == SidePanel::Search
                    && self.sidebar.progress == 1.0
                {
                    window.focus(&self.search_field.focus_handle(cx));
                    Self::qa_defer_keystrokes(&["p", "a", "g", "e"], window, cx)?;
                    self.qa_feature_phase = QaFeaturePhase::WaitSearch;
                }
            }
            QaFeaturePhase::WaitSearch => {
                if self.qa_viewport_is_settled() {
                    if self.search_field.read(cx).text() != "page" {
                        return Err("native search typing did not update the field".to_owned());
                    }
                    if self.search.order.len() < 2 {
                        return Err(format!(
                            "fixture search returned only {} result(s)",
                            self.search.order.len()
                        ));
                    }
                    Self::qa_defer_keystrokes(&["enter"], window, cx)?;
                    self.qa_feature_phase = QaFeaturePhase::WaitNavigation;
                }
            }
            QaFeaturePhase::WaitNavigation => {
                if self.qa_viewport_is_settled() {
                    let active_position = self
                        .search
                        .active
                        .and_then(|active| self.search.order.iter().position(|id| *id == active));
                    if active_position != Some(1) {
                        return Ok(false);
                    }
                    self.navigate_search(false, window, cx);
                    self.qa_feature_phase = QaFeaturePhase::WaitSearchReturn;
                }
            }
            QaFeaturePhase::WaitSearchReturn => {
                if self.qa_viewport_is_settled() {
                    // Closing a sidebar widens the viewport. Center the
                    // document first so preserving its center is geometrically
                    // possible instead of being dominated by a scroll-edge
                    // clamp near the search hit.
                    if let Some(layout) = self.layout() {
                        self.viewport.set_scroll(Offset::new(
                            (layout.content_width - self.viewport_width).max(0.0) * 0.5,
                            self.scroll.y,
                        ));
                        self.sync_viewport_snapshot();
                    }
                    self.toggle_sidebar(SidePanel::Search, window, cx);
                    self.qa_feature_phase = QaFeaturePhase::WaitSearchClosed;
                }
            }
            QaFeaturePhase::WaitSearchClosed => {
                if self.qa_viewport_is_settled() && self.sidebar.progress == 0.0 {
                    if !self.focus_handle.is_focused(window) {
                        return Err("closing Search left its hidden text field focused".to_owned());
                    }
                    if !self.search.query.is_empty()
                        || !self.search.order.is_empty()
                        || !self.search_field.read(cx).text().is_empty()
                    {
                        return Err("closing Search did not reset its query and results".to_owned());
                    }
                    self.show_sidebar(SidePanel::Search, window, cx);
                    self.qa_feature_phase = QaFeaturePhase::WaitSearchReopened;
                }
            }
            QaFeaturePhase::WaitSearchReopened => {
                if self.qa_viewport_is_settled()
                    && self.sidebar.panel == SidePanel::Search
                    && self.sidebar.progress == 1.0
                {
                    window.focus(&self.search_field.focus_handle(cx));
                    Self::qa_defer_keystrokes(&["p", "a", "g", "e"], window, cx)?;
                    self.qa_feature_phase = QaFeaturePhase::WaitSearchRepopulated;
                }
            }
            QaFeaturePhase::WaitSearchRepopulated => {
                if self.qa_viewport_is_settled() && self.search.order.len() >= 2 {
                    self.navigate_search(true, window, cx);
                    self.qa_feature_phase = QaFeaturePhase::WaitFinalNavigation;
                }
            }
            QaFeaturePhase::WaitFinalNavigation => {
                if self.qa_viewport_is_settled() {
                    self.qa_validate_feature_scenario()?;
                    self.qa_feature_phase = QaFeaturePhase::Complete;
                    return Ok(true);
                }
            }
            QaFeaturePhase::Complete => return Ok(true),
        }
        Ok(false)
    }

    /// Exercises Fluid-only interaction semantics through native editor input,
    /// production autosave/persistence, annotation hit testing, both comment
    /// pane slide directions, and the overlay search-panel geometry.
    #[cfg(debug_assertions)]
    pub fn qa_drive_fluid_scenario(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<bool, String> {
        match self.qa_fluid_phase {
            QaFluidPhase::Seed => {
                if !self.qa_viewport_is_settled()
                    || self.annotations_loading
                    || !self.page_text.contains_key(&0)
                {
                    return Ok(false);
                }
                if self
                    .annotations
                    .as_ref()
                    .is_none_or(|annotations| annotations.iter().next().is_some())
                {
                    return Err("fluid scenario requires a PDF with no existing sidecar".into());
                }
                self.set_view_mode(ReaderView::Fluid, window, cx);
                let range = self
                    .qa_text_range(0, "Select this sentence")
                    .ok_or_else(|| "fixture Fluid text was not extracted".to_owned())?;
                self.selection = Some(range.as_selection());
                if std::env::var_os("GPUI_PDF_READER_QA_FLUID_SELECTION_VISUAL").is_some() {
                    self.qa_fluid_phase = QaFluidPhase::Complete;
                    cx.notify();
                    return Ok(true);
                }
                self.add_highlight(HighlightColor::Yellow, cx);
                let id = self
                    .active_annotation
                    .ok_or_else(|| "Fluid highlight did not become active".to_owned())?;
                if self.annotation_at_text_position(range.start()) != Some(id) {
                    return Err("highlight hit lookup did not resolve the active annotation".into());
                }
                self.comment_on_context(window, cx);
                self.qa_fluid_phase = QaFluidPhase::WaitEditor;
            }
            QaFluidPhase::WaitEditor => {
                let Some(editor) = self.comment_editor.clone() else {
                    return Err("Fluid comment editor disappeared before painting".into());
                };
                if editor.read(cx).qa_has_painted()
                    && self.comment_pane.progress == 1.0
                    && self.sidebar.progress == 1.0
                {
                    Self::qa_defer_keystrokes(
                        &["f", "l", "u", "i", "d", "space", "n", "o", "t", "e"],
                        window,
                        cx,
                    )?;
                    self.qa_fluid_phase = QaFluidPhase::WaitAutosave;
                }
            }
            QaFluidPhase::WaitAutosave => {
                if !self.qa_viewport_is_settled() {
                    return Ok(false);
                }
                let (id, range, comment, highlight) = self
                    .annotations
                    .as_ref()
                    .and_then(|annotations| {
                        annotations.iter().find_map(|annotation| {
                            annotation.comment_markdown().map(|comment| {
                                (
                                    annotation.id(),
                                    annotation.range(),
                                    comment.to_owned(),
                                    annotation.highlight(),
                                )
                            })
                        })
                    })
                    .ok_or_else(|| "Fluid autosave produced no persisted comment".to_owned())?;
                if comment != "fluid note" || highlight != Some(HighlightColor::Yellow) {
                    return Err(format!(
                        "Fluid autosave produced unexpected annotation: comment={comment:?}, highlight={highlight:?}"
                    ));
                }
                if self.comment_editor.is_none()
                    || self.comment_draft_dirty
                    || self.editing_annotation != Some(id)
                {
                    return Err(
                        "Fluid autosave closed the editor or left the saved draft dirty".into(),
                    );
                }
                if self.context_range() != Some(range) || !self.context_has_comment() {
                    return Err("Fluid context did not switch from Add note to Edit note".into());
                }
                self.return_to_comment_list(window, cx);
                self.qa_fluid_phase = QaFluidPhase::WaitList;
            }
            QaFluidPhase::WaitList => {
                if !self.qa_viewport_is_settled()
                    || self.comment_editor.is_some()
                    || self.comment_pane.progress != 0.0
                {
                    return Ok(false);
                }
                let (id, start) = self
                    .annotations
                    .as_ref()
                    .and_then(|annotations| {
                        annotations.iter().find_map(|annotation| {
                            annotation
                                .comment_markdown()
                                .is_some()
                                .then_some((annotation.id(), annotation.range().start()))
                        })
                    })
                    .ok_or_else(|| "Fluid comment vanished after Back".to_owned())?;
                let page = self
                    .layout()
                    .and_then(|layout| layout.page_rect(start.page))
                    .ok_or_else(|| "Fluid hit-test page is not laid out".to_owned())?;
                let bounds = self
                    .page_text
                    .get(&start.page)
                    .and_then(|text| text.get(start.index))
                    .and_then(|character| character.bounds)
                    .ok_or_else(|| "Fluid hit-test character has no bounds".to_owned())?;
                let pointer = self.canvas_to_window(Offset {
                    x: page.x + (bounds.left + bounds.right) * page.width * 0.5 - self.scroll.x,
                    y: page.y + (bounds.top + bounds.bottom) * page.height * 0.5 - self.scroll.y,
                });
                self.active_annotation = None;
                self.on_mouse_down(
                    &MouseDownEvent {
                        button: MouseButton::Left,
                        position: pointer,
                        click_count: 1,
                        ..Default::default()
                    },
                    window,
                    cx,
                );
                self.on_mouse_up(
                    &MouseUpEvent {
                        button: MouseButton::Left,
                        position: pointer,
                        ..Default::default()
                    },
                    window,
                    cx,
                );
                if self.active_annotation != Some(id) || self.selection.is_some() {
                    return Err("clicking a Fluid highlight did not activate it cleanly".into());
                }
                if std::env::var_os("GPUI_PDF_READER_QA_FLUID_CONTEXT_VISUAL").is_some() {
                    self.qa_fluid_phase = QaFluidPhase::Complete;
                    return Ok(true);
                }
                self.open_comment_from_list(id, window, cx);
                self.qa_fluid_phase = QaFluidPhase::WaitReopenedEditor;
            }
            QaFluidPhase::WaitReopenedEditor => {
                let Some(editor) = self.comment_editor.clone() else {
                    return Err("comment-list navigation did not open the Fluid editor".into());
                };
                if self.comment_pane.progress == 1.0 && editor.read(cx).qa_has_painted() {
                    if editor.read(cx).markdown() != "fluid note" {
                        return Err("reopened Fluid editor did not preserve Markdown".into());
                    }
                    if std::env::var_os("GPUI_PDF_READER_QA_FLUID_EDITOR_VISUAL").is_some() {
                        self.qa_fluid_phase = QaFluidPhase::Complete;
                        return Ok(true);
                    }
                    self.return_to_comment_list(window, cx);
                    self.qa_fluid_phase = QaFluidPhase::WaitFinalList;
                }
            }
            QaFluidPhase::WaitFinalList => {
                if self.qa_viewport_is_settled()
                    && self.comment_editor.is_none()
                    && self.comment_pane.progress == 0.0
                {
                    self.show_sidebar(SidePanel::Search, window, cx);
                    self.qa_fluid_phase = QaFluidPhase::WaitSearchOpen;
                }
            }
            QaFluidPhase::WaitSearchOpen => {
                if self.qa_viewport_is_settled()
                    && self.sidebar.panel == SidePanel::Search
                    && self.sidebar.progress == 1.0
                {
                    window.focus(&self.search_field.focus_handle(cx));
                    Self::qa_defer_keystrokes(&["p", "a", "g", "e"], window, cx)?;
                    self.qa_fluid_phase = QaFluidPhase::WaitSearchResults;
                }
            }
            QaFluidPhase::WaitSearchResults => {
                if !self.qa_viewport_is_settled() {
                    return Ok(false);
                }
                let annotations = self
                    .annotations
                    .as_ref()
                    .ok_or_else(|| "Fluid annotations disappeared".to_owned())?;
                let annotation = annotations
                    .iter()
                    .next()
                    .ok_or_else(|| "Fluid scenario annotation disappeared".to_owned())?;
                if self.view_mode != ReaderView::Fluid
                    || annotations.len() != 1
                    || annotation.highlight() != Some(HighlightColor::Yellow)
                    || annotation.comment_markdown() != Some("fluid note")
                    || self.active_annotation != Some(annotation.id())
                    || !self.context_has_comment()
                    || self.context_anchor_in_viewport().is_none()
                    || self.search.order.len() < 2
                    || !self.search.complete
                {
                    return Err(format!(
                        "unexpected final Fluid state: view={:?}, annotations={}, results={}, complete={}",
                        self.view_mode,
                        annotations.len(),
                        self.search.order.len(),
                        self.search.complete
                    ));
                }
                let layout = self
                    .layout()
                    .ok_or_else(|| "Fluid layout disappeared".to_owned())?;
                let base_max = (layout.content_width - self.viewport_width).max(0.0);
                let expected_max = base_max + self.fluid_panel_occlusion();
                if (self.max_scroll_x(layout) - expected_max).abs() > 0.01
                    || self.fluid_panel_occlusion() <= SIDEBAR_WIDTH
                {
                    return Err("Fluid panel occlusion was not added to horizontal reach".into());
                }
                self.qa_fluid_phase = QaFluidPhase::Complete;
                return Ok(true);
            }
            QaFluidPhase::Complete => return Ok(true),
        }
        Ok(false)
    }
}

#[cfg(all(debug_assertions, feature = "installable-extensions"))]
fn qa_extension_package_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("extensions")
        .join(name)
        .join("package")
}

#[cfg(all(debug_assertions, feature = "installable-extensions"))]
fn qa_packages_are_active(packages: &[InstalledPackageSummary], expected: &[&str]) -> bool {
    expected.iter().all(|expected| {
        packages.iter().any(|package| {
            package.extension.as_str() == *expected && package.state == LifecycleState::Active
        })
    })
}

#[cfg(all(debug_assertions, feature = "installable-extensions"))]
fn qa_reject_failed_packages(packages: &[InstalledPackageSummary]) -> Result<(), String> {
    if let Some(package) = packages.iter().find(|package| {
        matches!(
            package.state,
            LifecycleState::Failed | LifecycleState::Suspended
        )
    }) {
        return Err(format!(
            "extension {} entered {:?}: {}",
            package.extension,
            package.state,
            package.restoration_error.as_deref().unwrap_or("no detail")
        ));
    }
    Ok(())
}

#[cfg(all(debug_assertions, feature = "installable-extensions"))]
fn qa_nested_state<'a>(
    state: &'a BTreeMap<String, DataValue>,
    path: &[&str],
) -> Option<&'a DataValue> {
    let (first, rest) = path.split_first()?;
    let mut value = state.get(*first)?;
    for segment in rest {
        let DataValue::Record(record) = value else {
            return None;
        };
        value = record.get(*segment)?;
    }
    Some(value)
}

#[cfg(all(debug_assertions, feature = "installable-extensions"))]
fn qa_nested_integer(state: &BTreeMap<String, DataValue>, path: &[&str]) -> i64 {
    match qa_nested_state(state, path) {
        Some(DataValue::Integer(value)) => *value,
        _ => 0,
    }
}
