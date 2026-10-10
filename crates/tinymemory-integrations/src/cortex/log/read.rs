//! Listing a scope's events and building recall packs.

use std::collections::HashSet;

use reqwest::Method;
use serde_json::Value;

use super::{Log, MAX_PAGES, PAGE_SIZE};
use crate::cortex::descriptor::Route;
use crate::cortex::error::{Error, Result};
use crate::cortex::transport::{Attempts, urlencode};

/// Most labels one listing names. Labels share one comma-separated
/// parameter (the hosted backend refuses a repeated `labels=`), so this
/// bounds the URL.
pub(crate) const LABELS_PER_QUERY: usize = 50;

/// Most scopes one scope listing asks for: three kinds for each of several
/// hundred namespace nodes. It is also the most CortexDB answers: it clamps a
/// larger `limit` to 1000 and has no cursor (measured on v0.10.5).
pub(crate) const SCOPES_LIMIT: usize = 1000;

/// One page of a scope listing.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Page {
    /// The events, as the engine sent them (duplicates included).
    pub(crate) items: Vec<Value>,
    /// The cursor of the next page; `None` at the end.
    pub(crate) next: Option<String>,
}

impl Log {
    /// One listing page of `scope`, newest first, narrowed to events
    /// carrying any one of `labels` when given.
    pub(crate) async fn page(
        &self,
        scope: &str,
        labels: Option<&[String]>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Page> {
        let mut path = format!(
            "{base}?scope={scope}&limit={limit}",
            base = self.client.wire().path(Route::Events),
            scope = urlencode(scope),
        );
        if let Some(labels) = labels.filter(|labels| !labels.is_empty()) {
            path.push_str(&format!("&labels={}", urlencode(&labels.join(","))));
        }
        if let Some(cursor) = cursor {
            path.push_str(&format!("&cursor={}", urlencode(cursor)));
        }
        let page = self
            .client
            .json(Method::GET, &path, None, Attempts::RetryTransient)
            .await?;
        let items = page
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let next = match (
            page.get("has_more").and_then(Value::as_bool),
            page.get("next_cursor").and_then(Value::as_str),
        ) {
            (Some(true), Some(next)) => Some(next.to_string()),
            _ => None,
        };
        if next.is_some() && next.as_deref() == cursor {
            return Err(Error::Engine(format!(
                "listing scope `{scope}` returned the cursor it was given; refusing to walk a \
                 listing that does not advance"
            )));
        }
        Ok(Page { items, next })
    }

    /// Every distinct event of `scope` carrying any one of `labels` (all of
    /// them when `labels` is `None`), newest first, following the cursor to
    /// the end and dropping the engine's duplicate copies by event id.
    ///
    /// # Errors
    ///
    /// Backend failures, and [`Error::Engine`] past [`MAX_PAGES`] pages.
    pub(crate) async fn walk(&self, scope: &str, labels: Option<&[String]>) -> Result<Vec<Value>> {
        let mut all = Vec::new();
        let mut seen = HashSet::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let page = self
                .page(scope, labels, cursor.as_deref(), PAGE_SIZE)
                .await?;
            for item in page.items {
                match item.get("id").and_then(Value::as_str) {
                    Some(id) if !seen.insert(id.to_string()) => {}
                    _ => all.push(item),
                }
            }
            match page.next {
                Some(next) => cursor = Some(next),
                None => return Ok(all),
            }
        }
        Err(Error::Engine(format!(
            "listing scope `{scope}` exceeded {MAX_PAGES} pages; refusing to answer from a \
             truncated log"
        )))
    }

    /// [`Self::walk`] for many labels, in batches of [`LABELS_PER_QUERY`],
    /// deduplicated across batches.
    pub(crate) async fn walk_labels(&self, scope: &str, labels: &[String]) -> Result<Vec<Value>> {
        let mut all = Vec::new();
        let mut seen = HashSet::new();
        for batch in labels.chunks(LABELS_PER_QUERY) {
            for event in self.walk(scope, Some(batch)).await? {
                let fresh = event
                    .get("id")
                    .and_then(Value::as_str)
                    .is_none_or(|id| seen.insert(id.to_string()));
                if fresh {
                    all.push(event);
                }
            }
        }
        Ok(all)
    }

