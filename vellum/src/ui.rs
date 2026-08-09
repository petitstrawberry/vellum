//! ScarletUI frontend for Vellum.

use std::sync::Arc;

use scarlet_ui::prelude::*;
use scarlet_ui::{
    BitmapImage, Button, Color, Image, ImageFit, KeyCode, KeyEvent, MenuBarModel, ScrollAxis,
    ScrollView, Size, State, Text, Window, WindowGroup, hstack, vstack,
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
    zoom: State<f32>,
    fit_to_window: State<bool>,
    status: State<String>,
}

impl VellumApp {
    fn new(document: Option<Arc<Document>>, initial_page: usize, status: String) -> Self {
        let page = document
            .as_ref()
            .map(|document| initial_page.min(document.page_count().saturating_sub(1)))
            .unwrap_or(0);
        let app = Self::default();
        app.document.set(document);
        app.current_page.set(page);
        app.zoom.set(1.0);
        app.fit_to_window.set(true);
        app.status.set(status);
        app
    }

    fn page_count(&self) -> usize {
        self.document
            .get()
            .map(|document| document.page_count())
            .unwrap_or(0)
    }

    fn current_bitmap(&self) -> Option<Arc<vellum_core::Bitmap>> {
        let document = self.document.get()?;
        document.page(self.current_page.get()).ok()
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

        match document.page(target) {
            Ok(_) => {
                self.current_page.set(target);
                self.status.set(format!(
                    "{} — page {} of {}",
                    document.title(),
                    target + 1,
                    document.page_count()
                ));
            }
            Err(error) => self.status.set(format!("Could not render page: {error}")),
        }
    }

    fn set_zoom(&self, zoom: f32) {
        self.fit_to_window.set(false);
        self.zoom.set(zoom.clamp(MIN_ZOOM, MAX_ZOOM));
    }

    fn change_zoom(&self, factor: f32) {
        self.set_zoom(self.zoom.get() * factor);
    }

    fn fit_to_window(&self) {
        self.fit_to_window.set(true);
    }

    fn handle_key(&self, event: KeyEvent) -> bool {
        match event {
            KeyEvent::Pressed { keycode, .. } => match keycode {
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
        let (width, height) = bitmap
            .as_ref()
            .map(|bitmap| (bitmap.width(), bitmap.height()))
            .unwrap_or((640, 480));
        let image = bitmap
            .map(|bitmap| BitmapImage::from_bgra(bitmap.to_bgra_words(), width, height))
            .map(Image::from_bitmap)
            .unwrap_or_else(|| Image::placeholder(width, height))
            .fit_mode(if fit_to_window {
                ImageFit::Contain
            } else {
                ImageFit::Fill
            });
        let (content_width, content_height) = if fit_to_window {
            (f32::INFINITY, f32::INFINITY)
        } else {
            (width as f32 * zoom, height as f32 * zoom)
        };

        let previous = self.clone();
        let next = self.clone();
        let zoom_out = self.clone();
        let zoom_in = self.clone();
        let fit = self.clone();
        let actual_size = self.clone();
        let key_app = self.clone();
        let page = self.current_page.get();
        let page_count = self.page_count();
        let zoom_label = if fit_to_window {
            String::from("Fit")
        } else {
            format!("{}%", (zoom * 100.0).round() as u32)
        };

        vstack! {
            hstack! {
                Button::new("Previous")
                    .on_click(move || previous.move_page(-1)),
                Button::new("Next")
                    .on_click(move || next.move_page(1)),
                Spacer::new(),
                Text::new(format!("Page {} / {}", page + 1, page_count))
                    .font_size(13.0),
                Spacer::new(),
                Button::new("Fit")
                    .on_click(move || fit.fit_to_window()),
                Button::new("100%")
                    .on_click(move || actual_size.set_zoom(1.0)),
                Button::new("−")
                    .on_click(move || zoom_out.change_zoom(0.8)),
                Text::new(zoom_label)
                    .font_size(13.0)
                    .frame_width(56.0),
                Button::new("+")
                    .on_click(move || zoom_in.change_zoom(1.25)),
            }
            .spacing(8.0)
            .padding(10.0),
            ScrollView::new(
                image
                    .frame(content_width, content_height),
            )
            .axes(ScrollAxis::Both)
            .frame(f32::INFINITY, f32::INFINITY)
            .background(Color::rgb(38, 40, 46)),
            Text::from_state(self.status.clone())
                .font_size(12.0)
                .padding(8.0)
                .frame(f32::INFINITY, 32.0),
        }
        .frame(f32::INFINITY, f32::INFINITY)
        .on_key(move |event| key_app.handle_key(event))
    }
}

impl Application for VellumApp {
    fn scenes(&self) -> impl Scene {
        let title = self
            .document
            .get()
            .map(|document| format!("Vellum — {}", document.title()))
            .unwrap_or_else(|| String::from("Vellum — Image & PDF Viewer"));

        WindowGroup::new(
            "main",
            Window::new(title, self.content())
                .app_id(APP_ID)
                .menu_bar(MenuBarModel::new(Vec::new()))
                .size(Size::new(WINDOW_WIDTH, WINDOW_HEIGHT)),
        )
    }

    fn debug_logging(&self) -> bool {
        false
    }
}
