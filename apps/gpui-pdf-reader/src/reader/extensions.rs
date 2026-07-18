use super::*;

impl PdfReader {
    fn render_extension_contribution_panel(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let palette = ReaderPalette::from_theme(Theme::global(cx));
        let Some(pane) = self.extension_contribution.as_ref() else {
            return empty_state(
                palette,
                IconName::LayoutDashboard,
                "No extension panel",
                "Open a contribution from the Extensions overview.",
            );
        };
        let title = pane.title.clone();
        let view = pane.view.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(palette.surface)
            .text_color(palette.text)
            .child(
                div()
                    .h(px(54.0))
                    .flex_none()
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(palette.separator)
                    .child(Self::chrome_button(
                        palette,
                        "extension-contribution-back",
                        Self::icon_label(IconName::ChevronLeft, "Extensions"),
                        ChromeButtonStyle::Ghost,
                        true,
                        cx.listener(|reader, _, window, cx| {
                            reader.manage_extensions(&ManageExtensions, window, cx)
                        }),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(Self::chrome_button(
                        palette,
                        "close-extension-contribution",
                        Icon::new(IconName::Close).size(px(16.0)),
                        ChromeButtonStyle::Ghost,
                        true,
                        cx.listener(|reader, _, window, cx| {
                            let _ = reader.close_extension_contribution(None, window, cx);
                        }),
                    )),
            )
            .child(
                div()
                    .id("extension-contribution-scroll")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .p_4()
                    .child(view),
            )
            .into_any_element()
    }

    pub(super) fn render_extension_ui_floating_panel(
        &mut self,
        palette: ReaderPalette,
        full_width: f32,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        self.extension_contribution.as_ref()?;
        let progress = self.extension_ui_panel.value();
        if progress <= 0.0 && self.extension_ui_panel.target() <= 0.0 {
            return None;
        }
        let width = SIDEBAR_WIDTH.min(
            (full_width - MIN_DOCUMENT_VIEWPORT_WIDTH - FLUID_PANEL_HORIZONTAL_MARGIN * 2.0)
                .max(280.0),
        );
        let hidden_distance = width + FLUID_PANEL_HORIZONTAL_MARGIN + 8.0;
        let right = FLUID_PANEL_HORIZONTAL_MARGIN - hidden_distance * (1.0 - progress);
        let content = self.render_extension_contribution_panel(cx);
        Some(
            div()
                .id("extension-ui-floating-panel")
                .absolute()
                .top(px(
                    self.reader_toolbar_height() + FLUID_PANEL_VERTICAL_MARGIN
                ))
                .bottom(px(FLUID_PANEL_VERTICAL_MARGIN))
                .right(px(right))
                .w(px(width))
                .child(FloatingPanel::new(palette, content))
                .into_any_element(),
        )
    }

    pub(super) fn render_extensions_panel(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        #[cfg(not(feature = "installable-extensions"))]
        return self.render_extensions_overview(cx);

        #[cfg(feature = "installable-extensions")]
        {
            let overview = self.render_extensions_overview(cx);
            let Some(page) = self.extension_manager_page.clone() else {
                return overview;
            };
            let progress = self.extension_manager_transition.value();
            let detail = match page {
                ExtensionManagerPage::InstallReview { preview, .. } => {
                    self.render_extension_install_review(*preview, cx)
                }
                ExtensionManagerPage::Details(extension) => {
                    self.render_extension_details_page(extension, cx)
                }
            };
            div()
                .relative()
                .size_full()
                .overflow_hidden()
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(-SIDEBAR_WIDTH * progress))
                        .w_full()
                        .child(overview),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(SIDEBAR_WIDTH * (1.0 - progress)))
                        .w_full()
                        .child(detail),
                )
                .into_any_element()
        }
    }

    fn render_extensions_overview(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let palette = ReaderPalette::from_theme(Theme::global(cx));
        let header = div()
            .h(px(54.0))
            .flex_none()
            .px_4()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(palette.separator)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Icon::new(IconName::LayoutDashboard).size(px(17.0)))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Extensions"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(Self::chrome_button(
                        palette,
                        "install-extension-from-manager",
                        Icon::new(IconName::Plus).size(px(16.0)),
                        ChromeButtonStyle::Ghost,
                        cfg!(feature = "installable-extensions"),
                        cx.listener(|reader, _, window, cx| {
                            reader.install_extension_dialog(&InstallExtension, window, cx)
                        }),
                    ))
                    .child(Self::chrome_button(
                        palette,
                        "close-extensions",
                        Icon::new(IconName::Close).size(px(16.0)),
                        ChromeButtonStyle::Ghost,
                        true,
                        cx.listener(|reader, _, window, cx| {
                            reader.toggle_sidebar(SidePanel::Extensions, window, cx)
                        }),
                    )),
            );

        #[cfg(not(feature = "installable-extensions"))]
        let body = empty_state(
            palette,
            IconName::SquareTerminal,
            "Installable extensions are not included",
            "Use the standard build to install declarative or sandboxed WebAssembly packages.",
        );

        #[cfg(feature = "installable-extensions")]
        let body = if self.extension_packages.is_empty() {
            div()
                .id("extensions-scroll")
                .flex_1()
                .min_h(px(0.0))
                .flex()
                .flex_col()
                .child(empty_state(
                    palette,
                    IconName::SquareTerminal,
                    "No local extensions",
                    "Install a .keyext archive or a development package. Native code is never loaded.",
                ))
                .child(
                    div()
                        .flex_none()
                        .p_4()
                        .pt_0()
                        .flex()
                        .justify_center()
                        .child(Self::chrome_button(
                            palette,
                            "install-first-extension",
                            Self::icon_label(IconName::Plus, "Install or Update…"),
                            ChromeButtonStyle::Primary,
                            true,
                            cx.listener(|reader, _, window, cx| {
                                reader.install_extension_dialog(&InstallExtension, window, cx)
                            }),
                        )),
                )
                .into_any_element()
        } else {
            let packages = self.extension_packages.clone();
            let commands = self.extension_commands.clone();
            div()
                .id("extensions-scroll")
                .flex_1()
                .min_h(px(0.0))
                .overflow_y_scroll()
                .p_3()
                .flex()
                .flex_col()
                .gap_3()
                .children(packages.into_iter().enumerate().map(|(index, package)| {
                    let active = package.state == LifecycleState::Active;
                    let (state_label, state_color) =
                        extension_state_presentation(package.state, palette);
                    let source = match package.source {
                        PackageSourceKind::KeyextArchive => "Archive",
                        PackageSourceKind::DevelopmentDirectory => "Development",
                    };
                    let trust = if package.publisher_verified {
                        "Verified publisher"
                    } else {
                        "Local · unverified"
                    };
                    let ui_kind = package
                        .ui_kind
                        .clone()
                        .unwrap_or_else(|| "host-rendered".into());
                    let license = format!("License · {}", package.license);
                    let extension = package.extension.clone();
                    let extension_for_details = extension.clone();
                    let extension_for_toggle = extension.clone();
                    let extension_for_remove = extension.clone();
                    let name_for_remove = package.name.clone();
                    let package_permissions = package.permissions.clone();
                    let restoration_error = package.restoration_error.clone();
                    let can_toggle = restoration_error.is_none();
                    let package_commands = commands
                        .iter()
                        .filter(|command| command.owner == extension)
                        .cloned()
                        .collect::<Vec<_>>();
                    div()
                        .id(("extension-package", index))
                        .w_full()
                        .overflow_hidden()
                        .rounded_lg()
                        .border_1()
                        .border_color(palette.separator)
                        .bg(palette.surface)
                        .child(
                            div()
                                .p_3()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .flex()
                                        .items_start()
                                        .justify_between()
                                        .gap_2()
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w(px(0.0))
                                                .child(
                                                    div()
                                                        .overflow_hidden()
                                                        .whitespace_nowrap()
                                                        .text_ellipsis()
                                                        .text_sm()
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .child(package.name.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .mt_1()
                                                        .text_xs()
                                                        .text_color(palette.text_secondary)
                                                        .child(format!(
                                                            "{} · v{}",
                                                            package.extension, package.version
                                                        )),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .px_2()
                                                .py_1()
                                                .rounded_full()
                                                .bg(state_color.opacity(0.12))
                                                .text_xs()
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(state_color)
                                                .child(state_label),
                                        ),
                                )
                                .child(
                                    div().flex().flex_wrap().gap_1().children(
                                        [source, trust, ui_kind.as_str(), license.as_str()]
                                            .into_iter()
                                            .map(|label| {
                                                div()
                                                    .px_2()
                                                    .py_1()
                                                    .rounded_full()
                                                    .bg(palette.surface_subtle)
                                                    .text_xs()
                                                    .text_color(palette.text_secondary)
                                                    .child(label.to_owned())
                                        }),
                                    ),
                                )
                                .when_some(restoration_error, |card, error| {
                                    card.child(
                                        div()
                                            .p_2()
                                            .rounded_md()
                                            .bg(palette.error_soft)
                                            .text_xs()
                                            .text_color(palette.error)
                                            .child(error),
                                    )
                                })
                                .when(!package_permissions.is_empty(), |card| {
                                    card.child(
                                        div()
                                            .pt_1()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(palette.text_secondary)
                                                    .child("Permissions"),
                                            )
                                            .children(
                                                package_permissions.into_iter().enumerate().map(
                                                    |(
                                                        permission_index,
                                                        (request, decision),
                                                    )| {
                                                        let granted =
                                                            decision == PermissionDecision::Granted;
                                                        let extension = extension.clone();
                                                        let permission = request.permission.clone();
                                                        let label =
                                                            extension_permission_label(&permission);
                                                        div()
                                                            .id((
                                                                "extension-permission",
                                                                index * 100 + permission_index,
                                                            ))
                                                            .p_2()
                                                            .flex()
                                                            .items_start()
                                                            .justify_between()
                                                            .gap_2()
                                                            .rounded_md()
                                                            .bg(palette.surface_subtle)
                                                            .child(
                                                                div()
                                                                    .flex_1()
                                                                    .min_w(px(0.0))
                                                                    .child(
                                                                        div()
                                                                            .flex()
                                                                            .items_center()
                                                                            .gap_1()
                                                                            .text_xs()
                                                                            .font_weight(
                                                                                FontWeight::MEDIUM,
                                                                            )
                                                                            .child(label)
                                                                            .when(
                                                                                request.required,
                                                                                |row| {
                                                                                    row.child(
                                                                                        div()
                                                                                            .px_1()
                                                                                            .rounded_sm()
                                                                                            .bg(palette.warning.opacity(0.14))
                                                                                            .text_color(palette.warning)
                                                                                            .child("Required"),
                                                                                    )
                                                                                },
                                                                            ),
                                                                    )
                                                                    .child(
                                                                        div()
                                                                            .mt_1()
                                                                            .text_xs()
                                                                            .text_color(
                                                                                palette
                                                                                    .text_secondary,
                                                                            )
                                                                            .child(request.reason),
                                                                    ),
                                                            )
                                                            .child(
                                                                div()
                                                                    .id((
                                                                        "extension-permission-toggle",
                                                                        index * 100
                                                                            + permission_index,
                                                                    ))
                                                                    .h(px(26.0))
                                                                    .px_2()
                                                                    .flex_none()
                                                                    .flex()
                                                                    .items_center()
                                                                    .rounded_md()
                                                                    .cursor_pointer()
                                                                    .text_xs()
                                                                    .font_weight(FontWeight::MEDIUM)
                                                                    .text_color(if granted {
                                                                        palette.error
                                                                    } else {
                                                                        palette.accent
                                                                    })
                                                                    .hover(move |button| {
                                                                        button.bg(if granted {
                                                                            palette.error_soft
                                                                        } else {
                                                                            palette.accent_soft
                                                                        })
                                                                    })
                                                                    .on_click(cx.listener(
                                                                        move |reader,
                                                                              _,
                                                                              window,
                                                                              cx| {
                                                                            reader
                                                                                .set_extension_permission(
                                                                                    extension
                                                                                        .clone(),
                                                                                    permission
                                                                                        .clone(),
                                                                                    !granted,
                                                                                    window,
                                                                                    cx,
                                                                                )
                                                                        },
                                                                    ))
                                                                    .child(if granted {
                                                                        "Revoke"
                                                                    } else {
                                                                        "Allow"
                                                                    }),
                                                            )
                                                    },
                                                ),
                                            ),
                                    )
                                })
                                .when(!package_commands.is_empty(), |card| {
                                    card.child(div().pt_1().flex().flex_col().gap_1().children(
                                        package_commands.into_iter().enumerate().map(
                                            |(command_index, command)| {
                                                let title = command.command.title.clone();
                                                let action = InvokeExtensionCommand {
                                                    command: command.command.id,
                                                    payload: None,
                                                };
                                                div()
                                                    .id(SharedString::from(format!(
                                                        "extension-command-{index}-{command_index}"
                                                    )))
                                                    .h(px(30.0))
                                                    .w_full()
                                                    .px_2()
                                                    .flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .rounded_md()
                                                    .cursor_pointer()
                                                    .text_xs()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(palette.accent)
                                                    .hover(move |button| {
                                                        button.bg(palette.accent_soft)
                                                    })
                                                    .on_click(cx.listener(
                                                        move |reader, _, window, cx| {
                                                            reader.invoke_extension_command(
                                                                &action, window, cx,
                                                            )
                                                        },
                                                    ))
                                                    .child(title)
                                                    .child(
                                                        Icon::new(IconName::ArrowRight)
                                                            .size(px(14.0)),
                                                    )
                                            },
                                        ),
                                    ))
                                })
                                .child(
                                    div()
                                        .pt_1()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .id(("extension-details", index))
                                                .h(px(28.0))
                                                .px_3()
                                                .flex()
                                                .items_center()
                                                .gap_1()
                                                .rounded_md()
                                                .cursor_pointer()
                                                .text_xs()
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(palette.accent)
                                                .hover(move |button| {
                                                    button.bg(palette.accent_soft)
                                                })
                                                .on_click(cx.listener(
                                                    move |reader, _, window, cx| {
                                                        reader.open_extension_details(
                                                            extension_for_details.clone(),
                                                            window,
                                                            cx,
                                                        )
                                                    },
                                                ))
                                                .child("Details")
                                                .child(
                                                    Icon::new(IconName::ChevronRight)
                                                        .size(px(13.0)),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .id(("extension-toggle", index))
                                                .h(px(28.0))
                                                .px_3()
                                                .flex()
                                                .items_center()
                                                .rounded_md()
                                                .border_1()
                                                .border_color(palette.separator)
                                                .text_xs()
                                                .font_weight(FontWeight::MEDIUM)
                                                .when(can_toggle, |button| {
                                                    button
                                                        .cursor_pointer()
                                                        .hover(move |button| {
                                                            button.bg(palette.control_hover)
                                                        })
                                                        .on_click(cx.listener(
                                                            move |reader, _, window, cx| {
                                                                if active {
                                                                    reader.disable_extension(
                                                                        extension_for_toggle
                                                                            .clone(),
                                                                        window,
                                                                        cx,
                                                                    );
                                                                } else {
                                                                    reader.enable_extension(
                                                                        extension_for_toggle
                                                                            .clone(),
                                                                        window,
                                                                        cx,
                                                                    );
                                                                }
                                                            },
                                                        ))
                                                })
                                                .child(if can_toggle {
                                                    if active { "Disable" } else { "Enable" }
                                                } else {
                                                    "Unavailable"
                                                }),
                                        )
                                        .child(
                                            div()
                                                .id(("extension-remove", index))
                                                .h(px(28.0))
                                                .px_3()
                                                .flex()
                                                .items_center()
                                                .rounded_md()
                                                .cursor_pointer()
                                                .text_xs()
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(palette.error)
                                                .hover(move |button| button.bg(palette.error_soft))
                                                .on_click(cx.listener(
                                                    move |reader, _, window, cx| {
                                                        reader.confirm_remove_extension(
                                                            extension_for_remove.clone(),
                                                            name_for_remove.clone(),
                                                            window,
                                                            cx,
                                                        )
                                                    },
                                                ))
                                                .child("Remove"),
                                        ),
                                ),
                        )
                }))
                .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .when(self.view_mode == ReaderView::Fluid, |panel| {
                panel.rounded_xl()
            })
            .overflow_hidden()
            .bg(palette.surface)
            .text_color(palette.text)
            .child(header)
            .child(body)
            .into_any_element()
    }

    #[cfg(feature = "installable-extensions")]
    fn render_extension_manager_header(
        &self,
        palette: ReaderPalette,
        title: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .h(px(54.0))
            .flex_none()
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(palette.separator)
            .child(Self::chrome_button(
                palette,
                "extension-manager-overview",
                Self::icon_label(IconName::ChevronLeft, "Overview"),
                ChromeButtonStyle::Ghost,
                true,
                cx.listener(|reader, _, window, cx| {
                    reader.show_extension_manager_overview(window, cx)
                }),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title.into()),
            )
            .child(Self::chrome_button(
                palette,
                "close-extension-manager-detail",
                Icon::new(IconName::Close).size(px(16.0)),
                ChromeButtonStyle::Ghost,
                true,
                cx.listener(|reader, _, window, cx| {
                    reader.toggle_sidebar(SidePanel::Extensions, window, cx)
                }),
            ))
            .into_any_element()
    }

    #[cfg(feature = "installable-extensions")]
    fn render_extension_install_review(
        &mut self,
        preview: PackageInstallPreview,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let palette = ReaderPalette::from_theme(Theme::global(cx));
        let source = match preview.source {
            PackageSourceKind::KeyextArchive => "Signed archive",
            PackageSourceKind::DevelopmentDirectory => "Development folder",
        };
        let trust = if preview.publisher_verified {
            "Verified publisher"
        } else {
            "Local package"
        };
        let action = if preview.is_upgrade {
            "Update & Enable"
        } else {
            "Install & Enable"
        };
        let permission_count = preview.required_permissions.len();
        let permissions = preview.required_permissions.clone();
        let description = if preview.description.trim().is_empty() {
            "No description was provided by this extension.".to_owned()
        } else {
            preview.description.clone()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(palette.surface)
            .text_color(palette.text)
            .child(self.render_extension_manager_header(
                palette,
                if preview.is_upgrade {
                    "Review update"
                } else {
                    "Review extension"
                },
                cx,
            ))
            .child(
                div()
                    .id("extension-install-review-scroll")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .p_4()
                            .rounded_xl()
                            .bg(palette.accent_soft)
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(preview.name.clone()),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(palette.text_secondary)
                                    .child(format!("{} · version {}", preview.extension, preview.version)),
                            )
                            .child(
                                div()
                                    .mt_3()
                                    .text_sm()
                                    .line_height(px(20.0))
                                    .child(description),
                            ),
                    )
                    .child(
                        div().flex().flex_wrap().gap_1().children(
                            [source.to_owned(), trust.to_owned(), format!("{} license", preview.license)]
                                .into_iter()
                                .map(|label| {
                                    div()
                                        .px_2()
                                        .py_1()
                                        .rounded_full()
                                        .bg(palette.surface_subtle)
                                        .text_xs()
                                        .text_color(palette.text_secondary)
                                        .child(label)
                                }),
                        ),
                    )
                    .child(
                        div()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(if permission_count == 0 {
                                        "No protected access requested".to_owned()
                                    } else {
                                        format!(
                                            "{} required permission{}",
                                            permission_count,
                                            if permission_count == 1 { "" } else { "s" }
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .line_height(px(18.0))
                                    .text_color(palette.text_secondary)
                                    .child("The reader grants only the capabilities listed here. Extension code remains sandboxed."),
                            )
                            .when(!permissions.is_empty(), |section| {
                                section.child(
                                    div()
                                        .mt_3()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .children(permissions.into_iter().enumerate().map(
                                            |(index, request)| {
                                                div()
                                                    .id(("install-permission", index))
                                                    .p_3()
                                                    .rounded_lg()
                                                    .bg(palette.surface_subtle)
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .font_weight(FontWeight::SEMIBOLD)
                                                            .child(extension_permission_label(
                                                                &request.permission,
                                                            )),
                                                    )
                                                    .child(
                                                        div()
                                                            .mt_1()
                                                            .text_xs()
                                                            .line_height(px(17.0))
                                                            .text_color(palette.text_secondary)
                                                            .child(request.reason),
                                                    )
                                            },
                                        )),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .p_3()
                    .border_t_1()
                    .border_color(palette.separator)
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::chrome_button(
                        palette,
                        "cancel-extension-install",
                        "Cancel",
                        ChromeButtonStyle::Ghost,
                        true,
                        cx.listener(|reader, _, window, cx| {
                            reader.show_extension_manager_overview(window, cx)
                        }),
                    ))
                    .child(Self::chrome_button(
                        palette,
                        "confirm-extension-install",
                        action,
                        ChromeButtonStyle::Primary,
                        true,
                        cx.listener(|reader, _, window, cx| {
                            reader.confirm_extension_install(window, cx)
                        }),
                    )),
            )
            .into_any_element()
    }

    #[cfg(feature = "installable-extensions")]
    fn render_extension_details_page(
        &mut self,
        extension: ExtensionId,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let palette = ReaderPalette::from_theme(Theme::global(cx));
        let Some(package) = self
            .extension_packages
            .iter()
            .find(|package| package.extension == extension)
            .cloned()
        else {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .bg(palette.surface)
                .child(self.render_extension_manager_header(palette, "Extension", cx))
                .child(empty_state(
                    palette,
                    IconName::SquareTerminal,
                    "Extension unavailable",
                    "It may have been removed while this page was open.",
                ))
                .into_any_element();
        };
        let active = package.state == LifecycleState::Active;
        let can_toggle = package.restoration_error.is_none();
        let (state_label, state_color) = extension_state_presentation(package.state, palette);
        let description = if package.description.trim().is_empty() {
            "No description was provided by this extension.".to_owned()
        } else {
            package.description.clone()
        };
        let permissions = package.permissions.clone();
        let settings = package
            .settings_schema
            .fields
            .iter()
            .filter(|setting| !setting.sensitive)
            .cloned()
            .collect::<Vec<_>>();
        let extension_for_toggle = extension.clone();
        let extension_for_remove = extension.clone();
        let name_for_remove = package.name.clone();

        let mut settings_section = div().flex().flex_col().gap_2();
        if settings.is_empty() {
            settings_section = settings_section.child(
                div()
                    .p_3()
                    .rounded_lg()
                    .bg(palette.surface_subtle)
                    .text_xs()
                    .text_color(palette.text_secondary)
                    .child("This extension has no configurable settings."),
            );
        } else {
            for (index, setting) in settings.into_iter().enumerate() {
                let current = extension_setting_value(&package, &setting.key)
                    .cloned()
                    .unwrap_or_else(|| setting.default.clone());
                let extension_for_setting = extension.clone();
                let key_for_setting = setting.key.clone();
                let control = match setting.value_type.clone() {
                    SettingType::Boolean => {
                        let enabled = matches!(current, DataValue::Boolean(true));
                        div()
                            .mt_2()
                            .id(("extension-setting-boolean", index))
                            .flex()
                            .items_center()
                            .justify_between()
                            .cursor_pointer()
                            .on_click(cx.listener(move |reader, _, window, cx| {
                                reader.apply_extension_setting(
                                    extension_for_setting.clone(),
                                    key_for_setting.clone(),
                                    DataValue::Boolean(!enabled),
                                    window,
                                    cx,
                                )
                            }))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(palette.text_secondary)
                                    .child(if enabled { "Enabled" } else { "Disabled" }),
                            )
                            .child(
                                div()
                                    .w(px(34.0))
                                    .h(px(20.0))
                                    .p(px(2.0))
                                    .flex()
                                    .items_center()
                                    .when(enabled, |toggle| toggle.justify_end().bg(palette.accent))
                                    .when(!enabled, |toggle| {
                                        toggle.justify_start().bg(palette.control_hover)
                                    })
                                    .rounded_full()
                                    .child(div().size(px(16.0)).rounded_full().bg(palette.surface)),
                            )
                            .into_any_element()
                    }
                    SettingType::Choice { options } => {
                        div()
                            .mt_2()
                            .flex()
                            .flex_wrap()
                            .gap_1()
                            .children(options.into_iter().enumerate().map(
                                |(option_index, option)| {
                                    let selected = option.value == current;
                                    let extension = extension_for_setting.clone();
                                    let key = key_for_setting.clone();
                                    let value = option.value.clone();
                                    div()
                                        .id(SharedString::from(format!(
                                            "extension-setting-choice-{index}-{option_index}"
                                        )))
                                        .h(px(28.0))
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .rounded_full()
                                        .cursor_pointer()
                                        .border_1()
                                        .border_color(if selected {
                                            palette.accent
                                        } else {
                                            palette.separator
                                        })
                                        .bg(if selected {
                                            palette.accent_soft
                                        } else {
                                            palette.surface
                                        })
                                        .text_xs()
                                        .text_color(if selected {
                                            palette.accent
                                        } else {
                                            palette.text_secondary
                                        })
                                        .hover(move |button| button.bg(palette.control_hover))
                                        .on_click(cx.listener(move |reader, _, window, cx| {
                                            reader.apply_extension_setting(
                                                extension.clone(),
                                                key.clone(),
                                                value.clone(),
                                                window,
                                                cx,
                                            )
                                        }))
                                        .child(option.label)
                                },
                            ))
                            .into_any_element()
                    }
                    SettingType::String { .. }
                    | SettingType::Integer { .. }
                    | SettingType::Number { .. } => {
                        let mut control = div().mt_2();
                        if let Some(input) = self.extension_setting_inputs.get(&setting.key) {
                            control = control.child(input.clone()).child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(palette.text_tertiary)
                                    .child("Press Return to apply"),
                            );
                        }
                        control.into_any_element()
                    }
                    SettingType::StringList { .. } => div()
                        .mt_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(palette.text_tertiary)
                                .child("List settings are managed by the extension."),
                        )
                        .into_any_element(),
                };
                settings_section = settings_section.child(
                    div()
                        .id(("extension-setting", index))
                        .p_3()
                        .rounded_lg()
                        .bg(palette.surface_subtle)
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(setting.label),
                        )
                        .when(!setting.description.is_empty(), |row| {
                            row.child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .line_height(px(17.0))
                                    .text_color(palette.text_secondary)
                                    .child(setting.description),
                            )
                        })
                        .child(control),
                );
            }
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(palette.surface)
            .text_color(palette.text)
            .child(self.render_extension_manager_header(palette, package.name.clone(), cx))
            .child(
                div()
                    .id("extension-details-scroll")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .p_4()
                            .rounded_xl()
                            .bg(palette.accent_soft)
                            .child(
                                div()
                                    .flex()
                                    .items_start()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.0))
                                            .text_lg()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(package.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .px_2()
                                            .py_1()
                                            .rounded_full()
                                            .bg(state_color.opacity(0.14))
                                            .text_xs()
                                            .text_color(state_color)
                                            .child(state_label),
                                    ),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(palette.text_secondary)
                                    .child(format!("{} · version {}", package.extension, package.version)),
                            )
                            .child(
                                div()
                                    .mt_3()
                                    .text_sm()
                                    .line_height(px(20.0))
                                    .child(description),
                            ),
                    )
                    .child(
                        div()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Settings"),
                            )
                            .child(div().mt_2().child(settings_section)),
                    )
                    .when(!permissions.is_empty(), |body| {
                        body.child(
                            div()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("Permissions"),
                                )
                                .child(
                                    div()
                                        .mt_2()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .children(permissions.into_iter().enumerate().map(
                                            |(index, (request, decision))| {
                                                let granted = decision == PermissionDecision::Granted;
                                                let extension = extension.clone();
                                                let permission = request.permission.clone();
                                                div()
                                                    .id(("extension-detail-permission", index))
                                                    .p_3()
                                                    .rounded_lg()
                                                    .bg(palette.surface_subtle)
                                                    .child(
                                                        div()
                                                            .flex()
                                                            .items_center()
                                                            .justify_between()
                                                            .gap_2()
                                                            .child(
                                                                div()
                                                                    .flex_1()
                                                                    .text_xs()
                                                                    .font_weight(FontWeight::MEDIUM)
                                                                    .child(extension_permission_label(&request.permission)),
                                                            )
                                                            .child(
                                                                div()
                                                                    .id(("extension-detail-permission-toggle", index))
                                                                    .px_2()
                                                                    .py_1()
                                                                    .rounded_md()
                                                                    .cursor_pointer()
                                                                    .text_xs()
                                                                    .text_color(if granted { palette.error } else { palette.accent })
                                                                    .hover(move |button| button.bg(palette.control_hover))
                                                                    .on_click(cx.listener(move |reader, _, window, cx| {
                                                                        reader.set_extension_permission(
                                                                            extension.clone(),
                                                                            permission.clone(),
                                                                            !granted,
                                                                            window,
                                                                            cx,
                                                                        )
                                                                    }))
                                                                    .child(if granted { "Revoke" } else { "Allow" }),
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .mt_1()
                                                            .text_xs()
                                                            .line_height(px(17.0))
                                                            .text_color(palette.text_secondary)
                                                            .child(request.reason),
                                                    )
                                            },
                                        )),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .p_3()
                    .border_t_1()
                    .border_color(palette.separator)
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(Self::chrome_button(
                        palette,
                        "extension-detail-remove",
                        "Remove",
                        ChromeButtonStyle::Ghost,
                        true,
                        cx.listener(move |reader, _, window, cx| {
                            reader.confirm_remove_extension(
                                extension_for_remove.clone(),
                                name_for_remove.clone(),
                                window,
                                cx,
                            )
                        }),
                    ))
                    .child(Self::chrome_button(
                        palette,
                        "extension-detail-toggle",
                        if active { "Disable" } else { "Enable" },
                        ChromeButtonStyle::Primary,
                        can_toggle,
                        cx.listener(move |reader, _, window, cx| {
                            if active {
                                reader.disable_extension(extension_for_toggle.clone(), window, cx)
                            } else {
                                reader.enable_extension(extension_for_toggle.clone(), window, cx)
                            }
                        }),
                    )),
            )
            .into_any_element()
    }
}
