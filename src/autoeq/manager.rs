// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! AutoEQ manager for fetching and caching profiles.

use super::{AutoEQError, AutoEQProfile, AutoEQProfileMetadata, Result};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

const INDEX_CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60); // 7 days
const PROFILE_CACHE_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60); // 30 days

/// Cap on the AutoEQ INDEX.md body. The real file is a few hundred KB; a
/// hostile or broken mirror returning an unbounded body must not be read
/// into memory in full.
const MAX_INDEX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// Cap on a single FixedBandEQ.txt profile body (normally well under 1 KB).
const MAX_PROFILE_RESPONSE_BYTES: usize = 256 * 1024;

/// Reads an async HTTP response body as UTF-8 text, capped at `max_bytes`.
/// Uses `Response::chunk()` (always available, no `stream` feature
/// needed) so a hostile or misbehaving server can't exhaust memory with
/// an unbounded body.
async fn read_capped_text(mut response: reqwest::Response, max_bytes: usize) -> Result<String> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        body.extend_from_slice(&chunk);
        if body.len() > max_bytes {
            return Err(AutoEQError::InvalidFormat(format!(
                "Response exceeded {max_bytes} byte limit"
            )));
        }
    }
    String::from_utf8(body)
        .map_err(|e| AutoEQError::InvalidFormat(format!("Response was not valid UTF-8: {e}")))
}

/// Manager for fetching and caching AutoEQ profiles.
pub struct AutoEQManager {
    cache_dir: PathBuf,
    http_client: reqwest::Client,
    memory_cache: HashMap<String, CacheEntry>,
    lru_order: VecDeque<String>,
    index_cache: Option<Vec<AutoEQProfileMetadata>>,
}

struct CacheEntry {
    profile: AutoEQProfile,
    last_accessed: std::time::Instant,
}

impl AutoEQManager {
    /// Create a new AutoEQ manager.
    pub fn new(cache_dir: PathBuf, timeout: Duration) -> Result<Self> {
        let http_client = reqwest::Client::builder().timeout(timeout).build()?;

        // Ensure cache directory exists
        if !cache_dir.exists() {
            std::fs::create_dir_all(&cache_dir)?;
        }

        Ok(Self {
            cache_dir,
            http_client,
            memory_cache: HashMap::new(),
            lru_order: VecDeque::new(),
            index_cache: None,
        })
    }

    /// Fetch the AutoEQ index (list of all profiles).
    pub async fn fetch_index(&mut self) -> Result<Vec<AutoEQProfileMetadata>> {
        // Check memory cache first
        if let Some(ref profiles) = self.index_cache {
            return Ok(profiles.clone());
        }

        // Check disk cache
        if let Some(profiles) = self.load_index_from_disk()? {
            self.index_cache = Some(profiles.clone());
            return Ok(profiles);
        }

        // Fetch from network
        let url = "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/results/INDEX.md";
        let response = self.http_client.get(url).send().await?;

        if !response.status().is_success() {
            return Err(AutoEQError::Network(
                response.error_for_status().unwrap_err(),
            ));
        }

        let content = read_capped_text(response, MAX_INDEX_RESPONSE_BYTES).await?;
        let profiles = super::parser::parse_index(&content)?;

        // Cache in memory
        self.index_cache = Some(profiles.clone());

        // Save to disk cache (ignore errors to not block on I/O issues)
        if let Err(e) = self.save_index_to_disk(&profiles) {
            eprintln!("Warning: Failed to save index to disk cache: {}", e);
        }

        Ok(profiles)
    }

    /// Fetch a specific AutoEQ profile by path.
    pub async fn fetch_profile(&mut self, path: &str) -> Result<AutoEQProfile> {
        // Check memory cache first
        if let Some(entry) = self.memory_cache.get(path) {
            let profile = entry.profile.clone();
            // Update access time and LRU in separate scope
            if let Some(entry) = self.memory_cache.get_mut(path) {
                entry.last_accessed = std::time::Instant::now();
            }
            self.update_lru(path);
            return Ok(profile);
        }

        // Check disk cache
        if let Some(profile) = self.load_profile_from_disk(path)? {
            self.add_to_memory_cache(path.to_string(), profile.clone());
            return Ok(profile);
        }

        // Fetch from network
        let profile = self.fetch_profile_from_network(path).await?;

        // Add to memory cache
        self.add_to_memory_cache(path.to_string(), profile.clone());

        // Save to disk cache (ignore errors to not block on I/O issues)
        if let Err(e) = self.save_profile_to_disk(path, &profile) {
            eprintln!("Warning: Failed to save profile to disk cache: {}", e);
        }

        Ok(profile)
    }

    async fn fetch_profile_from_network(&self, path: &str) -> Result<AutoEQProfile> {
        // URL-decode and construct the profile URL
        let decoded_path = urlencoding::decode(path)
            .map_err(|e| AutoEQError::InvalidFormat(format!("Invalid path encoding: {}", e)))?;

        let path_components: Vec<&str> = decoded_path.split('/').collect();
        if path_components.is_empty() {
            return Err(AutoEQError::InvalidFormat("Empty path".to_string()));
        }

        let headphone_name = path_components.last().unwrap();
        let encoded_path = path_components
            .iter()
            .map(|c| urlencoding::encode(c))
            .collect::<Vec<_>>()
            .join("/");

        let filename = format!("{} FixedBandEQ.txt", headphone_name);
        let encoded_filename = urlencoding::encode(&filename);

        let url = format!(
            "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/results/{}/{}",
            encoded_path, encoded_filename
        );

        let response = self.http_client.get(&url).send().await?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(AutoEQError::ProfileNotFound(path.to_string()));
        }

