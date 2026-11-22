use anyhow::{Context, Result, bail};
use nguonc_core::{Progress, download, resolve_one, scrape};
use std::io::Write;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".to_string());
    match command.as_str() {
        "scrape" => {
            let url = args.next().context_arg("movie URL")?;
            let movie = scrape(&url).await?;
            println!("TITLE: {}", movie.title);
            println!("YEAR: {}", movie.year);
            for (i, s) in movie.servers.iter().enumerate() {
                println!("SERVER {i}: {} ({} episodes)", s.name, s.list.len());
            }
        }
        "download" => {
            let url = args.next().context_arg("movie URL")?;
            let out_dir = args.next().context_arg("output directory")?;
            let server_idx: usize = parse_arg(args.next(), "server index", 0)?;
            // Episode *index* within the server (0-based), not the episode
            // number — `download … 0 0 1` is server 0, first episode.
            let ep_idx: Option<usize> = args.next().map(|s| s.parse()).transpose()?;
            let parallel: usize = parse_arg(args.next(), "parallel", 1)?;
            if !(1..=16).contains(&parallel) {
                bail!("parallel must be 1..=16, got {parallel}");
            }

            let movie = scrape(&url).await?;
            let server = movie
                .servers
                .get(server_idx)
                .ok_or_else(|| anyhow::anyhow!("server index out of range"))?;
            let mut episodes: Vec<_> = server.list.iter().collect();
            if let Some(i) = ep_idx {
                episodes = episodes.into_iter().skip(i).take(1).collect();
                if episodes.is_empty() {
                    bail!(
                        "episode index {i} out of range (server has {} episodes)",
                        server.list.len()
                    );
                }
            }
            // Episodes download sequentially here; `parallel` bounds segment
            // fetches within one episode. The GUI downloads episodes
            // concurrently instead.
            for ep in episodes {
                let referer = ep.embed.clone();
                let resolved = match resolve_one(
                    ep,
                    &movie.url,
                    &movie.english_title,
                    &movie.title,
                    1,
                )
                .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        println!("EP {}: RESOLVE FAIL: {e}", ep.name);
                        continue;
                    }
                };
                let out = std::path::Path::new(&out_dir).join(&resolved.filename);
                println!("EP {}: downloading -> {}", resolved.num, out.display());
                let sticky = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
                let s2 = sticky.clone();
                download(
                    &resolved.m3u8,
                    &out.to_string_lossy(),
                    &referer,
                    parallel,
                    move |p: Progress| {
                        if p.done != 1 && !p.done.is_multiple_of(20) && p.done != p.total {
                            return;
                        }
                        let line = format!(
                            "\r  {}/{} segments ({:.0} MiB)",
                            p.done,
                            p.total,
                            p.bytes as f64 / 1048576.0
                        );
                        let mut g = s2.lock().unwrap();
                        g.clear();
                        g.push_str(&line);
                        let _ = std::io::stdout().write_all(g.as_bytes());
                        let _ = std::io::stdout().flush();
                    },
                )
                .await?;
                println!();
            }
        }
        "help" | "-h" | "--help" => {
            println!("usage:");
            println!("  nguonc scrape <movie-url>");
            println!(
                "  nguonc download <movie-url> <out-dir> [server-index] [episode-index] [parallel]"
            );
            println!("  episode-index is 0-based within the server; parallel is segment");
            println!("  concurrency within one episode (1..=16, default 1)");
        }
        other => bail!("unknown command: {other}"),
    }
    Ok(())
}

fn parse_arg(arg: Option<String>, name: &str, default: usize) -> Result<usize> {
    match arg {
        None => Ok(default),
        Some(s) => s
            .parse()
            .with_context(|| format!("invalid {name}: {s:?} (expected a number)")),
    }
}

trait ContextArg<T> {
    fn context_arg(self, name: &str) -> Result<T>;
}
impl<T> ContextArg<T> for Option<T> {
    fn context_arg(self, name: &str) -> Result<T> {
        self.ok_or_else(|| anyhow::anyhow!("missing argument: {name}"))
    }
}
