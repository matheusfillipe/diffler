//! Which pull requests the PR list asks the forge for.

use serde::{Deserialize, Serialize};

use crate::ci::provider::ProviderKind;

/// One preset at a time; the toggles of a [`PrFilter`] narrow it further.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrPreset {
    #[default]
    All,
    /// Open PRs that request my review.
    Review,
    /// Open PRs I authored.
    Mine,
    Involves,
}

impl PrPreset {
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "all" => Some(Self::All),
            "review" => Some(Self::Review),
            "mine" => Some(Self::Mine),
            "involves" => Some(Self::Involves),
            _ => None,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Review => "review",
            Self::Mine => "mine",
            Self::Involves => "involves",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PrFilter {
    pub preset: PrPreset,
    pub hide_drafts: bool,
    pub passing_checks: bool,
    pub label: Option<String>,
    pub author: Option<String>,
    /// A raw GitHub search that replaces everything above.
    pub search: Option<String>,
}

/// What the filter popup offers, so each forge can decline what it cannot do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrFilterOption {
    Review,
    Mine,
    Involves,
    HideDrafts,
    PassingChecks,
    Label,
    Author,
    Search,
}

impl ProviderKind {
    #[must_use]
    pub fn filters_by(self, option: PrFilterOption) -> bool {
        match self {
            ProviderKind::GitHub => true,
            ProviderKind::GitLab | ProviderKind::Forgejo => !matches!(
                option,
                PrFilterOption::Involves | PrFilterOption::PassingChecks | PrFilterOption::Search
            ),
        }
    }
}

impl PrFilter {
    #[must_use]
    pub fn from_preset(preset: PrPreset) -> Self {
        Self {
            preset,
            ..Self::default()
        }
    }

    /// The first part of this filter `kind` cannot apply, in words, so a
    /// provider refuses it out loud.
    #[must_use]
    pub fn declined_by(&self, kind: ProviderKind) -> Option<&'static str> {
        [
            (
                self.preset == PrPreset::Review,
                PrFilterOption::Review,
                "filtering by review request",
            ),
            (
                self.preset == PrPreset::Mine,
                PrFilterOption::Mine,
                "filtering by authorship",
            ),
            (
                self.preset == PrPreset::Involves,
                PrFilterOption::Involves,
                "filtering by involvement",
            ),
            (
                self.hide_drafts,
                PrFilterOption::HideDrafts,
                "filtering out drafts",
            ),
            (
                self.passing_checks,
                PrFilterOption::PassingChecks,
                "filtering by check status",
            ),
            (
                self.label.is_some(),
                PrFilterOption::Label,
                "filtering by label",
            ),
            (
                self.author.is_some(),
                PrFilterOption::Author,
                "filtering by author",
            ),
            (
                self.search.is_some(),
                PrFilterOption::Search,
                "a raw pull request search",
            ),
        ]
        .into_iter()
        .find(|&(set, option, _)| set && !kind.filters_by(option))
        .map(|(_, _, what)| what)
    }

    /// Whether this is today's plain "every open PR" list.
    #[must_use]
    pub fn is_all(&self) -> bool {
        *self == Self::default()
    }

    /// The active parts in words, e.g. `["to review", "no drafts"]`.
    #[must_use]
    pub fn describe(&self) -> Vec<String> {
        if let Some(search) = &self.search {
            return vec![format!("search: {search}")];
        }
        let mut parts = Vec::new();
        match self.preset {
            PrPreset::All => {}
            PrPreset::Review => parts.push("to review".to_owned()),
            PrPreset::Mine => parts.push("mine".to_owned()),
            PrPreset::Involves => parts.push("involves me".to_owned()),
        }
        if self.hide_drafts {
            parts.push("no drafts".to_owned());
        }
        if self.passing_checks {
            parts.push("checks passing".to_owned());
        }
        if let Some(label) = &self.label {
            parts.push(format!("label: {label}"));
        }
        if let Some(author) = &self.author {
            parts.push(format!("author: {author}"));
        }
        parts
    }

    /// The GitHub search for this filter within `owner/name`.
    #[must_use]
    pub fn github_search(&self, slug: &str) -> String {
        if let Some(search) = &self.search {
            return format!("repo:{slug} is:pr {search}");
        }
        let mut query = format!("repo:{slug} is:pr is:open sort:created-desc");
        match self.preset {
            PrPreset::All => {}
            PrPreset::Review => query.push_str(" review-requested:@me"),
            PrPreset::Mine => query.push_str(" author:@me"),
            PrPreset::Involves => query.push_str(" involves:@me"),
        }
        if self.hide_drafts {
            query.push_str(" draft:false");
        }
        if self.passing_checks {
            query.push_str(" status:success");
        }
        if let Some(label) = &self.label {
            query.push_str(" label:");
            query.push_str(&quoted(label));
        }
        if let Some(author) = &self.author {
            query.push_str(" author:");
            query.push_str(&quoted(author));
        }
        query
    }
}

