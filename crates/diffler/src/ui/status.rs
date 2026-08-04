//! Status screen: hint line, head line, neogit-style sections with inline
//! diff expansion, recent commits, and the status bar.

use diffler_core::model::FileDiff;
use diffler_core::vcs::LogEntry;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use std::ops::Range;

use crate::app::{
    App, BRANCHES_TITLE, CI_TITLE, Group, PRS_TITLE, RECENT_TITLE, Row, Section, UNPUSHED_TITLE,
};
use crate::config::FileLayout;
use crate::keymap::Action;
use crate::theme::Theme;
use crate::transient::TransientKind;
use crate::ui::Hint;
use crate::ui::diff_render::{diff_line_height, hunk_gutter_width, render_hunk_lines};
use crate::ui::{
    age_spans, commit_meta_spans, cursor_line, diffstat_spans, highlight_spans, proportion_bar,
    status_bar, status_color,
};

/// Prefix-only hint entries: top-level keys and the transient prefixes,
/// rendered against the live keymap so remaps show. Sub-commands stay out of
/// the hint line: they appear in the which-key panel and the help popup.
const HINTS: &[Hint] = &[
    Hint::Prefix(TransientKind::Commit, "commit"),
    Hint::Prefix(TransientKind::Branch, "branch"),
    Hint::Leaf(&[Action::Stage], "stage"),
    Hint::Leaf(&[Action::Discard], "discard"),
    Hint::Leaf(&[Action::Help], "help"),
];

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let (body_area, bar) = super::screen_chrome(frame, app, HINTS);
    app.status.viewport = body_area.height;
    let (lines, scroll, line_rows) = body(app, body_area);
    app.status.body = body_area;
    app.status.scroll = scroll;
    app.status.line_rows = line_rows;
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), body_area);
    frame.render_widget(
        Paragraph::new(status_bar(app, bar.width)).style(Style::new().bg(app.theme.panel)),
        bar,
    );
}

/// Body lines, the vertical scroll keeping the cursor row in view, and a
/// per-rendered-line table of the `visible_rows` index each line belongs to
/// (`None` for headers/blanks) so mouse clicks map back to a row.
fn body(app: &App, area: Rect) -> (Vec<Line<'static>>, u16, Vec<Option<usize>>) {
    let mut lines = vec![head_line(app)];
    let (added, deleted) = Section::ALL
        .into_iter()
        .map(|section| section_diffstat(app, section))
        .fold((0, 0), |(a, d), (sa, sd)| (a + sa, d + sd));
    // omit the summary entirely when there is nothing to review
    if added != 0 || deleted != 0 {
        lines.push(changes_line(&app.theme, added, deleted));
    }
    lines.push(Line::default());
    let rows = app.visible_rows();
    let has_sections = rows
        .iter()
        .any(|row| matches!(row, Row::SectionHeader { .. }));
    if !has_sections {
        lines.push(centered_line(
            "nothing to review: working tree clean",
            app.theme.dim_style(),
            area.width,
        ));
        lines.push(Line::default());
    }

    let mut cursor_line_index = 0usize;
    // the preamble lines (head, optional changes summary, blanks, empty-state)
    // belong to no row
    let mut line_rows: Vec<Option<usize>> = vec![None; lines.len()];
    let mut index = 0;
    while let Some(row) = rows.get(index) {
        match row {
            // a hunk renders as one block: header + its diff lines, which all
            // follow contiguously in the flattened rows
            &Row::HunkHeader {
                section,
                file,
                hunk,
            } => {
                let Some(file_diff) = app.section_files(section).get(file) else {
                    index += 1;
                    continue;
                };
                let Some(hunk) = file_diff.hunks.get(hunk) else {
                    index += 1;
                    continue;
                };
                let mut accum = BodyAccum {
                    lines: &mut lines,
                    line_rows: &mut line_rows,
                    cursor_line_index: &mut cursor_line_index,
                };
                index += hunk_block(app, file_diff, hunk, index, area.width, &mut accum);
            }
            row => {
                if index > 0
                    && matches!(
                        row,
                        Row::SectionHeader { .. }
                            | Row::UnpushedHeader { .. }
                            | Row::RepoDivider
                            | Row::PrsHeader { .. }
                            | Row::BranchesHeader { .. }
                            | Row::RecentHeader { .. }
                            | Row::CiHeader { .. }
                    )
                {
                    lines.push(Line::default());
                    line_rows.push(None);
                }
                let on_cursor = index == app.status.cursor;
                if on_cursor {
                    cursor_line_index = lines.len();
                }
                let ranges = app
                    .search
                    .as_ref()
                    .map(|search| search.ranges_for(index))
                    .unwrap_or_default();
                lines.push(row_line(app, row, on_cursor, area.width, &ranges));
                // furniture: never a mouse-click or search target
                line_rows.push((!matches!(row, Row::RepoDivider)).then_some(index));
                index += 1;
            }
        }
    }

    let height = area.height.max(1) as usize;
    let scroll = cursor_line_index.saturating_sub(height - 1) as u16;
    (lines, scroll, line_rows)
}

/// The body's growing output: rendered lines, their line->row table, and the
/// screen line the cursor row starts at, threaded through the row loop.
struct BodyAccum<'a> {
    lines: &'a mut Vec<Line<'static>>,
    line_rows: &'a mut Vec<Option<usize>>,
    cursor_line_index: &'a mut usize,
}

/// Append one expanded hunk (header + wrapped diff lines) with its
/// line->row table entries; returns how many rows the block spans.
fn hunk_block(
    app: &App,
    file_diff: &FileDiff,
    hunk: &diffler_core::model::Hunk,
    index: usize,
    width: u16,
    accum: &mut BodyAccum<'_>,
) -> usize {
    let span = 1 + hunk.lines.len();
    let selected = app
        .status
        .cursor
        .checked_sub(index)
        .filter(|offset| *offset < span);
    // long lines wrap, so each diff line can span several terminal rows:
    // the header is one, then per-line heights
    let gutter = hunk_gutter_width(hunk);
    let heights: Vec<usize> = std::iter::once(1)
        .chain(
            hunk.lines
                .iter()
                .map(|line| diff_line_height(line, gutter, width)),
        )
        .collect();
    if let Some(offset) = selected {
        let above: usize = heights.iter().take(offset).sum();
        *accum.cursor_line_index = accum.lines.len() + above;
    }
    // enrichment lands asynchronously: the hash in the key ties the spans
    // to the exact content they were computed from
    let syntax = app
        .status
        .highlights
        .get(&(file_diff.path.clone(), file_diff.sides_hash()))
        .map(|cached| (cached.old.as_slice(), cached.new.as_slice()));
    accum
        .lines
        .extend(render_hunk_lines(&app.theme, hunk, syntax, width, selected));
    accum.line_rows.extend(
        heights
            .iter()
            .enumerate()
            .flat_map(|(offset, h)| std::iter::repeat_n(Some(index + offset), *h)),
    );
    span
}

