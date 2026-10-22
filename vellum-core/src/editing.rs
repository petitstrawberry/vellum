//! Bounded raster edits and undo history. PDF documents remain read-only.
use crate::{Bitmap, DocumentError};
use std::collections::VecDeque;
use std::sync::Arc;

pub const MAX_EDIT_PIXELS: u64 = 16 * 1024 * 1024;
const HISTORY_BYTES: usize = 128 * 1024 * 1024;
const HISTORY_STEPS: usize = 32;

#[derive(Clone)]
pub enum EditOperation {
    Crop {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    RotateClockwise,
    Resize {
        width: u32,
        height: u32,
    },
    Arrow {
        start: (u32, u32),
        end: (u32, u32),
        color: [u8; 4],
        width: u32,
    },
    /// An RGBA text/annotation stamp rasterized by the frontend's font engine.
    Stamp {
        x: u32,
        y: u32,
        bitmap: Arc<Bitmap>,
    },
}
fn check_size(width: u32, height: u32) -> Result<(), DocumentError> {
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_EDIT_PIXELS {
        Err(DocumentError::new(
            "editing supports non-empty images up to 16 megapixels",
        ))
    } else {
        Ok(())
    }
}
impl EditOperation {
    pub fn apply(&self, source: &Bitmap) -> Result<Bitmap, DocumentError> {
        check_size(source.width(), source.height())?;
        let (sw, sh) = (source.width(), source.height());
        match self {
            Self::Crop {
                x,
                y,
                width,
                height,
            } => {
                check_size(*width, *height)?;
                if x.checked_add(*width).is_none_or(|v| v > sw)
                    || y.checked_add(*height).is_none_or(|v| v > sh)
                {
                    return Err(DocumentError::new("crop rectangle is outside the image"));
                }
                let mut rgba = Vec::with_capacity((*width as usize) * (*height as usize) * 4);
                for row in *y..*y + *height {
                    let offset = ((row as usize) * (sw as usize) + *x as usize) * 4;
                    rgba.extend_from_slice(&source.rgba()[offset..offset + *width as usize * 4]);
                }
                Bitmap::from_rgba(*width, *height, rgba)
            }
            Self::RotateClockwise => {
                let mut rgba = vec![0; source.rgba().len()];
                for y in 0..sh {
                    for x in 0..sw {
                        let old = (y as usize * sw as usize + x as usize) * 4;
                        let new = (x as usize * sh as usize + (sh - 1 - y) as usize) * 4;
                        rgba[new..new + 4].copy_from_slice(&source.rgba()[old..old + 4]);
                    }
                }
                Bitmap::from_rgba(sh, sw, rgba)
            }
            Self::Resize { width, height } => {
                check_size(*width, *height)?;
                // Resample premultiplied color to avoid dark/color fringes at transparent edges.
                let mut premultiplied = source.rgba().to_vec();
                for p in premultiplied.chunks_exact_mut(4) {
                    for c in 0..3 {
                        p[c] = ((u32::from(p[c]) * u32::from(p[3]) + 127) / 255) as u8;
                    }
                }
                let image = image::RgbaImage::from_raw(sw, sh, premultiplied)
                    .ok_or_else(|| DocumentError::new("invalid image"))?;
                let resized = image::imageops::resize(
                    &image,
                    *width,
                    *height,
                    image::imageops::FilterType::Lanczos3,
                );
                let mut rgba = resized.into_raw();
                for p in rgba.chunks_exact_mut(4) {
                    if p[3] > 0 {
                        for c in 0..3 {
                            p[c] = ((u32::from(p[c]) * 255 + u32::from(p[3]) / 2) / u32::from(p[3]))
                                .min(255) as u8;
                        }
                    } else {
                        p[..3].fill(0);
                    }
                }
                Bitmap::from_rgba(*width, *height, rgba)
            }
            Self::Arrow {
                start,
                end,
                color,
                width,
            } => {
                if start.0 >= sw
                    || start.1 >= sh
                    || end.0 >= sw
                    || end.1 >= sh
                    || !(1..=64).contains(width)
                {
                    return Err(DocumentError::new(
                        "arrow coordinates or stroke width are invalid",
                    ));
                }
                let mut rgba = source.rgba().to_vec();
                let (ax, ay) = (start.0 as f32, start.1 as f32);
                let (bx, by) = (end.0 as f32, end.1 as f32);
                let (dx, dy) = (bx - ax, by - ay);
                let length = dx.hypot(dy);
                if length < 1.0 {
                    return Err(DocumentError::new("drag an arrow with distinct endpoints"));
                }
                let (ux, uy) = (dx / length, dy / length);
                let head = (8.0 + *width as f32 * 2.5).min(length * 0.65);
                let base = (bx - ux * head, by - uy * head);
                let half = head * 0.45;
                let triangle = [
                    (bx, by),
                    (base.0 - uy * half, base.1 + ux * half),
                    (base.0 + uy * half, base.1 - ux * half),
                ];
                let radius = *width as f32 / 2.0;
                let pad = radius.max(head) + 1.0;
                let x0 = (ax.min(bx) - pad).max(0.0) as u32;
                let x1 = (ax.max(bx) + pad).min(sw as f32 - 1.0) as u32;
                let y0 = (ay.min(by) - pad).max(0.0) as u32;
                let y1 = (ay.max(by) + pad).min(sh as f32 - 1.0) as u32;
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        let point = (x as f32 + 0.5, y as f32 + 0.5);
                        let t = (((point.0 - ax) * dx + (point.1 - ay) * dy) / (length * length))
                            .clamp(0.0, 1.0);
                        let distance = (point.0 - (ax + t * dx)).hypot(point.1 - (ay + t * dy));
                        let coverage = if inside_triangle(point, triangle) {
                            1.0
                        } else {
                            (radius + 0.5 - distance).clamp(0.0, 1.0)
                        };
                        if coverage > 0.0 {
                            let mut ink = *color;
                            ink[3] = (ink[3] as f32 * coverage).round() as u8;
                            blend(
                                &mut rgba[(y as usize * sw as usize + x as usize) * 4..][..4],
                                ink,
                            );
                        }
                    }
                }
                Bitmap::from_rgba(sw, sh, rgba)
            }
            Self::Stamp { x, y, bitmap } => {
                if *x >= sw || *y >= sh {
                    return Err(DocumentError::new("text position is outside the image"));
                }
                if bitmap.rgba().len() > 4 * 1024 * 1024 {
                    return Err(DocumentError::new("annotation stamp is too large"));
                }
                let mut rgba = source.rgba().to_vec();
                for row in 0..bitmap.height().min(sh - *y) {
                    for col in 0..bitmap.width().min(sw - *x) {
                        let src = (row as usize * bitmap.width() as usize + col as usize) * 4;
                        let dst = ((row + *y) as usize * sw as usize + (col + *x) as usize) * 4;
                        blend(
                            &mut rgba[dst..dst + 4],
                            bitmap.rgba()[src..src + 4].try_into().unwrap(),
                        );
                    }
                }
                Bitmap::from_rgba(sw, sh, rgba)
            }
        }
    }
}
fn inside_triangle(p: (f32, f32), t: [(f32, f32); 3]) -> bool {
    let cross =
        |a: (f32, f32), b: (f32, f32)| (p.0 - b.0) * (a.1 - b.1) - (a.0 - b.0) * (p.1 - b.1);
    let d = [cross(t[0], t[1]), cross(t[1], t[2]), cross(t[2], t[0])];
    !(d.iter().any(|v| *v < 0.0) && d.iter().any(|v| *v > 0.0))
}
fn blend(dst: &mut [u8], src: [u8; 4]) {
    let a = src[3] as f32 / 255.0;
    let da = dst[3] as f32 / 255.0;
    let out = a + da * (1.0 - a);
    if out <= 0.0 {
        return;
    }
    for channel in 0..3 {
        dst[channel] = ((src[channel] as f32 * a + dst[channel] as f32 * da * (1.0 - a)) / out)
            .round()
            .clamp(0.0, 255.0) as u8;
    }
    dst[3] = (out * 255.0).round() as u8;
}
#[derive(Clone)]
struct Revision {
    bitmap: Arc<Bitmap>,
    version: u64,
}
pub struct EditSession {
    current: Revision,
    undo: VecDeque<Revision>,
    redo: VecDeque<Revision>,
    next: u64,
    saved: u64,
}
impl EditSession {
    pub fn new(bitmap: Arc<Bitmap>) -> Self {
        Self {
            current: Revision { bitmap, version: 0 },
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            next: 0,
            saved: 0,
        }
    }
    pub fn bitmap(&self) -> Arc<Bitmap> {
        self.current.bitmap.clone()
    }
    pub fn version(&self) -> u64 {
        self.current.version
    }
    pub fn dirty(&self) -> bool {
        self.saved != self.version()
    }
    pub fn mark_saved(&mut self, version: u64) {
        self.saved = version;
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn commit(&mut self, bitmap: Arc<Bitmap>) {
        self.next += 1;
        self.undo.push_back(self.current.clone());
        self.redo.clear();
        self.current = Revision {
            bitmap,
            version: self.next,
        };
        self.trim();
    }
    fn trim(&mut self) {
        let mut bytes = self.current.bitmap.rgba().len()
            + self
                .undo
                .iter()
                .map(|r| r.bitmap.rgba().len())
                .sum::<usize>();
        while self.undo.len() > HISTORY_STEPS || bytes > HISTORY_BYTES {
            let Some(old) = self.undo.pop_front() else {
                break;
            };
            bytes = bytes.saturating_sub(old.bitmap.rgba().len());
        }
    }
    pub fn undo(&mut self) -> bool {
        let Some(old) = self.undo.pop_back() else {
            return false;
        };
        self.redo
            .push_back(std::mem::replace(&mut self.current, old));
        true
    }
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop_back() else {
            return false;
        };
        self.undo
            .push_back(std::mem::replace(&mut self.current, next));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bitmap() -> Bitmap {
        Bitmap::from_rgba(
            2,
            2,
            vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 128,
            ],
        )
        .unwrap()
    }
    #[test]
    fn crop_rotate_and_resize_have_expected_pixels() {
        let source = bitmap();
        assert_eq!(
            EditOperation::Crop {
                x: 1,
                y: 0,
                width: 1,
                height: 2
            }
            .apply(&source)
            .unwrap()
            .rgba(),
            &[0, 255, 0, 255, 255, 255, 255, 128]
        );
        let rotated = EditOperation::RotateClockwise.apply(&source).unwrap();
        assert_eq!(&rotated.rgba()[..4], &[0, 0, 255, 255]);
        let mut restored = rotated;
        for _ in 0..3 {
            restored = EditOperation::RotateClockwise.apply(&restored).unwrap();
        }
        assert_eq!(restored.rgba(), source.rgba());
        let resized = EditOperation::Resize {
            width: 4,
            height: 3,
        }
        .apply(&source)
        .unwrap();
        assert_eq!((resized.width(), resized.height()), (4, 3));
    }
    #[test]
    fn invalid_dimensions_fail_before_allocating() {
        let source = bitmap();
        for op in [
            EditOperation::Crop {
                x: u32::MAX,
                y: 0,
                width: 2,
                height: 1,
            },
            EditOperation::Crop {
                x: 0,
                y: 0,
                width: 0,
                height: 1,
            },
            EditOperation::Resize {
                width: u32::MAX,
                height: u32::MAX,
            },
        ] {
            assert!(op.apply(&source).is_err());
        }
    }
    #[test]
    fn alpha_stamp_is_clipped_and_composited() {
        let stamp = Bitmap::from_rgba(2, 1, vec![255, 0, 0, 128, 0, 0, 0, 255]).unwrap();
        let result = EditOperation::Stamp {
            x: 1,
            y: 0,
            bitmap: Arc::new(stamp),
        }
        .apply(&bitmap())
        .unwrap();
        assert_eq!(&result.rgba()[4..8], &[128, 127, 0, 255]);
        assert_eq!(&result.rgba()[8..12], &[0, 0, 255, 255]);
    }
    #[test]
    fn arrow_is_bounded_and_changes_visible_pixels() {
        let source = Bitmap::from_rgba(32, 24, vec![255; 32 * 24 * 4]).unwrap();
        let result = EditOperation::Arrow {
            start: (0, 0),
            end: (31, 23),
            color: [255, 0, 0, 255],
            width: 3,
        }
        .apply(&source)
        .unwrap();
        assert_eq!(result.rgba().len(), source.rgba().len());
        assert_ne!(result.rgba(), source.rgba());
        assert!(
            EditOperation::Arrow {
                start: (0, 0),
                end: (0, 0),
                color: [0; 4],
                width: 3
            }
            .apply(&source)
            .is_err()
        );
    }
    #[test]
    fn undo_redo_save_revision_and_branching() {
        let mut session = EditSession::new(Arc::new(bitmap()));
        session.commit(Arc::new(
            EditOperation::RotateClockwise
                .apply(&session.bitmap())
                .unwrap(),
        ));
        let saving = session.version();
        session.commit(Arc::new(bitmap()));
        session.mark_saved(saving);
        assert!(session.dirty());
        assert!(session.undo());
        assert!(!session.dirty());
        assert!(session.undo());
        assert!(session.dirty());
        assert!(session.redo());
        assert!(!session.dirty());
        session.commit(Arc::new(bitmap()));
        assert!(!session.can_redo());
    }
    #[test]
    fn history_has_a_hard_step_limit() {
        let mut session = EditSession::new(Arc::new(bitmap()));
        for _ in 0..100 {
            session.commit(Arc::new(bitmap()));
        }
        let mut count = 0;
        while session.undo() {
            count += 1;
        }
        assert_eq!(count, HISTORY_STEPS);
    }
    #[test]
    fn resize_does_not_bleed_hidden_transparent_color() {
        let source = Bitmap::from_rgba(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 0]).unwrap();
        let result = EditOperation::Resize {
            width: 12,
            height: 1,
        }
        .apply(&source)
        .unwrap();
        for pixel in result.rgba().chunks_exact(4).filter(|p| p[3] > 0) {
            assert_eq!(pixel[2], 0);
            assert!(pixel[0] >= 250);
        }
    }
    #[test]
    fn history_byte_budget_survives_undo_redo() {
        // Reuse one allocation while accounting each logical snapshot, as the
        // production budget does, so this checks the cap without a huge fixture.
        let bitmap = Arc::new(Bitmap::from_rgba(2048, 1024, vec![0; 2048 * 1024 * 4]).unwrap());
        let mut history = EditSession::new(bitmap.clone());
        for _ in 0..40 {
            history.commit(bitmap.clone());
        }
        assert_eq!(history.undo.len(), 15);
        for _ in 0..10 {
            assert!(history.undo());
        }
        for _ in 0..10 {
            assert!(history.redo());
        }
        assert_eq!(history.undo.len(), 15);
    }
}
