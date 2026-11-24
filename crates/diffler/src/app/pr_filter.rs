//! The PR list's filter menu: presets, toggles and the prompts behind them.

use crate::ci::{PrFilter, PrFilterOption, PrPreset, ProviderKind, PullRequest};
use crate::keymap::Action;
use crate::transient::TransientMarks;

use super::{App, CiRequest, InputOp};

const MENU: [(Action, PrFilterOption); 8] = [
    (Action::PrFilterReview, PrFilterOption::Review),
    (Action::PrFilterMine, PrFilterOption::Mine),
    (Action::PrFilterInvolves, PrFilterOption::Involves),
    (Action::PrFilterNoDrafts, PrFilterOption::HideDrafts),
    (Action::PrFilterPassing, PrFilterOption::PassingChecks),
    (Action::PrFilterLabel, PrFilterOption::Label),
    (Action::PrFilterAuthor, PrFilterOption::Author),
    (Action::PrFilterSearch, PrFilterOption::Search),
];

pub(crate) fn is_pr_filter_action(action: Action) -> bool {
    action == Action::PrFilterAll || MENU.iter().any(|(entry, _)| *entry == action)
}

/// `[prs] default_filter` narrowed to what `kind` can apply, with a warning
/// when we had to fall back to every open PR.
pub(crate) fn startup_filter(
    preset: PrPreset,
    kind: Option<ProviderKind>,
) -> (PrFilter, Option<String>) {
    let filter = PrFilter::from_preset(preset);
    match kind.and_then(|kind| filter.declined_by(kind).map(|what| (kind, what))) {
        Some((kind, what)) => (
            PrFilter::default(),
            Some(format!(
                "{kind} does not support {what}, so the PR list starts with every open PR"
            )),
        ),
        None => (filter, None),
    }
}

impl App {
    fn pr_forge(&self) -> Option<ProviderKind> {
        self.ci_remotes.first().map(|remote| remote.detected.kind)
    }

    /// The rows the PR list shows: every open PR, or the active filter's own.
    pub(crate) fn listed_prs(&self) -> &[PullRequest] {
        if self.pr_filter.is_all() {
            &self.prs
        } else {
            self.filtered_prs.as_deref().unwrap_or_default()
        }
    }

    /// Whether the forge answered for the rows the PR list shows.
    pub(crate) fn listed_prs_loaded(&self) -> bool {
        if self.pr_filter.is_all() {
            self.status.prs_loaded
        } else {
            self.filtered_prs.is_some()
        }
    }

    pub(crate) fn pr_list_request(&self) -> CiRequest {
        CiRequest::Prs(self.pr_filter.clone())
    }

    pub(crate) fn on_filtered_prs(&mut self, listed: Vec<crate::ci::ListedPr>) {
        let mut rows = Vec::with_capacity(listed.len());
        for crate::ci::ListedPr { pr, summary } in listed {
            self.pr_summaries.insert(pr.number, summary);
            rows.push(pr);
        }
        self.prs_cursor = self.prs_cursor.min(rows.len().saturating_sub(1));
        self.filtered_prs = Some(rows);
    }

    /// The options the menu greys: what the forge cannot do, and the toggles
    /// a raw search replaces.
    fn declines(&self, option: PrFilterOption) -> Option<String> {
        if let Some(kind) = self.pr_forge()
            && !crate::ci::capabilities_for(kind).filters_by(option)
        {
            return Some(format!(
                "{kind} cannot filter this way, pick another filter"
            ));
        }
        (option.is_toggle() && self.pr_filter.search.is_some())
            .then(|| "clear the search to use this filter".to_owned())
    }

    pub(crate) fn pr_filter_marks(&self) -> TransientMarks {
        let filter = &self.pr_filter;
        let all = (filter.preset == PrPreset::All).then_some(Action::PrFilterAll);
        let on = all
            .into_iter()
            .chain(
                MENU.iter()
                    .filter(|(_, option)| filter.is_set(*option))
                    .map(|(action, _)| *action),
            )
            .collect();
        let unavailable = MENU
            .iter()
            .filter(|(_, option)| self.declines(*option).is_some())
            .map(|(action, _)| *action)
            .collect();
        TransientMarks { on, unavailable }
    }

