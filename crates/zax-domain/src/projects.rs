//! Where zax finds the base checkout of a repository, when the worktree
//! convention alone cannot say: `projects.root` plus per-repository
//! overrides, and the verdicts on what an inspector finds when it looks.

use std::fmt;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::forge::RepoSlug;
use crate::repo::has_parent_traversal;

/// The configured directory that holds every repository's base checkout, one
/// level up from the per-repository convention `<root>/<name>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectsRoot(PathBuf);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProjectsRootError {
    #[error("projects root {0} must be an absolute path")]
    NotAbsolute(PathBuf),
    #[error(
        "projects root {0} contains a `..` component; give the path it resolves \
         to, because containment is checked without touching the disk"
    )]
    Traversal(PathBuf),
}

impl ProjectsRoot {
    pub fn new(path: PathBuf) -> Result<Self, ProjectsRootError> {
        if !path.is_absolute() {
            return Err(ProjectsRootError::NotAbsolute(path));
        }
        if has_parent_traversal(&path) {
            return Err(ProjectsRootError::Traversal(path));
        }
        Ok(Self(path))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// Per-repository overrides of the `<root>/<name>` convention, keyed by
/// slug and compared case-insensitively: an override is a promise about one
/// specific repository, and GitHub does not distinguish `Owner/Name` from
/// `owner/name`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectPaths(Vec<(RepoSlug, PathBuf)>);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProjectPathsError {
    #[error("override for {slug} at {path} must be an absolute path")]
    NotAbsolute { slug: RepoSlug, path: PathBuf },
    #[error("two overrides name the same repository: {0}")]
    Duplicate(RepoSlug),
}

impl ProjectPaths {
    pub fn new(entries: Vec<(RepoSlug, PathBuf)>) -> Result<Self, ProjectPathsError> {
        let mut accepted: Vec<(RepoSlug, PathBuf)> = Vec::with_capacity(entries.len());
        for (slug, path) in entries {
            if !path.is_absolute() {
                return Err(ProjectPathsError::NotAbsolute { slug, path });
            }
            if accepted
                .iter()
                .any(|(existing, _)| existing.same_repository(&slug))
            {
                return Err(ProjectPathsError::Duplicate(slug));
            }
            accepted.push((slug, path));
        }
        Ok(Self(accepted))
    }

    pub fn get(&self, slug: &RepoSlug) -> Option<&Path> {
        self.0
            .iter()
            .find(|(existing, _)| existing.same_repository(slug))
            .map(|(_, path)| path.as_path())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&RepoSlug, &Path)> {
        self.0.iter().map(|(slug, path)| (slug, path.as_path()))
    }
}

/// The root plus its overrides: everything needed to say where a
/// repository's base checkout should be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Projects {
    pub root: ProjectsRoot,
    pub paths: ProjectPaths,
}

impl Projects {
    /// Where to look for `slug`'s base checkout: the override when one is
    /// configured for it, otherwise the `<root>/<name>` convention. An
    /// override never falls back to the convention - it is a statement about
    /// exactly where that repository lives.
    ///
    /// The name is safe to join: `RepoSlug::parse` refuses `.` and `..`, so
    /// the convention cannot put back the traversal `ProjectsRoot` rejects.
    pub fn candidate(&self, slug: &RepoSlug) -> PathBuf {
        self.paths
            .get(slug)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root.as_path().join(slug.name()))
    }
}

/// What an inspector observed at a candidate path, before any verdict is
/// drawn on it. Gathering this is the one place I/O happens; everything that
/// follows is pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutProbe {
    /// Nothing exists at the path.
    Absent,
    /// Something exists there, but it is not a git checkout.
    NotACheckout,
    /// A git checkout, but a linked worktree rather than the main one.
    LinkedWorktree,
    /// The main checkout, with the canonical toplevel git reports and its
    /// origin URL, if it has one and could be read.
    Main {
        top: PathBuf,
        origin_url: Option<String>,
    },
}

