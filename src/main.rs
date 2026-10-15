//! obsidian-to-logseq: converts an Obsidian vault into a Logseq DB graph.
//!
//! The vault is converted to Logseq's EDN graph import format. With
//! `--graph`, the `logseq` CLI is used to create the DB graph, import the EDN
//! and install the assets; otherwise the EDN and an assets folder are written
//! for a manual import.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::Parser;

use obsidian_to_logseq::convert::{self, Conversion, ConvertOptions};
use obsidian_to_logseq::logseq::{self, LogseqCli};
use obsidian_to_logseq::phases;
use obsidian_to_logseq::vault::Vault;

#[derive(Parser, Debug)]
#[command(version, about = "Convert an Obsidian vault into a Logseq DB graph")]
struct Args {
    /// Path to the Obsidian vault
    vault: PathBuf,

    /// Create this Logseq DB graph and import the vault into it via the `logseq` CLI
    #[arg(short, long)]
    graph: Option<String>,

    /// Delete and recreate the graph if it already exists (with --graph)
    #[arg(long, requires = "graph")]
    replace: bool,

    /// Write the EDN to this file (default: <vault name>.edn; with --graph it is
    /// only written when this is given or the import fails)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Directory for converted assets when not using --graph
    /// (default: <output name>-assets next to the EDN file)
    #[arg(long, conflicts_with = "graph")]
    assets_dir: Option<PathBuf>,

    /// Daily note file name format (moment.js syntax); default from
    /// .obsidian/daily-notes.json, else YYYY-MM-DD
    #[arg(long)]
    daily_format: Option<String>,

    /// Daily notes folder, used when the format contains `/`
    #[arg(long)]
    daily_folder: Option<String>,

    /// Don't convert daily notes into Logseq journal pages
    #[arg(long)]
    no_journals: bool,

    /// Keep blocks flat instead of nesting content under headings
    #[arg(long)]
    flat: bool,

    /// Convert #tags into page references instead of Logseq tags (classes)
    #[arg(long)]
    tags_as_pages: bool,

    /// Keep %%comments%% instead of removing them
    #[arg(long)]
    keep_comments: bool,

    /// Vault-relative file or folder to skip (repeatable)
    #[arg(long, value_name = "PATH")]
    exclude: Vec<String>,

    /// Maximum number of blocks per import request (with --graph)
    #[arg(long, default_value_t = 15_000, value_parser = clap::value_parser!(u64).range(1..))]
    chunk_blocks: u64,

    /// Path to the logseq CLI executable
    #[arg(long, default_value = "logseq")]
    logseq_bin: String,

    /// Logseq CLI root dir (default: ~/logseq)
    #[arg(long)]
    root_dir: Option<PathBuf>,

    /// Timeout for logseq CLI and db-worker requests in milliseconds
    #[arg(long, default_value_t = 7_200_000)]
    timeout_ms: u64,