    pub(crate) fn apply_pr_filter_action(&mut self, action: Action) {
        let option = MENU
            .iter()
            .find(|(entry, _)| *entry == action)
            .map(|(_, option)| *option);
        if let Some(reason) = option.and_then(|option| self.declines(option)) {
            self.info(reason);
            return;
        }
        let mut filter = self.pr_filter.clone();
        let preset = |filter: &mut PrFilter, preset| {
            filter.preset = preset;
            filter.search = None;
        };
        match action {
            Action::PrFilterAll => preset(&mut filter, PrPreset::All),
            Action::PrFilterReview => preset(&mut filter, PrPreset::Review),
            Action::PrFilterMine => preset(&mut filter, PrPreset::Mine),
            Action::PrFilterInvolves => preset(&mut filter, PrPreset::Involves),
            Action::PrFilterNoDrafts => filter.hide_drafts = !filter.hide_drafts,
            Action::PrFilterPassing => filter.passing_checks = !filter.passing_checks,
            Action::PrFilterLabel if filter.label.is_some() => filter.label = None,
            Action::PrFilterAuthor if filter.author.is_some() => filter.author = None,
            Action::PrFilterLabel => return self.prompt_pr_filter(PrFilterOption::Label),
            Action::PrFilterAuthor => return self.prompt_pr_filter(PrFilterOption::Author),
            Action::PrFilterSearch => return self.prompt_pr_filter(PrFilterOption::Search),
            _ => return,
        }
        self.set_pr_filter(filter);
    }

    fn prompt_pr_filter(&mut self, field: PrFilterOption) {
        let (title, start) = match field {
            PrFilterOption::Label => ("Show pull requests with label", None),
            PrFilterOption::Author => ("Show pull requests by author", None),
            _ => (
                "Search pull requests (GitHub search syntax)",
                self.pr_filter.search.clone(),
            ),
        };
        self.open_input(
            title.to_owned(),
            start.unwrap_or_default(),
            InputOp::PrFilter(field),
        );
    }

    /// An empty value clears the field.
    pub(crate) fn submit_pr_filter(&mut self, field: PrFilterOption, value: &str) {
        let mut filter = self.pr_filter.clone();
        let value = (!value.is_empty()).then(|| value.to_owned());
        match field {
            PrFilterOption::Label => filter.label = value,
            PrFilterOption::Author => filter.author = value,
            PrFilterOption::Search => filter.search = value,
            _ => return,
        }
        self.set_pr_filter(filter);
    }

