//! Image editing controls and main-thread application of background results.
use super::*;
use scarlet_ui::file_dialog::{FileDialog, FileDialogMode, FileDialogOutcome};
use scarlet_ui::{
    Canvas, Event, MenuEntry, MenuItemModel, MouseButton, MouseEvent, Select, TextField, Toggle,
};
use std::path::PathBuf;
use std::thread;

impl VellumApp {
    pub(super) fn window_title(&self) -> String {
        let shared = self.runtime.get();
        let rt = shared.lock().unwrap();
        let name = rt
            .export_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .or_else(|| self.document.get().map(|d| d.title().to_owned()))
            .unwrap_or_else(|| "Image & PDF Viewer".into());
        format!("{}{} — Vellum", if rt.dirty() { "* " } else { "" }, name)
    }
    pub(super) fn sync_title(&self, window: &mut dyn PlatformWindow) {
        let title = self.window_title();
        let shared = self.runtime.get();
        let mut rt = shared.lock().unwrap();
        if rt.title != title {
            window.set_title(&title);
            rt.title = title;
        }
    }
    fn publish_edit(&self) {
        let shared = self.runtime.get();
        let rt = shared.lock().unwrap();
        self.edited_bitmap.set(rt.edit.as_ref().map(|e| e.bitmap()));
        drop(rt);
        self.drag.set(None);
        self.render_revision.update(|v| *v = v.wrapping_add(1));
    }
    fn edit_ready(&self) -> bool {
        let shared = self.runtime.get();
        let rt = shared.lock().unwrap();
        if rt.edit.is_none() {
            self.status
                .set("Editing is available for raster images; PDFs stay read-only".into());
            false
        } else if rt.busy.is_some() {
            self.status
                .set("Wait for the current file or image operation to finish".into());
            false
        } else {
            true
        }
    }
    fn apply_edit(&self, operation: EditOperation) {
        if !self.edit_ready() {
            return;
        }
        let shared = self.runtime.get();
        let (ticket, bitmap) = {
            let mut rt = shared.lock().unwrap();
            let bitmap = rt.edit.as_ref().unwrap().bitmap();
            (rt.begin(Job::Edit).unwrap(), bitmap)
        };
        self.status.set("Applying edit…".into());
        self.drag.set(None);
        thread::spawn(move || {
            let result = operation.apply(&bitmap);
            shared
                .lock()
                .unwrap()
                .results
                .push(Completion::Edit(ticket, result));
        });
    }
    fn undo_edit(&self, redo: bool) {
        if !self.edit_ready() {
            return;
        }
        let changed = {
            let shared = self.runtime.get();
            let mut rt = shared.lock().unwrap();
            let edit = rt.edit.as_mut().unwrap();
            if redo { edit.redo() } else { edit.undo() }
        };
        if changed {
            self.publish_edit();
            self.status.set(if redo { "Redo" } else { "Undo" }.into());
        }
    }
    fn set_tool(&self, tool: Tool) {
        if !self.edit_ready() {
            return;
        }
        self.fit_to_window();
        self.tool.set(tool);
        self.drag.set(None);
        self.status.set(
            match tool {
                Tool::Crop => "Drag a rectangle, then choose Apply Crop. Escape cancels.",
                Tool::Arrow => "Drag from the arrow tail to its tip. Undo removes the arrow.",
                Tool::Text => "Click the image to place the text. Undo removes it.",
                Tool::View => "View mode",
            }
            .into(),
        );
    }
    fn ink(&self) -> [u8; 4] {
        [
            [230, 55, 55, 255],
            [35, 115, 235, 255],
            [255, 220, 40, 255],
            [255, 255, 255, 255],
            [20, 20, 20, 255],
        ][self.annotation_color.get().min(4)]
    }
    pub(super) fn image_event(&self, event: &Event) -> bool {
        if self.tool.get() == Tool::View {
            return false;
        }
        let Event::Mouse(event) = event else {
            return false;
        };
        let mapping = self.runtime.get().lock().unwrap().mapping;
        match event {
            MouseEvent::ButtonPressed {
                button: MouseButton::Left,
                x,
                y,
                ..
            } => {
                if !self.edit_ready() {
                    return true;
                }
                let Some(point) = mapping.image_point(*x, *y, false) else {
                    return false;
                };
                if self.tool.get() == Tool::Text {
                    self.place_text(point);
                } else {
                    self.drag.set(Some(Drag {
                        start: point,
                        end: point,
                        active: true,
                    }));
                }
                true
            }
            MouseEvent::Moved { x, y } => {
                let Some(mut drag) = self.drag.get().filter(|d| d.active) else {
                    return false;
                };
                if let Some(point) = mapping.image_point(*x, *y, true) {
                    drag.end = point;
                    self.drag.set(Some(drag));
                }
                true
            }
            MouseEvent::ButtonReleased {
                button: MouseButton::Left,
                x,
                y,
                ..
            } => {
                let Some(mut drag) = self.drag.get().filter(|d| d.active) else {
                    return false;
                };
                if let Some(point) = mapping.image_point(*x, *y, true) {
                    drag.end = point;
                }
                drag.active = false;
                self.drag.set(Some(drag));
                if self.tool.get() == Tool::Arrow {
                    let width = self
                        .stroke_width
                        .get()
                        .parse::<u32>()
                        .unwrap_or(4)
                        .clamp(1, 64);
                    self.apply_edit(EditOperation::Arrow {
                        start: drag.start,
                        end: drag.end,
                        color: self.ink(),
                        width,
                    });
                }
                true
            }
            MouseEvent::ButtonCancelled { .. } | MouseEvent::Exited { .. } => {
                // A release outside the image must not leave a live drag that a
                // later movement silently completes. Completed crop selection stays.
                if self.drag.get().is_some_and(|d| d.active) {
                    self.drag.set(None);
                }
                false
            }
            _ => false,
        }
    }
    fn crop_selection(&self) {
        let Some(drag) = self.drag.get().filter(|d| !d.active) else {
            self.status
                .set("Select a crop rectangle by dragging on the image".into());
            return;
        };
        self.apply_edit(EditOperation::Crop {
            x: drag.start.0.min(drag.end.0),
            y: drag.start.1.min(drag.end.1),
            width: drag.start.0.abs_diff(drag.end.0) + 1,
            height: drag.start.1.abs_diff(drag.end.1) + 1,
        });
    }
    pub(super) fn draw_selection(
        &self,
        buffer: &mut [u8],
        width: u32,
        height: u32,
        mapping: CanvasMapping,
    ) {
        let Some(drag) = self.drag.get() else {
            return;
        };
        let a = mapping.canvas_point(drag.start);
        let b = mapping.canvas_point(drag.end);
        let scale = width as f32 / mapping.width.max(1.0);
        let (ax, ay, bx, by) = (
            (a.0 * scale) as i32,
            (a.1 * scale) as i32,
            (b.0 * scale) as i32,
            (b.1 * scale) as i32,
        );
        let mut canvas = Canvas::new(buffer, width, height);
        let color = Color::rgb(255, 220, 40);
        if self.tool.get() == Tool::Crop {
            canvas.draw_line(ax, ay, bx, ay, color);
            canvas.draw_line(bx, ay, bx, by, color);
            canvas.draw_line(bx, by, ax, by, color);
            canvas.draw_line(ax, by, ax, ay, color);
        } else {
            canvas.draw_line(ax, ay, bx, by, color);
        }
    }
    fn show_resize(&self) {
        if !self.edit_ready() {
            return;
        }
        if let Some(bitmap) = self.edited_bitmap.get() {
            self.resize_width.set(bitmap.width().to_string());
            self.resize_height.set(bitmap.height().to_string());
        }
        open_window("resize");
    }
    fn resize_apply(&self) {
        let Some(bitmap) = self.edited_bitmap.get() else {
            return;
        };
        let Ok(width) = self.resize_width.get().parse::<u32>() else {
            self.status
                .set("Width must be a positive pixel count".into());
            return;
        };
        let height = if self.aspect_lock.get() {
            ((u64::from(width) * u64::from(bitmap.height()) + u64::from(bitmap.width()) / 2)
                / u64::from(bitmap.width()))
            .max(1)
            .min(u32::MAX as u64) as u32
        } else {
            match self.resize_height.get().parse::<u32>() {
                Ok(h) => h,
                Err(_) => {
                    self.status
                        .set("Height must be a positive pixel count".into());
                    return;
                }
            }
        };
        self.apply_edit(EditOperation::Resize { width, height });
        dismiss_window("resize");
        self.fit_to_window();
    }
    fn show_text(&self) {
        if self.edit_ready() {
            open_window("text");
        }
    }
    fn place_text(&self, point: (u32, u32)) {
        let text = self.annotation_text.get();
        let size = self.annotation_size.get().parse::<f32>().unwrap_or(28.0);
        if text.trim().is_empty()
            || text.chars().count() > 256
            || !size.is_finite()
            || !(8.0..=128.0).contains(&size)
        {
            self.status
                .set("Use 1–256 characters and a text size between 8 and 128 pixels".into());
            return;
        }
        let (width, height) = scarlet_ui::graphics::measure_text_sized(&text, size);
        let (width, height) = (width.saturating_add(8), height.saturating_add(8));
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 1_048_576 {
            self.status.set("Text annotation is too large".into());
            return;
        }
        let mut pixels = vec![0; width as usize * height as usize * 4];
        let ink = self.ink();
        Canvas::new(&mut pixels, width, height).draw_text_sized(
            4,
            4,
            &text,
            Color::rgb(ink[0], ink[1], ink[2]),
            size,
        );
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        if !pixels.chunks_exact(4).any(|p| p[3] != 0) {
            self.status
                .set("No font is available to render this text".into());
            return;
        }
        match Bitmap::from_rgba(width, height, pixels) {
            Ok(bitmap) => self.apply_edit(EditOperation::Stamp {
                x: point.0,
                y: point.1,
                bitmap: Arc::new(bitmap),
            }),
            Err(error) => self.status.set(error.to_string()),
        }
    }
    pub(super) fn request_transition(&self, action: Pending) {
        let shared = self.runtime.get();
        let mut rt = shared.lock().unwrap();
        if rt.busy.is_some() {
            self.status
                .set("Finish or cancel the current operation first".into());
            return;
        }
        if rt.pending.is_some() {
            open_window("confirm");
            return;
        }
        if rt.dirty() {
            rt.pending = Some(action);
            drop(rt);
            open_window("confirm");
        } else {
            drop(rt);
            self.perform_transition(action);
        }
    }
    fn perform_transition(&self, action: Pending) {
        dismiss_window("confirm");
        dismiss_window("resize");
        dismiss_window("text");
        match action {
            Pending::Open => self.show_picker(Job::OpenPicker),
            Pending::Close => dismiss_window("main"),
        }
    }
    pub(super) fn cancel_pending(&self) {
        self.runtime.get().lock().unwrap().pending = None;
        dismiss_window("confirm");
    }
    fn discard_pending(&self) {
        let shared = self.runtime.get();
        let mut rt = shared.lock().unwrap();
        if rt.busy.is_some() {
            return;
        }
        let action = rt.pending.take();
        drop(rt);
        if let Some(action) = action {
            self.perform_transition(action);
        }
    }
    fn save_image(&self, save_as: bool) {
        if !self.edit_ready() {
            return;
        }
        let path = self.runtime.get().lock().unwrap().export_path.clone();
        if !save_as {
            if let Some(path) = path {
                self.save_to(path);
                return;
            }
        }
        self.show_picker(Job::SavePicker);
    }
    fn show_picker(&self, job: Job) {
        let shared = self.runtime.get();
        let mut rt = shared.lock().unwrap();
        let Some(owner) = rt.owner else {
            self.status
                .set("Window is not ready for file selection".into());
            return;
        };
        let Some(ticket) = rt.begin(job) else {
            self.status
                .set("A file operation is already in progress".into());
            return;
        };
        let save = job == Job::SavePicker;
        let mut dialog = FileDialog::new(if save {
            FileDialogMode::Save
        } else {
            FileDialogMode::Open
        });
        dialog.title = if save {
            "Save Edited Image as PNG"
        } else {
            "Open Image or PDF"
        }
        .into();
        let original = self.document.get().map(|d| d.path().to_path_buf());
        let folder = original
            .as_ref()
            .and_then(|p| p.parent())
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute())
            });
        dialog.initial_directory = folder.map(Into::into);
        if save {
            dialog
                .filters
                .push(scarlet_ui::file_dialog::FileDialogFilter {
                    name: "PNG image".into(),
                    extensions: vec!["png".into()],
                });
            let stem = original
                .as_ref()
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Image".into());
            dialog.default_name = Some(format!("{stem}-edited.png"));
        }
        rt.dialog = Some((ticket, job, dialog.show(owner)));
        drop(rt);
        self.status.set("Waiting for file selection".into());
    }
    fn open_path(&self, path: PathBuf) {
        let shared = self.runtime.get();
        let Some(ticket) = shared.lock().unwrap().begin(Job::Open) else {
            return;
        };
        self.status.set(format!("Opening {}…", path.display()));
        thread::spawn(move || {
            let result = Document::open(path);
            shared
                .lock()
                .unwrap()
                .results
                .push(Completion::Open(ticket, result));
        });
    }
    fn save_to(&self, path: PathBuf) {
        // Export has one clear format; never put PNG bytes behind a JPEG/PDF suffix.
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
        {
            self.status
                .set("Choose a .png filename for the edited image".into());
            return;
        }
        let shared = self.runtime.get();
        let mut rt = shared.lock().unwrap();
        let Some(bitmap) = rt.edit.as_ref().map(|e| e.bitmap()) else {
            return;
        };
        let Some(ticket) = rt.begin(Job::Save) else {
            return;
        };
        drop(rt);
        self.status.set(format!("Saving {}…", path.display()));
        thread::spawn(move || {
            let result = bitmap.save_png(&path);
            shared
                .lock()
                .unwrap()
                .results
                .push(Completion::Save(ticket, path, result));
        });
    }
    fn install_document(&self, document: Arc<Document>) {
        let bitmap = if document.is_image() {
            document.cached_page(0).ok().flatten()
        } else {
            None
        };
        let shared = self.runtime.get();
        let mut rt = shared.lock().unwrap();
        rt.document += 1;
        rt.edit = bitmap.clone().map(EditSession::new);
        rt.export_path = None;
        rt.pending = None;
        rt.busy = None;
        drop(rt);
        self.document.set(Some(document.clone()));
        self.edited_bitmap.set(bitmap);
        self.current_page.set(0);
        self.tool.set(Tool::View);
        self.drag.set(None);
        self.fit_to_window();
        self.render_revision.update(|v| *v = v.wrapping_add(1));
        self.status.set(format!("Opened {}", document.title()));
        self.prefetch_neighbors(0);
    }
    fn complete_picker(
        &self,
        ticket: crate::editor::Ticket,
        job: Job,
        result: scarlet_ui::file_dialog::FileDialogResult,
    ) {
        let shared = self.runtime.get();
        let mut rt = shared.lock().unwrap();
        let valid = rt.finish(ticket) && rt.unchanged(ticket);
        drop(rt);
        if valid {
            match result {
                Ok(FileDialogOutcome::Selected(paths)) if !paths.is_empty() => {
                    let path = paths[0].as_path().to_path_buf();
                    if job == Job::OpenPicker {
                        self.open_path(path)
                    } else {
                        self.save_to(path)
                    }
                }
                Ok(_) => {
                    self.cancel_pending();
                    self.status.set("File selection cancelled".into());
                }
                Err(scarlet_ui::file_dialog::FileDialogError::Unsupported(_)) => self.status.set(
                    "File dialogs are unavailable on this platform. Your edits are kept.".into(),
                ),
                Err(error) => self.status.set(format!("File selection failed: {error}")),
            }
        } else {
            self.status
                .set("The document changed during file selection; your edits were kept".into());
        }
    }
    pub(super) fn poll_editor(&self) {
        let shared = self.runtime.get();
        let dialog = {
            let mut rt = shared.lock().unwrap();
            let result = rt
                .dialog
                .as_ref()
                .and_then(|(ticket, job, handle)| handle.take_result().map(|r| (*ticket, *job, r)));
            if result.is_some() {
                rt.dialog = None;
            }
            result
        };
        if let Some((ticket, job, result)) = dialog {
            self.complete_picker(ticket, job, result);
        }
        let results = std::mem::take(&mut shared.lock().unwrap().results);
        for result in results {
            match result {
                Completion::Page {
                    document,
                    page,
                    result,
                } => {
                    if shared.lock().unwrap().document != document
                        || self.current_page.get() != page
                    {
                        continue;
                    }
                    match result {
                        Ok(()) => {
                            self.render_revision.update(|v| *v = v.wrapping_add(1));
                            if let Some(document) = self.document.get() {
                                let rendering = format!(
                                    "{} — rendering page {} of {}",
                                    document.title(),
                                    page + 1,
                                    document.page_count()
                                );
                                if self.status.get() == rendering {
                                    self.status.set(format!(
                                        "{} — page {} of {}",
                                        document.title(),
                                        page + 1,
                                        document.page_count()
                                    ));
                                }
                            }
                        }
                        Err(error) => {
                            let message = format!("Could not render page: {error}");
                            if self.status.get() != message {
                                self.status.set(message);
                            }
                        }
                    }
                }
                Completion::Open(ticket, result) => {
                    let mut rt = shared.lock().unwrap();
                    let valid = rt.finish(ticket) && rt.unchanged(ticket);
                    drop(rt);
                    if valid {
                        match result {
                            Ok(document) => self.install_document(document),
                            Err(error) => self.status.set(format!("Could not open: {error}")),
                        }
                    }
                }
                Completion::Edit(ticket, result) => {
                    let mut rt = shared.lock().unwrap();
                    if !rt.finish(ticket) || !rt.unchanged(ticket) {
                        continue;
                    }
                    match result {
                        Ok(bitmap) => {
                            let width = bitmap.width();
                            let height = bitmap.height();
                            if let Some(edit) = rt.edit.as_mut() {
                                edit.commit(Arc::new(bitmap));
                            }
                            drop(rt);
                            self.publish_edit();
                            self.status.set(format!(
                                "Modified · {width} × {height} pixels · Save As PNG to keep changes"
                            ));
                        }
                        Err(error) => {
                            drop(rt);
                            self.status.set(format!("Could not edit: {error}"));
                        }
                    }
                }
                Completion::Save(ticket, path, result) => {
                    let mut rt = shared.lock().unwrap();
                    if !rt.finish(ticket) {
                        continue;
                    }
                    match result {
                        Ok(()) => {
                            if let Some(edit) = rt.edit.as_mut() {
                                edit.mark_saved(ticket.version);
                            }
                            rt.export_path = Some(path.clone());
                            let dirty = rt.dirty();
                            let pending = if !dirty { rt.pending.take() } else { None };
                            drop(rt);
                            self.render_revision.update(|v| *v = v.wrapping_add(1));
                            self.status.set(if dirty {
                                "Saved earlier image; newer edits remain".into()
                            } else {
                                format!("Saved PNG: {}", path.display())
                            });
                            if let Some(action) = pending {
                                self.perform_transition(action);
                            }
                        }
                        Err(error) => {
                            drop(rt);
                            self.status.set(format!("Save failed: {error}"));
                        }
                    }
                }
            }
        }
    }
    pub(super) fn edit_key(&self, event: KeyEvent) -> bool {
        let KeyEvent::Pressed { keycode, modifiers } = event else {
            return false;
        };
        if keycode == KeyCode::Escape && self.tool.get() != Tool::View {
            self.tool.set(Tool::View);
            self.drag.set(None);
            self.status.set("Selection cancelled".into());
            return true;
        }
        if !modifiers.primary() {
            return false;
        }
        match keycode {
            KeyCode::Char('o' | 'O') => self.request_transition(Pending::Open),
            KeyCode::Char('s' | 'S') => self.save_image(modifiers.shift),
            KeyCode::Char('z' | 'Z') => self.undo_edit(modifiers.shift),
            KeyCode::Char('y' | 'Y') => self.undo_edit(true),
            KeyCode::Char('w' | 'W') => self.request_transition(Pending::Close),
            _ => return false,
        }
        true
    }
    fn action_button(&self, label: &str, action: impl Fn(&Self) + 'static) -> Button {
        let app = self.clone();
        Button::new(label)
            .header_style()
            .on_click(move || action(&app))
    }
    fn tool_button(&self, label: &str, tool: Tool, action: impl Fn(&Self) + 'static) -> Button {
        let palette = ColorPalette::default();
        let button = self.action_button(label, action);
        if self.tool.get() == tool {
            button
                .background_color(palette.primary_light())
                .text_color(palette.text_primary())
                .border_color(palette.primary())
        } else {
            button
        }
    }
    pub(super) fn editor_toolbar(&self) -> impl View + Clone + use<> {
        let shared = self.runtime.get();
        let rt = shared.lock().unwrap();
        let ready = rt.busy.is_none();
        let undo = ready && rt.edit.as_ref().is_some_and(|e| e.can_undo());
        let redo = ready && rt.edit.as_ref().is_some_and(|e| e.can_redo());
        drop(rt);
        let palette = ColorPalette::default();
        let bitmap = self.edited_bitmap.get();
        let is_image = bitmap.is_some();
        let tool = self.tool.get();
        let crop = scarlet_ui::if_view!(
            tool == Tool::Crop,
            self.action_button("Apply Crop", |a| a.crop_selection())
                .enabled(ready && self.drag.get().is_some())
                .background_color(palette.primary_light()),
            Text::new("")
        );
        let annotation = scarlet_ui::if_view!(matches!(tool, Tool::Arrow | Tool::Text),
            hstack! {
                Text::new("Color").font_size(11.0).color(palette.text_secondary()),
                Select::new(vec!["Red".into(),"Blue".into(),"Yellow".into(),"White".into(),"Black".into()],self.annotation_color.clone()).width(100.0),
                scarlet_ui::if_view!(tool == Tool::Arrow,
                    self.action_button(&format!("{} px", self.stroke_width.get()), |a| {
                        let width = a.stroke_width.get().parse::<u32>().unwrap_or(4);
                        a.stroke_width.set(if width >= 16 {1} else {width * 2}.to_string());
                    }), Text::new("")),
            }.spacing(8.0), Text::new("")
        );
        let tools = scarlet_ui::if_view!(is_image,
            hstack! {
                Text::new("TOOLS").font_size(10.0).color(palette.text_secondary()),
                hstack! {
                    self.tool_button("View", Tool::View, |a| a.set_tool(Tool::View)),
                    self.tool_button("Crop", Tool::Crop, |a| a.set_tool(Tool::Crop)),
                    self.tool_button("Arrow", Tool::Arrow, |a| a.set_tool(Tool::Arrow)),
                    self.tool_button("Text", Tool::Text, |a| a.show_text()),
                }.spacing(4.0),
                Rectangle::new().fill(palette.border()).frame(1.0, 24.0),
                self.action_button("Rotate", |a| a.apply_edit(EditOperation::RotateClockwise)).enabled(ready),
                self.action_button("Resize…", |a| a.show_resize()).enabled(ready),
                Spacer::new(),
                crop,
                annotation,
            }.spacing(12.0).padding(10.0).frame_width(f32::INFINITY)
                .background(palette.surface_variant()),
            Rectangle::new().fill(Color::TRANSPARENT).frame(f32::INFINITY, 0.0)
        );
        vstack! {
            hstack! {
                self.action_button("Open…", |a| a.request_transition(Pending::Open)),
                scarlet_ui::if_view!(is_image,
                    hstack! {
                        Rectangle::new().fill(palette.border()).frame(1.0, 24.0),
                        self.action_button("Undo", |a| a.undo_edit(false)).enabled(undo),
                        self.action_button("Redo", |a| a.undo_edit(true)).enabled(redo),
                    }.spacing(12.0), Text::new("Images & PDF").font_size(12.0).color(palette.text_secondary())),
                Spacer::new(),
                Text::new(bitmap.as_ref().map(|b| format!("{} × {} px", b.width(), b.height())).unwrap_or_default())
                    .font_size(12.0).color(palette.text_secondary()),
                scarlet_ui::if_view!(is_image,
                    self.action_button("Save As PNG…", |a| a.save_image(true)).enabled(ready)
                        .background_color(palette.surface_variant()), Text::new("")),
            }.spacing(16.0).padding(10.0).frame_width(f32::INFINITY),
            tools,
        }.alignment(Alignment::Leading).frame_width(f32::INFINITY)
            .background(palette.surface())
    }
    pub(super) fn confirm_content(&self) -> impl View + Clone + use<> {
        let palette = ColorPalette::default();
        vstack!{
            Text::new("Save changes?").font_size(20.0),
            Text::new("Export your edited image as a PNG before continuing.").font_size(13.0).color(palette.text_secondary()),
            Text::from_state(self.status.clone()).font_size(11.0).color(palette.text_secondary()),
            Spacer::new(),
            hstack!{
                self.action_button("Cancel",|a|a.cancel_pending()),
                Spacer::new(),
                self.action_button("Discard",|a|a.discard_pending()),
                self.action_button("Save PNG",|a|a.save_image(false)).background_color(palette.surface_variant()),
            }.spacing(12.0),
        }.alignment(Alignment::Leading).spacing(14.0).padding(24.0).frame(f32::INFINITY,f32::INFINITY)
    }
    pub(super) fn resize_content(&self) -> impl View + Clone + use<> {
        let palette = ColorPalette::default();
        vstack!{
            Text::new("Resize image").font_size(20.0),
            hstack!{
                vstack!{
                    Text::new("Width · px").font_size(12.0).color(palette.text_secondary()),
                    TextField::new(self.resize_width.clone()).autofocus(true).frame_width(160.0),
                }.alignment(Alignment::Leading).spacing(6.0),
                vstack!{
                    Text::new("Height · px").font_size(12.0).color(palette.text_secondary()),
                    TextField::new(self.resize_height.clone()).frame_width(160.0),
                }.alignment(Alignment::Leading).spacing(6.0),
            }.spacing(16.0),
            hstack!{Toggle::new(self.aspect_lock.clone()),Text::new("Keep aspect ratio")}.spacing(8.0),
            Text::new("When linked, width sets both dimensions.").font_size(11.0).color(palette.text_secondary()),
            Text::from_state(self.status.clone()).font_size(11.0).color(palette.text_secondary()),
            Spacer::new(),
            hstack!{
                Spacer::new(),
                Button::new("Cancel").header_style().on_click(||dismiss_window("resize")),
                self.action_button("Apply Resize",|a|a.resize_apply()).background_color(palette.surface_variant()),
            }.spacing(12.0),
        }.alignment(Alignment::Leading).spacing(14.0).padding(24.0).frame(f32::INFINITY,f32::INFINITY)
    }
    pub(super) fn text_content(&self) -> impl View + Clone + use<> {
        let palette = ColorPalette::default();
        vstack!{
            Text::new("Add text").font_size(20.0),
            TextField::new(self.annotation_text.clone()).autofocus(true).placeholder("Annotation text").frame_width(f32::INFINITY),
            hstack!{Text::new("Text size · px").font_size(12.0).color(palette.text_secondary()),TextField::new(self.annotation_size.clone()).frame_width(80.0)}.spacing(12.0),
            Text::new("Choose Place Text, then click a position on the image.").font_size(12.0).color(palette.text_secondary()),
            Text::from_state(self.status.clone()).font_size(11.0).color(palette.text_secondary()),
            Spacer::new(),
            hstack!{
                Spacer::new(),
                Button::new("Cancel").header_style().on_click(||dismiss_window("text")),
                self.action_button("Place Text",|a|{a.set_tool(Tool::Text);dismiss_window("text");}).background_color(palette.surface_variant()),
            }.spacing(12.0),
        }.alignment(Alignment::Leading).spacing(14.0).padding(24.0).frame(f32::INFINITY,f32::INFINITY)
    }
    pub(super) fn editor_menu(&self) -> MenuBarModel {
        let item = |id: &str, title: &str, shortcut: &str, action: fn(&Self)| {
            let app = self.clone();
            MenuEntry::Item(
                MenuItemModel::new(id, title)
                    .shortcut(shortcut)
                    .on_activate(Arc::new(move || action(&app))),
            )
        };
        MenuBarModel::new(vec![
            MenuItemModel::new("file", "File").children(vec![
                item("open", "Open…", "Ctrl+O", |a| {
                    a.request_transition(Pending::Open)
                }),
                item("save", "Save PNG", "Ctrl+S", |a| a.save_image(false)),
                item("save-as", "Save As PNG…", "Ctrl+Shift+S", |a| {
                    a.save_image(true)
                }),
                item("close", "Close", "Ctrl+W", |a| {
                    a.request_transition(Pending::Close)
                }),
            ]),
            MenuItemModel::new("edit", "Edit").children(vec![
                item("undo", "Undo", "Ctrl+Z", |a| a.undo_edit(false)),
                item("redo", "Redo", "Ctrl+Shift+Z", |a| a.undo_edit(true)),
            ]),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    fn app() -> VellumApp {
        let app = VellumApp::new(None, 0, String::new());
        let bitmap =
            Arc::new(Bitmap::from_rgba(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 255]).unwrap());
        app.runtime.get().lock().unwrap().edit = Some(EditSession::new(bitmap.clone()));
        app.edited_bitmap.set(Some(bitmap));
        app
    }
    fn modify(app: &VellumApp) {
        let shared = app.runtime.get();
        let mut rt = shared.lock().unwrap();
        let old = rt.edit.as_ref().unwrap().bitmap();
        rt.edit.as_mut().unwrap().commit(Arc::new(
            EditOperation::RotateClockwise.apply(&old).unwrap(),
        ));
        drop(rt);
        app.publish_edit();
    }
    fn wait(app: &VellumApp) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            app.poll_editor();
            if app.runtime.get().lock().unwrap().busy.is_none() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "worker timed out");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    fn directory() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "vellum-ui-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        p
    }
    #[test]
    fn close_cancel_and_failed_save_keep_edits() {
        let app = app();
        modify(&app);
        app.request_transition(Pending::Close);
        assert_eq!(
            app.runtime.get().lock().unwrap().pending,
            Some(Pending::Close)
        );
        app.cancel_pending();
        assert!(app.runtime.get().lock().unwrap().dirty());
        assert!(app.runtime.get().lock().unwrap().pending.is_none());
        let dir = directory();
        app.request_transition(Pending::Close);
        app.save_to(dir.join("missing").join("edited.png"));
        wait(&app);
        let shared = app.runtime.get();
        let rt = shared.lock().unwrap();
        assert!(rt.dirty());
        assert_eq!(rt.pending, Some(Pending::Close));
        assert!(rt.export_path.is_none());
        drop(rt);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn save_before_close_writes_exact_image_and_clears_pending() {
        let app = app();
        modify(&app);
        let dir = directory();
        let path = dir.join("edited.png");
        app.request_transition(Pending::Close);
        app.save_to(path.clone());
        wait(&app);
        let shared = app.runtime.get();
        let rt = shared.lock().unwrap();
        assert!(!rt.dirty());
        assert!(rt.pending.is_none());
        assert_eq!(rt.export_path, Some(path.clone()));
        let expected = rt.edit.as_ref().unwrap().bitmap();
        drop(rt);
        let reopened = Document::open(path).unwrap().page(0).unwrap();
        assert_eq!(reopened.rgba(), expected.rgba());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn late_results_cannot_replace_another_document_or_clear_newer_edits() {
        let app = app();
        let shared = app.runtime.get();
        let ticket = shared.lock().unwrap().begin(Job::Edit).unwrap();
        let result = EditOperation::RotateClockwise
            .apply(&app.edited_bitmap.get().unwrap())
            .unwrap();
        shared.lock().unwrap().document += 1;
        shared
            .lock()
            .unwrap()
            .results
            .push(Completion::Edit(ticket, Ok(result)));
        app.poll_editor();
        assert_eq!(app.edited_bitmap.get().unwrap().width(), 2);
        assert!(!shared.lock().unwrap().dirty());
        let ticket = shared.lock().unwrap().begin(Job::Save).unwrap();
        modify(&app);
        shared.lock().unwrap().pending = Some(Pending::Close);
        shared.lock().unwrap().results.push(Completion::Save(
            ticket,
            PathBuf::from("fixture.png"),
            Ok(()),
        ));
        app.poll_editor();
        assert!(shared.lock().unwrap().dirty());
        assert_eq!(shared.lock().unwrap().pending, Some(Pending::Close));
    }
    #[test]
    fn failed_open_keeps_document_and_edits_and_busy_blocks_another_action() {
        let app = app();
        modify(&app);
        let dir = directory();
        app.open_path(dir.join("absent.png"));
        app.request_transition(Pending::Close);
        assert!(app.runtime.get().lock().unwrap().pending.is_none());
        wait(&app);
        assert!(app.runtime.get().lock().unwrap().dirty());
        assert_eq!(app.edited_bitmap.get().unwrap().height(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn cancelled_save_picker_cancels_close_without_losing_edits() {
        let app = app();
        modify(&app);
        app.request_transition(Pending::Close);
        let ticket = app
            .runtime
            .get()
            .lock()
            .unwrap()
            .begin(Job::SavePicker)
            .unwrap();
        app.complete_picker(ticket, Job::SavePicker, Ok(FileDialogOutcome::Cancelled));
        let shared = app.runtime.get();
        let rt = shared.lock().unwrap();
        assert!(rt.pending.is_none());
        assert!(rt.busy.is_none());
        assert!(rt.dirty());
    }
    #[test]
    fn stale_picker_does_not_replace_active_request() {
        let app = app();
        let shared = app.runtime.get();
        let old = shared.lock().unwrap().begin(Job::OpenPicker).unwrap();
        assert!(shared.lock().unwrap().finish(old));
        let active = shared.lock().unwrap().begin(Job::Save).unwrap();
        app.complete_picker(old, Job::OpenPicker, Ok(FileDialogOutcome::Cancelled));
        assert_eq!(shared.lock().unwrap().busy.unwrap().0, active);
    }

    #[test]
    fn missing_extension_cannot_bypass_picker_overwrite_confirmation() {
        let app = app();
        modify(&app);
        let dir = directory();
        let existing = dir.join("image.png");
        std::fs::write(&existing, b"keep").unwrap();
        app.save_to(dir.join("image"));
        assert!(app.runtime.get().lock().unwrap().busy.is_none());
        assert_eq!(std::fs::read(existing).unwrap(), b"keep");
        assert!(app.runtime.get().lock().unwrap().dirty());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn image_canvas_has_keyboard_focus_for_undo_capture() {
        use scarlet_ui::element::ElementTree;
        use scarlet_ui::event::{EventDispatcher, KeyModifiers};
        let app = app();
        modify(&app);
        let mut tree = ElementTree::new();
        tree.set_root(app.content().create_element());
        let mut dispatcher = EventDispatcher::new();
        assert!(dispatcher.dispatch(
            &mut tree,
            &Event::Keyboard(KeyEvent::Pressed {
                keycode: KeyCode::Char('z'),
                modifiers: KeyModifiers {
                    control: true,
                    super_key: cfg!(target_os = "macos"),
                    ..KeyModifiers::default()
                },
            })
        ));
        assert!(!app.runtime.get().lock().unwrap().dirty());
    }
}