fn centered_line(text: &str, style: Style, width: u16) -> Line<'static> {
    let pad = (width as usize).saturating_sub(text.chars().count()) / 2;
    Line::from(vec![
        Span::raw(" ".repeat(pad)),
        Span::styled(text.to_owned(), style),
    ])
}

/// How far HEAD has drifted from its upstream: `↑` for commits only this
/// branch has, `↓` for commits only the remote has. A branch level with its
/// upstream, or with none at all, shows nothing.
fn divergence_spans(theme: &Theme, ahead: usize, behind: usize) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if ahead > 0 {
        spans.push(Span::styled(
            format!(" ↑{ahead}"),
            Style::new().fg(theme.warn_fg).bg(theme.bg),
        ));
    }
    if behind > 0 {
        spans.push(Span::styled(
            format!(" ↓{behind}"),
            Style::new().fg(theme.accent).bg(theme.bg),
        ));
    }
    spans
}

/// Style a branch name carries wherever it appears: the head line and the
/// Branches section rows.
fn branch_name_style(theme: &Theme) -> Style {
    Style::new().fg(theme.purple).bg(theme.bg)
}

fn head_line(app: &App) -> Line<'static> {
    let theme = &app.theme;
    let mut spans = vec![Span::styled(" Head:     ", theme.dim_style())];
    match &app.head.branch {
        Some(branch) => spans.push(Span::styled(branch.clone(), branch_name_style(theme))),
        None => spans.push(Span::styled("(detached)", theme.dim_style())),
    }
    spans.extend(divergence_spans(theme, app.head.ahead, app.head.behind));
    if app.head.oid7.is_empty() {
        spans.push(Span::styled(" (no commits)", theme.dim_style()));
    } else {
        spans.push(Span::styled(
            format!(" {}", app.head.oid7),
            theme.dim_style(),
        ));
        spans.push(Span::styled(format!(" {}", app.head.subject), theme.base()));
    }
    Line::from(spans)
}

/// Grand-total diffstat summary: ` Changes  +A -B  <bar>`, aligned under the
/// head line. The bar is a compact green:red proportion of added to deleted.
fn changes_line(theme: &Theme, added: usize, deleted: usize) -> Line<'static> {
    let mut spans = vec![Span::styled(" Changes  ", theme.dim_style())];
    spans.extend(diffstat_spans(theme, added, deleted, theme.bg));
    spans.push(Span::styled("  ", theme.base()));
    spans.extend(proportion_bar(theme, added, deleted, theme.bg));
    Line::from(spans)
}

fn row_line(
    app: &App,
    row: &Row,
    selected: bool,
    width: u16,
    search: &[(Range<usize>, bool)],
) -> Line<'static> {
    let theme = &app.theme;
    let spans = match row {
        Row::SectionHeader { section, count } => {
            let mut spans = header_spans(
                theme,
                section.title(),
                Some(*count),
                app.is_folded(*section),
                search,
            );
            let (added, deleted) = section_diffstat(app, *section);
            spans.extend(diffstat_spans(theme, added, deleted, theme.bg));
            spans
        }
        Row::UnpushedHeader { count } => header_spans(
            theme,
            UNPUSHED_TITLE,
            Some(*count),
            app.is_group_folded(Group::Unpushed),
            search,
        ),
        Row::Unpushed { index } => {
            let entry = app.status.unpushed.get(*index);
            commit_spans(app, entry, theme, width, search)
        }
        Row::RecentHeader { count } => header_spans(
            theme,
            RECENT_TITLE,
            Some(*count),
            app.is_group_folded(Group::Recent),
            search,
        ),
        Row::Dir {
            section,
            path,
            name,
            depth,
        } => dir_spans(
            theme,
            name,
            app.is_dir_folded(*section, path),
            *depth,
            search,
        ),
        Row::File {
            section,
            index,
            depth,
        } => {
            let file = app.section_files(*section).get(*index);
            file_spans(app, file, theme, *depth, search)
        }
        Row::Commit { index } => {
            let entry = app.status.recent.get(*index);
            commit_spans(app, entry, theme, width, search)
        }
        Row::Pr => pr_spans(app, theme, search),
        Row::RepoDivider => repo_divider_spans(theme, width),
        Row::PrsHeader { count } => header_spans(
            theme,
            PRS_TITLE,
            *count,
            app.is_group_folded(Group::Prs),
            search,
        ),
        Row::OpenPr { index } => open_pr_spans(app, *index, theme, search),
        Row::BranchesHeader { count } => header_spans(
            theme,
            BRANCHES_TITLE,
            Some(*count),
            app.is_group_folded(Group::Branches),
            search,
        ),
        Row::Branch { index } => branch_spans(app, *index, theme, width, search),
        Row::CiHeader { count } => header_spans(
            theme,
            CI_TITLE,
            Some(*count),
            app.is_group_folded(Group::Ci),
            search,
        ),
        Row::CiRun { index, nested } => ci_run_spans(app, *index, *nested, theme, width, search),
        // hunk rows are rendered as blocks in `body`, never through here
        Row::HunkHeader { .. } | Row::DiffLine { .. } => Vec::new(),
    };
    let line = Line::from(spans);
    if selected {
        cursor_line(line, theme, width)
    } else {
        line
    }
}

/// A collapsible section header: fold arrow, title, and an item count. `count`
/// is `None` while a lazily-fetched group's first fetch is still in flight, so
/// the header shows the group exists without claiming a total it doesn't
/// know yet.
fn header_spans(
    theme: &Theme,
    title: &str,
    count: Option<usize>,
    folded: bool,
    search: &[(Range<usize>, bool)],
) -> Vec<Span<'static>> {
    let title_style = Style::new().fg(theme.accent).bg(theme.bg);
    let mut spans = vec![Span::styled(
        if folded { " ▸ " } else { " ▾ " },
        theme.dim_style(),
    )];
    spans.extend(highlight_spans(title, title_style, search, theme));
    if let Some(count) = count {
        spans.push(Span::styled(format!(" ({count})"), theme.dim_style()));
    }
    spans
}

