use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Server {
    pub name: String,
    pub list: Vec<Episode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Episode {
    pub name: String,
    pub slug: Option<String>,
    pub embed: String,
}

#[derive(Debug, Clone)]
pub struct Movie {
    pub url: String,
    pub title: String,
    pub english_title: String,
    pub year: String,
    pub servers: Vec<Server>,
}

/// A successfully resolved episode: the decrypted playlist is on disk.
/// There is no `error` field — resolution failure is `Err(NguoncError)`,
/// so a `Resolved` can never claim both a playlist and an error.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub num: String,
    pub embed: String,
    pub m3u8: String,
    pub filename: String,
}

/// Output filename for an episode, e.g. `English.Title.S01E01.mp4`.
/// Lives here with the model instead of in the resolver, and takes the
/// movie title explicitly — callers must not pass empty titles.
pub fn generate_filename(
    english_title: &str,
    title: &str,
    episode_num: &str,
    season: u32,
) -> String {
    let name = if english_title.is_empty() {
        title
    } else {
        english_title
    };
    let safe: String = name
        .chars()
        .filter(|c| !matches!(c, '\\' | '/' | '*' | '?' | ':' | '"' | '<' | '>' | '|'))
        .collect();
    let dotted = safe.split_whitespace().collect::<Vec<_>>().join(".");
    let ep: u32 = episode_num.parse().unwrap_or(0);
    format!("{dotted}.S{season:02}E{ep:02}.mp4")
}

#[cfg(test)]
mod tests {
    use super::generate_filename;

    #[test]
    fn filename_uses_english_title_and_pads() {
        assert_eq!(
            generate_filename("Elite Force", "GIGN: Lực Lượng Tinh Nhuệ", "1", 1),
            "Elite.Force.S01E01.mp4"
        );
    }

    #[test]
    fn filename_falls_back_to_vietnamese_title_and_strips_illegal_chars() {
        assert_eq!(
            generate_filename("", "Phim: Hay / Test?", "12", 2),
            "Phim.Hay.Test.S02E12.mp4"
        );
    }

    #[test]
    fn filename_non_numeric_episode_is_zero() {
        assert_eq!(
            generate_filename("Test", "Test", "Tập 1", 1),
            "Test.S01E00.mp4"
        );
    }
}