    /// The registered scope paths under `prefix`, as the engine names them
    /// (the hosted backend may prefix the caller's tenant). CortexDB answers
    /// `{items: [{path}]}`; the hosted route may answer `{scopes: [path]}`.
    /// An engine without a scope listing (404) holds none worth naming.
    ///
    /// CortexDB's listing has no cursor and answers at most [`SCOPES_LIMIT`]
    /// paths; a listing that reaches it may be missing some, which is
    /// logged. A caller that must see every scope uses [`Self::all_scopes`].
    ///
    /// # Errors
    ///
    /// Backend failures other than a 404.
    pub(crate) async fn scopes(&self, prefix: &str) -> Result<Vec<String>> {
        let (paths, truncated) = self.scopes_listing(prefix).await?;
        if truncated {
            log::warn!(
                "[cortex] the scope listing under {prefix:?} reached its limit of {SCOPES_LIMIT}; \
                 scopes past it are not read"
            );
        }
        Ok(paths)
    }

    /// As [`Self::scopes`], but refuses a listing that may be missing scopes,
    /// for a caller that must see all of them (an export that moves memory).
    ///
    /// # Errors
    ///
    /// [`Error::Engine`] when the listing reaches [`SCOPES_LIMIT`], and
    /// backend failures other than a 404.
    pub(crate) async fn all_scopes(&self, prefix: &str) -> Result<Vec<String>> {
        let (paths, truncated) = self.scopes_listing(prefix).await?;
        if truncated {
            return Err(Error::Engine(format!(
                "more than {SCOPES_LIMIT} scopes under {prefix:?}; the engine cannot list them all"
            )));
        }
        Ok(paths)
    }

    /// The listing, and whether it reached [`SCOPES_LIMIT`].
    async fn scopes_listing(&self, prefix: &str) -> Result<(Vec<String>, bool)> {
        // An empty prefix (the hosted tenant's own root) sends none: the
        // backend bounds an unprefixed listing to the caller's tenant, and
        // refuses an empty one.
        //
        // The prefix is sent as the bare node path on both wires: CortexDB
        // and the hosted route both refuse a separator-terminated one
        // (`422 INVALID_SCOPE_GRAMMAR: segment N is empty`, measured on the
        // live server). Both match whole segments; a backend that matched
        // plain string prefixes would also list a sibling (`user:anna` under
        // `user:ann`), so the answer is narrowed to whole segments below.
        let wire = self.client.wire();
        let base = wire.path(Route::Scopes);
        let path = if prefix.is_empty() {
            format!("{base}?limit={SCOPES_LIMIT}")
        } else {
            format!(
                "{base}?prefix={prefix}&limit={SCOPES_LIMIT}",
                prefix = urlencode(prefix),
            )
        };
        let listed = match self
            .client
            .json(Method::GET, &path, None, Attempts::RetryTransient)
            .await
        {
            Ok(listed) => listed,
            Err(Error::NotFound(_)) => return Ok((Vec::new(), false)),
            Err(error) => return Err(error),
        };
        let items = listed
            .get("items")
            .or_else(|| listed.get("scopes"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let truncated = items.len() >= SCOPES_LIMIT;
        let paths = items
            .iter()
            .filter_map(|item| {
                item.as_str()
                    .or_else(|| item.get("path").and_then(Value::as_str))
            })
            .filter(|path| prefix.is_empty() || below_whole_segments(path, prefix))
            .map(str::to_owned)
            .collect();
        Ok((paths, truncated))
    }

    /// Builds a recall pack, and logs what the pack says about itself
    /// (`notes`). A read: retried on transient failures.
    pub(crate) async fn recall(&self, body: &Value) -> Result<Value> {
        let pack = self
            .client
            .json(
                Method::POST,
                self.client.wire().path(Route::Recall),
                Some(body),
                Attempts::RetryTransient,
            )
            .await?;
        let scope = body.get("scope").and_then(Value::as_str).unwrap_or("");
        super::notes::report(scope, &pack);
        Ok(pack)
    }

    /// Asks the answer route once, with a pack already built.
    pub(crate) async fn answer(&self, body: &Value) -> Result<Value> {
        self.client
            .json(
                Method::POST,
                self.client.wire().path(Route::Answer),
                Some(body),
                Attempts::Once,
            )
            .await
    }
}

/// Whether `path` is `prefix` or below it, matching whole segments: the
/// prefix may sit behind a tenant prefix the hosted backend adds, but
/// `user:anna` is never below `user:ann`.
fn below_whole_segments(path: &str, prefix: &str) -> bool {
    let prefix = prefix.trim_end_matches('/');
    path == prefix
        || path.starts_with(&format!("{prefix}/"))
        || path.contains(&format!("/{prefix}/"))
        || path.ends_with(&format!("/{prefix}"))
}
