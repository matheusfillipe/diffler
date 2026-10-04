//! Image files in the diff pane. A binary file whose path names an image shows
//! its two sides as pictures: the worker reads each side's bytes, decodes
//! them, and builds each one's terminal protocol at the size the pane asked
//! for, so a draw only writes an image that is already encoded. The protocol
//! (kitty, sixel, iTerm2, or halfblocks) is whatever the terminal answered to
//! at startup.

use diffler_core::model::{BlobIds, FileDiff, FileStatus};
use diffler_core::review::{BinarySide, BinarySides};
use ratatui::layout::Size;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use ratatui_image::{FilterType, Resize};

use super::{App, Flow};

/// The extensions previewed as images, the formats the `image` build decodes.
const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

/// Whether `file` previews as an image in the diff pane.
pub(crate) fn is_image(file: &FileDiff) -> bool {
    file.binary
        && std::path::Path::new(&file.path)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                IMAGE_EXTENSIONS
                    .iter()
                    .any(|known| ext.eq_ignore_ascii_case(known))
            })
}

/// What a preview shows: one file's sides, by content, fitted to `target`
/// cells. A side's blob id changes with its bytes, so an image rewritten on
/// disk asks for a new preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageKey {
    pub path: String,
    pub blobs: BlobIds,
    pub deleted: bool,
    pub target: Size,
}

impl ImageKey {
    pub(crate) fn of(file: &FileDiff, target: Size) -> Self {
        Self {
            path: file.path.clone(),
            blobs: file.blobs.clone(),
            deleted: file.status == FileStatus::Deleted,
            target,
        }
    }
}

/// A queued preview.
#[derive(Debug, Clone)]
pub struct ImageRequest {
    pub token: u64,
    pub key: ImageKey,
}

/// One side of a previewed image as the pane draws it.
#[derive(Clone)]
pub enum PreviewSide {
    Image {
        protocol: Box<Protocol>,
        width: u32,
        height: u32,
        bytes: u64,
    },
    /// The side is over the preview's size cap; its size in bytes.
    TooLarge(u64),
    /// The bytes are there but do not decode as an image.
    Unreadable,
}

/// A file's previewed sides.
#[derive(Clone)]
pub struct ImagePreview {
    pub key: ImageKey,
    pub old: Option<PreviewSide>,
    pub new: Option<PreviewSide>,
}

impl std::fmt::Debug for ImagePreview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImagePreview")
            .field("key", &self.key)
            .field("old", &self.old.is_some())
            .field("new", &self.new.is_some())
            .finish()
    }
}

/// Decode and encode both sides of `request` for the terminal `picker`
/// speaks to. Runs on the blocking pool.
pub fn build_preview(picker: &Picker, request: &ImageRequest, sides: BinarySides) -> ImagePreview {
    let side = |side: BinarySide| match side {
        BinarySide::TooLarge(size) => PreviewSide::TooLarge(size),
        BinarySide::Bytes(bytes) => {
            let size = bytes.len() as u64;
            let Ok(image) = image::load_from_memory(&bytes) else {
                return PreviewSide::Unreadable;
            };
            let (width, height) = (image.width(), image.height());
            let resize = fit_to_frame(picker, request.key.target, width, height);
            match picker.new_protocol(image, request.key.target, resize) {
                Ok(protocol) => PreviewSide::Image {
                    protocol: Box::new(protocol),
                    width,
                    height,
                    bytes: size,
                },
                Err(_) => PreviewSide::Unreadable,
            }
        }
    };
    ImagePreview {
        key: request.key.clone(),
        old: sides.old.map(side),
        new: sides.new.map(side),
    }
}

/// Fill the frame either way: an icon smaller than it scales up with hard
/// edges so its pixels stay readable, a photo larger than it scales down
/// smooth.
fn fit_to_frame(picker: &Picker, target: Size, width: u32, height: u32) -> Resize {
    let font = picker.font_size();
    let fits = width <= u32::from(target.width) * u32::from(font.width)
        && height <= u32::from(target.height) * u32::from(font.height);
    if fits {
        Resize::Scale(Some(FilterType::Nearest))
    } else {
        Resize::Fit(Some(FilterType::Triangle))
    }
}

impl App {
    /// Queue the preview the last draw asked for, unless it is already on
    /// screen or on its way. Called after every draw, like enrichment.
    pub(crate) fn queue_image_preview(&mut self) {
        let Some(diff) = self.diff.as_ref() else {
            return;
        };
        let Some(want) = diff.image_want.clone() else {
            return;
        };
        let shown = diff
            .image_preview
            .as_ref()
            .is_some_and(|preview| preview.key == want);
        if shown || self.image_in_flight.as_ref() == Some(&want) {
            return;
        }
        self.image_token = self.image_token.wrapping_add(1);
        self.pending_image = Some(ImageRequest {
            token: self.image_token,
            key: want.clone(),
        });
        self.image_in_flight = Some(want);
    }

    /// Install a landed preview, dropping one the view has moved past.
    pub(crate) fn on_image_preview(&mut self, token: u64, preview: ImagePreview) -> Flow {
        if token != self.image_token {
            return Flow::Idle;
        }
        self.image_in_flight = None;
        let Some(diff) = self.diff.as_mut() else {
            return Flow::Idle;
        };
        diff.image_preview = Some(preview);
        Flow::Continue
    }

