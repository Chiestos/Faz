use crate::quad::TripleLayerQuadAllocator;
use crate::tabbar::TabBarItem;
use crate::termwindow::render::{forces_opaque_faz_tui_window_background, RenderScreenLineParams};
use crate::termwindow::{UIItem, UIItemType};
use crate::utilsprites::RenderMetrics;
use config::ConfigHandle;
use mux::renderable::RenderableDimensions;
use wezterm_term::color::ColorAttribute;
use window::color::LinearRgba;

impl crate::TermWindow {
    pub fn paint_tab_bar(&mut self, layers: &mut TripleLayerQuadAllocator) -> anyhow::Result<()> {
        let border = self.get_os_border();
        let tab_bar_height = self.tab_bar_pixel_height()?;
        // Sit directly under the standard window buttons rather than at the
        // very top of the window. The buttons are centred in the titlebar, so
        // their bottom edge is not the tab bar's height — the tab bar keeps its
        // one-line height and is pushed down instead, leaving the strip above
        // it as window-drag space.
        let title_bar_height = self
            .os_parameters
            .as_ref()
            .and_then(|p| p.title_bar.height)
            .map(|h| h.get() as f32)
            .unwrap_or(0.);
        let tab_bar_y = if self.config.tab_bar_at_bottom {
            ((self.dimensions.pixel_height as f32) - tab_bar_height - border.bottom.get() as f32)
                .max(0.)
        } else {
            // Offset below the OS top inset so cells aren't clipped by the
            // macOS rounded corner / integrated buttons window mask.
            let under_buttons = (title_bar_height - tab_bar_height).max(0.);
            (border.top.get() as f32).max(under_buttons)
        };
        let panes = self.get_panes_to_render();
        let force_opaque_tab_bar_background = forces_opaque_faz_tui_window_background(&panes);

        let palette = self.palette().clone();

        let tab_metrics = if self.config.tab_bar_at_bottom {
            // Bottom tabs have no rounded titlebar mask above them, so keep the
            // compact natural height used by earlier releases.
            RenderMetrics::with_font_metrics(&self.fonts.default_font()?.metrics())
        } else {
            // Top tabs sit under the macOS titlebar mask; honor line_height so
            // tall fonts don't clip against the top edge.
            self.render_metrics
        };

        // The strip the tab bar leaves above itself — the space the standard
        // window buttons sit in — is window chrome, not terminal content. Mark
        // it as a drag target so clicking and dragging there moves the window,
        // the way a titlebar does. Pushed *before* the tab entries because the
        // hit test walks the list in reverse, so real tabs still win.
        if tab_bar_y > 0. {
            self.ui_items.push(UIItem {
                x: 0,
                width: self.dimensions.pixel_width,
                y: 0,
                height: tab_bar_y as usize,
                item_type: UIItemType::TabBar(TabBarItem::None),
            });
        }

        self.ui_items.append(&mut self.tab_bar.compute_ui_items(
            tab_bar_y as usize,
            tab_metrics.cell_size.height as usize,
            tab_metrics.cell_size.width as usize,
        ));

        let window_is_transparent =
            !self.window_background.is_empty() || self.config.window_background_opacity != 1.0;
        let effective_window_is_transparent =
            window_is_transparent && !force_opaque_tab_bar_background;
        let gl_state = self.render_state.as_ref().unwrap();
        let white_space = gl_state.util_sprites.white_space.texture_coords();
        let filled_box = gl_state.util_sprites.filled_box.texture_coords();
        let default_bg = palette
            .resolve_bg(ColorAttribute::Default)
            .to_linear()
            .mul_alpha(if effective_window_is_transparent {
                0.
            } else {
                self.config.text_background_opacity
            });

        if effective_window_is_transparent {
            let tab_bar_bg = if let Some(active) = self.get_active_pane_or_overlay() {
                active
                    .palette()
                    .background
                    .to_linear()
                    .mul_alpha(self.config.window_background_opacity)
            } else {
                palette
                    .background
                    .to_linear()
                    .mul_alpha(self.config.window_background_opacity)
            };
            self.filled_rectangle(
                layers,
                0,
                euclid::rect(
                    0.0,
                    tab_bar_y,
                    self.dimensions.pixel_width as f32,
                    tab_bar_height,
                ),
                tab_bar_bg,
            )?;
        }

        self.render_screen_line(
            RenderScreenLineParams {
                top_pixel_y: tab_bar_y,
                left_pixel_x: 0.,
                pixel_width: self.dimensions.pixel_width as f32,
                stable_line_idx: None,
                line: self.tab_bar.line(),
                selection: 0..0,
                cursor: &Default::default(),
                palette: &palette,
                dims: &RenderableDimensions {
                    cols: self.dimensions.pixel_width / tab_metrics.cell_size.width as usize,
                    physical_top: 0,
                    scrollback_rows: 0,
                    scrollback_top: 0,
                    viewport_rows: 1,
                    dpi: self.terminal_size.dpi,
                    pixel_height: tab_metrics.cell_size.height as usize,
                    pixel_width: self.terminal_size.pixel_width,
                    reverse_video: false,
                },
                config: &self.config,
                cursor_border_color: LinearRgba::default(),
                foreground: palette.foreground.to_linear(),
                pane: None,
                is_active: true,
                selection_fg: LinearRgba::default(),
                selection_bg: LinearRgba::default(),
                cursor_fg: LinearRgba::default(),
                cursor_bg: LinearRgba::default(),
                cursor_is_default_color: true,
                white_space,
                filled_box,
                window_is_transparent: effective_window_is_transparent,
                default_bg,
                style: None,
                font: None,
                use_pixel_positioning: self.config.experimental_pixel_positioning,
                render_metrics: tab_metrics,
                shape_key: None,
                password_input: false,
            },
            layers,
        )?;

        Ok(())
    }

    pub fn tab_bar_pixel_height_impl(
        config: &ConfigHandle,
        fontconfig: &wezterm_font::FontConfiguration,
        render_metrics: &RenderMetrics,
    ) -> anyhow::Result<f32> {
        let _ = fontconfig;
        if config.tab_bar_at_bottom {
            Ok(render_metrics.natural_cell_height as f32)
        } else {
            Ok(render_metrics.cell_size.height as f32)
        }
    }

    /// Cheap approximation of tab bar height that avoids the ~485ms cost of
    /// resolving the title font on macOS cold start (CoreText substitution
    /// lookup + HarfBuzz shaper init). Used only to compute initial window
    /// dimensions; the real height is computed lazily on first render via
    /// `tab_bar_pixel_height()`.
    pub fn estimated_tab_bar_pixel_height(
        config: &ConfigHandle,
        render_metrics: &RenderMetrics,
    ) -> f32 {
        if config.tab_bar_at_bottom {
            render_metrics.natural_cell_height as f32
        } else {
            render_metrics.cell_size.height as f32
        }
    }

    pub fn tab_bar_pixel_height(&self) -> anyhow::Result<f32> {
        Self::tab_bar_pixel_height_impl(&self.config, &self.fonts, &self.render_metrics)
    }
}
