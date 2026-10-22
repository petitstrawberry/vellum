//! ScarletUI frontend for Vellum.

use std::rc::Rc;
use std::sync::{Arc, Mutex};

#[path = "ui_edit.rs"]
mod edit_ui;
use crate::editor::{CanvasMapping, Completion, Drag, Job, Pending, Runtime, Tool};
use vellum_core::{
    Bitmap,
    editing::{EditOperation, EditSession},
};

use scarlet_ui::prelude::*;
use scarlet_ui::{
    Button, CanvasView, Color, HeaderBar, Icon, KeyCode, KeyEvent, MenuBarModel, PlatformWindow,
    Rectangle, ScrollAxis, ScrollView, Size, State, Text, Window, hstack, vstack, zstack,
};
use scarlet_ui_macros::View;
use vellum_core::Document;

const APP_ID: &str = "org.scarlet-os.vellum";
const WINDOW_WIDTH: f32 = 1000.0;
const WINDOW_HEIGHT: f32 = 760.0;
const MIN_ZOOM: f32 = 0.1;
const MAX_ZOOM: f32 = 8.0;

/// Run the Vellum window.
pub fn run(
    document: Option<Arc<Document>>,
    initial_page: usize,
    status: String,
) -> scarlet_ui::Result<()> {
    let mut app = VellumApp::new(document, initial_page, status);
    app.run()
}

#[derive(View, Clone)]
struct VellumApp {
    document: State<Option<Arc<Document>>>,
    current_page: State<usize>,
    render_revision: State<u64>,
    zoom: State<f32>,
    fit_to_window: State<bool>,
    fullscreen: State<bool>,
    fullscreen_applied: State<bool>,
    decorations_hidden: State<bool>,
    chrome_visible: State<bool>,
    status: State<String>,
    edited_bitmap: State<Option<Arc<Bitmap>>>,
    runtime: State<Arc<Mutex<Runtime>>>,
    tool: State<Tool>,
    drag: State<Option<Drag>>,
    canvas_focus: State<bool>,
    annotation_text: State<String>,
    annotation_size: State<String>,
    annotation_color: State<usize>,
    stroke_width: State<String>,
    resize_width: State<String>,
    resize_height: State<String>,
    aspect_lock: State<bool>,
}

impl VellumApp {
    fn new(document: Option<Arc<Document>>, initial_page: usize, status: String) -> Self {
        let page = document
            .as_ref()
            .map(|document| initial_page.min(document.page_count().saturating_sub(1)))
            .unwrap_or(0);
        let app = Self::default();
        app.document.set(document.clone());
        if let Some(document) = document.filter(|d| d.is_image()) {
            if let Ok(Some(bitmap)) = document.cached_page(0) {
                app.edited_bitmap.set(Some(bitmap.clone()));
                app.runtime.get().lock().unwrap().edit = Some(EditSession::new(bitmap));
            }
        }
        app.canvas_focus.set(true);
        app.annotation_size.set("28".into());
        app.stroke_width.set("4".into());
        app.aspect_lock.set(true);
        app.current_page.set(page);
        app.zoom.set(1.0);
        app.fit_to_window.set(true);
        app.status.set(status);
        app.prefetch_neighbors(page);
        app
    }

    fn page_count(&self) -> usize {
        self.document
            .get()
            .map(|document| document.page_count())
            .unwrap_or(0)
    }

    fn current_bitmap(&self) -> Option<Arc<vellum_core::Bitmap>> {
        if let Some(bitmap) = self.edited_bitmap.get() {
            return Some(bitmap);
        }
        let document = self.document.get()?;
        let page = self.current_page.get();
        match document.cached_page(page) {
            Ok(Some(bitmap)) => Some(bitmap),
            Ok(None) => {
                self.request_page(page);
                None
            }
            Err(_) => None,
        }
    }

    fn request_page(&self, page: usize) {
        let Some(document) = self.document.get() else {
            return;
        };

        let shared = self.runtime.get();
        let generation = shared.lock().unwrap().document;
        document.prefetch(page, move |result| {
            shared.lock().unwrap().results.push(Completion::Page {
                document: generation,
                page,
                result,
            });
        });
    }

    fn request_page_at_scale(&self, page: usize, render_scale_milli: u32) {
        let Some(document) = self.document.get() else {
            return;
        };
        if document.is_image() {
            return;
        }
        let shared = self.runtime.get();
        let generation = shared.lock().unwrap().document;
        document.prefetch_at_scale(page, render_scale_milli, move |result| {
            shared.lock().unwrap().results.push(Completion::Page {
                document: generation,
                page,
                result,
            });
        });
    }

