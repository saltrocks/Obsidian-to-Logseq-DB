//! Thin wrapper around the `logseq` CLI used to create and import DB graphs.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use uuid::Uuid;

use crate::edn::Edn;
use crate::transit;

pub struct LogseqCli {
    bin: String,
    root_dir: Option<PathBuf>,
    timeout_ms: u64,
}

impl LogseqCli {
    pub fn new(bin: String, root_dir: Option<PathBuf>, timeout_ms: u64) -> Self {
        LogseqCli {
            bin,
            root_dir,
            timeout_ms,
        }
    }

    fn run(&self, args: &[&str]) -> Result<Value> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args).args(["--output", "json"]);
        cmd.args(["--timeout-ms", &self.timeout_ms.to_string()]);
        if let Some(root) = &self.root_dir {
            cmd.arg("--root-dir").arg(root);
        }
        let output = cmd.output().with_context(|| {
            format!(
                "failed to run `{}` (is the Logseq CLI installed?)",
                self.bin
            )
        })?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json = stdout
            .lines()
            .rev()
            .find_map(|l| {
                let l = l.trim();
                l.starts_with("{\"status\"")
                    .then(|| serde_json::from_str::<Value>(l).ok())
                    .flatten()
            })
            .ok_or_else(|| {
                anyhow!(
                    "unexpected output from `{} {}`:\n{}{}",
                    self.bin,
                    args.join(" "),
                    stdout,
                    String::from_utf8_lossy(&output.stderr)
                )
            })?;
        if json["status"] != "ok" {
            let err = &json["error"];
            bail!(
                "`logseq {}` failed: {} ({})",
                args.join(" "),
                err["message"].as_str().unwrap_or("unknown error"),
                err["code"].as_str().unwrap_or("?")
            );
        }
        Ok(json["data"].clone())
    }

    pub fn root_dir(&self) -> PathBuf {
        self.root_dir.clone().unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join("logseq")
        })
    }

    pub fn graph_exists(&self, graph: &str) -> Result<bool> {
        let data = self.run(&["graph", "list"])?;
        Ok(data["graphs"]
            .as_array()
            .is_some_and(|gs| gs.iter().any(|g| g == graph)))
    }

    pub fn graph_dir(&self, graph: &str) -> Result<PathBuf> {
        let data = self.run(&["graph", "list"])?;
        let dir = data["graph-items"]
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|i| i["graph-name"] == graph)
                    .and_then(|i| i["graph-dir"].as_str())
            })
            .unwrap_or(graph)
            .to_string();
        Ok(self.root_dir().join("graphs").join(dir))
    }

    pub fn create_graph(&self, graph: &str) -> Result<()> {
        self.run(&["graph", "create", "--graph", graph]).map(|_| ())
    }

    pub fn remove_graph(&self, graph: &str) -> Result<()> {
        self.run(&["graph", "remove", "--graph", graph]).map(|_| ())
    }

    /// Titles of all pages (including tags and properties) in the graph.
    pub fn page_titles(&self, graph: &str) -> Result<HashSet<String>> {
        let data = self.run(&[
            "query",
            "--graph",
            graph,
            "--query",
            "[:find [?t ...] :where [?p :block/name] [?p :block/title ?t]]",
        ])?;
        Ok(data["result"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn start_server(&self, graph: &str) -> Result<()> {
        self.run(&["server", "start", "--graph", graph]).map(|_| ())
    }

    pub fn stop_server(&self, graph: &str) -> Result<()> {
        self.run(&["server", "stop", "--graph", graph]).map(|_| ())
    }

    /// Returns (repo, base URL) of the running db-worker for `graph`.
    pub fn server(&self, graph: &str) -> Result<(String, String)> {
        let data = self.run(&["server", "list"])?;
        let server = data["servers"]
            .as_array()
            .and_then(|servers| servers.iter().find(|s| s["graph"] == graph))
            .ok_or_else(|| anyhow!("no running db-worker found for graph {graph:?}"))?;
        let repo = server["repo"].as_str().unwrap_or_default().to_string();
        let url = server["base-url"].as_str().unwrap_or_default().to_string();
        if repo.is_empty() || url.is_empty() {
            bail!("db-worker for graph {graph:?} has no repo or URL: {server}");
        }
        Ok((repo, url))
    }

    /// Imports an EDN export map by calling the db-worker's `import-edn`
    /// thread API directly. `logseq graph import` does the same, but its EDN
    /// reader is quadratic in the file size and it ignores import errors.
    /// The whole graph is validated afterwards.
    pub fn import_edn(&self, base_url: &str, repo: &str, export: &Edn) -> Result<()> {
        let args = Edn::Vector(vec![Edn::str(repo), export.clone()]);
        let result = self.invoke(base_url, "thread-api/import-edn", &args)?;
        check_transit_error(&result)
    }

    /// Imports an EDN export map as an outliner `:batch-import-edn` op that
    /// only validates the entities it touches, which keeps follow-up imports
    /// into a large graph fast.
    pub fn batch_import_edn(&self, base_url: &str, repo: &str, export: &Edn) -> Result<()> {
        let op = Edn::Vector(vec![
            Edn::kw("batch-import-edn"),
            Edn::Vector(vec![
                export.clone(),
                Edn::Map(vec![(Edn::kw("validate-scope"), Edn::kw("tx"))]),
            ]),
        ]);
        let args = Edn::Vector(vec![
            Edn::str(repo),
            Edn::Vector(vec![op]),
            Edn::Map(vec![]),
        ]);
        let result = self.invoke(base_url, "thread-api/apply-outliner-ops", &args)?;
        check_transit_error(&result)
    }

    /// Calls a db-worker thread API and returns its transit-JSON result.
    fn invoke(&self, base_url: &str, method: &str, args: &Edn) -> Result<Value> {
        let body = serde_json::json!({
            "method": method,
            "argsTransit": transit::to_transit_json(args),
        })
        .to_string();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_millis(self.timeout_ms)))
            .http_status_as_error(false)
            .build()
            .into();
        let mut response = agent
            .post(format!("{}/v1/invoke", base_url.trim_end_matches('/')))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .send(body)
            .context("request to the db-worker failed")?;
        let status = response.status();
        let text = response
            .body_mut()
            .with_config()
            .limit(64 * 1024 * 1024)
            .read_to_string()
            .context("cannot read db-worker response")?;
        if !status.is_success() {
            bail!("db-worker returned HTTP {status}: {text}");
        }
        let json: Value = serde_json::from_str(&text)
            .with_context(|| format!("unexpected db-worker response: {text}"))?;
        Ok(match json["resultTransit"].as_str() {
            Some(t) => serde_json::from_str(t).unwrap_or(Value::Null),
            None => Value::Null,
        })
    }

    pub fn entity_exists(&self, graph: &str, uuid: Uuid) -> Result<bool> {
        let query = format!("[:find ?e . :where [?e :block/uuid #uuid \"{uuid}\"]]");
        let data = self.run(&["query", "--graph", graph, "--query", &query])?;
        Ok(!data["result"].is_null())
    }

    pub fn count(&self, graph: &str, query: &str) -> Result<i64> {
        let data = self.run(&["query", "--graph", graph, "--query", query])?;
        Ok(data["result"].as_i64().unwrap_or(0))
    }
}

