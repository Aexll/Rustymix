//! Recommandations de morceaux similaires à un morceau choisi, par l'API
//! publique de Deezer (sans compte ni clé) :
//!
//! 1. on retrouve le morceau (titre, artiste) sur Deezer ;
//! 2. on prend la « radio » de son artiste (morceaux de l'artiste et
//!    d'artistes proches, choisis par Deezer) et les titres phares des
//!    artistes que Deezer juge proches ;
//! 3. on mélange, on retire les doublons et le morceau de départ.
//!
//! Les morceaux proposés s'installent ensuite par YouTube (yt-dlp), comme une
//! recherche normale : l'adresse est une recherche `ytsearch1:artiste - titre`.

use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::downloader::{SearchResult, Source};

const API: &str = "https://api.deezer.com";
/// Nombre maximal de morceaux proposés.
pub const MAX_RECOMMENDATIONS: usize = 30;
/// Artistes proches dont on prend les titres phares.
const RELATED_ARTISTS: usize = 6;
/// Titres phares pris par artiste proche.
const TOP_PER_ARTIST: usize = 3;

/// Morceau de départ.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seed {
    pub title: String,
    pub artist: String,
}

impl Seed {
    pub fn label(&self) -> String {
        if self.artist.is_empty() {
            self.title.clone()
        } else {
            format!("{} — {}", self.artist, self.title)
        }
    }
}

/// Morceau trouvé sur Deezer.
#[derive(Clone, Debug, PartialEq)]
struct Track {
    id: u64,
    title: String,
    artist: String,
    artist_id: u64,
    album: String,
    duration: Option<f32>,
}

/// Résultat : le morceau reconnu (« Artiste — Titre ») et les propositions.
pub type Recommendations = (String, Vec<SearchResult>);