    /// Run the queued preview inline, the way the runtime's worker does.
    #[cfg(test)]
    pub(crate) fn settle_image_preview(&mut self) {
        let Some(request) = self.pending_image.take() else {
            return;
        };
        let key = &request.key;
        let sides = diffler_core::review::Review::compute_binary_sides(
            &self.review.repo_root,
            &key.path,
            &key.blobs,
            key.deleted,
        );
        let preview = build_preview(&self.image_picker, &request, sides);
        self.on_image_preview(request.token, preview);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::config::LoadedConfig;
    use crate::test_support::Fixture;

    /// A `width`×`height` PNG of one colour.
    pub(crate) fn png(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
        let image = image::RgbImage::from_pixel(width, height, image::Rgb(rgb));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .expect("encode png");
        bytes.into_inner()
    }

    /// `logo.png` committed red, then blue in the working tree.
    pub(crate) fn changed_logo() -> Fixture {
        let fixture = Fixture::new();
        std::fs::write(fixture.root.join("logo.png"), png(12, 6, [200, 30, 30])).expect("write");
        fixture.commit_all("base");
        std::fs::write(fixture.root.join("logo.png"), png(16, 8, [30, 30, 200])).expect("write");
        fixture
    }

    #[test]
    fn only_a_binary_file_named_as_an_image_previews() {
        let fixture = changed_logo();
        let app = App::new(fixture.review(), LoadedConfig::default());
        let file = &app.review.model().files[0];
        assert!(is_image(file), "{file:?}");
        let mut text = file.clone();
        text.binary = false;
        assert!(!is_image(&text), "a text file named .png is text");
        let mut archive = file.clone();
        archive.path = "bundle.zip".to_owned();
        assert!(!is_image(&archive));
    }

    /// Ask for `path`'s preview at `target` cells, the way a draw does.
    fn want(app: &mut App, path: &str, target: Size) {
        let file = app
            .review
            .model()
            .files
            .iter()
            .find(|file| file.path == path)
            .expect("the file is in the diff")
            .clone();
        app.diff.as_mut().expect("diff").image_want = Some(ImageKey::of(&file, target));
        app.queue_image_preview();
    }

    fn shown(app: &App) -> ImagePreview {
        app.diff
            .as_ref()
            .and_then(|diff| diff.image_preview.clone())
            .expect("a preview")
    }

    fn dims(side: Option<&PreviewSide>) -> Option<(u32, u32)> {
        match side {
            Some(PreviewSide::Image { width, height, .. }) => Some((*width, *height)),
            _ => None,
        }
    }

    #[test]
    fn the_worker_reads_the_committed_side_by_blob_and_the_new_side_from_disk() {
        let fixture = changed_logo();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_file("logo.png");
        want(&mut app, "logo.png", Size::new(20, 10));
        let request = app.pending_image.clone().expect("a request");
        assert!(
            request.key.blobs.old.is_some(),
            "the committed side has a blob"
        );

        app.settle_image_preview();
        let preview = shown(&app);
        assert_eq!(dims(preview.old.as_ref()), Some((12, 6)));
        assert_eq!(dims(preview.new.as_ref()), Some((16, 8)));
    }

    #[test]
    fn a_shown_preview_is_not_requested_again() {
        let fixture = changed_logo();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_file("logo.png");
        want(&mut app, "logo.png", Size::new(20, 10));
        app.settle_image_preview();
        want(&mut app, "logo.png", Size::new(20, 10));
        assert!(app.pending_image.is_none(), "the preview on screen fits");

        want(&mut app, "logo.png", Size::new(30, 10));
        assert!(app.pending_image.is_some(), "a new pane size asks again");
    }

    #[test]
    fn an_image_rewritten_on_disk_previews_again() {
        let fixture = changed_logo();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_file("logo.png");
        want(&mut app, "logo.png", Size::new(20, 10));
        app.settle_image_preview();

        std::fs::write(fixture.root.join("logo.png"), png(10, 10, [30, 200, 30])).expect("write");
        app.queue_refresh();
        app.settle_refresh();
        want(&mut app, "logo.png", Size::new(20, 10));
        app.settle_image_preview();
        assert_eq!(dims(shown(&app).new.as_ref()), Some((10, 10)));
    }

    #[test]
    fn a_viewed_image_rewritten_on_disk_is_unviewed() {
        let fixture = changed_logo();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let hash = app.review.model().files[0].content_hash();
        app.review.session.mark_viewed("logo.png", &hash);

        std::fs::write(fixture.root.join("logo.png"), png(10, 10, [30, 200, 30])).expect("write");
        app.queue_refresh();
        app.settle_refresh();
        let file = &app.review.model().files[0];
        assert!(
            !app.review
                .session
                .is_viewed("logo.png", &file.content_hash())
        );
    }

    #[test]
    fn bytes_that_do_not_decode_say_so() {
        let fixture = Fixture::new();
        std::fs::write(fixture.root.join("broken.png"), b"\x89PNG\0not really").expect("write");
        fixture.commit_all("base");
        std::fs::write(fixture.root.join("broken.png"), b"\x89PNG\0still not").expect("write");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_file("broken.png");
        want(&mut app, "broken.png", Size::new(20, 10));
        app.settle_image_preview();
        assert!(matches!(shown(&app).new, Some(PreviewSide::Unreadable)));
    }
}