/// Fails if a transit result contains an `:error` entry, as returned by the
/// import APIs when Logseq rejects the data.
fn check_transit_error(result: &Value) -> Result<()> {
    fn find(v: &Value) -> Option<String> {
        let items = v.as_array()?;
        if let Some(i) = items.iter().position(|x| x == "~:error")
            && let Some(msg) = items.get(i + 1).and_then(Value::as_str)
        {
            return Some(msg.to_string());
        }
        items.iter().find_map(find)
    }
    match find(result) {
        Some(msg) => bail!("{msg}"),
        None => Ok(()),
    }
}

/// Extracts the most recent import validation error from the graph's
/// db-worker logs. `logseq graph import` reports success even when the
/// import transaction was rejected, so this is the only place it shows up.
pub fn find_import_error(graph_dir: &Path) -> Option<String> {
    let mut logs: Vec<PathBuf> = fs::read_dir(graph_dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("db-worker-node") && n.ends_with(".log"))
        })
        .collect();
    logs.sort();
    let content = fs::read_to_string(logs.last()?).ok()?;
    let start = content.rfind("validation error")?;
    let line_start = content[..start].rfind('\n').map_or(0, |i| i + 1);
    let excerpt: Vec<&str> = content[line_start..]
        .lines()
        .take(60)
        .map(|l| match l.find("[stdout] ") {
            Some(i) => &l[i + 9..],
            None => l,
        })
        .collect();
    Some(excerpt.join("\n"))
}