/// Indentation for a tree row at `depth` within a section: a base indent that
/// clears the header's fold arrow, plus two cells per level.
fn tree_indent(depth: usize) -> String {
    " ".repeat(5 + depth * 2)
}

/// A directory row: indent, fold arrow, the dim directory name.
fn dir_spans(
    theme: &Theme,
    name: &str,
    folded: bool,
    depth: usize,
    search: &[(Range<usize>, bool)],
) -> Vec<Span<'static>> {
    let mut spans = vec![
        Span::styled(tree_indent(depth), theme.base()),
        Span::styled(
            if folded { "▸ " } else { "▾ " }.to_owned(),
            theme.dim_style(),
        ),
    ];
    spans.extend(highlight_spans(name, theme.base(), search, theme));
    spans
}

/// A file row. In the tree layout: indent, status glyph (colored), basename;
/// the directory rows above carry the path. In the flat magit list: a status
/// glyph plus the full repo-relative path, no indent. Both trail the viewed
/// check and the file's `+A -B` diffstat.
fn file_spans(
    app: &App,
    file: Option<&FileDiff>,
    theme: &Theme,
    depth: usize,
    search: &[(Range<usize>, bool)],
) -> Vec<Span<'static>> {
    let Some(file) = file else {
        return Vec::new();
    };
    let glyph = file.status.glyph();
    let flat = app.config.ui.status_file_layout == FileLayout::List;
    let name = app.status_file_name(file);
    let indent = if flat {
        " ".to_owned()
    } else {
        tree_indent(depth)
    };
    let mut spans = vec![
        Span::styled(indent, theme.base()),
        Span::styled(
            format!("{glyph} "),
            Style::new()
                .fg(status_color(theme, file.status))
                .bg(theme.bg),
        ),
    ];
    spans.extend(highlight_spans(name, theme.base(), search, theme));
    if app.is_path_viewed(&file.path) {
        spans.push(Span::styled(" ✓", theme.dim_style()));
    }
    let (added, deleted) = file.diffstat();
    spans.extend(diffstat_spans(theme, added, deleted, theme.bg));
    spans
}

/// The rollup glyph slot rows share with `file_spans`' status glyph: the
/// worst status among a row's matching CI runs, or two blank cells so a row
/// with none stays aligned with one that has some.
/// The leading three cells of a repo-band row: margin, rolled-up CI glyph,
/// separator. Blank when no run matches, which keeps every row in a group
/// aligned whether or not its CI has been seen.
fn ci_glyph_spans(rollup: Option<crate::ci::JobStatus>, theme: &Theme) -> Vec<Span<'static>> {
    match rollup {
        Some(status) => vec![Span::styled(
            format!(" {} ", status.glyph()),
            Style::new()
                .fg(super::ci_status_color(theme, status))
                .bg(theme.bg),
        )],
        None => vec![Span::raw("   ")],
    }
}

fn commit_spans(
    app: &App,
    entry: Option<&LogEntry>,
    theme: &Theme,
    width: u16,
    search: &[(Range<usize>, bool)],
) -> Vec<Span<'static>> {
    let Some(entry) = entry else {
        return Vec::new();
    };
    let mut spans = ci_glyph_spans(app.ci_rollup(&app.runs_for_commit(&entry.oid)), theme);
    spans.push(Span::styled(
        format!("  {} ", entry.oid7),
        Style::new().fg(theme.warn_fg),
    ));
    spans.extend(highlight_spans(
        &entry.subject,
        Style::new().fg(theme.fg),
        search,
        theme,
    ));
    let used: usize = spans.iter().map(Span::width).sum();
    spans.extend(commit_meta_spans(
        theme,
        &entry.author,
        entry.time_unix,
        app.now_unix,
        used,
        width as usize,
    ));
    spans
}

/// The branch's open PR as a selectable row: `⇄ PR #12 title → base`.
/// One PR row: `⇄ PR #12 title → base`, shared by the branch's own PR and
/// the repo-band list of other open PRs.
fn pr_row_spans(
    pr: &crate::ci::PullRequest,
    theme: &Theme,
    search: &[(Range<usize>, bool)],
) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled("⇄ ".to_owned(), Style::new().fg(theme.accent))];
    spans.extend(highlight_spans(
        &format!("PR #{} {}", pr.number, pr.title),
        Style::new().fg(theme.fg),
        search,
        theme,
    ));
    spans.push(Span::styled(
        format!(" → {}", pr.base_ref),
        theme.dim_style(),
    ));
    spans
}

fn pr_spans(app: &App, theme: &Theme, search: &[(Range<usize>, bool)]) -> Vec<Span<'static>> {
    app.pr.as_ref().map_or_else(Vec::new, |pr| {
        let mut spans = vec![Span::raw("   ")];
        spans.extend(pr_row_spans(pr, theme, search));
        spans
    })
}

fn open_pr_spans(
    app: &App,
    index: usize,
    theme: &Theme,
    search: &[(Range<usize>, bool)],
) -> Vec<Span<'static>> {
    let others = app.other_prs();
    let Some(pr) = others.get(index) else {
        return Vec::new();
    };
    let mut spans = ci_glyph_spans(app.ci_rollup(&app.runs_for_commit(&pr.head_oid)), theme);
    spans.extend(pr_row_spans(pr, theme, search));
    spans
}

/// A local branch row: name, divergence from its upstream, age right-aligned
/// at the pane edge.
fn branch_spans(
    app: &App,
    index: usize,
    theme: &Theme,
    width: u16,
    search: &[(Range<usize>, bool)],
) -> Vec<Span<'static>> {
    let Some(branch) = app.status.branches.get(index) else {
        return Vec::new();
    };
    let mut spans = ci_glyph_spans(app.ci_rollup(&app.runs_for_branch(&branch.name)), theme);
    // the same head marker the branch picker uses, so the two lists agree
    let marker = if branch.is_head { "* " } else { "  " };
    spans.push(Span::styled(marker, theme.dim_style()));
    spans.extend(highlight_spans(
        &branch.name,
        branch_name_style(theme),
        search,
        theme,
    ));
    spans.extend(divergence_spans(theme, branch.ahead, branch.behind));
    let used: usize = spans.iter().map(Span::width).sum();
    spans.extend(age_spans(
        theme,
        branch.tip_unix,
        app.now_unix,
        used,
        width as usize,
    ));
    spans
}

