use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use git2::{ObjectType, Repository, Signature};
use tokio::fs;
use tokio::task;

#[derive(Clone)]
pub struct ConfigRepository {
    inner: Arc<ConfigRepositoryInner>,
}

struct ConfigRepositoryInner {
    repo: Arc<std::sync::Mutex<Repository>>,
    config_path: PathBuf,
    rel_config_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ConfigTag {
    pub name: String,
    pub message: Option<String>,
}

impl ConfigRepository {
    pub fn open(config_path: &Path) -> Result<Self> {
        let config_path = config_path
            .canonicalize()
            .with_context(|| format!("failed to resolve path {}", config_path.display()))?;
        let repo = Repository::discover(&config_path).with_context(|| {
            format!(
                "failed to find git repository for {}",
                config_path.display()
            )
        })?;
        let workdir = repo
            .workdir()
            .map(Path::to_path_buf)
            .context("configuration repository must have a working directory")?;
        let rel_config_path = config_path
            .strip_prefix(&workdir)
            .with_context(|| {
                format!(
                    "configuration file {} is not within repository workdir {}",
                    config_path.display(),
                    workdir.display()
                )
            })?
            .to_path_buf();
        Ok(Self {
            inner: Arc::new(ConfigRepositoryInner {
                repo: Arc::new(std::sync::Mutex::new(repo)),
                config_path,
                rel_config_path,
            }),
        })
    }

    pub async fn list_tags(&self) -> Result<Vec<ConfigTag>> {
        let inner = self.inner.clone();
        task::spawn_blocking(move || -> Result<Vec<ConfigTag>> {
            let repo = inner
                .repo
                .lock()
                .map_err(|_| anyhow!("configuration repository lock poisoned"))?;
            let mut tags = Vec::new();
            let names = repo
                .tag_names(Some("config/*"))
                .context("failed to enumerate configuration tags")?;
            for name in names.iter().flatten() {
                let short = name.trim_start_matches("config/").to_string();
                let reference = repo
                    .find_reference(&format!("refs/tags/{name}"))
                    .with_context(|| format!("failed to resolve tag {name}"))?;
                let object = reference
                    .peel(ObjectType::Any)
                    .with_context(|| format!("failed to peel tag {name}"))?;
                let commit = object
                    .peel_to_commit()
                    .with_context(|| format!("tag {name} does not reference a commit"))?;
                let message = commit.summary().map(|s| s.to_string());
                tags.push(ConfigTag {
                    name: short,
                    message,
                });
            }
            tags.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(tags)
        })
        .await?
    }

    pub async fn detect_active_tag(&self) -> Result<Option<String>> {
        let current = fs::read_to_string(&self.inner.config_path)
            .await
            .context("failed to read active configuration")?;
        let tags = self.list_tags().await?;
        for tag in tags {
            if let Ok(content) = self.load_tag_yaml(&tag.name).await {
                if normalize_config(&content) == normalize_config(&current) {
                    return Ok(Some(tag.name));
                }
            }
        }
        Ok(None)
    }

    pub async fn load_tag_yaml(&self, tag: &str) -> Result<String> {
        let inner = self.inner.clone();
        let tag = tag.to_string();
        task::spawn_blocking(move || -> Result<String> {
            let repo = inner
                .repo
                .lock()
                .map_err(|_| anyhow!("configuration repository lock poisoned"))?;
            let reference = repo
                .resolve_reference_from_short_name(&format!("config/{tag}"))
                .with_context(|| format!("unknown configuration tag {tag}"))?;
            let object = reference
                .peel(ObjectType::Commit)
                .with_context(|| format!("tag config/{tag} does not resolve to a commit"))?;
            let commit = object
                .peel_to_commit()
                .with_context(|| format!("tag config/{tag} is not a commit"))?;
            let tree = commit.tree()?;
            let entry = tree.get_path(&inner.rel_config_path)?;
            let blob = repo.find_blob(entry.id())?;
            let content = std::str::from_utf8(blob.content())
                .context("configuration blob is not valid UTF-8")?
                .to_string();
            Ok(content)
        })
        .await?
    }

    pub async fn make_active(&self, tag: &str) -> Result<()> {
        let content = self.load_tag_yaml(tag).await?;
        self.write_active(&content).await
    }

    pub async fn write_active(&self, content: &str) -> Result<()> {
        fs::write(&self.inner.config_path, content)
            .await
            .context("failed to write configuration file")
    }

    pub async fn commit_and_tag(&self, tag: &str, content: &str) -> Result<()> {
        self.write_active(content).await?;
        let inner = self.inner.clone();
        let tag = tag.to_string();
        task::spawn_blocking(move || -> Result<()> {
            let repo = inner
                .repo
                .lock()
                .map_err(|_| anyhow!("configuration repository lock poisoned"))?;
            let mut index = repo.index()?;
            index.add_path(&inner.rel_config_path)?;
            index.write()?;
            let tree_id = index.write_tree()?;
            let tree = repo.find_tree(tree_id)?;
            let sig = repo
                .signature()
                .or_else(|_| Signature::now("Photo Frame", "photo-frame@local"))?;
            let head = repo.head();
            let parents = match head {
                Ok(head) => {
                    let commit = head.peel_to_commit()?;
                    vec![commit]
                }
                Err(_) => Vec::new(),
            };
            let message = format!("Update configuration tag {tag}");
            let parent_refs: Vec<_> = parents.iter().collect();
            let commit_id = repo.commit(Some("HEAD"), &sig, &sig, &message, &tree, &parent_refs)?;
            let object = repo.find_object(commit_id, None)?;
            repo.tag(&format!("config/{tag}"), &object, &sig, &message, true)?;
            Ok(())
        })
        .await??;
        Ok(())
    }

    pub async fn delete_tag(&self, tag: &str) -> Result<()> {
        let inner = self.inner.clone();
        let tag = tag.to_string();
        task::spawn_blocking(move || -> Result<()> {
            let repo = inner
                .repo
                .lock()
                .map_err(|_| anyhow!("configuration repository lock poisoned"))?;
            repo.tag_delete(&format!("config/{tag}"))?;
            Ok(())
        })
        .await??;
        Ok(())
    }
}

fn normalize_config(text: &str) -> String {
    let unix = text.replace("\r\n", "\n").replace('\r', "\n");
    unix.trim().to_string()
}