/// Why a candidate path was refused as the base checkout.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Rejection {
    #[error("absent")]
    Absent,
    #[error("not a git checkout")]
    NotACheckout,
    #[error("a linked worktree, not a main checkout")]
    LinkedWorktree,
    #[error("has no origin remote")]
    NoOrigin,
    #[error("origin {0} is not owner/name")]
    UnreadableOrigin(String),
    #[error("origin is {0}")]
    OriginIs(RepoSlug),
}

/// A checkout that passed verification: the main working tree, with its
/// origin when one was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainCheckout {
    top: PathBuf,
    origin: Option<RepoSlug>,
}

impl MainCheckout {
    pub fn path(&self) -> &Path {
        &self.top
    }

    pub fn origin(&self) -> Option<&RepoSlug> {
        self.origin.as_ref()
    }

    /// The last component of the checkout's path, lossily - good enough for
    /// display, never for a filesystem operation.
    pub fn name(&self) -> String {
        self.top
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// Must be the main checkout of `expected`: a candidate found by following
/// `projects.root` or the convention has to prove it is the repository it
/// was supposed to be, not merely a checkout of something.
pub fn verify(probe: CheckoutProbe, expected: &RepoSlug) -> Result<MainCheckout, Rejection> {
    let (top, origin_url) = main_or_reject(probe)?;
    let url = origin_url.ok_or(Rejection::NoOrigin)?;
    let origin =
        RepoSlug::from_remote_url(&url).ok_or_else(|| Rejection::UnreadableOrigin(url.clone()))?;
    if !origin.same_repository(expected) {
        return Err(Rejection::OriginIs(origin));
    }
    Ok(MainCheckout {
        top,
        origin: Some(origin),
    })
}

/// Must be a main checkout, but of whatever repository it turns out to be:
/// `--repo <path>` names the path directly, so there is nothing to check it
/// against. The origin is still parsed when there is one, since later use
/// cases want to know what repository they ended up in.
pub fn accept(probe: CheckoutProbe) -> Result<MainCheckout, Rejection> {
    let (top, origin_url) = main_or_reject(probe)?;
    let origin = origin_url.and_then(|url| RepoSlug::from_remote_url(&url));
    Ok(MainCheckout { top, origin })
}

/// The shape both verdict functions share: reject anything that is not the
/// main checkout outright, then hand the caller its fields to judge further.
fn main_or_reject(probe: CheckoutProbe) -> Result<(PathBuf, Option<String>), Rejection> {
    match probe {
        CheckoutProbe::Absent => Err(Rejection::Absent),
        CheckoutProbe::NotACheckout => Err(Rejection::NotACheckout),
        CheckoutProbe::LinkedWorktree => Err(Rejection::LinkedWorktree),
        CheckoutProbe::Main { top, origin_url } => Ok((top, origin_url)),
    }
}

/// Every candidate path tried for `slug`, and why each was refused, when
/// none of them panned out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoCheckout {
    pub slug: RepoSlug,
    pub tried: Vec<(PathBuf, Rejection)>,
    pub projects_off: bool,
}

impl fmt::Display for NoCheckout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "no checkout of {} found", self.slug)?;
        for (path, rejection) in &self.tried {
            writeln!(f, "  {}: {rejection}", path.display())?;
        }
        if self.projects_off {
            write!(
                f,
                "set projects.root in ~/.config/zax/config.toml to look beyond this repository"
            )?;
        }
        Ok(())
    }
}

/// So an application-layer error can wrap this one with `#[error(transparent)]`.
impl std::error::Error for NoCheckout {}

/// The checkouts offered to a user asked to pick one, with the index already
/// selected when there is an unambiguous default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoChoices {
    pub checkouts: Vec<MainCheckout>,
    pub preselected: Option<usize>,
}