    fn prefetch_neighbors(&self, page: usize) {
        if page > 0 {
            self.request_page(page - 1);
        }
        if page + 1 < self.page_count() {
            self.request_page(page + 1);
        }
        self.request_page(page);
    }

    fn move_page(&self, delta: isize) {
        let Some(document) = self.document.get() else {
            return;
        };
        let current = self.current_page.get();
        let last = document.page_count().saturating_sub(1);
        let target = if delta.is_negative() {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current.saturating_add(delta as usize).min(last)
        };
        if target == current {
            return;
        }

        self.current_page.set(target);
        self.status.set(format!(
            "{} — rendering page {} of {}",
            document.title(),
            target + 1,
            document.page_count()
        ));
        self.prefetch_neighbors(target);
    }

    fn set_zoom(&self, zoom: f32) {
        let dimensions = self
            .edited_bitmap
            .get()
            .map(|b| (b.width() as f32, b.height() as f32))
            .or_else(|| {
                self.document
                    .get()
                    .and_then(|d| d.page_dimensions(self.current_page.get()).ok())
            });
        let limit = dimensions
            .map(|(w, h)| (8_388_608.0 / (w * h).max(1.0)).sqrt().min(MAX_ZOOM))
            .unwrap_or(MAX_ZOOM);
        let zoom = if zoom.is_finite() {
            zoom.max(MIN_ZOOM).min(limit)
        } else {
            1.0f32.min(limit)
        };
        self.fit_to_window.set(false);
        self.zoom.set(zoom);
    }

    fn change_zoom(&self, factor: f32) {
        self.set_zoom(self.zoom.get() * factor);
    }

    fn fit_to_window(&self) {
        self.fit_to_window.set(true);
    }

    fn set_fullscreen_mode(&self, fullscreen: bool) {
        self.fullscreen.set(fullscreen);
        self.chrome_visible.set(!fullscreen);
    }

    fn toggle_fullscreen(&self) {
        self.set_fullscreen_mode(!self.fullscreen.get());
    }

    fn update_chrome_visibility(&self, y: i32) {
        if self.fullscreen.get() {
            self.chrome_visible.set(y < 48);
        }
    }

    fn draw_canvas(
        &self,
        buffer: &mut [u8],
        width: u32,
        height: u32,
        page: usize,
        fallback: Option<Arc<vellum_core::Bitmap>>,
    ) {
        if let Some(bitmap) = self.edited_bitmap.get() {
            let scale = scarlet_ui::graphics::current_scale_milli().max(1) as f32 / 1000.0;
            let mapping = CanvasMapping {
                width: width as f32 / scale,
                height: height as f32 / scale,
                image_width: bitmap.width(),
                image_height: bitmap.height(),
            };
            self.runtime.get().lock().unwrap().mapping = mapping;
            draw_bitmap_to_canvas(buffer, width, height, Some(&bitmap));
            self.draw_selection(buffer, width, height, mapping);
            return;
        }
        let Some(document) = self.document.get() else {
            fill_canvas(buffer, [38, 40, 46, 255]);
            return;
        };

        let render_scale_milli = document
            .render_scale_milli_for(page, width, height)
            .unwrap_or(1_000);
        self.request_page_at_scale(page, render_scale_milli);

        let bitmap = document
            .cached_page_at_scale(page, render_scale_milli)
            .ok()
            .flatten()
            .or(fallback);
        draw_bitmap_to_canvas(buffer, width, height, bitmap.as_deref());
    }

    fn handle_key(&self, event: KeyEvent) -> bool {
        if self.edit_key(event.clone()) {
            return true;
        }
        match event {
            KeyEvent::Pressed { keycode, .. } => match keycode {
                KeyCode::F(11) => {
                    self.toggle_fullscreen();
                    true
                }
                KeyCode::Escape if self.fullscreen.get() => {
                    self.set_fullscreen_mode(false);
                    true
                }
                KeyCode::Left | KeyCode::PageUp => {
                    self.move_page(-1);
                    true
                }
                KeyCode::Right | KeyCode::PageDown | KeyCode::Space => {
                    self.move_page(1);
                    true
                }
                KeyCode::Home => {
                    let current = self.current_page.get();
                    self.move_page(-(current as isize));
                    true
                }
                KeyCode::End => {
                    let remaining = self
                        .page_count()
                        .saturating_sub(1)
                        .saturating_sub(self.current_page.get());
                    self.move_page(remaining as isize);
                    true
                }
                _ => false,
            },
            KeyEvent::Char { c: '+' | '=' } => {
                self.change_zoom(1.25);
                true
            }
            KeyEvent::Char { c: '-' | '_' } => {
                self.change_zoom(0.8);
                true
            }
            KeyEvent::Char { c: '0' } => {
                self.set_zoom(1.0);
                true
            }
            _ => false,
        }
    }