/// Cherche des morceaux similaires à `seed` (appels réseau : à lancer hors
/// du thread de l'interface).
pub fn recommend(seed: &Seed) -> Result<Recommendations, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .new_agent();
    let get = |path: &str| -> Result<Value, String> {
        let url = format!("{API}{path}");
        let mut response = agent
            .get(&url)
            .call()
            .map_err(|error| format!("Deezer injoignable : {error}"))?;
        let json: Value = response
            .body_mut()
            .read_json()
            .map_err(|error| format!("réponse de Deezer illisible : {error}"))?;
        if let Some(error) = json.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("erreur inconnue");
            return Err(format!("Deezer : {message}"));
        }
        Ok(json)
    };

    // 1. Le morceau de départ (ou, à défaut, son artiste).
    let (title, artist) = clean_seed(seed);
    let mut queries = Vec::new();
    if !artist.is_empty() {
        queries.push(format!("artist:\"{artist}\" track:\"{title}\""));
        queries.push(format!("{artist} {title}"));
    } else {
        queries.push(title.clone());
    }
    let mut found = None;
    for query in &queries {
        let json = get(&format!("/search?limit=5&q={}", encode(query)))?;
        if let Some(track) = parse_tracks(&json).into_iter().next() {
            found = Some(track);
            break;
        }
    }
    let (artist_id, artist_name, seed_id, label) = match found {
        Some(track) => (
            track.artist_id,
            track.artist.clone(),
            Some(track.id),
            format!("{} — {}", track.artist, track.title),
        ),
        None if !artist.is_empty() => {
            let json = get(&format!("/search/artist?limit=1&q={}", encode(&artist)))?;
            let (id, name) = parse_artists(&json)
                .into_iter()
                .next()
                .ok_or_else(|| format!("« {} » introuvable sur Deezer", seed.label()))?;
            (id, name.clone(), None, name)
        }
        None => return Err(format!("« {} » introuvable sur Deezer", seed.label())),
    };

    // 2. Radio de l'artiste et titres phares des artistes proches (en parallèle).
    let radio = get(&format!("/artist/{artist_id}/radio?limit=40")).map(|json| parse_tracks(&json))?;
    let related = get(&format!("/artist/{artist_id}/related?limit={RELATED_ARTISTS}"))
        .map(|json| parse_artists(&json))
        .unwrap_or_default();
    let tops: Vec<Vec<Track>> = thread::scope(|scope| {
        let handles: Vec<_> = related
            .iter()
            .take(RELATED_ARTISTS)
            .map(|(id, _)| {
                let get = &get;
                scope.spawn(move || {
                    get(&format!("/artist/{id}/top?limit={TOP_PER_ARTIST}"))
                        .map(|json| parse_tracks(&json))
                        .unwrap_or_default()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or_default())
            .collect()
    });

    // 3. Mélange : radio et artistes proches en alternance.
    Ok((label, merge(&artist_name, seed_id, radio, tops)))
}

/// Alterne la radio et les titres phares des artistes proches, sans
/// doublon ni morceau de départ, et en fait des résultats installables.
fn merge(seed_artist: &str, seed_id: Option<u64>, radio: Vec<Track>, tops: Vec<Vec<Track>>) -> Vec<SearchResult> {
    let mut related: Vec<Track> = Vec::new();
    // Un titre de chaque artiste proche, puis le deuxième de chacun…
    for rank in 0..TOP_PER_ARTIST {
        related.extend(tops.iter().filter_map(|tracks| tracks.get(rank).cloned()));
    }
    let mut radio = radio.into_iter();
    let mut related = related.into_iter();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    while out.len() < MAX_RECOMMENDATIONS {
        let (a, b) = (radio.next(), related.next());
        if a.is_none() && b.is_none() {
            break;
        }
        for (track, from_related) in [(a, false), (b, true)] {
            let Some(track) = track else { continue };
            if Some(track.id) == seed_id || out.len() >= MAX_RECOMMENDATIONS {
                continue;
            }
            if !seen.insert(normalize(&format!("{} {}", track.artist, track.title))) {
                continue;
            }
            let note = if same_artist(&track.artist, seed_artist) {
                "même artiste".to_string()
            } else if from_related {
                "artiste proche".to_string()
            } else {
                "radio de l'artiste".to_string()
            };
            out.push(SearchResult {
                source: Source::YouTube,
                url: format!("ytsearch1:{} - {}", track.artist, track.title),
                title: track.title,
                artist: track.artist,
                album: track.album,
                duration: track.duration,
                note,
            });
        }
    }
    out
}

fn same_artist(a: &str, b: &str) -> bool {
    normalize(a) == normalize(b)
}

/// Titre et artiste nettoyés pour la recherche : sans « (Official Video) »,
/// « [Lyrics] »… ; « Artiste - Titre » séparé quand l'artiste manque ou
/// n'est qu'une chaîne YouTube (« … - Topic », « …VEVO »).
pub fn clean_seed(seed: &Seed) -> (String, String) {
    let mut title = strip_noise(&strip_brackets(&seed.title));
    let mut artist = seed.artist.trim().to_string();
    for suffix in [" - Topic", "VEVO", " Official"] {
        if let Some(stripped) = artist.strip_suffix(suffix) {
            artist = stripped.trim().to_string();
        }
    }
    if let Some((left, right)) = title.split_once(" - ") {
        let (left, right) = (left.trim().to_string(), right.trim().to_string());
        let known = normalize(&artist);
        if !known.is_empty() && normalize(&right).contains(&known) && !normalize(&left).contains(&known) {
            // « Titre - Artiste ».
            title = left;
        } else {
            // « Artiste - Titre » : l'artiste du titre l'emporte sur la chaîne.
            artist = left;
            title = right;
        }
    }
    (title, artist)
}

/// Coupe le titre au premier mot parasite (« Official Video », « Lyrics »,
/// « HD »…), pris comme mot entier et jamais en tête.
fn strip_noise(title: &str) -> String {
    const NOISE: [&[&str]; 9] = [
        &["official"],
        &["lyric", "video"],
        &["lyrics"],
        &["clip", "officiel"],
        &["audio"],
        &["video"],
        &["hd"],
        &["4k"],
        &["visualizer"],
    ];
    let words: Vec<&str> = title.split_whitespace().collect();
    let lowered: Vec<String> = words.iter().map(|word| normalize(word)).collect();
    let cut = (1..words.len()).find(|&start| {
        NOISE.iter().any(|phrase| {
            phrase.len() <= words.len() - start
                && phrase
                    .iter()
                    .enumerate()
                    .all(|(offset, word)| lowered[start + offset] == *word)
        })
    });
    let kept = &words[..cut.unwrap_or(words.len())];
    kept.join(" ")
        .trim()
        .trim_end_matches(['-', '|', '·', '–'])
        .trim()
        .to_string()
}

/// Retire les parties entre parenthèses ou crochets.
fn strip_brackets(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Minuscules, sans accents ni ponctuation : pour comparer des titres.
pub fn normalize(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'â' | 'ä' | 'á' | 'ã' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' | 'í' => 'i',
            'ô' | 'ö' | 'ó' | 'õ' => 'o',
            'ù' | 'û' | 'ü' | 'ú' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            c if c.is_alphanumeric() => c,
            _ => ' ',
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Encodage d'un paramètre d'URL.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "%20".to_string(),
            b => format!("%{b:02X}"),
        })
        .collect()
}

fn parse_tracks(json: &Value) -> Vec<Track> {
    json.get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("readable").and_then(Value::as_bool).unwrap_or(true))
        .filter_map(|entry| {
            let artist = entry.get("artist")?;
            Some(Track {
                id: entry.get("id")?.as_u64()?,
                title: entry.get("title")?.as_str()?.to_string(),
                artist: artist.get("name")?.as_str()?.to_string(),
                artist_id: artist.get("id")?.as_u64()?,
                album: entry
                    .get("album")
                    .and_then(|album| album.get("title"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                duration: entry.get("duration").and_then(Value::as_f64).map(|secs| secs as f32),
            })
        })
        .collect()
}

fn parse_artists(json: &Value) -> Vec<(u64, String)> {
    json.get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| Some((entry.get("id")?.as_u64()?, entry.get("name")?.as_str()?.to_string())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(title: &str, artist: &str) -> Seed {
        Seed {
            title: title.into(),
            artist: artist.into(),
        }
    }

    #[test]
    fn cleans_youtube_titles() {
        assert_eq!(
            clean_seed(&seed("Daft Punk - One More Time (Official Video)", "Daft Punk")),
            ("One More Time".into(), "Daft Punk".into())
        );
        assert_eq!(
            clean_seed(&seed("Genesis [HD]", "Justice - Topic")),
            ("Genesis".into(), "Justice".into())
        );
        assert_eq!(
            clean_seed(&seed("Strobe - Official Audio", "deadmau5VEVO")),
            ("Strobe".into(), "deadmau5".into())
        );
        assert_eq!(
            clean_seed(&seed("Midnight City", "")),
            ("Midnight City".into(), String::new())
        );
        // Un mot qui contient « hd » ou « audio » n'est pas coupé.
        assert_eq!(
            clean_seed(&seed("Childhood Audiomachine", "M83")),
            ("Childhood Audiomachine".into(), "M83".into())
        );
        assert_eq!(
            clean_seed(&seed("Midnight City - M83 (Lyrics)", "M83")),
            ("Midnight City".into(), "M83".into())
        );
    }

    #[test]
    fn parses_deezer_answers() {
        let json: Value = serde_json::from_str(
            r#"{"data":[
                {"id":1,"readable":true,"title":"One More Time","duration":320,
                 "artist":{"id":27,"name":"Daft Punk"},"album":{"title":"Discovery"}},
                {"id":2,"readable":false,"title":"Bloqué","artist":{"id":3,"name":"X"}}
            ]}"#,
        )
        .unwrap();
        let tracks = parse_tracks(&json);
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].artist_id, 27);
        assert_eq!(tracks[0].album, "Discovery");
        assert_eq!(tracks[0].duration, Some(320.0));
        let artists: Value = serde_json::from_str(r#"{"data":[{"id":6404,"name":"Justice"}]}"#).unwrap();
        assert_eq!(parse_artists(&artists), vec![(6404, "Justice".to_string())]);
    }

    #[test]
    fn merge_alternates_and_skips_duplicates() {
        let track = |id, title: &str, artist: &str| Track {
            id,
            title: title.into(),
            artist: artist.into(),
            artist_id: id,
            album: String::new(),
            duration: None,
        };
        let radio = vec![
            track(1, "One More Time", "Daft Punk"),
            track(2, "Around the World", "Daft Punk"),
            track(3, "D.A.N.C.E.", "Justice"),
        ];
        let tops = vec![
            vec![track(4, "Genesis", "Justice"), track(3, "D.A.N.C.E.", "Justice")],
            vec![track(5, "Strobe", "deadmau5")],
        ];
        let out = merge("Daft Punk", Some(1), radio, tops);
        let titles: Vec<&str> = out.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, vec!["Genesis", "Around the World", "Strobe", "D.A.N.C.E."]);
        assert_eq!(out[1].note, "même artiste");
        assert_eq!(out[0].note, "artiste proche");
        assert_eq!(out[0].url, "ytsearch1:Justice - Genesis");
    }

    #[test]
    fn encodes_queries() {
        assert_eq!(encode("artist:\"AC/DC\" é"), "artist%3A%22AC%2FDC%22%20%C3%A9");
    }
}