/// The band divider between "this branch" and "this repo": the same label
/// column the head lines use, then a rule filling the rest of the width.
const REPO_DIVIDER_LABEL: &str = " Repo      ";

fn repo_divider_spans(theme: &Theme, width: u16) -> Vec<Span<'static>> {
    let rule_width = (width as usize).saturating_sub(REPO_DIVIDER_LABEL.chars().count());
    vec![
        Span::styled(REPO_DIVIDER_LABEL, theme.dim_style()),
        Span::styled("─".repeat(rule_width), theme.dim_style()),
    ]
}

/// `nested` runs sit under the commit/branch/PR row that triggered them (one
/// tree level deeper); flat ones sit directly under the trailing CI header.
fn ci_run_spans(
    app: &App,
    index: usize,
    nested: bool,
    theme: &Theme,
    width: u16,
    search: &[(Range<usize>, bool)],
) -> Vec<Span<'static>> {
    let Some(run) = app.runs.get(index) else {
        return Vec::new();
    };
    let glyph = run.status.glyph();
    let color = super::ci_status_color(theme, run.status);
    let indent = if nested { "       " } else { "     " };
    let mut spans = vec![Span::styled(
        format!("{indent}{glyph} "),
        Style::new().fg(color),
    )];
    // tag the source remote when runs from several forges are aggregated
    if let Some(remote) = &run.remote {
        spans.push(Span::styled(
            format!("{remote}/"),
            Style::new().fg(theme.fg),
        ));
    }
    spans.extend(highlight_spans(
        &run.name,
        Style::new().fg(theme.accent),
        search,
        theme,
    ));
    let tag_width = run
        .remote
        .as_ref()
        .map_or(0, |remote| remote.chars().count() + 1);
    let pad = 14usize.saturating_sub(tag_width + run.name.chars().count());
    spans.push(Span::raw(" ".repeat(pad)));
    let short: String = run.commit.chars().take(7).collect();
    spans.push(Span::styled(
        format!("  {:<32}", super::elide(&run.title, 32)),
        Style::new().fg(theme.fg),
    ));
    spans.push(Span::styled(
        format!("  {:<18}", super::elide(&run.branch, 18)),
        Style::new().fg(theme.purple),
    ));
    spans.push(Span::styled(
        format!("  {short}"),
        Style::new().fg(theme.warn_fg),
    ));
    if let Some(created) = run.created {
        let age = super::relative_time(app.now_unix, created.unix_timestamp());
        let used: usize = spans.iter().map(Span::width).sum();
        if used + age.chars().count() + 1 < width as usize {
            let gap = width as usize - used - age.chars().count() - 1;
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(Span::styled(age, theme.dim_style()));
        }
    }
    spans
}