    fn content(&self) -> impl View + Clone + use<> {
        let bitmap = self.current_bitmap();
        let zoom = self.zoom.get();
        let fit_to_window = self.fit_to_window.get();
        let page = self.current_page.get();
        let (page_width, page_height) = bitmap
            .as_ref()
            .filter(|_| self.edited_bitmap.get().is_some())
            .map(|b| (b.width() as f32, b.height() as f32))
            .or_else(|| {
                self.document
                    .get()
                    .and_then(|d| d.page_dimensions(page).ok())
            })
            .unwrap_or((640.0, 480.0));
        let (canvas_width, canvas_height, content_width, content_height) =
            canvas_layout_dimensions(page_width, page_height, zoom, fit_to_window);

        let previous = self.clone();
        let next = self.clone();
        let zoom_out = self.clone();
        let zoom_in = self.clone();
        let fit = self.clone();
        let actual_size = self.clone();
        let fullscreen = self.clone();
        let key_app = self.clone();
        let page_count = self.page_count();
        let fullscreen_mode = self.fullscreen.get();
        let chrome_visible = self.chrome_visible.get();
        let zoom_label = if fit_to_window {
            String::from("Auto")
        } else {
            format!("{}%", (zoom * 100.0).round() as u32)
        };
        let canvas_app = self.clone();
        let canvas_fallback = bitmap.clone();
        let pointer = self.clone();
        let canvas = CanvasView::new(
            canvas_width,
            canvas_height,
            Rc::new(move |buffer, width, height| {
                canvas_app.draw_canvas(buffer, width, height, page, canvas_fallback.clone());
            }),
        )
        .on_event(move |event| pointer.image_event(event))
        .focusable(self.canvas_focus.clone());
        let canvas_content = zstack! {
            canvas.frame(content_width, content_height),
        }
        .alignment(Alignment::Center);
        let scroll = ScrollView::new(canvas_content).axes(ScrollAxis::Both);
        let scroll = if fit_to_window {
            scroll
        } else {
            scroll.content_size(content_width, content_height)
        };

        let header = HeaderBar::new(
            hstack! {
                scarlet_ui::if_view!(page_count > 1,
                    hstack! {
                        Button::icon_only(Icon::ChevronLeft).header_style().enabled(page > 0)
                            .on_click(move || previous.move_page(-1)),
                        Text::new(format!("Page {} of {}", page + 1, page_count)).font_size(12.0),
                        Button::icon_only(Icon::ChevronRight).header_style().enabled(page + 1 < page_count)
                            .on_click(move || next.move_page(1)),
                    }.spacing(8.0),
                    Text::new(if page_count == 0 { "No document" } else if self.edited_bitmap.get().is_some() { "Image" } else { "PDF · 1 page" })
                        .font_size(12.0).color(ColorPalette::default().text_secondary())
                ),
                Spacer::new(),
                Button::new("Fit")
                    .header_style()
                    .enabled(page_count > 0)
                    .background_color(if fit_to_window { ColorPalette::default().surface_variant() } else { Color::TRANSPARENT })
                    .on_click(move || fit.fit_to_window()),
                Button::new("100%")
                    .header_style()
                    .enabled(page_count > 0)
                    .on_click(move || actual_size.set_zoom(1.0)),
                Button::icon_only(Icon::ZoomOut)
                    .header_style()
                    .enabled(page_count > 0)
                    .on_click(move || zoom_out.change_zoom(0.8)),
                hstack! {
                    Spacer::new(),
                    Text::new(zoom_label).font_size(13.0),
                    Spacer::new(),
                }
                .frame_width(56.0),
                hstack! {
                    Button::icon_only(Icon::ZoomIn)
                        .header_style()
                        .enabled(page_count > 0)
                        .on_click(move || zoom_in.change_zoom(1.25)),
                    Button::icon_only(if fullscreen_mode {
                        Icon::ArrowsMinimize
                    } else {
                        Icon::ArrowsMaximize
                    })
                    .header_style()
                    .on_click(move || fullscreen.toggle_fullscreen()),
                }
                .spacing(8.0),
            }
            .spacing(8.0)
            .padding(10.0),
        );
        let hidden_chrome = Rectangle::new()
            .fill(Color::TRANSPARENT)
            .frame(f32::INFINITY, 48.0);
        let chrome =
            scarlet_ui::if_view!(!fullscreen_mode || chrome_visible, header, hidden_chrome);
        let hidden_status = Rectangle::new()
            .fill(Color::TRANSPARENT)
            .frame(f32::INFINITY, 0.0);
        let status = scarlet_ui::if_view!(
            !fullscreen_mode,
            Text::from_state(self.status.clone())
                .font_size(12.0)
                .padding(8.0)
                .frame(f32::INFINITY, 32.0),
            hidden_status
        );
        let open_empty = self.clone();
        let document_view = scarlet_ui::if_view!(page_count > 0,
            scroll.frame(f32::INFINITY, f32::INFINITY),
            vstack! {
                Spacer::new(),
                Text::new("Open an image or PDF").font_size(24.0).color(ColorPalette::default().text_primary()),
                Text::new("View documents, crop images, and add annotations.").font_size(13.0).color(ColorPalette::default().text_secondary()),
                Button::new("Open File…").padding(10.0).on_click(move || open_empty.request_transition(Pending::Open)),
                Text::from_state(self.status.clone()).font_size(12.0).color(ColorPalette::default().text_secondary()),
                Spacer::new(),
            }.spacing(18.0).padding(28.0).frame(f32::INFINITY, f32::INFINITY)
                .background(ColorPalette::default().surface_variant())
        );
        let viewer = vstack! {
            chrome,
            document_view,
        }
        .frame(f32::INFINITY, f32::INFINITY);

        let pointer_app = self.clone();
        vstack! {
            self.editor_toolbar(),
            viewer.background(Color::rgb(38, 40, 46)),
            status,
        }
        .frame(f32::INFINITY, f32::INFINITY)
        .on_mouse_move(move |_x, y| pointer_app.update_chrome_visibility(y))
        .on_event_capture(move |event| {
            if let scarlet_ui::Event::Keyboard(event) = event {
                key_app.handle_key(event.clone())
            } else {
                false
            }
        })
    }
}

