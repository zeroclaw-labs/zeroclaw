#![allow(dead_code)]
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use directories::ProjectDirs;
use futures::{StreamExt, TryStreamExt};

use graph_builder::prelude::Idx;
use rand::prelude::*;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const GRAPHALYTICS: &str = "https://pub-383410a98aef4cb686f0c7601eddd25f.r2.dev/graphalytics/";

/// Downloads a Graph 500 edge list from
/// [LDBC](https://ldbcouncil.org/benchmarks/graphalytics/)
/// and decompresses it into the application cache
/// directory.
///
/// The file is only downloaded if it's not already present.
///
/// Available scale factors are 22..=30
pub fn create_graph_500(scale: usize) -> Result<PathBuf> {
    let proj_dirs = ProjectDirs::from("", "", "graph").unwrap();

    let cache_dir = proj_dirs.cache_dir();
    let download_dir = cache_dir
        .join("datasets")
        .join(format!("graph-500-{scale}"));
    if !download_dir.exists() {
        std::fs::create_dir_all(&download_dir)?;
        let from = format!("{GRAPHALYTICS}graph500-{scale}.tar.zst");

        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async { download_and_decompress(&from, &download_dir).await })?;
    }

    Ok(download_dir.join(format!("graph500-{}.e", scale)))
}

async fn download_and_decompress<P: AsRef<Path>>(url: &str, download: P) -> Result<()> {
    let response = reqwest::get(url).await?.bytes_stream();
    let response = response.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e));
    let bufread = tokio_util::io::StreamReader::new(response);
    let bufread = async_compression::tokio::bufread::ZstdDecoder::new(bufread);
    let bufread = tokio::io::BufReader::new(bufread);
    let mut tar_archive = tokio_tar::Archive::new(bufread);
    let mut entries = tar_archive.entries()?;

    while let Some(file) = entries.next().await {
        let mut f = file?;
        let path = f.path()?;

        if path.extension() == Some(OsStr::new("e")) {
            f.unpack_in(&download).await?;
        }
    }

    Ok(())
}

#[derive(Clone, Copy)]
pub struct Input {
    pub name: &'static str,
    pub node_count: usize,
    pub edge_count: usize,
}

pub const SMALL: Input = Input {
    name: "small",
    node_count: 1_000,
    edge_count: 10_000,
};

pub const MEDIUM: Input = Input {
    name: "medium",
    node_count: 10_000,
    edge_count: 100_000,
};

pub const LARGE: Input = Input {
    name: "large",
    node_count: 100_000,
    edge_count: 1_000_000,
};

pub fn uniform_edge_list<NI, EV, F>(
    node_count: usize,
    edge_count: usize,
    edge_value: F,
) -> Vec<(NI, NI, EV)>
where
    NI: Idx,
    F: Fn(NI, NI) -> EV,
{
    let mut rng = StdRng::seed_from_u64(42);

    (0..edge_count)
        .map(|_| {
            let source = NI::new(rng.gen_range(0..node_count));
            let target = NI::new(rng.gen_range(0..node_count));

            (source, target, edge_value(source, target))
        })
        .collect::<Vec<_>>()
}

pub fn node_values<NV, F>(node_count: usize, node_value: F) -> Vec<NV>
where
    F: Fn(usize, &mut StdRng) -> NV,
{
    let mut rng = StdRng::seed_from_u64(42);

    (0..node_count)
        .map(|n| node_value(n, &mut rng))
        .collect::<Vec<_>>()
}
