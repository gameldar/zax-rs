//! Resolving a slug, a path, or a pull request to the main checkout zax
//! should treat as the base of a worktree. Every observation of the
//! filesystem comes through a `CheckoutInspector`; the verdict on what was
//! observed is drawn by the domain (`verify_checkout`, `accept_checkout`),
//! not here.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use thiserror::Error;
use zax_domain::{
    accept_checkout, verify_checkout, CheckoutProbe, MainCheckout, NoCheckout, PrRef, Projects,
    Rejection, RepoChoices, RepoSlug, RepoTarget,
};

use crate::ports::{CheckoutInspector, PortError};

pub struct ResolveCheckout<'a, I> {
    inspector: &'a I,
    projects: Option<&'a Projects>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResolveCheckoutError {
    #[error(transparent)]
    NoCheckout(NoCheckout),
    #[error("{path} is not a main checkout: {why}")]
    NotMain { path: PathBuf, why: Rejection },
    #[error("this is not inside a git repository; pass --repo <owner/name or path>")]
    NoRepository,
    #[error("outside a repository a pull request number cannot be resolved; give its URL")]
    NeedsUrl,
    #[error(transparent)]
    Port(#[from] PortError),
}

impl<'a, I> ResolveCheckout<'a, I>
where
    I: CheckoutInspector,
{
    pub fn new(inspector: &'a I, projects: Option<&'a Projects>) -> Self {
        Self {
            inspector,
            projects,
        }
    }

    /// (1) the cwd repo's main worktree, when there is one - a match wins
    /// outright, before the candidate is even looked at; (2) `projects`'
    /// candidate for `slug`, when `projects` is configured; (3) neither
    /// worked, so every miss is reported together.
    pub fn by_slug(
        &self,
        slug: &RepoSlug,
        here: Option<&Path>,
    ) -> Result<MainCheckout, ResolveCheckoutError> {
        let mut tried = Vec::new();

        if let Some(here) = here {
            let probe = self.inspector.inspect(here)?;
            match verify_checkout(probe, slug) {
                Ok(checkout) => return Ok(checkout),
                Err(rejection) => tried.push((here.to_owned(), rejection)),
            }
        }

        if let Some(projects) = self.projects {
            let candidate = projects.candidate(slug);
            let probe = self.inspector.inspect(&candidate)?;
            match verify_checkout(probe, slug) {
                Ok(checkout) => return Ok(checkout),
                Err(rejection) => tried.push((candidate, rejection)),
            }
        }

        Err(ResolveCheckoutError::NoCheckout(NoCheckout {
            slug: slug.clone(),
            tried,
            projects_off: self.projects.is_none(),
        }))
    }

    /// `path` was named directly, so there is no slug to compare it against -
    /// it only has to be a main checkout at all.
    pub fn by_path(&self, path: &Path) -> Result<MainCheckout, ResolveCheckoutError> {
        let probe = self.inspector.inspect(path)?;
        accept_checkout(probe).map_err(|why| ResolveCheckoutError::NotMain {
            path: path.to_owned(),
            why,
        })
    }

    pub fn target(
        &self,
        target: &RepoTarget,
        here: Option<&Path>,
    ) -> Result<MainCheckout, ResolveCheckoutError> {
        match target {
            RepoTarget::Slug(slug) => self.by_slug(slug, here),
            RepoTarget::Path(path) => self.by_path(path),
        }
    }

    /// `explicit` always wins. Otherwise, with `projects` configured and the
    /// pull request's URL naming a repository, that repository is resolved
    /// through `by_slug`. Short of that, a cwd repository is today's single-
    /// repository behaviour and is left untouched by returning `None`; with
    /// no cwd repository either there is nothing to resolve against, so a
    /// bare number is refused rather than guessed at.
    pub fn for_review(
        &self,
        pr: &PrRef,
        explicit: Option<&RepoTarget>,
        here: Option<&Path>,
    ) -> Result<Option<MainCheckout>, ResolveCheckoutError> {
        if let Some(target) = explicit {
            return self.target(target, here).map(Some);
        }
        if self.projects.is_some() {
            if let Some(slug) = pr.repository() {
                return self.by_slug(&slug, here).map(Some);
            }
        }
        if here.is_some() {
            return Ok(None);
        }
        Err(ResolveCheckoutError::NeedsUrl)
    }

    /// `explicit` always wins; otherwise a cwd repository is today's
    /// behaviour (`None`), and with neither there is no repository to put the
    /// new worktree in.
    pub fn for_new(
        &self,
        explicit: Option<&RepoTarget>,
        here: Option<&Path>,
    ) -> Result<Option<MainCheckout>, ResolveCheckoutError> {
        if let Some(target) = explicit {
            return self.target(target, here).map(Some);
        }
        if here.is_some() {
            return Ok(None);
        }
        Err(ResolveCheckoutError::NoRepository)
    }

    /// Every main checkout found under `projects.root` plus every override,
    /// deduped and sorted. Dedupe and the preselected index both compare the
    /// toplevel the inspector reports, never the path a candidate was found
    /// at, so a symlinked root or an override that happens to repeat the
    /// convention still collapses to one entry. A candidate that is not a
    /// main checkout is dropped silently; only a `PortError` from the port
    /// propagates.
    pub fn list(&self, here: Option<&Path>) -> Result<RepoChoices, PortError> {
        let Some(projects) = self.projects else {
            return Ok(RepoChoices {
                checkouts: Vec::new(),
                preselected: None,
            });
        };

        let mut candidates = self.inspector.subdirectories(projects.root.as_path())?;
        candidates.extend(projects.paths.iter().map(|(_, path)| path.to_owned()));

        let mut seen_tops: HashSet<PathBuf> = HashSet::new();
        let mut checkouts: Vec<MainCheckout> = Vec::new();
        for candidate in candidates {
            let probe = self.inspector.inspect(&candidate)?;
            let Ok(checkout) = accept_checkout(probe) else {
                continue;
            };
            if seen_tops.insert(checkout.path().to_owned()) {
                checkouts.push(checkout);
            }
        }

        checkouts.sort_by(|a, b| a.name().cmp(&b.name()).then_with(|| a.path().cmp(b.path())));

        let preselected = here
            .map(|here| self.inspector.inspect(here))
            .transpose()?
            .and_then(|probe| match probe {
                CheckoutProbe::Main { top, .. } => Some(top),
                _ => None,
            })
            .and_then(|top| checkouts.iter().position(|checkout| checkout.path() == top));

        Ok(RepoChoices {
            checkouts,
            preselected,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::{Path, PathBuf};

    use zax_domain::{PrRef, ProjectPaths, Projects, ProjectsRoot, RepoSlug};

    use super::*;
    use crate::testing::FakeCheckouts;

    fn projects_root() -> Projects {
        Projects {
            root: ProjectsRoot::new(PathBuf::from("/h/src")).unwrap(),
            paths: ProjectPaths::default(),
        }
    }

    fn projects_with_override(slug: &str, path: &str) -> Projects {
        let slug = RepoSlug::parse(slug).unwrap();
        Projects {
            root: ProjectsRoot::new(PathBuf::from("/h/src")).unwrap(),
            paths: ProjectPaths::new(vec![(slug, PathBuf::from(path))]).unwrap(),
        }
    }

    #[test]
    fn the_current_checkout_wins_when_its_origin_matches() {
        let inspector = FakeCheckouts::default().with_main(
            "/h/work/omega",
            "/h/work/omega",
            Some("git@github.com:acme/omega.git"),
        );
        let projects = projects_root();
        let slug = RepoSlug::parse("acme/omega").unwrap();

        let checkout = ResolveCheckout::new(&inspector, Some(&projects))
            .by_slug(&slug, Some(Path::new("/h/work/omega")))
            .unwrap();

        assert_eq!(checkout.path(), Path::new("/h/work/omega"));
        assert!(
            !inspector
                .inspected
                .borrow()
                .contains(&PathBuf::from("/h/src/omega")),
            "the candidate must not be inspected once the current checkout verifies"
        );
    }

    #[test]
    fn an_override_is_used_before_the_convention() {
        let slug = RepoSlug::parse("gameldar/zed-ai-extensions").unwrap();
        let projects = projects_with_override(
            "gameldar/zed-ai-extensions",
            "/h/src/tools/zed-ai-extensions",
        );
        let inspector = FakeCheckouts::default().with_main(
            "/h/src/tools/zed-ai-extensions",
            "/h/src/tools/zed-ai-extensions",
            Some("git@github.com:gameldar/zed-ai-extensions.git"),
        );

        let checkout = ResolveCheckout::new(&inspector, Some(&projects))
            .by_slug(&slug, None)
            .unwrap();

        assert_eq!(checkout.path(), Path::new("/h/src/tools/zed-ai-extensions"));
        assert!(!inspector
            .inspected
            .borrow()
            .contains(&PathBuf::from("/h/src/zed-ai-extensions")));
    }

    #[test]
    fn the_convention_is_root_slash_name() {
        let slug = RepoSlug::parse("acme/omega").unwrap();
        let projects = projects_root();
        let inspector = FakeCheckouts::default().with_main(
            "/h/src/omega",
            "/h/src/omega",
            Some("https://github.com/acme/omega"),
        );

        let checkout = ResolveCheckout::new(&inspector, Some(&projects))
            .by_slug(&slug, None)
            .unwrap();

        assert_eq!(checkout.path(), Path::new("/h/src/omega"));
    }

    #[test]
    fn a_same_named_checkout_from_another_owner_is_not_a_match() {
        let slug = RepoSlug::parse("acme/omega").unwrap();
        let projects = projects_root();
        let inspector = FakeCheckouts::default().with_main(
            "/h/src/omega",
            "/h/src/omega",
            Some("git@github.com:someone/omega.git"),
        );

        let err = ResolveCheckout::new(&inspector, Some(&projects))
            .by_slug(&slug, None)
            .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("acme/omega"), "got: {message}");
        assert!(message.contains("/h/src/omega"), "got: {message}");
        assert!(
            message.contains("origin is someone/omega"),
            "got: {message}"
        );
    }

    #[test]
    fn a_miss_lists_the_current_checkout_and_the_candidate() {
        let slug = RepoSlug::parse("acme/zed").unwrap();
        let projects = projects_root();
        let inspector = FakeCheckouts::default().with_main(
            "/h/work/zed",
            "/h/work/zed",
            Some("git@github.com:someone/zed.git"),
        );

        let err = ResolveCheckout::new(&inspector, Some(&projects))
            .by_slug(&slug, Some(Path::new("/h/work/zed")))
            .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("/h/work/zed"), "got: {message}");
        assert!(message.contains("/h/src/zed"), "got: {message}");
        assert!(message.contains("origin is someone/zed"), "got: {message}");
        assert!(message.contains("absent"), "got: {message}");
    }

    #[test]
    fn without_projects_only_the_current_checkout_is_considered() {
        let slug = RepoSlug::parse("acme/omega").unwrap();
        let inspector = FakeCheckouts::default().with_main(
            "/h/work/omega",
            "/h/work/omega",
            Some("git@github.com:someone/omega.git"),
        );

        let err = ResolveCheckout::new(&inspector, None)
            .by_slug(&slug, Some(Path::new("/h/work/omega")))
            .unwrap_err();

        assert!(err.to_string().contains("projects.root"));
    }

    #[test]
    fn a_path_target_must_be_a_main_checkout() {
        let inspector = FakeCheckouts::default().with_linked("/h/work/wt");
        let projects = projects_root();

        let err = ResolveCheckout::new(&inspector, Some(&projects))
            .by_path(Path::new("/h/work/wt"))
            .unwrap_err();

        assert!(matches!(err, ResolveCheckoutError::NotMain { .. }));
        assert!(err.to_string().contains("linked worktree"));
    }

    #[test]
    fn review_resolves_a_url_through_projects() {
        let projects = projects_root();
        let inspector = FakeCheckouts::default().with_main(
            "/h/src/omega",
            "/h/src/omega",
            Some("git@github.com:acme/omega.git"),
        );
        let pr = PrRef::parse("https://github.com/acme/omega/pull/7").unwrap();

        let checkout = ResolveCheckout::new(&inspector, Some(&projects))
            .for_review(&pr, None, None)
            .unwrap();

        assert_eq!(checkout.unwrap().path(), Path::new("/h/src/omega"));
    }

    #[test]
    fn review_without_projects_keeps_todays_behaviour() {
        let inspector = FakeCheckouts::default();
        let pr = PrRef::parse("https://github.com/acme/omega/pull/7").unwrap();

        let result = ResolveCheckout::new(&inspector, None)
            .for_review(&pr, None, Some(Path::new("/h/work/omega")))
            .unwrap();

        assert_eq!(result, None);
    }

    #[test]
    fn a_bare_number_outside_a_repository_asks_for_a_url() {
        let projects = projects_root();
        let inspector = FakeCheckouts::default();
        let pr = PrRef::Number(7);

        let err = ResolveCheckout::new(&inspector, Some(&projects))
            .for_review(&pr, None, None)
            .unwrap_err();

        assert!(matches!(err, ResolveCheckoutError::NeedsUrl));
    }

    #[test]
    fn a_bare_number_inside_a_repository_uses_it() {
        let projects = projects_root();
        let inspector = FakeCheckouts::default();
        let pr = PrRef::Number(7);

        let result = ResolveCheckout::new(&inspector, Some(&projects))
            .for_review(&pr, None, Some(Path::new("/h/work/omega")))
            .unwrap();

        assert_eq!(result, None);
    }

    #[test]
    fn new_outside_a_repository_needs_a_repo_flag() {
        let inspector = FakeCheckouts::default();

        let err = ResolveCheckout::new(&inspector, None)
            .for_new(None, None)
            .unwrap_err();

        assert!(matches!(err, ResolveCheckoutError::NoRepository));
    }

    #[test]
    fn the_listing_skips_anything_that_is_not_a_main_checkout() {
        let projects = projects_root();
        let inspector = FakeCheckouts::default()
            .with_children(
                "/h/src",
                &["/h/src/wt", "/h/src/omega", "/h/src/gone", "/h/src/notes"],
            )
            .with_main(
                "/h/src/omega",
                "/h/src/omega",
                Some("git@github.com:acme/omega.git"),
            )
            .with_plain("/h/src/notes")
            .with_linked("/h/src/wt");

        let choices = ResolveCheckout::new(&inspector, Some(&projects))
            .list(None)
            .unwrap();

        assert_eq!(choices.checkouts.len(), 1);
        assert_eq!(choices.checkouts[0].name(), "omega");
    }

    #[test]
    fn the_listing_adds_overrides_dedupes_and_sorts_by_name() {
        let alpha_slug = RepoSlug::parse("acme/alpha").unwrap();
        let zed_slug = RepoSlug::parse("gameldar/zed-ai-extensions").unwrap();
        let projects = Projects {
            root: ProjectsRoot::new(PathBuf::from("/h/src")).unwrap(),
            paths: ProjectPaths::new(vec![
                (alpha_slug, PathBuf::from("/h/src/alpha")),
                (zed_slug, PathBuf::from("/h/src/tools/zed-ai-extensions")),
            ])
            .unwrap(),
        };
        let inspector = FakeCheckouts::default()
            .with_children("/h/src", &["/h/src/zeta", "/h/src/alpha"])
            .with_main(
                "/h/src/zeta",
                "/h/src/zeta",
                Some("git@github.com:acme/zeta.git"),
            )
            .with_main(
                "/h/src/alpha",
                "/h/src/alpha",
                Some("git@github.com:acme/alpha.git"),
            )
            .with_main(
                "/h/src/tools/zed-ai-extensions",
                "/h/src/tools/zed-ai-extensions",
                Some("git@github.com:gameldar/zed-ai-extensions.git"),
            );

        let choices = ResolveCheckout::new(&inspector, Some(&projects))
            .list(None)
            .unwrap();

        let names: Vec<String> = choices.checkouts.iter().map(|c| c.name()).collect();
        assert_eq!(names, vec!["alpha", "zed-ai-extensions", "zeta"]);
    }

    #[test]
    fn the_listing_preselects_the_current_checkout() {
        let projects = projects_root();
        let inspector = FakeCheckouts::default()
            .with_children("/h/src", &["/h/src/omega", "/h/src/alpha"])
            .with_main(
                "/h/src/omega",
                "/h/src/omega",
                Some("git@github.com:acme/omega.git"),
            )
            .with_main(
                "/h/src/alpha",
                "/h/src/alpha",
                Some("git@github.com:acme/alpha.git"),
            );

        let choices = ResolveCheckout::new(&inspector, Some(&projects))
            .list(Some(Path::new("/h/src/omega")))
            .unwrap();

        let index = choices.checkouts.iter().position(|c| c.name() == "omega");
        assert_eq!(choices.preselected, index);
    }

    #[test]
    fn a_symlinked_root_still_preselects_and_dedupes() {
        let slug = RepoSlug::parse("acme/omega").unwrap();
        let projects = Projects {
            root: ProjectsRoot::new(PathBuf::from("/h/src")).unwrap(),
            paths: ProjectPaths::new(vec![(slug, PathBuf::from("/Volumes/code/omega"))]).unwrap(),
        };
        let inspector = FakeCheckouts::default()
            .with_children("/h/src", &["/h/src/omega"])
            .with_main(
                "/h/src/omega",
                "/Volumes/code/omega",
                Some("git@github.com:acme/omega.git"),
            )
            .with_main(
                "/Volumes/code/omega",
                "/Volumes/code/omega",
                Some("git@github.com:acme/omega.git"),
            );

        let choices = ResolveCheckout::new(&inspector, Some(&projects))
            .list(Some(Path::new("/Volumes/code/omega")))
            .unwrap();

        assert_eq!(choices.checkouts.len(), 1);
        assert_eq!(choices.preselected, Some(0));
    }
}