fn canvas_layout_dimensions(
    page_width: f32,
    page_height: f32,
    zoom: f32,
    fit_to_window: bool,
) -> (f32, f32, f32, f32) {
    let page_width = if page_width.is_finite() && page_width > 0.0 {
        page_width
    } else {
        640.0
    };
    let page_height = if page_height.is_finite() && page_height > 0.0 {
        page_height
    } else {
        480.0
    };

    if fit_to_window {
        let cap = (8_388_608.0 / (page_width * page_height)).sqrt().min(1.0);
        return (
            page_width * cap,
            page_height * cap,
            f32::INFINITY,
            f32::INFINITY,
        );
    }

    let zoom = if zoom.is_finite() && zoom > 0.0 {
        zoom
    } else {
        1.0
    };
    let width = (page_width * zoom).max(1.0);
    let height = (page_height * zoom).max(1.0);
    let cap = (8_388_608.0 / (width * height)).sqrt().min(1.0);
    let width = width * cap;
    let height = height * cap;
    (width, height, width, height)
}

fn fill_canvas(buffer: &mut [u8], color: [u8; 4]) {
    for pixel in buffer.chunks_exact_mut(4) {
        pixel.copy_from_slice(&color);
    }
}

fn draw_bitmap_to_canvas(
    buffer: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
    bitmap: Option<&vellum_core::Bitmap>,
) {
    fill_canvas(buffer, [38, 40, 46, 255]);

    let Some(bitmap) = bitmap else {
        return;
    };
    let source_width = bitmap.width();
    let source_height = bitmap.height();
    if source_width == 0 || source_height == 0 || canvas_width == 0 || canvas_height == 0 {
        return;
    }

    let scale_x = canvas_width as f32 / source_width as f32;
    let scale_y = canvas_height as f32 / source_height as f32;
    let scale = scale_x.min(scale_y).max(0.001);
    let draw_width = ((source_width as f32 * scale).round() as u32)
        .max(1)
        .min(canvas_width);
    let draw_height = ((source_height as f32 * scale).round() as u32)
        .max(1)
        .min(canvas_height);
    let offset_x = (canvas_width - draw_width) / 2;
    let offset_y = (canvas_height - draw_height) / 2;
    let source = bitmap.rgba();

    for y in 0..draw_height {
        let source_y = ((u64::from(y) * u64::from(source_height)) / u64::from(draw_height))
            .min(u64::from(source_height - 1)) as usize;
        for x in 0..draw_width {
            let source_x = ((u64::from(x) * u64::from(source_width)) / u64::from(draw_width))
                .min(u64::from(source_width - 1)) as usize;
            let source_offset = (source_y * source_width as usize + source_x) * 4;
            let destination_offset = ((u64::from(offset_y + y) * u64::from(canvas_width)
                + u64::from(offset_x + x))
                * 4) as usize;
            let Some(pixel) = source.get(source_offset..source_offset + 4) else {
                continue;
            };
            let Some(destination) = buffer.get_mut(destination_offset..destination_offset + 4)
            else {
                continue;
            };
            destination.copy_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::canvas_layout_dimensions;

    #[test]
    fn fit_canvas_keeps_a_finite_preferred_size() {
        let (canvas_width, canvas_height, frame_width, frame_height) =
            canvas_layout_dimensions(424.0, 424.0, 1.0, true);

        assert_eq!((canvas_width, canvas_height), (424.0, 424.0));
        assert!(frame_width.is_infinite());
        assert!(frame_height.is_infinite());
    }

    #[test]
    fn invalid_document_dimensions_use_a_finite_fallback() {
        let (canvas_width, canvas_height, _, _) =
            canvas_layout_dimensions(f32::INFINITY, f32::NAN, f32::NAN, true);

        assert_eq!((canvas_width, canvas_height), (640.0, 480.0));
    }
}

impl Application for VellumApp {
    fn on_window_sync(&mut self, ctx: &WindowContext, window: &mut dyn PlatformWindow) {
        if ctx.scene_key.as_str() != "main" {
            return;
        }
        self.sync_title(window);
        let desired = self.fullscreen.get();
        let applied = self.fullscreen_applied.get();

        if desired == applied {
            return;
        }

        if window.set_fullscreen(desired).is_ok() {
            self.fullscreen_applied.set(desired);
        } else {
            self.fullscreen.set(applied);
            self.chrome_visible.set(!applied);
            self.status.set(if desired {
                String::from("Could not enter fullscreen")
            } else {
                String::from("Could not leave fullscreen")
            });
        }
    }

    fn on_window_fullscreen_changed(&mut self, ctx: &WindowContext, fullscreen: bool) {
        if ctx.scene_key.as_str() != "main" {
            return;
        }
        self.fullscreen.set(fullscreen);
        self.fullscreen_applied.set(fullscreen);
        self.decorations_hidden.set(fullscreen);
        self.chrome_visible.set(!fullscreen);
    }

    fn on_window_resize(&mut self, ctx: &WindowContext, _width: u32, _height: u32) {
        if ctx.scene_key.as_str() != "main" {
            return;
        }
        let fullscreen = self.fullscreen.get();

        if self.fullscreen_applied.get() == fullscreen
            && self.decorations_hidden.get() != fullscreen
        {
            self.decorations_hidden.set(fullscreen);
        }
    }

    fn on_window_created(&mut self, ctx: &WindowContext, _: &mut dyn PlatformWindow) {
        if ctx.scene_key.as_str() == "main" {
            self.runtime.get().lock().unwrap().owner = Some(ctx.window_id);
        }
    }
    fn on_window_close_requested(&mut self, ctx: &WindowContext) -> bool {
        if ctx.scene_key.as_str() == "main" {
            self.request_transition(Pending::Close);
            false
        } else {
            if ctx.scene_key.as_str() == "confirm" {
                self.cancel_pending();
            }
            true
        }
    }
    fn on_idle(&mut self) {
        self.poll_editor();
    }
    fn scenes(&self) -> impl Scene {
        scarlet_ui::scenes! {
            Window::new(self.window_title(), self.content()).scene_key("main")
                .app_id(APP_ID).menu_bar(self.editor_menu())
                .decorated(!self.decorations_hidden.get())
                .min_size(Size::new(850.0,560.0))
                .size(Size::new(WINDOW_WIDTH,WINDOW_HEIGHT)),
            Window::new("Save Changes — Vellum",self.confirm_content()).scene_key("confirm")
                .open_at_launch(false).app_id(APP_ID).size(Size::new(560.0,230.0)),
            Window::new("Resize Image — Vellum",self.resize_content()).scene_key("resize")
                .open_at_launch(false).app_id(APP_ID).size(Size::new(440.0,340.0))
                .min_size(Size::new(440.0,340.0)),
            Window::new("Text Annotation — Vellum",self.text_content()).scene_key("text")
                .open_at_launch(false).app_id(APP_ID).size(Size::new(540.0,340.0))
                .min_size(Size::new(540.0,340.0)),
        }
    }

    fn debug_logging(&self) -> bool {
        false
    }
}
