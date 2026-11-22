//! Site scraping. Ported from the Python engine: the site redesign of
//! 2026-09-24 moved episode data to a JSON endpoint referenced by
//! `data-episode-url`; inline patterns are legacy fallbacks.
//!
//! The episode payload is parsed into typed structs, not `serde_json::Value`:
//! a malformed server or episode is skipped instead of discarding the whole
//! list, and a failed fetch reports its real cause (HTTP status, bad JSON)
//! instead of the old generic "Could not find episode data on page".
use crate::error::{NguoncError, Result};
use crate::http;
use crate::model::{Episode, Movie, Server};
use regex::Regex;
use serde::Deserialize;
use std::sync::OnceLock;

fn regex(pat: &str) -> &'static Regex {
    static REGEXES: OnceLock<Vec<Regex>> = OnceLock::new();
    // Patterns are compiled once per process, in declaration order.
    const PATTERNS: &[&str] = &[
        r"<title>(.*?)</title>",
        r"/phim/([^/]+?)(?:-\d+)?(?:\.html?)?$",
        r#"data-episode-url="([^"]+)""#,
        r#"id="nc-episode-data">(\[.*?\])</"#,
        r"var episodes\s*=\s*(\[.*?\]);",
    ];
    let regexes = REGEXES.get_or_init(|| {
        PATTERNS
            .iter()
            .map(|p| Regex::new(p).expect("scrape regex must compile"))
            .collect()
    });
    let idx = PATTERNS
        .iter()
        .position(|p| p == &pat)
        .expect("unknown regex");
    &regexes[idx]
}

fn group1(re: &Regex, text: &str) -> Option<String> {
    re.captures(text).map(|c| c[1].to_string())
}

/// Typed shape of the episodes endpoint:
/// `{"servers":[{"name","list":[{name,slug,embed}]}]}`.
#[derive(Debug, Deserialize)]
struct ServersPayload {
    servers: Vec<ServerPayload>,
}

#[derive(Debug, Deserialize)]
struct ServerPayload {
    name: String,
    list: Option<Vec<Episode>>,
}

/// Typed shape of the film API response, only the parts we need:
/// `movie.category` is a map of category groups; the "Năm" (year) group
/// lists one entry whose `name` is the year.
#[derive(Debug, Deserialize)]
struct FilmPayload {
    movie: Option<FilmMovie>,
}

#[derive(Debug, Deserialize)]
struct FilmMovie {
    category: Option<std::collections::HashMap<String, FilmCategory>>,
}

#[derive(Debug, Deserialize)]
struct FilmCategory {
    group: Option<FilmGroup>,
    list: Option<Vec<FilmEntry>>,
}