/// How a user names a repository on the command line: a slug, or a path to
/// a checkout they already have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoTarget {
    Slug(RepoSlug),
    Path(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RepoTargetError {
    #[error("`{0}` is neither owner/name nor a path; write a relative path as ./{0}")]
    Neither(String),
}

impl RepoTarget {
    /// A leading `/`, `.` or `~` reads as a path and is kept verbatim - `~`
    /// is expanded later, by the CLI layer that knows the home directory.
    /// Anything else is tried as a slug, so a bare repository name like
    /// `omega` fails with a message that shows how to spell it as a path
    /// instead of silently resolving against the current directory.
    pub fn parse(raw: &str) -> Result<Self, RepoTargetError> {
        if raw.starts_with('/') || raw.starts_with('.') || raw.starts_with('~') {
            return Ok(Self::Path(PathBuf::from(raw)));
        }
        RepoSlug::parse(raw)
            .map(Self::Slug)
            .map_err(|_| RepoTargetError::Neither(raw.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::Path;

    use super::*;

    #[test]
    fn a_projects_root_is_absolute_and_free_of_traversal() {
        assert!(ProjectsRoot::new(PathBuf::from("/home/k/src")).is_ok());
        assert!(matches!(
            ProjectsRoot::new(PathBuf::from("src")),
            Err(ProjectsRootError::NotAbsolute(_))
        ));
        assert!(matches!(
            ProjectsRoot::new(PathBuf::from("/home/k/../src")),
            Err(ProjectsRootError::Traversal(_))
        ));
    }

    #[test]
    fn an_override_is_found_whatever_the_case() {
        let slug = RepoSlug::parse("gameldar/zed-ai-extensions").unwrap();
        let path = PathBuf::from("/h/src/tools/zed-ai-extensions");
        let paths = ProjectPaths::new(vec![(slug, path.clone())]).unwrap();

        let lookup = RepoSlug::parse("Gameldar/Zed-AI-Extensions").unwrap();
        assert_eq!(paths.get(&lookup), Some(path.as_path()));
    }

    #[test]
    fn two_overrides_for_one_repository_are_refused() {
        let entries = vec![
            (
                RepoSlug::parse("Acme/Omega").unwrap(),
                PathBuf::from("/h/src/omega"),
            ),
            (
                RepoSlug::parse("acme/omega").unwrap(),
                PathBuf::from("/h/src/omega-2"),
            ),
        ];
        assert!(matches!(
            ProjectPaths::new(entries),
            Err(ProjectPathsError::Duplicate(_))
        ));
    }

    #[test]
    fn a_relative_override_is_refused() {
        let entries = vec![(
            RepoSlug::parse("acme/omega").unwrap(),
            PathBuf::from("omega"),
        )];
        assert!(matches!(
            ProjectPaths::new(entries),
            Err(ProjectPathsError::NotAbsolute { .. })
        ));
    }

    #[test]
    fn the_candidate_is_the_override_else_root_slash_name() {
        let override_slug = RepoSlug::parse("gameldar/zed-ai-extensions").unwrap();
        let override_path = PathBuf::from("/h/src/tools/zed-ai-extensions");
        let paths =
            ProjectPaths::new(vec![(override_slug.clone(), override_path.clone())]).unwrap();
        let root = ProjectsRoot::new(PathBuf::from("/h/src")).unwrap();
        let projects = Projects { root, paths };

        assert_eq!(projects.candidate(&override_slug), override_path);

        let miss = RepoSlug::parse("acme/omega").unwrap();
        assert_eq!(projects.candidate(&miss), PathBuf::from("/h/src/omega"));
    }

    #[test]
    fn verify_accepts_a_main_checkout_whose_origin_matches() {
        let slug = RepoSlug::parse("acme/omega").unwrap();
        let probe = CheckoutProbe::Main {
            top: PathBuf::from("/h/src/omega"),
            origin_url: Some("git@github.com:Acme/omega.git".to_owned()),
        };
        let checkout = verify(probe, &slug).unwrap();
        assert_eq!(checkout.path(), Path::new("/h/src/omega"));
    }

    #[test]
    fn verify_names_the_origin_it_found_instead() {
        let slug = RepoSlug::parse("acme/omega").unwrap();
        let probe = CheckoutProbe::Main {
            top: PathBuf::from("/h/src/omega"),
            origin_url: Some("git@github.com:someone/omega.git".to_owned()),
        };
        let err = verify(probe, &slug).unwrap_err();
        assert_eq!(
            err,
            Rejection::OriginIs(RepoSlug::parse("someone/omega").unwrap())
        );
        assert_eq!(err.to_string(), "origin is someone/omega");
    }

    #[test]
    fn verify_rejects_each_non_checkout_shape() {
        let slug = RepoSlug::parse("acme/omega").unwrap();

        assert_eq!(verify(CheckoutProbe::Absent, &slug), Err(Rejection::Absent));
        assert_eq!(
            verify(CheckoutProbe::NotACheckout, &slug),
            Err(Rejection::NotACheckout)
        );
        assert_eq!(
            verify(CheckoutProbe::LinkedWorktree, &slug),
            Err(Rejection::LinkedWorktree)
        );
        assert_eq!(
            verify(
                CheckoutProbe::Main {
                    top: PathBuf::from("/h/src/omega"),
                    origin_url: None,
                },
                &slug
            ),
            Err(Rejection::NoOrigin)
        );
        assert!(matches!(
            verify(
                CheckoutProbe::Main {
                    top: PathBuf::from("/h/src/omega"),
                    origin_url: Some("/srv/x.git".to_owned()),
                },
                &slug
            ),
            Err(Rejection::UnreadableOrigin(_))
        ));
    }

    #[test]
    fn accept_takes_a_main_checkout_with_or_without_origin() {
        let with_origin = CheckoutProbe::Main {
            top: PathBuf::from("/h/src/omega"),
            origin_url: Some("git@github.com:acme/omega.git".to_owned()),
        };
        let checkout = accept(with_origin).unwrap();
        assert_eq!(
            checkout.origin(),
            Some(&RepoSlug::parse("acme/omega").unwrap())
        );

        let without_origin = CheckoutProbe::Main {
            top: PathBuf::from("/h/src/omega"),
            origin_url: None,
        };
        let checkout = accept(without_origin).unwrap();
        assert_eq!(checkout.origin(), None);

        assert!(matches!(
            accept(CheckoutProbe::LinkedWorktree),
            Err(Rejection::LinkedWorktree)
        ));
    }

    #[test]
    fn the_miss_message_lists_every_path_and_why() {
        let slug = RepoSlug::parse("acme/omega").unwrap();
        let no_checkout = NoCheckout {
            slug: slug.clone(),
            tried: vec![
                (
                    PathBuf::from("/h/src/zed"),
                    Rejection::OriginIs(RepoSlug::parse("someone/zed").unwrap()),
                ),
                (PathBuf::from("/h/src/omega"), Rejection::Absent),
            ],
            projects_off: false,
        };
        let message = no_checkout.to_string();
        assert!(message.contains("acme/omega"));
        assert!(message.contains("/h/src/zed: origin is someone/zed"));
        assert!(message.contains("/h/src/omega: absent"));
        assert!(!message.contains("projects.root"));

        let turned_off = NoCheckout {
            projects_off: true,
            ..no_checkout
        };
        assert!(turned_off.to_string().contains("projects.root"));
    }

    #[test]
    fn a_repo_target_is_a_path_or_a_slug() {
        for raw in ["/x/y", "./y", "../y", "~/src/y"] {
            assert!(
                matches!(RepoTarget::parse(raw), Ok(RepoTarget::Path(_))),
                "expected `{raw}` to be a path"
            );
        }

        assert!(matches!(
            RepoTarget::parse("acme/omega"),
            Ok(RepoTarget::Slug(_))
        ));

        let err = RepoTarget::parse("omega").unwrap_err();
        assert!(err.to_string().contains("./omega"), "got: {err}");
    }
}
