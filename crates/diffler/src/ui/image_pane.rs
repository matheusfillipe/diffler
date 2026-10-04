//! An image file's sides in the diff pane: before and after beside each other
//! (stacked when the pane is narrow), or the one side an add or a delete has,
//! each in its own frame, the picture fitted and centred inside it.

use diffler_core::model::{FileDiff, FileStatus};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect, Size};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Paragraph};

use crate::app::image::{ImageKey, ImagePreview, PreviewSide};
use crate::theme::Theme;

/// Below this many columns the two frames stack instead of sitting side by
/// side, so neither picture gets too narrow to read.
const SIDE_BY_SIDE_MIN_WIDTH: u16 = 60;

/// Frame each side of `file` inside `area`, drawing whatever of `preview`
/// matches, and return the preview this frame wants, which the caller
/// requests.
pub(super) fn draw_image_sides(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &Theme,
    file: &FileDiff,
    preview: Option<&ImagePreview>,
) -> ImageKey {
    let sides = frames(file, area);
    // A fixed protocol larger than its area draws nothing, so we size both
    // pictures to the smaller frame.
    let target = sides
        .iter()
        .map(|(_, rect)| inner_size(*rect))
        .reduce(|a, b| Size::new(a.width.min(b.width), a.height.min(b.height)))
        .unwrap_or(Size::new(0, 0));
    let key = ImageKey::of(file, target);
    let preview = preview.filter(|p| p.key == key);
    for (side, rect) in sides {
        let picture = preview.and_then(|p| match side {
            Side::Old => p.old.as_ref(),
            Side::New => p.new.as_ref(),
        });
        draw_frame(
            frame,
            rect,
            theme,
            side.title(file.status),
            picture,
            preview.is_some(),
        );
    }
    key
}

#[derive(Clone, Copy)]
enum Side {
    Old,
    New,
}

impl Side {
    fn title(self, status: FileStatus) -> &'static str {
        match (self, status) {
            (Self::New, FileStatus::Added | FileStatus::Untracked) => " added ",
            (Self::Old, FileStatus::Deleted) => " deleted ",
            (Self::Old, _) => " before ",
            (Self::New, _) => " after ",
        }
    }
}

/// The frame of each side `file` has, laid out in `area`.
fn frames(file: &FileDiff, area: Rect) -> Vec<(Side, Rect)> {
    let has_old = !matches!(
        file.status,
        FileStatus::Added | FileStatus::Untracked | FileStatus::Unchanged
    );
    let has_new = file.status != FileStatus::Deleted;
    match (has_old, has_new) {
        (true, true) => {
            let [old, new] = if area.width >= SIDE_BY_SIDE_MIN_WIDTH {
                Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)])
                    .spacing(1)
                    .areas(area)
            } else {
                Layout::vertical([Constraint::Fill(1), Constraint::Fill(1)]).areas(area)
            };
            vec![(Side::Old, old), (Side::New, new)]
        }
        (true, false) => vec![(Side::Old, area)],
        _ => vec![(Side::New, area)],
    }
}

fn inner_size(rect: Rect) -> Size {
    Size::new(rect.width.saturating_sub(2), rect.height.saturating_sub(2))
}

fn draw_frame(
    frame: &mut Frame<'_>,
    rect: Rect,
    theme: &Theme,
    title: &'static str,
    picture: Option<&PreviewSide>,
    loaded: bool,
) {
    let bg = theme.panel;
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.border).bg(bg))
        .title(Line::styled(title, Style::new().fg(theme.dim).bg(bg)))
        .style(Style::new().bg(bg));
    if let Some(PreviewSide::Image {
        width,
        height,
        bytes,
        ..
    }) = picture
    {
        let caption = format!(" {width}×{height} · {} ", super::graph::human_size(*bytes));
        block = block.title_bottom(
            Line::styled(caption, Style::new().fg(theme.dim).bg(bg)).alignment(Alignment::Right),
        );
    }
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let note = match picture {
        Some(PreviewSide::Image { protocol, .. }) => {
            let size = protocol.size();
            let centred = inner.centered(
                Constraint::Length(size.width),
                Constraint::Length(size.height),
            );
            frame.render_widget(ratatui_image::Image::new(protocol), centred);
            return;
        }
        Some(PreviewSide::TooLarge(bytes)) => format!(
            "too large to preview ({})",
            super::graph::human_size(*bytes)
        ),
        Some(PreviewSide::Unreadable) => "cannot decode this image".to_owned(),
        None if loaded => "no image on this side".to_owned(),
        None => "loading…".to_owned(),
    };
    let middle = inner.centered_vertically(Constraint::Length(1));
    frame.render_widget(
        Paragraph::new(Line::styled(note, Style::new().fg(theme.dim).bg(bg)))
            .alignment(Alignment::Center),
        middle,
    );
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use crate::app::App;
    use crate::app::image::tests::{changed_logo, png};
    use crate::config::LoadedConfig;
    use crate::test_support::{Fixture, render};

    /// Before and after sit side by side, each fitted and centred in its own
    /// frame with its size under it.
    #[test]
    fn a_changed_image_shows_before_and_after() {
        let fixture = changed_logo();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_file("logo.png");
        let terminal = render(&mut app);
        let buffer = terminal.backend().buffer();
        let row = 21;
        assert_eq!(
            buffer[(53, row)].bg,
            Color::Rgb(200, 30, 30),
            "before is red"
        );
        assert_eq!(
            buffer[(97, row)].bg,
            Color::Rgb(30, 30, 200),
            "after is blue"
        );
        insta::assert_snapshot!(terminal.backend());
    }

    /// The first frame, before the worker answers, says it is loading.
    #[test]
    fn an_image_says_it_is_loading_until_the_worker_answers() {
        let fixture = changed_logo();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_file("logo.png");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 20)).expect("terminal");
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .expect("draw");
        let screen = format!("{}", terminal.backend());
        assert_eq!(screen.matches("loading…").count(), 2, "{screen}");
        assert!(app.pending_image.is_some(), "the draw queued the preview");
    }

    /// An added image has one side, framed as added.
    #[test]
    fn an_added_image_shows_one_frame() {
        let fixture = Fixture::new();
        fixture.write("README.md", "readme\n");
        fixture.commit_all("base");
        std::fs::write(fixture.root.join("new.png"), png(8, 8, [30, 160, 60])).expect("write");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_file("new.png");
        let screen = format!("{}", render(&mut app).backend());
        assert!(screen.contains(" added "), "{screen}");
        assert!(!screen.contains(" before "), "{screen}");
        assert!(screen.contains("8×8"), "{screen}");
    }
}