    /// Print all warnings
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<()> {
    let mut vault = Vault::scan(&args.vault, &args.exclude)?;
    if !args.no_journals {
        vault.load_daily_config(args.daily_format.as_deref(), args.daily_folder.as_deref())?;
    }
    eprintln!(
        "Scanned {}: {} notes, {} attachments",
        vault.root.display(),
        vault.notes.len(),
        vault.attachments.len()
    );
    match &args.graph {
        Some(graph) => import_into_graph(args, &vault, graph),
        None => write_edn_only(args, &vault),
    }
}

fn options(args: &Args, reserved_titles: HashSet<String>) -> ConvertOptions {
    ConvertOptions {
        nest_under_headings: !args.flat,
        keep_comments: args.keep_comments,
        tags_as_pages: args.tags_as_pages,
        journals: !args.no_journals,
        reserved_titles: reserved_titles
            .into_iter()
            .map(|t| t.to_lowercase())
            .collect(),
    }
}

fn write_edn_only(args: &Args, vault: &Vault) -> Result<()> {
    let conversion = convert::convert(vault, &options(args, convert::bundled_reserved_titles()))?;
    let output = args.output.clone().unwrap_or_else(|| default_output(vault));
    write_edn(&conversion, &output)?;
    let assets_dir = args.assets_dir.clone().unwrap_or_else(|| {
        let stem = output.file_stem().unwrap_or_default().to_string_lossy();
        output.with_file_name(format!("{stem}-assets"))
    });
    if !conversion.assets.is_empty() {
        copy_assets(&conversion, &assets_dir)?;
    }
    report(args, &conversion);
    println!("\nWrote {}", output.display());
    println!("Import it with the Logseq CLI:");
    println!(
        "  logseq graph import --graph <name> --type edn --input {}",
        output.display()
    );
    println!("or in the app via \"Import\" > \"EDN to DB graph\".");
    if !conversion.assets.is_empty() {
        println!(
            "Then copy the {} files from {} into the graph's assets folder\n  (~/logseq/graphs/<name>/assets/).",
            conversion.assets.len(),
            assets_dir.display()
        );
    }
    Ok(())
}

fn import_into_graph(args: &Args, vault: &Vault, graph: &str) -> Result<()> {
    let cli = LogseqCli::new(
        args.logseq_bin.clone(),
        args.root_dir.clone(),
        args.timeout_ms,
    );
    if cli.graph_exists(graph)? {
        if !args.replace {
            bail!("graph {graph:?} already exists; pass --replace to recreate it");
        }
        eprintln!("Removing existing graph {graph:?}");
        cli.remove_graph(graph)?;
    }
    eprintln!("Creating graph {graph:?}");
    cli.create_graph(graph)?;
    // CLI commands from here on start a db-worker for the graph; stop it on
    // every exit so a failed run doesn't leave the graph locked.
    let _worker = WorkerGuard { cli: &cli, graph };
    let reserved = cli
        .page_titles(graph)
        .context("failed to read built-in page titles from the new graph")?;
    let conversion = convert::convert(vault, &options(args, reserved))?;

    if let Some(output) = &args.output {
        write_edn(&conversion, output)?;
    }
    report(args, &conversion);

    eprintln!("Importing into graph {graph:?}...");
    cli.start_server(graph)?;
    let (repo, url) = cli.server(graph)?;
    let graph_dir = cli.graph_dir(graph)?;
    let steps = phases::split(&conversion.edn, args.chunk_blocks as usize);
    for (i, step) in steps.iter().enumerate() {
        let label = &step.label;
        eprint!("  importing {label}...");
        let started = Instant::now();
        // The first import validates the whole (still small) graph; later
        // ones only validate what they touch.
        let result = if i == 0 {
            cli.import_edn(&url, &repo, &step.edn)
        } else {
            cli.batch_import_edn(&url, &repo, &step.edn)
        };
        eprintln!(" {:.1}s", started.elapsed().as_secs_f64());
        if let Err(e) = result {
            let details = logseq::find_import_error(&graph_dir).unwrap_or_default();
            let kept = match &args.output {
                Some(p) => p.clone(),
                None => {
                    let p = std::env::temp_dir().join(format!(
                        "obsidian-to-logseq-{}.edn",
                        sanitize_file_name(graph)
                    ));
                    write_edn(&conversion, &p)?;
                    p
                }
            };
            bail!(
                "Logseq rejected the import ({label}): {e:#}\nThe EDN was written to {}\n{details}",
                kept.display()
            );
        }
    }
    if !conversion.assets.is_empty() {
        copy_assets(&conversion, &graph_dir.join("assets"))?;
    }
    if let Some(&sample) = conversion.sample_page_uuids.first()
        && !cli.entity_exists(graph, sample)?
    {
        bail!("import reported success but converted pages are missing from graph {graph:?}");
    }
    let pages = cli.count(graph, "[:find (count ?p) . :where [?p :block/name]]")?;
    println!(
        "\nImported {} notes into Logseq DB graph {graph:?} ({pages} pages in graph).",
        conversion.stats.notes
    );
    println!("Graph folder: {}", graph_dir.display());
    Ok(())
}

struct WorkerGuard<'a> {
    cli: &'a LogseqCli,
    graph: &'a str,
}

impl Drop for WorkerGuard<'_> {
    fn drop(&mut self) {
        // Fails harmlessly when no worker is running.
        let _ = self.cli.stop_server(self.graph);
    }
}

fn default_output(vault: &Vault) -> PathBuf {
    let name = vault
        .root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "vault".to_string());
    PathBuf::from(format!("{}.edn", sanitize_file_name(&name)))
}

fn sanitize_file_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn write_edn(conversion: &Conversion, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, conversion.edn.to_edn_string())
        .with_context(|| format!("cannot write {}", path.display()))
}

fn copy_assets(conversion: &Conversion, dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    for asset in &conversion.assets {
        let dest = dir.join(&asset.file_name);
        fs::copy(&asset.source, &dest).with_context(|| {
            format!(
                "cannot copy {} to {}",
                asset.source.display(),
                dest.display()
            )
        })?;
    }
    Ok(())
}

fn report(args: &Args, conversion: &Conversion) {
    let s = &conversion.stats;
    eprintln!(
        "Converted {} notes: {} pages, {} journals, {} tags, {} properties, {} blocks, {} assets",
        s.notes, s.pages, s.journals, s.classes, s.properties, s.blocks, s.assets
    );
    if s.placeholder_pages + s.alias_pages > 0 {
        eprintln!(
            "  plus {} pages for unresolved links and {} alias pages",
            s.placeholder_pages, s.alias_pages
        );
    }
    if s.templates + s.favorites > 0 {
        eprintln!("  {} templates and {} favorites", s.templates, s.favorites);
    }
    if s.unreferenced_attachments > 0 {
        eprintln!(
            "  {} attachments are not linked from any note and were skipped",
            s.unreferenced_attachments
        );
    }
    let limit = if args.verbose { usize::MAX } else { 20 };
    for w in conversion.warnings.iter().take(limit) {
        eprintln!("warning: {w}");
    }
    if conversion.warnings.len() > limit {
        eprintln!(
            "... and {} more warnings (use --verbose to see all)",
            conversion.warnings.len() - limit
        );
    }
}
