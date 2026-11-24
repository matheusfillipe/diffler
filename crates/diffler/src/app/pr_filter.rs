//! The PR list's filter menu: presets, toggles and the prompts behind them.

use crate::ci::{PrFilter, PrFilterOption, PrPreset, ProviderKind};
use crate::keymap::Action;
use crate::transient::TransientMarks;

use super::{App, CiRequest, InputOp};

/// The prompt a filter menu entry opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrFilterField {
    Label,
    Author,
    Search,
}

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

impl App {
    fn pr_forge(&self) -> Option<ProviderKind> {
        self.ci_remotes.first().map(|remote| remote.detected.kind)
    }

    pub(crate) fn pr_filter_marks(&self) -> TransientMarks {
        let filter = &self.pr_filter;
        let on = [
            (Action::PrFilterAll, filter.preset == PrPreset::All),
            (Action::PrFilterReview, filter.preset == PrPreset::Review),
            (Action::PrFilterMine, filter.preset == PrPreset::Mine),
            (
                Action::PrFilterInvolves,
                filter.preset == PrPreset::Involves,
            ),
            (Action::PrFilterNoDrafts, filter.hide_drafts),
            (Action::PrFilterPassing, filter.passing_checks),
            (Action::PrFilterLabel, filter.label.is_some()),
            (Action::PrFilterAuthor, filter.author.is_some()),
            (Action::PrFilterSearch, filter.search.is_some()),
        ]
        .into_iter()
        .filter(|&(_, set)| set)
        .map(|(action, _)| action)
        .collect();
        let unavailable = self
            .pr_forge()
            .map(|kind| {
                MENU.iter()
                    .filter(|(_, option)| !kind.filters_by(*option))
                    .map(|(action, _)| *action)
                    .collect()
            })
            .unwrap_or_default();
        TransientMarks { on, unavailable }
    }

    pub(crate) fn apply_pr_filter_action(&mut self, action: Action) {
        if let Some(kind) = self.pr_forge()
            && let Some((_, option)) = MENU.iter().find(|(entry, _)| *entry == action)
            && !kind.filters_by(*option)
        {
            self.info(format!("{kind} cannot filter pull requests this way"));
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
            Action::PrFilterLabel => return self.prompt_pr_filter(PrFilterField::Label),
            Action::PrFilterAuthor => return self.prompt_pr_filter(PrFilterField::Author),
            Action::PrFilterSearch => return self.prompt_pr_filter(PrFilterField::Search),
            _ => return,
        }
        self.set_pr_filter(filter);
    }

    fn prompt_pr_filter(&mut self, field: PrFilterField) {
        let (title, start) = match field {
            PrFilterField::Label => ("Show pull requests with label", String::new()),
            PrFilterField::Author => ("Show pull requests by author", String::new()),
            PrFilterField::Search => (
                "Search pull requests (GitHub search syntax)",
                self.pr_filter.search.clone().unwrap_or_default(),
            ),
        };
        self.open_input(title.to_owned(), start, InputOp::PrFilter(field));
    }

    pub(crate) fn submit_pr_filter(&mut self, field: PrFilterField, value: &str) {
        let mut filter = self.pr_filter.clone();
        let value = Some(value.to_owned());
        match field {
            PrFilterField::Label => filter.label = value,
            PrFilterField::Author => filter.author = value,
            PrFilterField::Search => filter.search = value,
        }
        self.set_pr_filter(filter);
    }

    fn set_pr_filter(&mut self, filter: PrFilter) {
        if filter == self.pr_filter {
            return;
        }
        self.pr_filter = filter;
        self.prs.clear();
        self.pr_summaries.clear();
        self.prs_cursor = 0;
        self.prs_scroll = 0;
        self.status.prs_loaded = false;
        self.pending_ci = Some(CiRequest::Prs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LoadedConfig;
    use crate::test_support::standard_fixture;

    fn app() -> App {
        let fixture = standard_fixture();
        let mut app = App::new(fixture.review(), LoadedConfig::default());
        app.push_screen(super::super::Screen::Prs);
        app
    }

    #[test]
    fn a_preset_replaces_the_last_and_refetches() {
        let mut app = app();
        app.apply_pr_filter_action(Action::PrFilterReview);
        assert_eq!(app.pr_filter.preset, PrPreset::Review);
        assert!(matches!(app.pending_ci, Some(CiRequest::Prs)));
        app.pending_ci = None;
        app.apply_pr_filter_action(Action::PrFilterMine);
        assert_eq!(app.pr_filter.preset, PrPreset::Mine);
        assert!(matches!(app.pending_ci, Some(CiRequest::Prs)));
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
        app.submit_pr_filter(PrFilterField::Label, "backend");
        assert_eq!(app.pr_filter.label.as_deref(), Some("backend"));
        app.apply_pr_filter_action(Action::PrFilterLabel);
        assert_eq!(app.pr_filter.label, None);
        assert!(app.modal.is_none(), "clearing asks nothing");
    }

    #[test]
    fn a_preset_drops_a_raw_search() {
        let mut app = app();
        app.submit_pr_filter(PrFilterField::Search, "is:closed");
        app.apply_pr_filter_action(Action::PrFilterMine);
        assert_eq!(app.pr_filter, PrFilter::from_preset(PrPreset::Mine));
    }

    #[test]
    fn a_forge_that_cannot_filter_greys_the_entry_and_declines_it() {
        let mut app = app();
        app.ci_remotes = vec![super::super::CiRemote {
            name: "origin".to_owned(),
            detected: crate::ci::Detected {
                kind: ProviderKind::GitLab,
                host: None,
            },
            url: None,
        }];
        assert!(
            app.pr_filter_marks()
                .unavailable
                .contains(&Action::PrFilterPassing)
        );
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
    fn marks_name_the_active_preset_and_toggles() {
        let mut app = app();
        app.apply_pr_filter_action(Action::PrFilterReview);
        app.apply_pr_filter_action(Action::PrFilterNoDrafts);
        assert_eq!(
            app.pr_filter_marks().on,
            [Action::PrFilterReview, Action::PrFilterNoDrafts]
        );
    }
}