/// Summed `(added, deleted)` over every file in a section.
fn section_diffstat(app: &App, section: Section) -> (usize, usize) {
    app.section_files(section)
        .iter()
        .map(FileDiff::diffstat)
        .fold((0, 0), |(a, d), (fa, fd)| (a + fa, d + fd))
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::app::{App, Group, Row, Section};
    use crate::config::LoadedConfig;
    use crate::event::AppEvent;
    use crate::test_support::{
        Fixture, key, mouse_click, mouse_scroll, render, standard_fixture, two_hunk_fixture,
    };

    #[test]
    fn status_screen_shows_ci_rollup_glyphs_and_one_unfolded_row() {
        use crate::ci::{CiRun, JobStatus, RunId};
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let commit_oid = app.status.recent[0].oid.clone();
        let run = |name: &str, branch: &str, sha: &str, status| CiRun {
            id: RunId(name.to_owned()),
            name: name.to_owned(),
            title: "ci run".to_owned(),
            branch: branch.to_owned(),
            commit: sha.to_owned(),
            author: String::new(),
            created: None,
            status,
            url: None,
            remote: None,
        };
        app.runs = vec![
            run("CI", "main", "abc1234def", JobStatus::Failed),
            run("Release", "main", "9988776655", JobStatus::Ok),
            run("CI", "other", &commit_oid, JobStatus::Ok),
        ];
        // the branch row is individually unfolded: its runs show as children
        app.status.group_folded[Group::Branches.index()] = false;
        app.status.unfolded_branches.insert("main".to_owned());
        // the recent-commits row picks up a glyph too, but stays folded
        app.status.group_folded[Group::Recent.index()] = false;
        // pin "now" so the ages render stably
        app.now_unix = app
            .status
            .recent
            .iter()
            .map(|e| e.time_unix)
            .max()
            .unwrap_or(0)
            + 3661;
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn status_tags_ci_runs_by_remote_when_aggregated() {
        use crate::ci::{CiRun, JobStatus, RunId};
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let run = |remote: &str, status| CiRun {
            id: RunId(format!("{remote}-CI")),
            name: "CI".to_owned(),
            title: "ci run".to_owned(),
            branch: "main".to_owned(),
            commit: "abc1234def".to_owned(),
            author: String::new(),
            created: None,
            status,
            url: None,
            remote: Some(remote.to_owned()),
        };
        app.runs = vec![
            run("origin", JobStatus::Ok),
            run("codeberg", JobStatus::Running),
        ];
        app.status.group_folded[Group::Branches.index()] = false;
        app.status.unfolded_branches.insert("main".to_owned());
        // pin "now" an hour past the branch tip so the age renders stably
        app.now_unix = app
            .status
            .branches
            .iter()
            .map(|b| b.tip_unix)
            .max()
            .unwrap_or(0)
            + 3661;
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn ci_header_shows_the_branch_pr() {
        use crate::ci::{CiRun, JobStatus, PullRequest, RunId};
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.runs = vec![CiRun {
            id: RunId("CI".into()),
            name: "CI".into(),
            title: "ci run".into(),
            branch: "feat/x".into(),
            commit: "abc1234def".into(),
            author: String::new(),
            created: None,
            status: JobStatus::Ok,
            url: None,
            remote: None,
        }];
        app.pr = Some(PullRequest {
            number: 28,
            title: "Inline CI runs".into(),
            url: None,
            head_ref: "feat/x".into(),
            author: String::new(),
            base_ref: "main".into(),
            head_oid: "feedc0de".into(),
        });
        let rendered = format!("{:?}", render(&mut app).backend());
        assert!(rendered.contains("PR #28"), "header shows the PR number");
    }

    /// Screen position rendering `visible_rows()[row]`, via the geometry the
    /// last render stored.
    fn screen_pos(app: &App, row: usize) -> (u16, u16) {
        let line = app
            .status
            .line_rows
            .iter()
            .position(|r| *r == Some(row))
            .expect("row is on screen");
        let y = app.status.body.y + (line as u16 - app.status.scroll);
        (app.status.body.x + 1, y)
    }

    #[test]
    fn mouse_wheel_scrolls_the_status_cursor() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        render(&mut app);
        let before = app.status.cursor;
        app.handle(mouse_scroll(true, 5, 5));
        let after = app.status.cursor;
        assert!(after > before, "wheel down advanced the cursor");
        app.handle(mouse_scroll(false, 5, 5));
        assert!(app.status.cursor < after, "wheel up moved it back");
    }

    #[test]
    fn clicking_a_file_row_selects_it() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        render(&mut app);
        // pick a non-zero file row to prove the click maps there, not just to 0
        let rows = app.visible_rows();
        let target = rows
            .iter()
            .position(|r| matches!(r, Row::File { .. }))
            .expect("a file row");
        let (x, y) = screen_pos(&app, target);
        app.handle(mouse_click(x, y));
        assert_eq!(app.status.cursor, target);
    }

    #[test]
    fn single_click_on_a_section_header_only_selects() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        render(&mut app);
        let Some(Row::SectionHeader { section, .. }) = app.visible_rows().first().cloned() else {
            panic!("first row is a section header");
        };
        let folded = app.is_folded(section);
        let (x, y) = screen_pos(&app, 0);
        app.handle(mouse_click(x, y));
        assert_eq!(app.status.cursor, 0);
        assert_eq!(app.is_folded(section), folded, "single click does not fold");
    }

    #[test]
    fn double_clicking_a_section_header_toggles_its_fold() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        render(&mut app);
        let Some(Row::SectionHeader { section, .. }) = app.visible_rows().first().cloned() else {
            panic!("first row is a section header");
        };
        let folded = app.is_folded(section);
        let (x, y) = screen_pos(&app, 0);
        app.handle(mouse_click(x, y));
        app.handle(mouse_click(x, y));
        assert_eq!(app.status.cursor, 0);
        assert_ne!(
            app.is_folded(section),
            folded,
            "double-click toggled the fold"
        );
    }

    #[test]
    fn double_clicking_the_recent_commits_header_toggles_it() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        render(&mut app);
        let target = app
            .visible_rows()
            .iter()
            .position(|r| matches!(r, Row::RecentHeader { .. }))
            .expect("a recent-commits header");
        let folded = app.is_group_folded(Group::Recent);
        let (x, y) = screen_pos(&app, target);
        app.handle(mouse_click(x, y));
        app.handle(mouse_click(x, y));
        assert_eq!(app.status.cursor, target);
        assert_ne!(
            app.is_group_folded(Group::Recent),
            folded,
            "double-click toggled the fold"
        );
    }

    fn app_for(fixture: &Fixture) -> App {
        App::new(fixture.review(), LoadedConfig::default())
    }

    fn cursor_to_file(app: &mut App, section: Section) {
        let rows = app.visible_rows();
        app.status.cursor = rows
            .iter()
            .position(|row| matches!(row, Row::File { section: s, .. } if *s == section))
            .expect("file row");
    }

    #[test]
    fn the_head_line_counts_commits_the_remote_does_not_have() {
        let fixture = standard_fixture();
        fixture.track("main", "HEAD");
        fixture.write("shipped.rs", "pub fn shipped() {}\n");
        fixture.commit_all("only here");
        let mut app = app_for(&fixture);
        let screen = render(&mut app).backend().to_string();
        assert!(
            screen.contains("↑1"),
            "unpushed commit is on screen: {screen}"
        );
        assert!(!screen.contains("↓"), "nothing to pull: {screen}");
    }

    #[test]
    fn the_head_line_counts_commits_only_the_remote_has() {
        let fixture = standard_fixture();
        fixture.write("remote_only.rs", "pub fn theirs() {}\n");
        fixture.commit_all("landed upstream");
        fixture.track("main", "HEAD");
        // rewind the branch, leaving the tracking ref one commit ahead
        let parent = fixture.repo.revparse_single("HEAD~1").expect("parent").id();
        fixture
            .repo
            .reference("refs/heads/main", parent, true, "rewind")
            .expect("rewind");
        let mut app = app_for(&fixture);
        let screen = render(&mut app).backend().to_string();
        assert!(
            screen.contains("↓1"),
            "a commit to pull is on screen: {screen}"
        );
        assert!(!screen.contains("↑"), "nothing to push: {screen}");
    }

    #[test]
    fn a_branch_level_with_its_upstream_says_nothing() {
        let fixture = standard_fixture();
        fixture.track("main", "HEAD");
        let mut app = app_for(&fixture);
        let screen = render(&mut app).backend().to_string();
        assert!(!screen.contains("↑"), "in sync stays quiet: {screen}");
        assert!(!screen.contains("↓"), "in sync stays quiet: {screen}");
    }

    #[test]
    fn a_branch_with_no_upstream_says_nothing() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        let screen = render(&mut app).backend().to_string();
        assert!(!screen.contains("↑"), "{screen}");
    }

    #[test]
    fn status_screen_renders() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    /// Weight is reserved for content that is itself bold: markdown emphasis
    /// and syntax keywords. Chrome says what it is with colour.
    #[test]
    fn no_chrome_on_the_status_screen_reaches_for_bold() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        let terminal = render(&mut app);
        let buffer = terminal.backend().buffer();
        let bold: Vec<String> = buffer
            .content()
            .iter()
            .filter(|cell| cell.modifier.contains(ratatui::style::Modifier::BOLD))
            .map(|cell| cell.symbol().to_owned())
            .collect();
        assert!(bold.is_empty(), "bold chrome: {bold:?}");
    }

    #[test]
    fn no_chrome_in_the_which_key_panel_reaches_for_bold() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        app.handle(key('b'));
        app.handle(AppEvent::Tick);
        assert!(app.which_key_panel().is_some(), "the panel is revealed");
        let terminal = render(&mut app);
        let bold = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .filter(|cell| cell.modifier.contains(ratatui::style::Modifier::BOLD))
            .count();
        assert_eq!(bold, 0, "bold chrome in the which-key panel");
    }

    #[test]
    fn no_chrome_in_the_command_palette_reaches_for_bold() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        app.handle(crate::test_support::ctrl_key('k'));
        let terminal = render(&mut app);
        let buffer = terminal.backend().buffer();
        assert!(
            terminal.backend().to_string().contains("Commands"),
            "the palette is open"
        );
        let bold = buffer
            .content()
            .iter()
            .filter(|cell| cell.modifier.contains(ratatui::style::Modifier::BOLD))
            .count();
        assert_eq!(bold, 0, "bold chrome in the palette");
    }

    #[test]
    fn status_screen_renders_as_a_tree_when_configured() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.status_file_layout = crate::config::FileLayout::Tree;
        let mut app = App::new(fixture.review(), loaded);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn folded_sections_show_headers_only() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        // fold each section: cursor lands back on the header after a fold,
        // so one j reaches the next section's header
        for _ in 0..3 {
            app.handle(key('\t'));
            app.handle(key('j'));
        }
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn expanded_file_shows_inline_hunks_with_emphasis() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        cursor_to_file(&mut app, Section::Unstaged);
        app.handle(key('\t'));
        let terminal = render(&mut app);
        // the text snapshot carries no styles: assert the intra-line
        // emphasis backgrounds made it into the buffer separately
        let styles = format!("{:?}", terminal.backend().buffer());
        let add_emph = format!("{:?}", app.theme.add_emph_bg);
        let del_emph = format!("{:?}", app.theme.del_emph_bg);
        assert!(styles.contains(&add_emph), "added emphasis bg rendered");
        assert!(styles.contains(&del_emph), "deleted emphasis bg rendered");
        // the inline diff is syntax-highlighted like the diff pane: the lazy
        // cache filled for the expanded rust file produced styled ranges
        let lib = app
            .status
            .highlights
            .iter()
            .find_map(|((path, _), cached)| (path == "src/lib.rs").then_some(cached))
            .expect("expanded file highlighted");
        assert!(
            lib.new.iter().any(|line| !line.is_empty()),
            "rust syntax produced styled ranges for the inline diff"
        );
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn same_file_staged_and_unstaged_appears_in_both_sections() {
        let fixture = standard_fixture();
        fixture.write("src/lib.rs", "pub fn answer() -> u32 {\n    42\n}\n");
        fixture.stage("src/lib.rs");
        fixture.write("src/lib.rs", "pub fn answer() -> u32 {\n    42 + 0\n}\n");
        let mut app = app_for(&fixture);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn cursor_on_a_diff_line_highlights_the_row() {
        let fixture = two_hunk_fixture();
        let mut app = app_for(&fixture);
        cursor_to_file(&mut app, Section::Unstaged);
        app.handle(key('\t'));
        // expanding puts the hunk header directly under the file row
        app.handle(key('j'));
        app.handle(key('j'));
        assert!(matches!(
            app.visible_rows()[app.status.cursor],
            Row::DiffLine { .. }
        ));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn confirm_dialog_renders_over_the_status_screen() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        cursor_to_file(&mut app, Section::Unstaged);
        app.handle(key('x'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn viewed_file_renders_collapsed_with_a_check_mark() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        cursor_to_file(&mut app, Section::Unstaged);
        app.handle(key('\t'));
        app.handle(key('v'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn recent_commits_unfold_to_oid_and_subject_rows() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        let rows = app.visible_rows();
        app.status.cursor = rows
            .iter()
            .position(|row| matches!(row, Row::RecentHeader { .. }))
            .expect("recent header");
        app.handle(key('\t'));
        // pin "now" an hour past the newest commit so the ages render stably
        app.now_unix = app
            .status
            .recent
            .iter()
            .map(|e| e.time_unix)
            .max()
            .unwrap_or(0)
            + 3661;
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn unpushed_section_renders_above_recent_commits() {
        // commit_all stages everything in the worktree, so the upstream and
        // the unpushed commits are set up before any dirty files, keeping
        // Untracked alongside Unpushed in the rendered order
        let fixture = Fixture::new();
        fixture.write("base.rs", "pub fn base() {}\n");
        fixture.commit_all("initial commit");
        fixture.track("main", "HEAD");
        fixture.write("shipped_one.rs", "pub fn one() {}\n");
        fixture.commit_all("first unpushed");
        fixture.write("shipped_two.rs", "pub fn two() {}\n");
        fixture.commit_all("second unpushed");
        fixture.write("todo.md", "- [ ] review\n");
        let mut app = app_for(&fixture);
        // pin "now" an hour past the newest commit so the ages render stably
        app.now_unix = app
            .status
            .unpushed
            .iter()
            .chain(app.status.recent.iter())
            .map(|e| e.time_unix)
            .max()
            .unwrap_or(0)
            + 3661;
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn repo_band_shows_the_divider_and_branches_header_folded() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        let terminal = render(&mut app);
        let screen = terminal.backend().to_string();
        assert!(screen.contains("Repo"), "{screen}");
        assert!(screen.contains("Branches (1)"), "{screen}");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn open_prs_group_unfolded_lists_the_repos_other_prs() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        app.ci_remotes = vec![crate::app::CiRemote {
            name: "origin".into(),
            detected: crate::ci::Detected {
                kind: crate::ci::ProviderKind::GitHub,
                host: None,
            },
            url: None,
        }];
        app.prs = vec![
            crate::ci::PullRequest {
                number: 141,
                title: "stop burning the GitHub rate limit on CI polls".into(),
                url: None,
                base_ref: "main".into(),
                head_ref: "feat/rate-limit".into(),
                head_oid: "0000000000000000000000000000000000000abc".into(),
                author: "reviewer".into(),
            },
            crate::ci::PullRequest {
                number: 142,
                title: "mark a whole folder viewed from the tree".into(),
                url: None,
                base_ref: "main".into(),
                head_ref: "feat/mark-folder".into(),
                head_oid: "0000000000000000000000000000000000000def".into(),
                author: "reviewer".into(),
            },
        ];
        app.status.prs_loaded = true;
        app.status.group_folded[Group::Prs.index()] = false;
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn branches_unfolded_show_divergence_and_age() {
        let fixture = standard_fixture();
        fixture.branch("feat/topic");
        fixture.track("main", "HEAD");
        fixture.write("shipped.rs", "pub fn shipped() {}\n");
        fixture.commit_all("only here");
        let mut app = app_for(&fixture);
        app.status.group_folded[Group::Branches.index()] = false;
        // pin "now" an hour past the newest branch tip so the ages render stably
        app.now_unix = app
            .status
            .branches
            .iter()
            .map(|b| b.tip_unix)
            .max()
            .unwrap_or(0)
            + 3661;
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn collapsed_dir_chain_renders_its_joined_name() {
        let fixture = Fixture::new();
        fixture.write("keep.txt", "x\n");
        fixture.commit_all("initial commit");
        // a single-child chain: docs/ -> api/ -> intro.md
        fixture.write("docs/api/intro.md", "# intro\n");
        let mut app = app_for(&fixture);
        let screen = render(&mut app).backend().to_string();
        assert!(
            screen.contains("docs/api"),
            "the collapsed chain shows its joined name, not just the last segment:\n{screen}"
        );
    }

    #[test]
    fn clean_repo_renders_empty_state_with_recent_commits() {
        let fixture = Fixture::new();
        fixture.write("src/lib.rs", "pub fn answer() -> u32 {\n    41\n}\n");
        fixture.commit_all("initial commit");
        let mut app = app_for(&fixture);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn status_bar_shows_message() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        // sending feedback surfaces an info message in the bar
        app.handle(key('Z'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn status_search_highlights_a_matching_row() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        // "o" matches todo.md (the cursor lands there) and the Recent commits
        // header, whose matched letters carry the search background
        app.handle(key('/'));
        app.handle(key('o'));
        app.handle(key('\n'));
        let terminal = render(&mut app);
        let buffer = format!("{:?}", terminal.backend().buffer());
        assert!(
            buffer.contains(&format!("{:?}", app.theme.search)),
            "a non-cursor match carries the search background"
        );
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn file_row_highlights_only_the_matched_substring() {
        let fixture = standard_fixture();
        // flat layout renders the whole path "src/lib.rs"
        let app = app_for(&fixture);
        let file = app.section_files(Section::Unstaged).first().expect("file");
        // "lib" sits at bytes 4..7 of "src/lib.rs"
        let spans = super::file_spans(&app, Some(file), &app.theme, 0, &[(4..7, true)]);
        let highlighted: Vec<&str> = spans
            .iter()
            .filter(|s| s.style.bg == Some(app.theme.search_current))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(
            highlighted,
            vec!["lib"],
            "only the matched word, not the whole row: {spans:?}"
        );
    }

    #[test]
    fn help_popup_lists_the_active_keymap_and_transient_groups() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        app.handle(key('?'));
        let terminal = render(&mut app);
        let content = terminal.backend().to_string();
        assert!(
            content.contains("open the full working-tree diff"),
            "{content}"
        );
        // transients appear as a prefix line plus their grouped sub-keys
        assert!(content.contains("Commit …"), "{content}");
        assert!(content.contains("Amend"), "{content}");
        assert!(content.contains("Branch …"), "{content}");
        assert!(content.contains("Create and checkout"), "{content}");
        insta::assert_snapshot!(terminal.backend());
    }

    #[test]
    fn hint_line_reflects_config_remaps() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded
            .config
            .keys
            .status
            .insert("stage".to_owned(), "<c-s>".to_owned());
        let mut app = App::new(fixture.review(), loaded);
        let content = render(&mut app).backend().to_string();
        assert!(content.contains("<c-s> stage"), "{content}");
        assert!(!content.contains(" s stage"), "{content}");
    }

    #[test]
    fn which_key_branch_panel_renders_after_the_reveal_tick() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        app.handle(key('b'));
        // the reveal timer has not elapsed: no panel yet (no flash)
        assert!(app.which_key_panel().is_none());
        app.handle(AppEvent::Tick);
        assert!(app.which_key_panel().is_some());
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn which_key_diff_panel_renders_after_the_reveal_tick() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        app.handle(key('d'));
        app.handle(AppEvent::Tick);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn the_diff_transient_picks_a_branch_to_review_against() {
        let fixture = crate::test_support::branch_fixture();
        let mut app = app_for(&fixture);
        app.handle(key('d'));
        app.handle(key('b'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn which_key_commit_panel_renders_after_the_reveal_tick() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        app.handle(key('c'));
        app.handle(AppEvent::Tick);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn which_key_push_panel_renders_after_the_reveal_tick() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        app.handle(key('P'));
        app.handle(AppEvent::Tick);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn which_key_fetch_panel_renders_after_the_reveal_tick() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        app.handle(key('f'));
        app.handle(AppEvent::Tick);
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn a_fast_resolving_key_never_flashes_the_panel() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        app.handle(key('b'));
        // resolving before the reveal tick: the panel is never shown and the
        // transient closes
        assert!(app.which_key_panel().is_none());
        app.handle(key('n'));
        assert!(app.transient.is_none(), "n resolved create");
        assert!(app.which_key_panel().is_none());
    }

    #[test]
    fn prefix_only_hint_line_shows_no_sub_commands() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        let content = render(&mut app).backend().to_string();
        let hint = content.lines().next().unwrap_or_default();
        assert!(hint.contains("c commit"), "{hint}");
        assert!(hint.contains("b branch"), "{hint}");
        assert!(hint.contains("? help"), "{hint}");
        // everything else lives in which-key and the ? help popup
        assert!(!hint.contains("push"), "{hint}");
        assert!(!hint.contains("amend"), "{hint}");
        assert!(!hint.contains("checkout"), "{hint}");
    }

    #[test]
    fn branch_list_modal_renders_with_the_head_marker() {
        let fixture = standard_fixture();
        fixture.branch("feat/topic");
        let mut app = app_for(&fixture);
        app.handle(key('b'));
        app.handle(key('b'));
        insta::assert_snapshot!(render(&mut app).backend());
    }

    #[test]
    fn palette_lists_commands_and_filters_on_typing() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        app.handle(crate::event::AppEvent::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('k'),
                crossterm::event::KeyModifiers::CONTROL,
            ),
        ));
        insta::assert_snapshot!("palette_open", render(&mut app).backend());
        for c in "amend".chars() {
            app.handle(key(c));
        }
        insta::assert_snapshot!("palette_filtered", render(&mut app).backend());
    }

    #[test]
    fn cursor_highlight_moves_with_the_cursor() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        // styles live in the buffer debug output, not the text view
        app.handle(key('j'));
        let moved = format!("{:?}", render(&mut app).backend().buffer());
        app.handle(key('k'));
        let back = format!("{:?}", render(&mut app).backend().buffer());
        assert_ne!(moved, back, "cursor movement must change the rendered rows");
    }

    #[test]
    fn body_scrolls_to_keep_the_cursor_visible() {
        let fixture = two_hunk_fixture();
        let mut app = app_for(&fixture);
        cursor_to_file(&mut app, Section::Unstaged);
        app.handle(key('\t'));
        app.handle(key('G'));
        let backend = TestBackend::new(120, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .expect("draw");
        let content = terminal.backend().to_string();
        assert!(
            content.contains("Recent commits"),
            "view follows the cursor to the bottom: {content}"
        );
    }

    #[test]
    fn ticks_do_not_change_the_screen() {
        let fixture = standard_fixture();
        let mut app = app_for(&fixture);
        let before = render(&mut app).backend().to_string();
        app.handle(AppEvent::Tick);
        let after = render(&mut app).backend().to_string();
        assert_eq!(before, after);
    }

    use crate::theme::Theme;
    use crate::ui::{diffstat_spans, proportion_bar, status_color};
    use diffler_core::model::FileStatus;

    fn bar_cells(spans: &[ratatui::text::Span<'_>], fg: ratatui::style::Color) -> usize {
        spans
            .iter()
            .filter(|s| s.style.fg == Some(fg))
            .map(|s| s.content.chars().count())
            .sum()
    }

    #[test]
    fn proportion_bar_is_empty_without_changes() {
        let theme = Theme::github_dark();
        assert!(proportion_bar(&theme, 0, 0, theme.bg).is_empty());
    }

    #[test]
    fn proportion_bar_fills_five_cells_split_by_ratio() {
        let theme = Theme::github_dark();
        for (add, del) in [(5, 0), (0, 5), (8, 4), (1, 100), (100, 1)] {
            let spans = proportion_bar(&theme, add, del, theme.bg);
            let green = bar_cells(&spans, theme.added);
            let red = bar_cells(&spans, theme.error_fg);
            assert_eq!(green + red, 5, "({add},{del}) must fill 5 cells");
            // a non-zero side always keeps at least one cell so it stays visible
            assert_eq!(add > 0, green > 0, "({add},{del}) green visibility");
            assert_eq!(del > 0, red > 0, "({add},{del}) red visibility");
        }
    }

    #[test]
    fn status_color_distinguishes_the_status_groups() {
        let theme = Theme::github_dark();
        assert_eq!(status_color(&theme, FileStatus::Added), theme.added);
        assert_eq!(status_color(&theme, FileStatus::Untracked), theme.added);
        assert_eq!(status_color(&theme, FileStatus::Deleted), theme.error_fg);
        assert_eq!(status_color(&theme, FileStatus::Modified), theme.warn_fg);
        assert_eq!(status_color(&theme, FileStatus::Renamed), theme.warn_fg);
    }

    #[test]
    fn diffstat_spans_color_each_side_and_dim_a_zero() {
        let theme = Theme::github_dark();
        assert!(diffstat_spans(&theme, 0, 0, theme.bg).is_empty());

        let spans = diffstat_spans(&theme, 3, 0, theme.bg);
        assert_eq!(spans[0].content, " +3");
        assert_eq!(spans[0].style.fg, Some(theme.added));
        assert_eq!(spans[1].content, " -0");
        assert_eq!(spans[1].style.fg, Some(theme.dim), "a zero side is dimmed");

        let spans = diffstat_spans(&theme, 0, 7, theme.bg);
        assert_eq!(spans[0].style.fg, Some(theme.dim));
        assert_eq!(spans[1].style.fg, Some(theme.error_fg));
    }

    #[test]
    fn tree_file_row_glyph_is_colored_by_status_and_shows_the_basename() {
        let fixture = standard_fixture();
        let mut loaded = LoadedConfig::default();
        loaded.config.ui.status_file_layout = crate::config::FileLayout::Tree;
        let app = App::new(fixture.review(), loaded);
        // the unstaged section holds a modified file in the standard fixture
        let file = app.section_files(Section::Unstaged).first().expect("file");
        let spans = super::file_spans(&app, Some(file), &app.theme, 1, &[]);
        let glyph = spans
            .iter()
            .find(|s| s.content.trim() == file.status.glyph().to_string())
            .expect("status glyph span");
        assert_eq!(glyph.style.fg, Some(status_color(&app.theme, file.status)));
        // the tree shows the basename, not the full path
        assert!(
            spans.iter().any(|s| s.content == "lib.rs"),
            "basename present: {spans:?}"
        );
        assert!(
            spans.iter().all(|s| s.content != file.path),
            "full path dropped: {spans:?}"
        );
    }

    #[test]
    fn flat_list_file_row_shows_the_full_repo_relative_path() {
        let fixture = standard_fixture();
        // default layout is the flat magit list
        let app = app_for(&fixture);
        let file = app.section_files(Section::Unstaged).first().expect("file");
        let spans = super::file_spans(&app, Some(file), &app.theme, 0, &[]);
        // the whole path shows, not just the basename
        assert!(
            spans.iter().any(|s| s.content == file.path),
            "full path present: {spans:?}"
        );
        assert!(
            spans.iter().all(|s| s.content != "lib.rs"),
            "basename alone not shown: {spans:?}"
        );
    }

    #[test]
    fn a_branch_row_without_matching_runs_renders_no_glyph_but_stays_aligned() {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        let no_run = super::branch_spans(&app, 0, &app.theme, 80, &[]);
        assert_eq!(no_run[0].content, "   ");

        app.runs = vec![crate::ci::CiRun {
            id: crate::ci::RunId("1".into()),
            name: "CI".into(),
            title: String::new(),
            branch: "main".into(),
            commit: "abc".into(),
            author: String::new(),
            created: None,
            status: crate::ci::JobStatus::Failed,
            url: None,
            remote: None,
        }];
        let with_run = super::branch_spans(&app, 0, &app.theme, 80, &[]);
        assert_eq!(with_run[0].content, " × ");
        assert_eq!(
            with_run[0].content.chars().count(),
            no_run[0].content.chars().count(),
            "the glyph slot keeps the same width whether or not a run matched"
        );
    }
}
