//! Forge authors' pictures on comment author lines. A line leaves a gap before
//! the author and records its column; once the pane has drawn its lines, each
//! picture goes over its gap.

use std::cell::RefCell;
use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Span;

use crate::app::avatar::{AVATAR_SIZE, AvatarState, is_forge_author};

pub(super) struct AvatarCtx<'a> {
    /// `None` where avatars are off or the terminal draws halfblocks.
    states: Option<&'a HashMap<String, AvatarState>>,
    human: &'a str,
    /// Gaps the line being built left, waiting for its row in the pane.
    pending: RefCell<Vec<(u16, String)>>,
    /// `(line, column, login)` for every gap on a drawn line.
    placed: RefCell<Vec<(usize, u16, String)>>,
    /// A popup draws over the pane this frame. Kitty sends a picture on its
    /// first draw only, so we hold every picture back until nothing hides it.
    covered: bool,
}

impl<'a> AvatarCtx<'a> {
    pub(super) fn new(
        states: Option<&'a HashMap<String, AvatarState>>,
        human: &'a str,
        covered: bool,
    ) -> Self {
        Self {
            states,
            human,
            pending: RefCell::default(),
            placed: RefCell::default(),
            covered,
        }
    }

    /// Leaves room for `author`'s picture at the end of `spans` once a fetch
    /// for it has started, and nowhere once it has failed.
    pub(super) fn gap(
        &self,
        spans: &mut Vec<Span<'static>>,
        bg: Color,
        author: &str,
        remote: bool,
    ) {
        let Some(states) = self.states else {
            return;
        };
        if !is_forge_author(author, remote, self.human)
            || !matches!(
                states.get(author),
                Some(AvatarState::Loading | AvatarState::Ready(_))
            )
        {
            return;
        }
        let column: usize = spans.iter().map(Span::width).sum();
        spans.push(Span::styled(
            " ".repeat(usize::from(AVATAR_SIZE.width) + 1),
            Style::new().bg(bg),
        ));
        self.pending
            .borrow_mut()
            .push((u16::try_from(column).unwrap_or(u16::MAX), author.to_owned()));
    }

    /// Ties the gaps the last built row left to `line`, its index among the
    /// pane's lines, or drops them when the row drew nothing.
    pub(super) fn claim(&self, line: usize, drawn: bool) {
        let pending = std::mem::take(&mut *self.pending.borrow_mut());
        if drawn {
            self.placed.borrow_mut().extend(
                pending
                    .into_iter()
                    .map(|(column, login)| (line, column, login)),
            );
        }
    }

    /// Draws every ready picture whose line shows in `area`, `first` being the
    /// line on its top row.
    pub(super) fn draw(&self, frame: &mut Frame<'_>, area: Rect, first: usize) {
        let placed = std::mem::take(&mut *self.placed.borrow_mut());
        let Some(states) = self.states.filter(|_| !self.covered) else {
            return;
        };
        for (line, column, login) in placed {
            let Some(AvatarState::Ready(protocol)) = states.get(&login) else {
                continue;
            };
            let Some(row) = line
                .checked_sub(first)
                .and_then(|row| u16::try_from(row).ok())
                .filter(|row| *row < area.height)
            else {
                continue;
            };
            if column.saturating_add(AVATAR_SIZE.width) > area.width {
                continue;
            }
            let rect = Rect::new(
                area.x + column,
                area.y + row,
                AVATAR_SIZE.width,
                AVATAR_SIZE.height,
            );
            frame.render_widget(ratatui_image::Image::new(protocol), rect);
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui_image::picker::Picker;
    use ratatui_image::{FilterType, Resize};

    use crate::app::avatar::tests::{forge_review, kitty_picker};
    use crate::app::avatar::{AVATAR_SIZE, AvatarState};
    use crate::test_support::{key, render};
    use crate::ui::diff::test_support::open_lib_diff;

    fn screen(app: &mut crate::app::App) -> String {
        render(app).backend().to_string()
    }

    /// Halfblocks would draw each picture as two coloured cells, so the
    /// cards and the comments pane keep their plain layout.
    #[test]
    fn halfblocks_draws_no_avatar() {
        let (_fixture, mut app) = forge_review(Picker::halfblocks());
        open_lib_diff(&mut app);
        app.handle(key('C'));
        insta::assert_snapshot!(screen(&mut app));
        assert!(app.avatars.is_empty());
    }

    /// A forge author's line leaves a gap for the picture while it loads; the
    /// human's and the agent's lines stay as they were.
    #[test]
    fn a_forge_author_gets_a_gap_before_the_name() {
        let (_fixture, mut app) = forge_review(kitty_picker());
        open_lib_diff(&mut app);
        app.handle(key('C'));
        insta::assert_snapshot!(screen(&mut app));
    }

    #[test]
    fn a_failed_avatar_leaves_no_gap() {
        let (_fixture, mut plain) = forge_review(Picker::halfblocks());
        open_lib_diff(&mut plain);
        let (_other, mut failed) = forge_review(kitty_picker());
        open_lib_diff(&mut failed);
        for login in ["alice", "bob"] {
            failed.avatars.insert(login.to_owned(), AvatarState::Failed);
        }
        assert_eq!(screen(&mut failed), screen(&mut plain));
    }

    fn ready_alice(app: &mut crate::app::App) {
        let picture = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            16,
            16,
            image::Rgb([200, 30, 30]),
        ));
        let protocol = kitty_picker()
            .new_protocol(picture, AVATAR_SIZE, Resize::Fit(Some(FilterType::Nearest)))
            .expect("encode");
        app.avatars
            .insert("alice".to_owned(), AvatarState::Ready(Box::new(protocol)));
    }

    /// Kitty sends a picture on its first draw only, so a popup over it would lose it for good.
    #[test]
    fn a_popup_holds_every_avatar_back() {
        let (_fixture, mut app) = forge_review(kitty_picker());
        open_lib_diff(&mut app);
        ready_alice(&mut app);
        app.handle(key('?'));
        assert!(app.modal.is_some(), "help is open");
        render(&mut app);
        app.modal = None;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).expect("terminal");
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .expect("draw");
        let sent = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|cell| cell.symbol().contains('\u{1b}'));
        assert!(sent, "the first uncovered frame sends the picture");
    }

    #[test]
    fn a_ready_avatar_draws_over_its_gap() {
        let (_fixture, mut app) = forge_review(kitty_picker());
        open_lib_diff(&mut app);
        ready_alice(&mut app);
        let terminal = render(&mut app);
        let buffer = terminal.backend().buffer();
        let area = buffer.area;
        let y = (area.y..area.y + area.height)
            .find(|&y| {
                let row: String = (area.x..area.x + area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect();
                row.contains("alice · replied")
            })
            .expect("alice's card is on screen");
        let name_x = (area.x..area.x + area.width - 5)
            .find(|&x| {
                (0..5).all(|i| {
                    buffer[(x + i, y)].symbol() == &"alice"[usize::from(i)..=usize::from(i)]
                })
            })
            .expect("the name");
        let gap_x = name_x - 3;
        assert_ne!(
            buffer[(gap_x, y)].symbol(),
            " ",
            "the picture fills the gap before the name"
        );
    }
}
