use anyhow::{Context, Result};
use rand::rng;
use rand::seq::IndexedRandom;
use std::fs;
use std::path::Path;

pub fn generate_from_wordlist(path: &Path, count: usize) -> Result<(String, Vec<String>)> {
    let data = fs::read_to_string(path)
        .with_context(|| format!("failed to read wordlist at {}", path.display()))?;
    let words: Vec<&str> = data
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    if words.len() < count {
        anyhow::bail!("wordlist requires at least {count} entries");
    }
    let mut rng = rng();
    let mut selected = Vec::with_capacity(count);
    for _ in 0..count {
        let word = words
            .as_slice()
            .choose(&mut rng)
            .context("wordlist exhausted while generating password")?;
        selected.push((*word).to_string());
    }
    let password = selected.join("-");
    Ok((password, selected))
}
