//! UI-independent editing coordination and image/canvas coordinates.
use scarlet_ui::WindowId;
use scarlet_ui::file_dialog::FileDialogHandle;
use std::path::PathBuf;
use std::sync::Arc;
use vellum_core::editing::EditSession;
use vellum_core::{Bitmap, Document, DocumentError};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    #[default]
    View,
    Crop,
    Arrow,
    Text,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pending {
    Open,
    Close,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Job {
    OpenPicker,
    SavePicker,
    Open,
    Edit,
    Save,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket {
    pub request: u64,
    pub document: u64,
    pub version: u64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Drag {
    pub start: (u32, u32),
    pub end: (u32, u32),
    pub active: bool,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct CanvasMapping {
    pub width: f32,
    pub height: f32,
    pub image_width: u32,
    pub image_height: u32,
}
impl CanvasMapping {
    pub fn image_point(&self, x: i32, y: i32, clamp: bool) -> Option<(u32, u32)> {
        if !self.width.is_finite()
            || !self.height.is_finite()
            || self.width <= 0.0
            || self.height <= 0.0
            || self.image_width == 0
            || self.image_height == 0
        {
            return None;
        }
        let scale =
            (self.width / self.image_width as f32).min(self.height / self.image_height as f32);
        let left = (self.width - self.image_width as f32 * scale) / 2.0;
        let top = (self.height - self.image_height as f32 * scale) / 2.0;
        let x = (x as f32 - left) / scale;
        let y = (y as f32 - top) / scale;
        if !clamp
            && (x < 0.0 || y < 0.0 || x >= self.image_width as f32 || y >= self.image_height as f32)
        {
            return None;
        }
        Some((
            x.floor()
                .max(0.0)
                .min(self.image_width.saturating_sub(1) as f32) as u32,
            y.floor()
                .max(0.0)
                .min(self.image_height.saturating_sub(1) as f32) as u32,
        ))
    }
    pub fn canvas_point(&self, point: (u32, u32)) -> (f32, f32) {
        let scale = (self.width / self.image_width.max(1) as f32)
            .min(self.height / self.image_height.max(1) as f32);
        (
            (self.width - self.image_width as f32 * scale) / 2.0 + point.0 as f32 * scale,
            (self.height - self.image_height as f32 * scale) / 2.0 + point.1 as f32 * scale,
        )
    }
}

pub enum Completion {
    Open(Ticket, std::result::Result<Arc<Document>, DocumentError>),
    Edit(Ticket, std::result::Result<Bitmap, DocumentError>),
    Save(Ticket, PathBuf, std::result::Result<(), DocumentError>),
    Page {
        document: u64,
        page: usize,
        result: std::result::Result<(), DocumentError>,
    },
}
#[derive(Default)]
pub struct Runtime {
    pub owner: Option<WindowId>,
    pub document: u64,
    pub request: u64,
    pub edit: Option<EditSession>,
    pub export_path: Option<PathBuf>,
    pub pending: Option<Pending>,
    pub busy: Option<(Ticket, Job)>,
    pub dialog: Option<(Ticket, Job, FileDialogHandle)>,
    pub results: Vec<Completion>,
    pub mapping: CanvasMapping,
    pub title: String,
}
impl Runtime {
    pub fn dirty(&self) -> bool {
        self.edit.as_ref().is_some_and(|e| e.dirty())
    }
    pub fn version(&self) -> u64 {
        self.edit.as_ref().map_or(0, |e| e.version())
    }
    pub fn begin(&mut self, job: Job) -> Option<Ticket> {
        if self.busy.is_some() {
            return None;
        }
        self.request += 1;
        let ticket = Ticket {
            request: self.request,
            document: self.document,
            version: self.version(),
        };
        self.busy = Some((ticket, job));
        Some(ticket)
    }
    pub fn finish(&mut self, ticket: Ticket) -> bool {
        if self.busy.is_none_or(|(active, _)| active != ticket) {
            return false;
        }
        self.busy = None;
        self.document == ticket.document
    }
    pub fn unchanged(&self, ticket: Ticket) -> bool {
        self.document == ticket.document && self.version() == ticket.version
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coordinates_handle_letterbox_and_edges() {
        let mapping = CanvasMapping {
            width: 800.0,
            height: 600.0,
            image_width: 400,
            image_height: 200,
        };
        assert_eq!(mapping.image_point(0, 99, false), None);
        assert_eq!(mapping.image_point(0, 100, false), Some((0, 0)));
        assert_eq!(mapping.image_point(800, 600, true), Some((399, 199)));
        assert_eq!(mapping.canvas_point((200, 100)), (400.0, 300.0));
    }
    #[test]
    fn duplicate_or_stale_completions_cannot_finish_new_jobs() {
        let mut rt = Runtime::default();
        let first = rt.begin(Job::Open).unwrap();
        assert!(rt.begin(Job::Save).is_none());
        assert!(rt.finish(first));
        let second = rt.begin(Job::Edit).unwrap();
        assert!(!rt.finish(first));
        assert_eq!(rt.busy.unwrap().0, second);
        rt.document += 1;
        assert!(!rt.finish(second));
    }
}