    fn set_pr_filter(&mut self, filter: PrFilter) {
        if filter == self.pr_filter {
            return;
        }
        self.pr_filter = filter;
        self.filtered_prs = None;
        self.prs_cursor = 0;
        self.prs_scroll = 0;
        self.pending_ci = Some(self.pr_list_request());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LoadedConfig;
    use crate::event::AppEvent;
    use crate::test_support::standard_fixture;

    fn app() -> App {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.push_screen(super::super::Screen::Prs);
        app
    }

    fn on(app: &mut App, kind: ProviderKind) {
        app.ci_remotes = vec![super::super::CiRemote {
            name: "origin".to_owned(),
            detected: crate::ci::Detected { kind, host: None },
            url: None,
        }];
    }

    fn listed(number: u64) -> crate::ci::ListedPr {
        crate::ci::ListedPr {
            pr: PullRequest {
                number,
                title: format!("PR {number}"),
                url: None,
                base_ref: "main".to_owned(),
                head_ref: format!("feat/{number}"),
                head_oid: "0".repeat(40),
                author: "reviewer".to_owned(),
            },
            summary: crate::ci::PrSummary::default(),
        }
    }

    fn requested(app: &App) -> Option<PrFilter> {
        match &app.pending_ci {
            Some(CiRequest::Prs(filter)) => Some(filter.clone()),
            _ => None,
        }
    }

    #[test]
    fn a_preset_replaces_the_last_and_refetches() {
        let mut app = app();
        app.apply_pr_filter_action(Action::PrFilterReview);
        assert_eq!(app.pr_filter.preset, PrPreset::Review);
        assert_eq!(
            requested(&app),
            Some(PrFilter::from_preset(PrPreset::Review))
        );
        app.pending_ci = None;
        app.apply_pr_filter_action(Action::PrFilterMine);
        assert_eq!(requested(&app), Some(PrFilter::from_preset(PrPreset::Mine)));
    }

    #[test]
    fn a_toggle_pressed_twice_turns_off() {
        let mut app = app();
        app.apply_pr_filter_action(Action::PrFilterNoDrafts);
        assert!(app.pr_filter.hide_drafts);
        app.apply_pr_filter_action(Action::PrFilterNoDrafts);
        assert!(app.pr_filter.is_all());
    }

    #[test]
    fn a_label_prompts_then_a_second_press_clears_it() {
        let mut app = app();
        app.apply_pr_filter_action(Action::PrFilterLabel);
        assert!(app.modal.is_some(), "the label prompt opens");
        app.modal = None;
        app.submit_pr_filter(PrFilterOption::Label, "backend");
        assert_eq!(app.pr_filter.label.as_deref(), Some("backend"));
        app.apply_pr_filter_action(Action::PrFilterLabel);
        assert_eq!(app.pr_filter.label, None);
        assert!(app.modal.is_none(), "clearing asks nothing");
    }

    #[test]
    fn an_empty_search_clears_it() {
        let mut app = app();
        app.submit_pr_filter(PrFilterOption::Search, "is:closed");
        app.apply_pr_filter_action(Action::PrFilterSearch);
        let Some(super::super::Modal::Input { buffer, .. }) = app.modal.as_mut() else {
            panic!("the search prompt opens");
        };
        buffer.clear();
        app.submit_input();
        assert!(app.pr_filter.is_all());
    }

    #[test]
    fn a_preset_drops_a_raw_search() {
        let mut app = app();
        app.submit_pr_filter(PrFilterOption::Search, "is:closed");
        app.apply_pr_filter_action(Action::PrFilterMine);
        assert_eq!(app.pr_filter, PrFilter::from_preset(PrPreset::Mine));
    }

    #[test]
    fn a_raw_search_greys_and_declines_the_toggles() {
        let mut app = app();
        app.submit_pr_filter(PrFilterOption::Search, "is:closed");
        let marks = app.pr_filter_marks();
        assert!(marks.unavailable.contains(&Action::PrFilterNoDrafts));
        assert!(!marks.unavailable.contains(&Action::PrFilterMine));
        app.pending_ci = None;
        app.apply_pr_filter_action(Action::PrFilterNoDrafts);
        assert!(!app.pr_filter.hide_drafts);
        assert!(app.pending_ci.is_none());
    }

    #[test]
    fn a_forge_that_cannot_filter_greys_the_entry_and_declines_it() {
        let mut app = app();
        on(&mut app, ProviderKind::GitLab);
        assert!(
            app.pr_filter_marks()
                .unavailable
                .contains(&Action::PrFilterPassing)
        );
        app.pending_ci = None;
        app.apply_pr_filter_action(Action::PrFilterPassing);
        assert!(app.pr_filter.is_all());
        assert!(app.pending_ci.is_none());
        assert!(
            app.message
                .as_ref()
                .is_some_and(|m| m.text.contains("GitLab cannot"))
        );
    }

    #[test]
    fn a_default_filter_the_forge_cannot_do_falls_back_to_every_open_pr() {
        let (filter, warning) = startup_filter(PrPreset::Involves, Some(ProviderKind::GitLab));
        assert!(filter.is_all());
        assert!(warning.is_some_and(|w| w.contains("GitLab")));
        let (filter, warning) = startup_filter(PrPreset::Involves, Some(ProviderKind::GitHub));
        assert_eq!(filter, PrFilter::from_preset(PrPreset::Involves));
        assert!(warning.is_none());
    }

    #[test]
    fn marks_name_the_active_preset_and_toggles() {
        let mut app = app();
        app.apply_pr_filter_action(Action::PrFilterReview);
        app.apply_pr_filter_action(Action::PrFilterNoDrafts);
        assert_eq!(
            app.pr_filter_marks().on,
            [Action::PrFilterReview, Action::PrFilterNoDrafts]
        );
    }

    #[test]
    fn filtered_rows_stay_out_of_the_open_pr_list() {
        let mut app = app();
        app.handle(AppEvent::CiPrs(PrFilter::default(), vec![listed(7)]));
        app.submit_pr_filter(PrFilterOption::Search, "is:closed");
        let search = app.pr_filter.clone();
        app.handle(AppEvent::CiPrs(search, vec![listed(3)]));
        assert_eq!(app.listed_prs()[0].number, 3);
        assert_eq!(app.prs.len(), 1);
        assert_eq!(app.prs[0].number, 7);
        app.handle(AppEvent::CiPrs(
            PrFilter::from_preset(PrPreset::Mine),
            vec![listed(4)],
        ));
        assert_eq!(
            app.listed_prs()[0].number,
            3,
            "a stale filter's reply drops"
        );
    }

    #[test]
    fn the_open_pr_group_and_focus_see_every_open_pr_while_a_filter_is_on() {
        let mut app = app();
        app.submit_pr_filter(PrFilterOption::Search, "is:closed");
        app.handle(AppEvent::CiPrs(
            PrFilter::default(),
            vec![listed(7), listed(8)],
        ));
        assert!(app.listed_prs().is_empty());
        assert_eq!(app.other_prs().len(), 2);
        assert!(app.known_pr(8).is_some());
    }
}