#[derive(Debug, Deserialize)]
struct FilmGroup {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FilmEntry {
    name: Option<String>,
}

pub async fn scrape(url: &str) -> Result<Movie> {
    let html = http::get(url, None).await?;

    let title = regex(r"<title>(.*?)</title>")
        .captures(&html)
        .map(|c| c[1].trim().to_string())
        .unwrap_or_default();
    let english_title = title
        .split_once(" - ")
        .map(|(_, e)| e.trim().to_string())
        .unwrap_or_else(|| title.clone());

    let slug = regex(r"/phim/([^/]+?)(?:-\d+)?(?:\.html?)?$")
        .captures(url)
        .map(|c| c[1].trim_end_matches('/').to_string());
    // The year is nice-to-have: a failed year lookup must not fail the load.
    let year = match &slug {
        Some(s) => fetch_year(s).await.unwrap_or_default(),
        None => String::new(),
    };

    let servers = fetch_servers(&html, url).await?;
    Ok(Movie {
        url: url.to_string(),
        title,
        english_title,
        year,
        servers,
    })
}

async fn fetch_year(slug: &str) -> Result<String> {
    let api = format!("https://phim.nguonc.com/api/film/{slug}");
    let text = http::get(&api, None).await?;
    let data: FilmPayload = serde_json::from_str(&text)?;
    let categories = data
        .movie
        .and_then(|m| m.category)
        .ok_or_else(|| NguoncError::parse("no movie/category in film API response"))?;
    for cat in categories.values() {
        if cat.group.as_ref().and_then(|g| g.name.as_deref()) == Some("Năm")
            && let Some(name) = cat
                .list
                .as_ref()
                .and_then(|l| l.first())
                .and_then(|v| v.name.as_deref())
        {
            return Ok(name.to_string());
        }
    }
    Err(NguoncError::parse("no year in API response"))
}

async fn fetch_servers(html: &str, url: &str) -> Result<Vec<Server>> {
    let mut last_err: Option<NguoncError> = None;

    // Primary: data-episode-url -> JSON.
    if let Some(ep_url) = group1(regex(r#"data-episode-url="([^"]+)""#), html) {
        match try_servers_url(&ep_url, url).await {
            Ok(s) => return Ok(s),
            Err(e) => {
                if last_err.is_none() {
                    last_err = Some(e);
                }
            }
        }
    }
    // Legacy inline patterns.
    for pat in [
        r#"id="nc-episode-data">(\[.*?\])</"#,
        r"var episodes\s*=\s*(\[.*?\]);",
    ] {
        if let Some(json) = group1(regex(pat), html) {
            match parse_servers_json(&json) {
                Ok(s) => return Ok(s),
                Err(e) => {
                    if last_err.is_none() {
                        last_err = Some(e);
                    }
                }
            }
        }
    }
    // Last resort: {url}/episodes.
    let ep_url = format!("{url}/episodes");
    match try_servers_url(&ep_url, url).await {
        Ok(s) => return Ok(s),
        Err(e) => {
            if last_err.is_none() {
                last_err = Some(e);
            }
        }
    }

    Err(last_err.unwrap_or_else(|| NguoncError::parse("Could not find episode data on page")))
}

async fn try_servers_url(ep_url: &str, referer: &str) -> Result<Vec<Server>> {
    let text = http::get(ep_url, Some(referer)).await?;
    parse_servers_json(&text)
}

fn parse_servers_json(text: &str) -> Result<Vec<Server>> {
    // The endpoint returns {"servers": [...]}; the legacy inline patterns
    // return the bare array, so accept both shapes.
    if let Ok(payload) = serde_json::from_str::<ServersPayload>(text) {
        return Ok(servers_from(payload.servers));
    }
    let list = serde_json::from_str::<Vec<ServerPayload>>(text)
        .map_err(|e| NguoncError::parse(format!("episode data is not valid JSON: {e}")))?;
    Ok(servers_from(list))
}

/// Convert parsed servers, skipping malformed entries instead of failing
/// the whole load because one server/episode is bad.
fn servers_from(payload: Vec<ServerPayload>) -> Vec<Server> {
    payload
        .into_iter()
        .filter_map(|s| {
            let list = s.list?;
            if list.is_empty() {
                return None;
            }
            Some(Server { name: s.name, list })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{parse_servers_json, servers_from};

    #[test]
    fn parse_servers_accepts_object_shape() {
        let servers = parse_servers_json(
            r#"{"servers":[{"name":"Vietsub","list":[{"name":"1","slug":"tap-1","embed":"https://e/x"}]}]}"#,
        )
        .unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].list.len(), 1);
    }

    #[test]
    fn parse_servers_accepts_bare_array_shape() {
        let servers = parse_servers_json(
            r#"[{"name":"Vietsub","list":[{"name":"1","embed":"https://e/x"}]}]"#,
        )
        .unwrap();
        assert_eq!(servers.len(), 1);
    }

    #[test]
    fn parse_servers_skips_empty_and_malformed_servers() {
        let servers = servers_from(vec![
            super::ServerPayload {
                name: "Empty".into(),
                list: Some(vec![]),
            },
            super::ServerPayload {
                name: "NoList".into(),
                list: None,
            },
            super::ServerPayload {
                name: "Good".into(),
                list: Some(vec![super::Episode {
                    name: "1".into(),
                    slug: None,
                    embed: "https://e/x".into(),
                }]),
            },
        ]);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "Good");
    }

    #[test]
    fn parse_servers_rejects_garbage() {
        assert!(parse_servers_json("not json").is_err());
        assert!(parse_servers_json(r#"{"servers":"nope"}"#).is_err());
    }
}
