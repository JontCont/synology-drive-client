#![allow(dead_code)]

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteEntryKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMetadata {
    pub path: String,
    pub kind: RemoteEntryKind,
    pub size: u64,
    pub etag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteError {
    NotFound,
    AlreadyExists,
    NotDirectory,
    IsDirectory,
}

pub trait RemoteProvider {
    fn list(&self, path: &str) -> Result<Vec<RemoteMetadata>, RemoteError>;
    fn stat(&self, path: &str) -> Result<RemoteMetadata, RemoteError>;
    fn read_file(&self, path: &str) -> Result<Vec<u8>, RemoteError>;
    fn write_file(&mut self, path: &str, content: Vec<u8>) -> Result<(), RemoteError>;
    fn delete(&mut self, path: &str) -> Result<(), RemoteError>;
    fn move_item(&mut self, from: &str, to: &str) -> Result<(), RemoteError>;
    fn mkdir(&mut self, path: &str) -> Result<(), RemoteError>;
}

#[derive(Debug, Clone)]
enum FakeNode {
    Directory,
    File { content: Vec<u8> },
}

#[derive(Debug, Default)]
pub struct FakeRemote {
    nodes: HashMap<String, FakeNode>,
}

impl FakeRemote {
    pub fn new() -> Self {
        let mut nodes = HashMap::new();
        nodes.insert("/".to_string(), FakeNode::Directory);
        Self { nodes }
    }

    fn normalized(path: &str) -> String {
        if path.is_empty() || path == "/" {
            return "/".to_string();
        }

        let mut p = path.trim().replace('\\', "/");
        if !p.starts_with('/') {
            p.insert(0, '/');
        }
        while p.ends_with('/') && p.len() > 1 {
            p.pop();
        }
        p
    }

    fn parent(path: &str) -> Option<String> {
        if path == "/" {
            return None;
        }
        let mut parts: Vec<&str> = path.split('/').filter(|it| !it.is_empty()).collect();
        parts.pop();
        if parts.is_empty() {
            Some("/".to_string())
        } else {
            Some(format!("/{}", parts.join("/")))
        }
    }

    fn ensure_parent_dir(&self, path: &str) -> Result<(), RemoteError> {
        let parent = Self::parent(path).ok_or(RemoteError::NotDirectory)?;
        match self.nodes.get(&parent) {
            Some(FakeNode::Directory) => Ok(()),
            Some(FakeNode::File { .. }) => Err(RemoteError::NotDirectory),
            None => Err(RemoteError::NotFound),
        }
    }

    fn as_metadata(path: &str, node: &FakeNode) -> RemoteMetadata {
        match node {
            FakeNode::Directory => RemoteMetadata {
                path: path.to_string(),
                kind: RemoteEntryKind::Directory,
                size: 0,
                etag: None,
            },
            FakeNode::File { content } => RemoteMetadata {
                path: path.to_string(),
                kind: RemoteEntryKind::File,
                size: content.len() as u64,
                etag: Some(format!("{}-{}", path, content.len())),
            },
        }
    }
}

impl RemoteProvider for FakeRemote {
    fn list(&self, path: &str) -> Result<Vec<RemoteMetadata>, RemoteError> {
        let normalized = Self::normalized(path);
        match self.nodes.get(&normalized) {
            Some(FakeNode::Directory) => {}
            Some(FakeNode::File { .. }) => return Err(RemoteError::NotDirectory),
            None => return Err(RemoteError::NotFound),
        }

        let mut result = Vec::new();
        for (node_path, node) in &self.nodes {
            if node_path == &normalized {
                continue;
            }

            let parent = Self::parent(node_path);
            if parent.as_deref() == Some(normalized.as_str()) {
                result.push(Self::as_metadata(node_path, node));
            }
        }
        result.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(result)
    }

    fn stat(&self, path: &str) -> Result<RemoteMetadata, RemoteError> {
        let normalized = Self::normalized(path);
        self.nodes
            .get(&normalized)
            .map(|node| Self::as_metadata(&normalized, node))
            .ok_or(RemoteError::NotFound)
    }

    fn read_file(&self, path: &str) -> Result<Vec<u8>, RemoteError> {
        let normalized = Self::normalized(path);
        match self.nodes.get(&normalized) {
            Some(FakeNode::File { content }) => Ok(content.clone()),
            Some(FakeNode::Directory) => Err(RemoteError::IsDirectory),
            None => Err(RemoteError::NotFound),
        }
    }

    fn write_file(&mut self, path: &str, content: Vec<u8>) -> Result<(), RemoteError> {
        let normalized = Self::normalized(path);
        self.ensure_parent_dir(&normalized)?;
        if matches!(self.nodes.get(&normalized), Some(FakeNode::Directory)) {
            return Err(RemoteError::IsDirectory);
        }
        self.nodes.insert(normalized, FakeNode::File { content });
        Ok(())
    }

    fn delete(&mut self, path: &str) -> Result<(), RemoteError> {
        let normalized = Self::normalized(path);
        if normalized == "/" {
            return Err(RemoteError::IsDirectory);
        }
        let node = self.nodes.get(&normalized).ok_or(RemoteError::NotFound)?;
        if matches!(node, FakeNode::Directory) {
            let has_children = self
                .nodes
                .keys()
                .any(|key| Self::parent(key).as_deref() == Some(normalized.as_str()));
            if has_children {
                return Err(RemoteError::IsDirectory);
            }
        }
        self.nodes.remove(&normalized);
        Ok(())
    }

    fn move_item(&mut self, from: &str, to: &str) -> Result<(), RemoteError> {
        let source = Self::normalized(from);
        let target = Self::normalized(to);
        if source == "/" || target == "/" {
            return Err(RemoteError::NotDirectory);
        }
        if self.nodes.contains_key(&target) {
            return Err(RemoteError::AlreadyExists);
        }
        self.ensure_parent_dir(&target)?;
        let node = self.nodes.remove(&source).ok_or(RemoteError::NotFound)?;
        self.nodes.insert(target, node);
        Ok(())
    }

    fn mkdir(&mut self, path: &str) -> Result<(), RemoteError> {
        let normalized = Self::normalized(path);
        if self.nodes.contains_key(&normalized) {
            return Err(RemoteError::AlreadyExists);
        }
        self.ensure_parent_dir(&normalized)?;
        self.nodes.insert(normalized, FakeNode::Directory);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{FakeRemote, RemoteProvider};

    #[test]
    fn creates_and_reads_file() {
        let mut remote = FakeRemote::new();
        remote.mkdir("/sync").expect("mkdir /sync should pass");
        remote
            .write_file("/sync/a.txt", b"hello".to_vec())
            .expect("write_file should pass");
        let content = remote
            .read_file("/sync/a.txt")
            .expect("read_file should pass");
        assert_eq!(content, b"hello".to_vec());
    }

    #[test]
    fn moves_file() {
        let mut remote = FakeRemote::new();
        remote.mkdir("/sync").expect("mkdir /sync should pass");
        remote
            .write_file("/sync/a.txt", b"x".to_vec())
            .expect("write_file should pass");
        remote
            .move_item("/sync/a.txt", "/sync/b.txt")
            .expect("move_item should pass");

        assert!(remote.read_file("/sync/a.txt").is_err());
        assert_eq!(
            remote.read_file("/sync/b.txt").expect("b.txt should exist"),
            b"x".to_vec()
        );
    }
}