/// GitHub search reads a quoted value as one term, so a label with a space stays whole.
fn quoted(value: &str) -> String {
    if value.contains(char::is_whitespace) {
        format!("\"{}\"", value.replace('"', ""))
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_every_open_pr() {
        assert!(PrFilter::default().is_all());
        assert!(!PrFilter::from_preset(PrPreset::Mine).is_all());
        assert_eq!(
            PrFilter::default().github_search("acme/widgets"),
            "repo:acme/widgets is:pr is:open sort:created-desc"
        );
    }

    #[test]
    fn the_search_combines_the_preset_with_every_toggle() {
        let filter = PrFilter {
            preset: PrPreset::Review,
            hide_drafts: true,
            passing_checks: true,
            label: Some("good first issue".to_owned()),
            author: Some("reviewer".to_owned()),
            search: None,
        };
        assert_eq!(
            filter.github_search("acme/widgets"),
            "repo:acme/widgets is:pr is:open sort:created-desc review-requested:@me \
             draft:false status:success label:\"good first issue\" author:reviewer"
        );
        assert_eq!(
            filter.describe(),
            [
                "to review",
                "no drafts",
                "checks passing",
                "label: good first issue",
                "author: reviewer"
            ]
        );
    }

    #[test]
    fn each_preset_names_its_qualifier() {
        for (preset, qualifier) in [
            (PrPreset::Mine, "author:@me"),
            (PrPreset::Involves, "involves:@me"),
        ] {
            let search = PrFilter::from_preset(preset).github_search("acme/widgets");
            assert!(search.ends_with(qualifier), "{search}");
        }
    }

    #[test]
    fn a_raw_search_replaces_the_structured_filter() {
        let filter = PrFilter {
            preset: PrPreset::Mine,
            hide_drafts: true,
            search: Some("is:closed label:bug".to_owned()),
            ..PrFilter::default()
        };
        assert_eq!(
            filter.github_search("acme/widgets"),
            "repo:acme/widgets is:pr is:closed label:bug"
        );
        assert_eq!(filter.describe(), ["search: is:closed label:bug"]);
    }

    #[test]
    fn presets_round_trip_through_their_config_names() {
        for preset in [
            PrPreset::All,
            PrPreset::Review,
            PrPreset::Mine,
            PrPreset::Involves,
        ] {
            assert_eq!(PrPreset::parse(preset.name()), Some(preset));
        }
        assert_eq!(PrPreset::parse("everything"), None);
    }

    #[test]
    fn a_filter_names_the_first_part_a_forge_declines() {
        let filter = PrFilter {
            preset: PrPreset::Involves,
            label: Some("bug".to_owned()),
            ..PrFilter::default()
        };
        assert_eq!(filter.declined_by(ProviderKind::GitHub), None);
        assert_eq!(
            filter.declined_by(ProviderKind::GitLab),
            Some("filtering by involvement")
        );
    }

    #[test]
    fn gitlab_and_forgejo_decline_what_they_cannot_filter() {
        for kind in [ProviderKind::GitLab, ProviderKind::Forgejo] {
            assert!(kind.filters_by(PrFilterOption::Review));
            assert!(kind.filters_by(PrFilterOption::Label));
            assert!(!kind.filters_by(PrFilterOption::Involves));
            assert!(!kind.filters_by(PrFilterOption::PassingChecks));
            assert!(!kind.filters_by(PrFilterOption::Search));
        }
    }
}
