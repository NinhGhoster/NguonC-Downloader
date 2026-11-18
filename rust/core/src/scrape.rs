//! Site scraping. Ported from the Python engine: the site redesign of
//! 2026-09-24 moved episode data to a JSON endpoint referenced by
//! `data-episode-url`; inline patterns are legacy fallbacks.
use crate::http;
use crate::model::{Episode, Movie, Server};
use anyhow::{Context, Result, bail};
use regex::Regex;

fn regex(pat: &str) -> Result<Regex> {
    Regex::new(pat).context("invalid regex")
}

fn group1(re: &Regex, text: &str) -> Option<String> {
    re.captures(text).map(|c| c[1].to_string())
}

pub async fn scrape(url: &str) -> Result<Movie> {
    let html = http::get(url, None).await?;

    let title = regex(r"<title>(.*?)</title>")?
        .captures(&html)
        .map(|c| c[1].trim().to_string())
        .unwrap_or_default();
    let english_title = title
        .split_once(" - ")
        .map(|(_, e)| e.trim().to_string())
        .unwrap_or_else(|| title.clone());

    let slug = regex(r"/phim/([^/]+?)(?:-\d+)?(?:\.html?)?$")?
        .captures(url)
        .map(|c| c[1].trim_end_matches('/').to_string());
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
    let data: serde_json::Value = serde_json::from_str(&text)?;
    for cat in data["movie"]["category"]
        .as_object()
        .unwrap_or(&serde_json::Map::new())
        .values()
    {
        if cat["group"]["name"] == "Năm"
            && let Some(name) = cat["list"]
                .as_array()
                .and_then(|l| l.first())
                .and_then(|v| v["name"].as_str())
        {
            return Ok(name.to_string());
        }
    }
    bail!("no year in API response")
}

async fn fetch_servers(html: &str, url: &str) -> Result<Vec<Server>> {
    // Primary: data-episode-url -> JSON.
    if let Some(ep_url) = group1(&regex(r#"data-episode-url="([^"]+)""#)?, html)
        && let Ok(text) = http::get(&ep_url, Some(url)).await
        && let Ok(data) = serde_json::from_str::<serde_json::Value>(&text)
        && let Some(s) = parse_servers(&data)
    {
        return Ok(s);
    }
    // Legacy inline patterns.
    for pat in [
        r#"id="nc-episode-data">(\[.*?\])</"#,
        r"var episodes\s*=\s*(\[.*?\]);",
    ] {
        if let Some(json) = group1(&regex(pat)?, html)
            && let Ok(data) = serde_json::from_str::<serde_json::Value>(&json)
            && let Some(s) = parse_servers(&data)
        {
            return Ok(s);
        }
    }
    // Last resort: {url}/episodes.
    let ep_url = format!("{url}/episodes");
    if let Ok(text) = http::get(&ep_url, Some(url)).await
        && let Ok(data) = serde_json::from_str::<serde_json::Value>(&text)
        && let Some(s) = parse_servers(&data)
    {
        return Ok(s);
    }
    bail!("Could not find episode data on page")
}

fn parse_servers(data: &serde_json::Value) -> Option<Vec<Server>> {
    let arr = data.get("servers")?.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for s in arr {
        let name = s.get("name")?.as_str()?.to_string();
        let list: Vec<Episode> = serde_json::from_value(s.get("list")?.clone()).ok()?;
        out.push(Server { name, list });
    }
    if out.is_empty() { None } else { Some(out) }
}
