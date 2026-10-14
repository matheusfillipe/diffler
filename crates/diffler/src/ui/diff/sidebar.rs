//! File sidebar of the diff screen.

use std::collections::HashMap;

use diffler_core::model::{DiffModel, FileDiff};
use diffler_core::session::{CommentStatus, Session};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use super::{
    HeaderCtx, RenderCtx, clip_spans, group_header_line, pad_line, pane_heading, push_right,
    sidebar_bg, sidebar_row_bg, tree_lead,
};
use crate::app::walkthrough::stop_title;
use crate::app::{DiffView, Pane};
use crate::config::FileLayout;
use crate::theme::Theme;
use crate::tree::{Bucket, TreeNode};
use crate::ui::{diffstat_spans, status_color};

#[derive(Clone, Copy)]
struct TreeRowCtx<'a> {
    theme: &'a Theme,
    depth: usize,
    width: u16,
    on_cursor: bool,
    focused: bool,
    search: &'a [(std::ops::Range<usize>, bool)],
}

pub(super) fn draw_sidebar(
    frame: &mut Frame<'_>,
    area: Rect,
    ctx: &RenderCtx<'_>,
    diff: &mut DiffView,
) {
    let (theme, session, review_model, search) =
        (ctx.theme, ctx.session, ctx.review_model, ctx.search);
    let focused = diff.focus == Pane::List;
    let surface = sidebar_bg(theme);
    frame.render_widget(Block::new().style(Style::new().bg(surface)), area);
    let [heading, inner] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    frame.render_widget(
        Paragraph::new(pane_heading(
            theme,
            &sidebar_title(ctx, diff),
            focused,
            surface,
        )),
        heading,
    );
    diff.sidebar = inner;
    let Some(model) = diff.commit_model.as_ref().or(review_model) else {
        return;
    };
    // we style only the visible slice since the tree can be far taller than the pane
    let height = inner.height.max(1) as usize;
    let rows = diff.tree_rows(model, session);
    let stat = GroupStat::collect(diff, model, session);
    let scroll =
        crate::ui::scroll_to_cursor(diff.tree_cursor, diff.sidebar_scroll, height, rows.len());
    diff.sidebar_scroll = scroll;
    let active_walkthrough = diff.active_walkthrough(session);
    let lines: Vec<Line<'static>> = rows
        .iter()
        .enumerate()
        .skip(scroll)
        .take(height)
        .map(|(row_index, row)| {
            let on_cursor = row_index == diff.tree_cursor;
            let ranges = search
                .filter(|_| focused)
                .map(|s| s.ranges_for(row_index))
                .unwrap_or_default();
            let row_ctx = TreeRowCtx {
                theme,
                depth: row.depth,
                width: inner.width,
                on_cursor,
                focused,
                search: &ranges,
            };
            match &row.node {
                TreeNode::Dir { name, path } => sidebar_dir_line(
                    &row_ctx,
                    name,
                    diff.folded_dirs.contains(path),
                    stat.dirs.get(path).copied().unwrap_or_default(),
                ),
                TreeNode::Section {
                    bucket,
                    count,
                    folded,
                } => sidebar_section_line(
                    &row_ctx,
                    *bucket,
                    *count,
                    stat.sections.get(bucket).copied().unwrap_or_default(),
                    *folded,
                ),
                TreeNode::File { index, name } => {
                    let Some(file) = model.files.get(*index) else {
                        return Line::default();
                    };
                    let viewed = session.is_viewed(&file.path, &file.content_hash());
                    let open = open_comment_count(session, &file.path);
                    sidebar_file_line(&row_ctx, file, name, viewed, open)
                }
                TreeNode::Stop { index } => {
                    sidebar_stop_line(&row_ctx, session, active_walkthrough, *index)
                }
                TreeNode::WalkthroughSummary => sidebar_summary_line(&row_ctx),
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// "Files", or the walkthrough's name in its layout so the reader knows whose
/// reading order it is. A broken pin concerns the whole walkthrough, so we say it here.
fn sidebar_title(ctx: &RenderCtx<'_>, diff: &DiffView) -> String {
    match (diff.layout, diff.active_walkthrough(ctx.session)) {
        (FileLayout::Walkthrough, Some(walkthrough)) => {
            let progress = crate::app::walkthrough::progress_label(ctx.session, walkthrough);
            if diff.pin_broken {
                format!("{} ({progress}, pin lost)", walkthrough.title)
            } else {
                format!("{} ({progress})", walkthrough.title)
            }
        }
        _ => "Files".to_owned(),
    }
}

fn sidebar_stop_line(
    rc: &TreeRowCtx<'_>,
    session: &Session,
    walkthrough: Option<&diffler_core::walkthrough::Walkthrough>,
    index: usize,
) -> Line<'static> {
    let &TreeRowCtx {
        theme,
        width,
        on_cursor,
        focused,
        search,
        ..
    } = rc;
    let Some(primary) = walkthrough
        .and_then(|walkthrough| walkthrough.stops.get(index))
        .and_then(|id| session.comments.iter().position(|c| c.id == *id))
    else {
        return Line::default();
    };
    let Some(stop) = session.comments.get(primary) else {
        return Line::default();
    };
    let held = crate::app::walkthrough::slide_comments(session, primary).len();
    let bg = sidebar_row_bg(theme, on_cursor, focused);
    let dim = Style::new().fg(theme.dim).bg(bg);
    let title_style = Style::new()
        .fg(if on_cursor { theme.accent } else { theme.fg })
        .bg(bg);
    // stops never re-sort by seen state, so we trail the check where a viewed file leads with it
    let mut spans = vec![tree_lead(theme, 0, bg, on_cursor)];
    spans.extend(crate::ui::highlight_spans(
        &stop_title(stop),
        title_style,
        search,
        theme,
    ));
    if session.is_stop_seen(&stop.id) {
        spans.push(Span::styled(" ✓".to_owned(), dim));
    }
    spans.push(Span::styled(
        format!("  {}", base_name(&stop.anchor.file)),
        dim,
    ));
    if held > 1 {
        spans.push(Span::styled(format!(" · {held}"), dim));
    }
    pad_line(spans, bg, width)
}

fn sidebar_summary_line(rc: &TreeRowCtx<'_>) -> Line<'static> {
    let &TreeRowCtx {
        theme,
        width,
        on_cursor,
        focused,
        search,
        ..
    } = rc;
    let bg = sidebar_row_bg(theme, on_cursor, focused);
    let title_style = Style::new()
        .fg(if on_cursor { theme.accent } else { theme.fg })
        .bg(bg);
    let mut spans = vec![tree_lead(theme, 0, bg, on_cursor)];
    spans.extend(crate::ui::highlight_spans(
        "Summary",
        title_style,
        search,
        theme,
    ));
    pad_line(spans, bg, width)
}

fn base_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

pub(super) fn open_comment_count(session: &Session, path: &str) -> usize {
    session
        .comments
        .iter()
        .filter(|c| c.anchor.file == path && c.status != CommentStatus::Resolved)
        .count()
}

/// Each sidebar group's `(added, deleted)` totals, from one pass over the model
/// per frame, since a header row knows its name and not its members.
#[derive(Default)]
struct GroupStat {
    dirs: HashMap<String, (usize, usize)>,
    sections: HashMap<Bucket, (usize, usize)>,
}

impl GroupStat {
    fn collect(diff: &DiffView, model: &DiffModel, session: &Session) -> Self {
        let mut stat = Self::default();
        for file in &model.files {
            let (added, deleted) = file.diffstat();
            let viewed = session.is_viewed(&file.path, &file.content_hash());
            let tally = |slot: &mut (usize, usize)| {
                slot.0 += added;
                slot.1 += deleted;
            };
            match diff.layout {
                FileLayout::Kinds => {
                    let bucket = Bucket::Kind(diff.kind_of(&file.path));
                    tally(stat.sections.entry(bucket).or_default());
                }
                FileLayout::Review => {
                    let bucket = if viewed {
                        Bucket::Viewed
                    } else {
                        Bucket::ToReview
                    };
                    tally(stat.sections.entry(bucket).or_default());
                }
                FileLayout::Walkthrough => {}
                FileLayout::Tree | FileLayout::List => {
                    for (at, _) in file.path.match_indices('/') {
                        let dir = &file.path[..at];
                        tally(stat.dirs.entry(dir.to_owned()).or_default());
                    }
                }
            }
        }
        stat
    }
}

fn sidebar_dir_line(
    rc: &TreeRowCtx<'_>,
    name: &str,
    folded: bool,
    stat: (usize, usize),
) -> Line<'static> {
    let &TreeRowCtx {
        theme,
        depth,
        width,
        on_cursor,
        focused,
        search,
    } = rc;
    let bg = sidebar_row_bg(theme, on_cursor, focused);
    let arrow = if folded { "▸ " } else { "▾ " };
    let name_style = Style::new()
        .fg(if on_cursor { theme.accent } else { theme.fg })
        .bg(bg);
    let mut spans = vec![
        tree_lead(theme, depth, bg, on_cursor),
        Span::styled(arrow.to_owned(), Style::new().fg(theme.dim).bg(bg)),
    ];
    spans.extend(crate::ui::highlight_spans(name, name_style, search, theme));
    let tail = diffstat_spans(theme, stat.0, stat.1, bg);
    push_right(&mut spans, tail, width, bg);
    pad_line(spans, bg, width)
}

fn sidebar_section_line(
    rc: &TreeRowCtx<'_>,
    bucket: Bucket,
    count: usize,
    stat: (usize, usize),
    folded: bool,
) -> Line<'static> {
    let &TreeRowCtx {
        theme,
        width,
        on_cursor,
        focused,
        ..
    } = rc;
    let bg = sidebar_row_bg(theme, on_cursor, focused);
    let tail = diffstat_spans(theme, stat.0, stat.1, bg);
    group_header_line(
        HeaderCtx {
            theme,
            bg,
            width,
            on_cursor,
        },
        bucket.label(),
        count,
        folded,
        tail,
    )
}

fn sidebar_file_line(
    rc: &TreeRowCtx<'_>,
    file: &FileDiff,
    name: &str,
    viewed: bool,
    open: usize,
) -> Line<'static> {
    let &TreeRowCtx {
        theme,
        depth,
        width,
        on_cursor,
        focused,
        search,
    } = rc;
    let bg = sidebar_row_bg(theme, on_cursor, focused);
    let dim = Style::new().fg(theme.dim).bg(bg);
    let (glyph, glyph_colour) = if viewed {
        ('✓', theme.added)
    } else {
        (file.status.glyph(), status_color(theme, file.status))
    };
    let mut spans = vec![
        tree_lead(theme, depth, bg, on_cursor),
        Span::styled(format!("{glyph} "), Style::new().fg(glyph_colour).bg(bg)),
    ];
    // " ·{open}" is 2 + the count's digits wide
    let suffix_width = if open > 0 {
        2 + open.to_string().len()
    } else {
        0
    };
    let used = spans.iter().map(Span::width).sum::<usize>() + suffix_width;
    let room = (width as usize).saturating_sub(used + 1);
    let name_style = Style::new()
        .fg(if on_cursor { theme.accent } else { theme.fg })
        .bg(bg);
    // we highlight before clipping so a match stays lit; a path front-elides to keep its basename
    let parent = name.rfind('/').map_or(0, |at| at + 1);
    let highlighted =
        crate::ui::highlight_spans_split(name, parent, dim, name_style, search, theme);
    spans.extend(clip_spans(
        highlighted,
        room,
        name.contains('/'),
        name_style,
    ));
    if open > 0 {
        spans.push(Span::styled(format!(" ·{open}"), dim));
    }
    let (added, deleted) = file.diffstat();
    push_right(
        &mut spans,
        diffstat_spans(theme, added, deleted, bg),
        width,
        bg,
    );
    pad_line(spans, bg, width)
}

#[cfg(test)]
mod tests {

    use crate::app::{App, Pane};
    use crate::config::LoadedConfig;
    use crate::test_support::{
        Fixture, key, mouse_click, mouse_right_click, render, standard_fixture,
    };
    use crate::theme::Theme;
    use crate::ui::diff::test_support::{
        cursor_to_added_line, diff_app, open_lib_diff, walkthrough_app,
    };

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Sidebar file position on screen for the last render.
    fn sidebar_file_pos(app: &App) -> (u16, u16, usize) {
        let diff = app.diff.as_ref().unwrap();
        let session = app.review.session_for(&diff.source);
        let rows = diff.tree_rows(diff.model(&app.review), session);
        let target = rows
            .iter()
            .position(|r| matches!(r.node, crate::tree::TreeNode::File { .. }))
            .expect("a file row in the sidebar");
        let x = diff.sidebar.x + 1;
        let y = diff.sidebar.y + target as u16 - diff.sidebar_scroll as u16;
        (x, y, target)
    }

    #[test]
    fn the_walkthrough_layout_names_itself_in_the_sidebar_heading() {
        let fixture = standard_fixture();
        let mut app = walkthrough_app(
            &fixture,
            &[("The answer", Some("src/lib.rs#answer"), "why 42")],
        );
        let screen = render(&mut app).backend().to_string();
        assert!(screen.contains("How the answer moved"), "{screen}");
        assert!(!screen.contains(" Files"), "{screen}");
    }

    #[test]
    fn a_broken_pin_shows_in_the_sidebar_heading() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Walkthrough;
        let mut app = App::new(fixture.review(), loaded);
        app.author = "reviewer".to_owned();
        let source = crate::test_support::seat_walkthrough(
            &mut app,
            "tour",
            &[("The answer", Some("src/lib.rs#answer"), "why 42")],
        );
        app.review
            .session_for_mut(&source)
            .walkthrough
            .as_mut()
            .expect("walkthrough")
            .rev = Some("0000000000000000000000000000000000dead".to_owned());
        app.open_walkthrough_diff("w1");
        let request = app
            .pending_walkthrough
            .take()
            .expect("resolution queued for the walkthrough");
        let root = app.review.repo_root.clone();
        let read = diffler_core::review::Review::compute_walkthrough_files(
            &root,
            request.read_rev.as_deref(),
            request.read_first,
            &request.files,
        );
        assert!(read.pin_broken, "the garbage rev must not resolve");
        app.handle(crate::event::AppEvent::WalkthroughAnchors {
            contents: read.contents,
            pin_broken: read.pin_broken,
            token: request.token,
        });

        let screen = render(&mut app).backend().to_string();
        assert!(screen.contains("pin lost"), "{screen}");
    }

    #[test]
    fn a_stops_notes_count_toward_its_sidebar_row() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Walkthrough;
        let mut app = App::new(fixture.review(), loaded);
        app.author = "reviewer".to_owned();

        let stop = crate::mcp::StopParams {
            id: None,
            title: "The answer".to_owned(),
            anchor: Some("src/lib.rs#answer".to_owned()),
            body: "why 42".to_owned(),
            notes: Some(vec![
                crate::mcp::NoteParams {
                    id: None,
                    anchor: None,
                    body: "a first remark".to_owned(),
                },
                crate::mcp::NoteParams {
                    id: None,
                    anchor: Some("src/lib.rs:2".to_owned()),
                    body: "a second remark".to_owned(),
                },
            ]),
        };
        let crate::mcp::McpResponse::WalkthroughPublished(published) =
            app.handle_mcp(crate::mcp::McpRequestKind::PublishWalkthrough {
                id: None,
                title: "tour".to_owned(),
                stops: vec![stop],
                skipped: None,
                summary: None,
            })
        else {
            panic!("expected a published walkthrough");
        };

        app.open_walkthrough_diff(&published.id);
        let request = app
            .pending_walkthrough
            .take()
            .expect("resolution queued for the new walkthrough");
        let root = app.review.repo_root.clone();
        let contents = request
            .files
            .iter()
            .filter_map(|path| Some((path.clone(), std::fs::read_to_string(root.join(path)).ok()?)))
            .collect();
        app.handle(crate::event::AppEvent::WalkthroughAnchors {
            contents,
            pin_broken: false,
            token: request.token,
        });

        let screen = render(&mut app).backend().to_string();
        assert!(
            screen.contains(" · 3"),
            "the row counts the stop and both notes: {screen}"
        );
    }

    #[test]
    fn review_layout_sidebar_buckets_viewed_files() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Review;
        let mut app = App::new(fixture.review(), loaded);
        app.author = "reviewer".to_owned();
        app.open_working_tree_diff(None);
        // one viewed file in the folded bucket, two left to review
        app.handle(key('m'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn kinds_layout_sidebar_groups_files_by_what_they_are() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Kinds;
        let mut app = App::new(fixture.review(), loaded);
        app.author = "reviewer".to_owned();
        app.open_working_tree_diff(None);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn single_click_in_the_sidebar_selects_without_focusing() {
        let (_fixture, mut app) = diff_app();
        render(&mut app);
        let (x, y, target) = sidebar_file_pos(&app);
        app.handle(mouse_click(x, y));
        let diff = app.diff.as_ref().unwrap();
        assert_eq!(diff.tree_cursor, target);
        assert_eq!(diff.focus, Pane::List, "single click keeps sidebar focus");
    }

    #[test]
    fn double_click_in_the_sidebar_opens_the_file() {
        let (_fixture, mut app) = diff_app();
        render(&mut app);
        let (x, y, target) = sidebar_file_pos(&app);
        app.handle(mouse_click(x, y));
        app.handle(mouse_click(x, y));
        let diff = app.diff.as_ref().unwrap();
        assert_eq!(diff.tree_cursor, target);
        assert_eq!(diff.focus, Pane::Diff, "double-click opens into the pane");
    }

    #[test]
    fn a_right_click_on_a_folder_leaves_it_open() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        render(&mut app);
        let diff = app.diff.as_ref().unwrap();
        let rows = diff.sidebar_tree(&app.review);
        let folder = rows
            .iter()
            .position(|row| row.node.is_group())
            .expect("a folder row");
        let before = rows.len();
        let y = diff.sidebar.y + u16::try_from(folder - diff.sidebar_scroll).unwrap();
        app.handle(mouse_right_click(diff.sidebar.x + 2, y));
        let after = app.diff.as_ref().unwrap().sidebar_tree(&app.review).len();
        assert_eq!(
            after, before,
            "the menu acts on the folder without folding it"
        );
    }

    #[test]
    fn a_click_on_a_folder_folds_it() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        render(&mut app);
        let diff = app.diff.as_ref().unwrap();
        let rows = diff.sidebar_tree(&app.review);
        let folder = rows
            .iter()
            .position(|row| matches!(row.node, crate::tree::TreeNode::Dir { .. }))
            .expect("a folder row");
        let before = rows.len();
        let y = diff.sidebar.y + u16::try_from(folder - diff.sidebar_scroll).unwrap();
        let x = diff.sidebar.x + 2;
        app.handle(mouse_click(x, y));
        let after = app.diff.as_ref().unwrap().sidebar_tree(&app.review).len();
        assert!(
            after < before,
            "the folder's files are hidden: {before} -> {after}"
        );
    }

    #[test]
    fn sidebar_focus_renders_the_file_list_and_first_file_diff() {
        let (_fixture, mut app) = diff_app();
        assert_eq!(app.diff.as_ref().unwrap().focus, Pane::List);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn sidebar_search_does_not_bleed_into_the_diff_pane() {
        let (_fixture, mut app) = diff_app();
        assert_eq!(app.diff.as_ref().unwrap().focus, Pane::List);
        // a filename match while the sidebar is focused must not paint the pane
        app.handle(key('/'));
        for c in "lib".chars() {
            app.handle(key(c));
        }
        app.handle(key('\n'));
        let terminal = render(&mut app);
        let buffer = terminal.backend().buffer();
        let sidebar = crate::ui::diff::sidebar_width(120);
        let search_bgs = [app.theme.search, app.theme.search_current];
        let mut highlighted = 0;
        for y in 0..40 {
            for x in 0..120 {
                if search_bgs.contains(&buffer[(x, y)].bg) {
                    assert!(
                        x < sidebar,
                        "search bg at col {x} bleeds past sidebar {sidebar}"
                    );
                    highlighted += 1;
                }
            }
        }
        assert!(
            highlighted > 0,
            "the focused sidebar should still highlight the match"
        );
    }

    #[test]
    fn sidebar_file_row_highlights_only_the_matched_substring() {
        let (_fixture, app) = diff_app();
        let file = app
            .review
            .model()
            .files
            .iter()
            .find(|f| f.path == "src/lib.rs")
            .cloned()
            .expect("src/lib.rs present");
        // a wide row so the name is not clipped; "lib" is bytes 0..3 of "lib.rs"
        let rc = super::TreeRowCtx {
            theme: &app.theme,
            depth: 0,
            width: 80,
            on_cursor: false,
            focused: true,
            search: &[(0..3, true)],
        };
        let spans = super::sidebar_file_line(&rc, &file, "lib.rs", false, 0);
        let highlighted: Vec<&str> = spans
            .iter()
            .filter(|s| s.style.bg == Some(app.theme.search_current))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(
            highlighted,
            vec!["lib"],
            "only the matched word lights up, not the whole row: {spans:?}"
        );
    }

    #[test]
    fn a_directory_header_sums_the_diffstat_of_every_file_below_it() {
        let fixture = crate::test_support::Fixture::new();
        fixture.write("src/a.rs", "one\n");
        fixture.write("src/nested/b.rs", "one\n");
        fixture.write("top.txt", "one\n");
        fixture.commit_all("base");
        // 2 added 1 deleted under src/, split across two depths
        fixture.write("src/a.rs", "two\n");
        fixture.write("src/nested/b.rs", "one\nthree\n");
        fixture.write("top.txt", "one\nfour\n");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(None);

        let diff = app.diff.as_ref().expect("diff view");
        let session = app.review.session_for(&diff.source);
        let stat = super::GroupStat::collect(diff, diff.model(&app.review), session);
        assert_eq!(stat.dirs.get("src"), Some(&(2, 1)));
        assert_eq!(stat.dirs.get("src/nested"), Some(&(1, 0)));
        assert_eq!(stat.dirs.get("top.txt"), None, "a file is not a group");

        let text = render(&mut app).backend().to_string();
        assert!(text.contains("+2 -1"), "src/ shows its diffstat: {text}");
    }

    #[test]
    fn a_kind_header_sums_the_diffstat_of_its_bucket() {
        let fixture = crate::test_support::Fixture::new();
        fixture.write("src/a.rs", "one\n");
        fixture.write("src/b.rs", "one\n");
        fixture.write("README.md", "docs\n");
        fixture.commit_all("base");
        fixture.write("src/a.rs", "one\ntwo\n");
        fixture.write("src/b.rs", "one\nthree\n");
        fixture.write("README.md", "more docs\n");
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Kinds;
        let mut app = App::new(fixture.review(), loaded);
        app.open_working_tree_diff(None);
        let text = render(&mut app).backend().to_string();
        assert!(
            text.contains("Source (2)") && text.contains("+2 -0"),
            "Source keeps its count and adds up its files: {text}"
        );
        assert!(text.contains("Docs (1)"), "the count stays: {text}");
    }

    #[test]
    fn the_review_buckets_split_the_diffstat_by_what_is_left() {
        let fixture = crate::test_support::Fixture::new();
        fixture.write("a.rs", "one\n");
        fixture.write("b.rs", "one\n");
        fixture.commit_all("base");
        fixture.write("a.rs", "one\ntwo\n");
        fixture.write("b.rs", "one\nthree\nfour\n");
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.diff_file_layout = crate::config::FileLayout::Review;
        let mut app = App::new(fixture.review(), loaded);
        app.open_working_tree_diff(None);
        let hash = app
            .review
            .model()
            .files
            .iter()
            .find(|file| file.path == "a.rs")
            .expect("a.rs in the diff")
            .content_hash();
        app.review
            .session_for_mut(&diffler_core::source::ReviewSource::WorkingTree)
            .mark_viewed("a.rs", &hash);

        let diff = app.diff.as_ref().expect("diff view");
        let session = app.review.session_for(&diff.source);
        let stat = super::GroupStat::collect(diff, diff.model(&app.review), session);
        assert_eq!(stat.sections.get(&super::Bucket::Viewed), Some(&(1, 0)));
        assert_eq!(stat.sections.get(&super::Bucket::ToReview), Some(&(2, 0)));
    }

    #[test]
    fn a_group_header_carries_the_same_diffstat_colors_as_a_file_row() {
        let theme = Theme::github_dark();
        let rc = super::TreeRowCtx {
            theme: &theme,
            depth: 0,
            width: 40,
            on_cursor: false,
            focused: true,
            search: &[],
        };
        let spans = super::sidebar_dir_line(&rc, "src", false, (3, 1));
        let painted: Vec<(&str, Option<ratatui::style::Color>)> = spans
            .spans
            .iter()
            .filter(|span| span.content.contains('+') || span.content.contains('-'))
            .map(|span| (span.content.as_ref(), span.style.fg))
            .collect();
        assert_eq!(
            painted,
            vec![(" +3", Some(theme.added)), (" -1", Some(theme.error_fg))],
            "the header reuses the file row's diffstat, no bar: {spans:?}"
        );
    }

    #[test]
    fn sidebar_scrolls_to_keep_the_cursor_visible() {
        let fixture = crate::test_support::Fixture::new();
        fixture.write(".keep", "x\n");
        fixture.commit_all("base");
        for i in 0..40 {
            fixture.write(&format!("f{i:02}.txt"), "x\n");
        }
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(None);
        let count = {
            let diff = app.diff.as_ref().unwrap();
            let session = app.review.session_for(&diff.source);
            diff.tree_rows(diff.model(&app.review), session).len()
        };
        app.diff.as_mut().unwrap().tree_cursor = count - 1;

        let backend = TestBackend::new(120, 14);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .expect("draw");
        let content = terminal.backend().to_string();
        assert!(content.contains("f39.txt"), "cursor row visible: {content}");
        // f00 also shows in the pane header, so we check f01 to prove the sidebar scrolled
        assert!(
            !content.contains("f01.txt"),
            "top rows scrolled off: {content}"
        );
    }

    #[test]
    fn viewed_file_shows_a_check_and_comment_count_in_the_sidebar() {
        let (_fixture, mut app) = diff_app();
        open_lib_diff(&mut app);
        cursor_to_added_line(&mut app);
        app.handle(key('c'));
        for c in "look".chars() {
            app.handle(key(c));
        }
        app.handle(key('\n'));
        // mark src/lib.rs viewed without advancing the selection off it
        let hash = app
            .review
            .model()
            .files
            .iter()
            .find(|f| f.path == "src/lib.rs")
            .map(diffler_core::model::FileDiff::content_hash)
            .unwrap();
        app.review.session.mark_viewed("src/lib.rs", &hash);
        app.diff.as_mut().unwrap().invalidate();
        let terminal = render(&mut app);
        let content = terminal.backend().to_string();
        assert!(
            content.contains("✓"),
            "viewed check in the sidebar: {content}"
        );
        assert!(
            content.contains("·1"),
            "comment count in the sidebar: {content}"
        );
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn viewed_walk_advances_the_selection() {
        let (_fixture, mut app) = diff_app();
        app.handle(key('m'));
        app.handle(key('m'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn viewed_files_lead_with_a_check_and_sort_above_the_rest() {
        let fixture = Fixture::new();
        fixture.write("a.txt", "one\n");
        fixture.write("b.txt", "one\n");
        fixture.write("c.txt", "one\n");
        fixture.commit_all("base");
        fixture.write("a.txt", "one\ntwo\n");
        fixture.write("b.txt", "one\ntwo\n");
        fixture.write("c.txt", "one\ntwo\n");
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.open_working_tree_diff(None);
        let model = app.review.model().clone();
        let hash_of = |path: &str| {
            model
                .files
                .iter()
                .find(|f| f.path == path)
                .expect("file in the diff")
                .content_hash()
        };
        let session = app
            .review
            .session_for_mut(&diffler_core::source::ReviewSource::WorkingTree);
        session.mark_viewed("a.txt", &hash_of("a.txt"));
        session.mark_viewed("b.txt", &hash_of("b.txt"));
        app.diff.as_mut().unwrap().invalidate();
        insta::assert_snapshot!(render(&mut app).backend());
    }
}
