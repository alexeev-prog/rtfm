use crate::trie::Trie;
use anyhow::{Result, anyhow};
use regex::Regex;
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task;

/// Man page database with caching
#[derive(Clone)]
pub struct ManDb {
    commands: Vec<String>,
    man_map: HashMap<String, String>,
    man_cache: Arc<Mutex<HashMap<String, Arc<Vec<String>>>>>,
    tldr_cache: Arc<Mutex<HashMap<String, Arc<Vec<String>>>>>, // New tldr cache
    trie: Arc<Trie>,
}

impl ManDb {
    /// Loads man database for specified section
    pub fn load(section: u8) -> Result<Self> {
        let (commands, man_map) = Self::load_man_k(section)?;
        let mut trie = Trie::new();

        for cmd in &commands {
            trie.insert(cmd);
        }

        Ok(Self {
            commands,
            man_map,
            man_cache: Arc::new(Mutex::new(HashMap::new())),
            tldr_cache: Arc::new(Mutex::new(HashMap::new())), // Initialize tldr cache
            trie: Arc::new(trie),
        })
    }

    /// Gets all commands
    pub fn get_commands(&self) -> &Vec<String> {
        &self.commands
    }

    /// Gets commands starting with prefix
    pub fn commands_starting_with(&self, prefix: &str) -> Vec<String> {
        self.trie.words_starting_with(prefix)
    }

    /// Displays man page in terminal
    pub fn display_man_page(&self, command: &str) -> Result<()> {
        Command::new("man")
            .arg(command)
            .stdout(Stdio::inherit())
            .status()?;
        Ok(())
    }

    /// Gets man page content (cached)
    pub async fn get_man_page(&self, command: &str) -> Arc<Vec<String>> {
        // Check cache
        {
            let cache = self.man_cache.lock().await;
            if let Some(content) = cache.get(command) {
                return content.clone();
            }
        }

        // Load man page
        let command_str = command.to_string();
        let content = task::spawn_blocking(move || {
            Self::load_man_page(&command_str)
                .unwrap_or_else(|_| vec![format!("Failed to load man page: {}", command_str)])
        })
        .await
        .unwrap();

        let content_arc = Arc::new(content);

        // Update cache
        let mut cache = self.man_cache.lock().await;
        cache.insert(command.to_string(), content_arc.clone());

        content_arc
    }

    /// Gets tldr page content (cached)
    pub async fn get_tldr_page(&self, command: &str) -> Arc<Vec<String>> {
        // Check cache
        {
            let cache = self.tldr_cache.lock().await;
            if let Some(content) = cache.get(command) {
                return content.clone();
            }
        }

        // Load tldr page
        let command_str = command.to_string();
        let content = task::spawn_blocking(move || {
            Self::load_tldr_page(&command_str)
                .unwrap_or_else(|_| vec![format!("Failed to load tldr page: {}", command_str)])
        })
        .await
        .unwrap();

        let content_arc = Arc::new(content);

        // Update cache
        let mut cache = self.tldr_cache.lock().await;
        cache.insert(command.to_string(), content_arc.clone());

        content_arc
    }

    /// Loads man page index
    fn load_man_k(section: u8) -> Result<(Vec<String>, HashMap<String, String>)> {
        let output = Command::new("man").arg("-k").arg(".").output()?;

        if !output.status.success() {
            return Err(anyhow!("Command failed"));
        }

        let output_str = String::from_utf8_lossy(&output.stdout);
        let mut man_map = HashMap::new();
        let mut commands = Vec::new();
        let re = Regex::new(r"\((\d)\)")?;

        for line in output_str.lines() {
            if let Some((name, desc)) = line.split_once(" - ") {
                let name_part = name.trim();

                // Extract section number
                let mut section_match = None;
                if let Some(caps) = re.captures(name_part) {
                    if let Some(sec) = caps.get(1) {
                        if sec.as_str().parse::<u8>().unwrap_or(0) == section {
                            section_match = Some(sec.as_str());
                        }
                    }
                }

                // Apply section filter
                if section_match.is_some() {
                    let cleaned_name = name_part
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string();

                    if !cleaned_name.is_empty() {
                        man_map.insert(cleaned_name.clone(), desc.trim().to_string());
                        commands.push(cleaned_name);
                    }
                }
            }
        }
        commands.sort_unstable();
        commands.dedup();
        Ok((commands, man_map))
    }

    pub fn get_description(&self, command: &str) -> Option<String> {
        self.man_map.get(command).cloned()
    }

    /// Loads man page content
    fn load_man_page(command: &str) -> Result<Vec<String>> {
        let output = Command::new("man")
            .arg(command)
            .env("PAGER", "cat")
            .output()?;

        if !output.status.success() {
            return Err(anyhow!("man command failed"));
        }

        let content = String::from_utf8(output.stdout)?;
        Ok(content.lines().map(|s| s.to_string()).collect())
    }

    /// Loads tldr page content
    fn load_tldr_page(command: &str) -> Result<Vec<String>> {
        let output = Command::new("tldr").arg(command).output()?;

        if !output.status.success() {
            return Err(anyhow!("tldr command failed"));
        }

        let content = String::from_utf8(output.stdout)?;
        Ok(content.lines().map(|s| s.to_string()).collect())
    }
}

#[cfg(test)]
mod man_db_tests {
    use super::*;

    use tokio::runtime::Runtime;

    #[test]
    fn test_cache_behavior() {
        let rt = Runtime::new().unwrap();
        let man_db = ManDb::load(1).unwrap();

        rt.block_on(async {
            let content = man_db.get_man_page("ls").await;
            assert!(!content.is_empty());

            let cached_content = man_db.get_man_page("ls").await;
            assert_eq!(content.len(), cached_content.len());

            let cache = man_db.man_cache.lock().await;
            assert!(cache.contains_key("ls"));
        });
    }
}
