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

#[derive(Debug, Clone)]
pub struct Resolved {
    pub num: String,
    pub embed: String,
    /// Path to the decrypted local playlist, or `None` with an error.
    pub m3u8: Option<String>,
    pub error: Option<String>,
    pub filename: String,
}
