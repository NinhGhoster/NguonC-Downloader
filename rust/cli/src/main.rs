use anyhow::{Result, bail};
use nguonc_core::{Progress, download, generate_filename, scrape};
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
            let server_idx: usize = args.next().unwrap_or("0".into()).parse()?;
            let only: Option<usize> = args.next().map(|s| s.parse()).transpose()?;
            let parallel: usize = args.next().unwrap_or("1".into()).parse()?;

            let movie = scrape(&url).await?;
            let server = movie
                .servers
                .get(server_idx)
                .ok_or_else(|| anyhow::anyhow!("server index out of range"))?;
            let mut episodes: Vec<_> = server.list.iter().collect();
            if let Some(i) = only {
                episodes = episodes.into_iter().skip(i).take(1).collect();
            }
            for ep in episodes {
                let referer = ep.embed.clone();
                // Resolve inline (reuse the engine's resolver via core).
                let resolved = nguonc_core::resolve::resolve_one(ep, &movie.url, 1).await;
                if let Some(err) = &resolved.error {
                    println!("EP {}: RESOLVE FAIL: {}", resolved.num, err);
                    continue;
                }
                let m3u8 = resolved.m3u8.as_ref().unwrap();
                let filename = generate_filename(&movie.english_title, &movie.title, &ep.name, 1);
                let out = std::path::Path::new(&out_dir).join(filename);
                println!("EP {}: downloading -> {}", resolved.num, out.display());
                let sticky = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
                let s2 = sticky.clone();
                download(
                    m3u8,
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
            println!("  nguonc download <movie-url> <out-dir> [server] [ep-index] [parallel]");
        }
        other => bail!("unknown command: {other}"),
    }
    Ok(())
}

trait ContextArg<T> {
    fn context_arg(self, name: &str) -> Result<T>;
}
impl<T> ContextArg<T> for Option<T> {
    fn context_arg(self, name: &str) -> Result<T> {
        self.ok_or_else(|| anyhow::anyhow!("missing argument: {name}"))
    }
}