        if !response.status().is_success() {
            return Err(AutoEQError::Network(
                response.error_for_status().unwrap_err(),
            ));
        }

        let content = read_capped_text(response, MAX_PROFILE_RESPONSE_BYTES).await?;
        super::parser::parse_fixed_band_eq(path, &content)
    }

    fn add_to_memory_cache(&mut self, path: String, profile: AutoEQProfile) {
        const MAX_CACHE_SIZE: usize = 20;

        // Evict LRU if at capacity
        if self.memory_cache.len() >= MAX_CACHE_SIZE
            && !self.memory_cache.contains_key(&path)
            && let Some(lru_key) = self.lru_order.pop_front()
        {
            self.memory_cache.remove(&lru_key);
        }

        // Add or update entry
        self.memory_cache.insert(
            path.clone(),
            CacheEntry {
                profile,
                last_accessed: std::time::Instant::now(),
            },
        );

        // Update LRU
        self.lru_order.retain(|k| k != &path);
        self.lru_order.push_back(path);
    }

    fn update_lru(&mut self, path: &str) {
        self.lru_order.retain(|k| k != path);
        self.lru_order.push_back(path.to_string());
    }

    // Disk cache functions

    fn save_index_to_disk(&self, profiles: &[AutoEQProfileMetadata]) -> Result<()> {
        let cache_path = self.cache_dir.join("index.json");
        let cache_data = CachedIndex {
            timestamp: SystemTime::now(),
            profiles: profiles.to_vec(),
        };
        let json = serde_json::to_string_pretty(&cache_data)?;
        write_cache_atomically(&cache_path, &json)?;
        Ok(())
    }

    fn load_index_from_disk(&self) -> Result<Option<Vec<AutoEQProfileMetadata>>> {
        let cache_path = self.cache_dir.join("index.json");
        if !cache_path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(cache_path)?;
        // A corrupt/truncated cache file (e.g. left behind by a crash or
        // unclean shutdown mid-write) is a cache miss, not a fatal error:
        // fall through to a fresh fetch instead of permanently failing.
        let Ok(cache_data) = serde_json::from_str::<CachedIndex>(&content) else {
            return Ok(None);
        };

        // Check TTL
        let age = SystemTime::now()
            .duration_since(cache_data.timestamp)
            .unwrap_or(Duration::MAX);

        if age > INDEX_CACHE_TTL {
            return Ok(None);
        }

        Ok(Some(cache_data.profiles))
    }

    fn save_profile_to_disk(&self, path: &str, profile: &AutoEQProfile) -> Result<()> {
        let filename = self.cache_filename(path);
        let cache_path = self.cache_dir.join(filename);

        let cache_data = CachedProfile {
            timestamp: SystemTime::now(),
            profile: profile.clone(),
        };

        let json = serde_json::to_string_pretty(&cache_data)?;
        write_cache_atomically(&cache_path, &json)?;
        Ok(())
    }

    fn load_profile_from_disk(&self, path: &str) -> Result<Option<AutoEQProfile>> {
        let filename = self.cache_filename(path);
        let cache_path = self.cache_dir.join(filename);

        if !cache_path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(cache_path)?;
        // See `load_index_from_disk`: a corrupt cache file is a miss, not
        // a fatal error.
        let Ok(cache_data) = serde_json::from_str::<CachedProfile>(&content) else {
            return Ok(None);
        };

        // Check TTL
        let age = SystemTime::now()
            .duration_since(cache_data.timestamp)
            .unwrap_or(Duration::MAX);

        if age > PROFILE_CACHE_TTL {
            return Ok(None);
        }

        Ok(Some(cache_data.profile))
    }

    fn cache_filename(&self, path: &str) -> String {
        let hash = format!("{:x}", md5::compute(path.as_bytes()));
        format!("{}.json", hash)
    }
}

/// Writes `content` to `path` via a temp-file-then-rename, so a crash or
/// power loss mid-write can never leave a truncated/corrupt cache file in
/// `path`'s place (`rename` is atomic on the same filesystem).
fn write_cache_atomically(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, content)?;
    std::fs::rename(&tmp_path, path)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedIndex {
    timestamp: SystemTime,
    profiles: Vec<AutoEQProfileMetadata>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedProfile {
    timestamp: SystemTime,
    profile: AutoEQProfile,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cache_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lyra-autoeq-test-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn corrupt_index_cache_is_treated_as_a_miss_not_an_error() {
        // A truncated/corrupt cache file — e.g. left behind by a crash
        // mid-write — must fall back to a fresh fetch, not permanently
        // break the feature with a propagated parse error.
        let dir = temp_cache_dir("index");
        std::fs::write(dir.join("index.json"), b"not valid json{{{").unwrap();
        let manager = AutoEQManager::new(dir, Duration::from_secs(5)).unwrap();
        assert!(matches!(manager.load_index_from_disk(), Ok(None)));
    }

    #[test]
    fn corrupt_profile_cache_is_treated_as_a_miss_not_an_error() {
        let dir = temp_cache_dir("profile");
        let manager = AutoEQManager::new(dir.clone(), Duration::from_secs(5)).unwrap();
        let filename = manager.cache_filename("oratory1990/over-ear/Test");
        std::fs::write(dir.join(filename), b"not valid json{{{").unwrap();
        assert!(matches!(
            manager.load_profile_from_disk("oratory1990/over-ear/Test"),
            Ok(None)
        ));
    }
}
